//! # Web Module
//!
//! Embedded HTTP server for Alice Engine.
//! Replaces the standalone Java web server.
//!
//! ## Architecture
//!
//! Web and Engine run in the same process:
//! - Engine heartbeat loop runs in a dedicated OS thread
//! - HTTP server runs on the tokio async runtime
//! - Chat communication via SQLite (WAL mode, separate connections)
//! - Future: direct memory sharing for observe/inference APIs
//!
//! ## Security
//!
//! All routes except /login and /api/auth require authentication.
//! Authentication via:
//! - Cookie: alice_session={sha256(secret+salt)}, HttpOnly, SameSite=Strict
//! - Bearer token: Authorization: Bearer {secret}

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use chrono::TimeZone;
use axum::{
    Router,
    routing::{get, post},
    extract::{State, Path as AxumPath, Query},
    response::{IntoResponse, Response, Redirect},
    http::{StatusCode, HeaderMap, header},
    Json,
};
use serde::Deserialize;
use sha2::{Sha256, Digest};
use tracing::{info, warn};

use crate::chat::ChatHistory;

use tokio::sync::RwLock;

// ─── AppState ────────────────────────────────────────────────────

/// Shared application state for the web server.
pub struct AppState {
    /// Base directory containing all instances.
    pub instances_dir: PathBuf,
    /// Logs directory.
    pub logs_dir: PathBuf,
    /// Authentication secret (password).
    pub auth_secret: String,
    /// User ID for messages.
    pub user_id: String,
    /// Pre-computed session token: SHA-256(secret + salt).
    pub session_token: String,
    /// Directory containing static web files (index.html, login.html, etc.)
    pub web_dir: PathBuf,
    /// Bind address for the web server.
    pub bind_addr: String,
    /// Whether setup page is enabled (for first-run configuration).
    pub setup_enabled: bool,
    /// Whether to skip authentication (for local/dev use).
    pub skip_auth: bool,
    /// Base directory (parent of instances/, logs/, web/)
    pub base_dir: PathBuf,
    /// Cached ChatHistory connections per instance (connection reuse).
    chat_connections: RwLock<HashMap<String, Arc<Mutex<ChatHistory>>>>,
}

impl AppState {
    pub fn new(
        instances_dir: PathBuf,
        logs_dir: PathBuf,
        auth_secret: String,
        user_id: String,
        web_dir: PathBuf,
        bind_addr: String,
        setup_enabled: bool,
        skip_auth: bool,
        base_dir: PathBuf,
    ) -> Self {
        let session_token = compute_session_token(&auth_secret);
        Self {
            instances_dir,
            logs_dir,
            auth_secret,
            user_id,
            session_token,
            web_dir,
            bind_addr,
            setup_enabled,
            skip_auth,
            base_dir,
            chat_connections: RwLock::new(HashMap::new()),
        }
    }

    /// Get or create a cached ChatHistory connection for an instance.
    pub(crate) async fn get_chat(&self, name: &str) -> anyhow::Result<Arc<Mutex<ChatHistory>>> {
        // Fast path: read lock
        {
            let cache = self.chat_connections.read().await;
            if let Some(ch) = cache.get(name) {
                return Ok(ch.clone());
            }
        }
        // Slow path: write lock, open connection
        let mut cache = self.chat_connections.write().await;
        // Double-check after acquiring write lock
        if let Some(ch) = cache.get(name) {
            return Ok(ch.clone());
        }
        let db_path = self.instances_dir.join(name).join("data").join("chat.db");
        let ch = ChatHistory::open(&db_path)?;
        let arc = Arc::new(Mutex::new(ch));
        cache.insert(name.to_string(), arc.clone());
        Ok(arc)
    }
}

fn compute_session_token(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{}:alice-session-salt", secret).as_bytes());
    hex::encode(hasher.finalize())
}

// ─── Auth helpers ────────────────────────────────────────────────

const SESSION_COOKIE_NAME: &str = "alice_session";

/// Extract session token from cookies.
fn get_session_token_from_cookies(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for cookie in cookie_header.split(';') {
        let trimmed = cookie.trim();
        if let Some(value) = trimmed.strip_prefix(&format!("{}=", SESSION_COOKIE_NAME)) {
            return Some(value.to_string());
        }
    }
    None
}

/// Check if request is authenticated.
fn is_authenticated(headers: &HeaderMap, state: &AppState) -> bool {
    // Skip auth when configured (e.g. local/dev mode)
    if state.skip_auth {
        return true;
    }
    // Check cookie
    if let Some(token) = get_session_token_from_cookies(headers) {
        if token == state.session_token {
            return true;
        }
    }
    // Check Bearer token
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                if token == state.auth_secret {
                    return true;
                }
            }
        }
    }
    false
}

// ─── JSON helpers ────────────────────────────────────────────────

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({"error": msg}))).into_response()
}



// ─── Route handlers ──────────────────────────────────────────────

// --- Auth ---

async fn handle_auth(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Response {
    let secret = body.trim();
    if secret == state.auth_secret {
        let cookie = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800",
            SESSION_COOKIE_NAME, state.session_token
        );
        let mut headers = HeaderMap::new();
        headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
        info!("Auth success");
        (headers, Json(serde_json::json!({"status": "ok"}))).into_response()
    } else {
        warn!("Auth failed");
        json_error(StatusCode::FORBIDDEN, "Invalid secret")
    }
}

async fn handle_auth_check(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if is_authenticated(&headers, &state) {
        Json(serde_json::json!({"authenticated": true})).into_response()
    } else {
        Json(serde_json::json!({"authenticated": false})).into_response()
    }
}

async fn handle_logout() -> Response {
    let cookie = format!(
        "{}=; Path=/; HttpOnly; Max-Age=0",
        SESSION_COOKIE_NAME
    );
    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
    (headers, Json(serde_json::json!({"status": "logged_out"}))).into_response()
}

// --- Static pages ---

async fn handle_login_page(State(state): State<Arc<AppState>>) -> Response {
    serve_file(&state.web_dir.join("login.html"), "text/html; charset=utf-8").await
}

async fn handle_index_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return Redirect::to("/login").into_response();
    }
    // If setup is enabled and no instances exist, redirect to setup
    if state.setup_enabled {
        if let Ok(entries) = std::fs::read_dir(&state.instances_dir) {
            let has_instance = entries
                .filter_map(|e| e.ok())
                .any(|e| {
                    let p = e.path();
                    p.is_dir() && !e.file_name().to_string_lossy().starts_with('.')
                        && p.join("settings.json").exists()
                });
            if !has_instance {
                return Redirect::to("/setup").into_response();
            }
        }
    }
    serve_file(&state.web_dir.join("index.html"), "text/html; charset=utf-8").await
}

async fn handle_backup_page(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return Redirect::to("/login").into_response();
    }
    serve_file(&state.web_dir.join("backup.html"), "text/html; charset=utf-8").await
}

async fn handle_setup_page(
    State(state): State<Arc<AppState>>,
) -> Response {
    serve_file(&state.web_dir.join("setup.html"), "text/html; charset=utf-8").await
}


async fn handle_knowledge_page(
    State(state): State<Arc<AppState>>,
) -> Response {
    serve_file(&state.web_dir.join("knowledge.html"), "text/html; charset=utf-8").await
}

async fn handle_setup_api(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response {
    // Only allow setup when setup is enabled and no instances exist
    if !state.setup_enabled {
        return json_error(StatusCode::FORBIDDEN, "Setup not enabled");
    }

    // Check if any instance already exists
    if let Ok(entries) = std::fs::read_dir(&state.instances_dir) {
        let has_instance = entries
            .filter_map(|e| e.ok())
            .any(|e| {
                let p = e.path();
                p.is_dir() && !e.file_name().to_string_lossy().starts_with('.')
                    && p.join("settings.json").exists()
            });
        if has_instance {
            return json_error(StatusCode::CONFLICT, "An instance already exists");
        }
    }

    let api_key = body.get("api_key").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("anthropic/claude-opus-4.6").trim().to_string();
    let api_url = body.get("api_url").and_then(|v| v.as_str()).unwrap_or("zenmux").trim().to_string();

    if api_key.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "api_key is required");
    }

    // Build model string: api_url@model (api_url can be shortcut like "openrouter" or a full URL)
    let full_model = if api_url.is_empty() {
        format!("zenmux@{}", model)
    } else {
        format!("{}@{}", api_url, model)
    };

    // Generate 6-char random hex name
    let name: String = (0..6).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect();
    let instance_dir = state.instances_dir.join(&name);

    if let Err(e) = std::fs::create_dir_all(&instance_dir) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to create dir: {}", e));
    }

    // Random color from presets
    const PRESET_COLORS: &[&str] = &[
        "#6c5ce7", "#00b894", "#e17055", "#0984e3", "#fdcb6e",
        "#e84393", "#00cec9", "#a29bfe", "#ff7675", "#55efc4",
    ];
    let color = PRESET_COLORS[rand::random::<usize>() % PRESET_COLORS.len()];

    let settings = serde_json::json!({
        "api_key": api_key,
        "model": full_model,
        "user_id": "user",
        "color": color,
    });

    let settings_path = instance_dir.join("settings.json");
    if let Err(e) = std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to write settings: {}", e));
    }

    info!("[WEB] Setup: created instance {} (awaiting engine hot-scan)", name);

    Json(serde_json::json!({"success": true, "instance": name})).into_response()
}

async fn serve_file(path: &Path, content_type: &str) -> Response {
    match tokio::fs::read(path).await {
        Ok(data) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
            (headers, data).into_response()
        }
        Err(_) => json_error(StatusCode::NOT_FOUND, "File not found"),
    }
}

// --- Instance API ---

async fn handle_list_instances(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let instances_dir = state.instances_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        list_instances_sync(&instances_dir)
    }).await;

    match result {
        Ok(Ok(instances)) => Json(instances).into_response(),
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// Create a new instance: generate random 6-hex name, write minimal settings.json.
/// Engine hot-scan will discover and initialize the instance.
async fn handle_create_instance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    // Use shared instance creation logic
    let (name, _instance_dir) = match crate::engine::create_instance_dir(
        &state.instances_dir, &state.user_id, None,
    ) {
        Ok(result) => result,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };

    info!("[WEB] Created new instance: {} (awaiting engine hot-scan)", name);

    Json(serde_json::json!({
        "name": name,
        "status": "created",
    })).into_response()
}

fn list_instances_sync(instances_dir: &Path) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut instances = Vec::new();
    let entries = std::fs::read_dir(instances_dir)?;

    for entry in entries {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden directories (e.g. .trash)
        if name.starts_with('.') {
            continue;
        }
        let settings_path = entry.path().join("settings.json");
        if !settings_path.exists() {
            continue;
        }
        let mut display_name = name.clone();
        let mut idle = true;
        let mut last_active: i64 = 0;
        let mut unread: i64 = 0;
        let mut engine_online = false;
        let mut born = false;
        let mut idle_timeout_secs: Option<i64> = None;
        let mut idle_since: Option<i64> = None;

        // Read settings for display name, color, avatar, privileged
        let mut color: Option<String> = None;
        let mut avatar: Option<String> = None;
        let mut privileged = false;
        let settings_path = entry.path().join("settings.json");
        if let Ok(content) = std::fs::read_to_string(&settings_path) {
            if let Ok(settings_val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(val) = settings_val.get("name").and_then(|v| v.as_str()) {
                    if !val.is_empty() {
                        display_name = val.to_string();
                    }
                }
                color = settings_val.get("color").and_then(|v| v.as_str()).map(|s| s.to_string());
                avatar = settings_val.get("avatar").and_then(|v| v.as_str()).map(|s| s.to_string());
                privileged = settings_val.get("privileged").and_then(|v| v.as_bool()).unwrap_or(false);
            }
        }

        // Read engine status from chat.db (lightweight query, no full ChatHistory init)
        let db_path = entry.path().join("data").join("chat.db");
        if db_path.exists() {
            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                if let Ok(status_json) = conn.query_row(
                    "SELECT value FROM engine_status WHERE key = 'status'",
                    [],
                    |row| row.get::<_, String>(0),
                ) {
                    let (online, is_idle, _, _, is_born) = parse_engine_status(&status_json);
                    engine_online = online;
                    idle = is_idle;
                    born = is_born;
                    unread = extract_json_i64_simple(&status_json, "unread").unwrap_or(0);
                    idle_timeout_secs = extract_json_i64_simple(&status_json, "idleTimeoutSecs");
                    idle_since = extract_json_i64_simple(&status_json, "idleSince");
                // lastActive: use latest message id for sorting (like IM conversation list)
                if let Ok(max_id) = conn.query_row(
                    "SELECT COALESCE(MAX(id), 0) FROM messages",
                    [],
                    |row| row.get::<_, i64>(0),
                ) {
                    last_active = max_id;
                }
                }
            }
        }

        let mut inst_json = serde_json::json!({
            "id": name,
            "name": name,
            "displayName": display_name,
            "lastActive": last_active,
            "unread": unread,
            "idle": idle,
            "engineOnline": engine_online,
            "born": born,
            "privileged": privileged,
            "idleTimeoutSecs": idle_timeout_secs,
            "idleSince": idle_since,
        });
        if let Some(c) = color {
            inst_json["color"] = serde_json::json!(c);
        }
        if let Some(a) = avatar {
            inst_json["avatar"] = serde_json::json!(a);
        }
        // Read knowledge size
        let knowledge_path = entry.path().join("memory").join("knowledge.md");
        let knowledge_size = knowledge_path.metadata().map(|m| m.len()).unwrap_or(0);
        inst_json["knowledgeSize"] = serde_json::json!(knowledge_size);

        instances.push(inst_json);
    }

    instances.sort_by(|a, b| {
        a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
    });

    Ok(instances)
}

fn extract_json_bool_simple(json: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if rest.starts_with("true") { Some(true) }
    else if rest.starts_with("false") { Some(false) }
    else { None }
}

pub(crate) fn extract_json_i64_simple(json: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub(crate) fn extract_json_string_simple(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if rest.starts_with("null") { return None; }
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Parse engine status JSON to determine if engine is online and idle.
/// Engine writes two formats:
///   inferring: {"status":"inferring","instance":"...","logPath":"..."}
///   idle:      {"status":"idle","instance":"...","lastBeat":"20260222083246","duration":7.4}
/// Returns (engine_online, idle, is_inferring, infer_log_path)
pub(crate) fn parse_engine_status(status_json: &str) -> (bool, bool, bool, Option<String>, bool) {
    let status_str = extract_json_string_simple(status_json, "status")
        .unwrap_or_default();
    let is_inferring = status_str == "inferring";
    let idle = !is_inferring;

    // Parse lastBeat timestamp string "YYYYMMDDHHmmSS" to millis
    let last_beat_ms = extract_json_string_simple(status_json, "lastBeat")
        .and_then(|s| parse_timestamp_to_millis(&s))
        .unwrap_or(0);

    // If inferring, engine is definitely online.
    // If idle, check lastBeat within 30 seconds.
    let engine_online = if is_inferring {
        true
    } else {
        let now_ms = chrono::Utc::now().timestamp_millis();
        (now_ms - last_beat_ms) < 30000
    };

    // logPath is used during inferring
    let infer_log_path = extract_json_string_simple(status_json, "logPath");

    // born: instance has completed first idle (ready for user interaction)
    let born = extract_json_bool_simple(status_json, "born").unwrap_or(false);

    (engine_online, idle, is_inferring, infer_log_path, born)
}

/// Parse "20260222083246" format to milliseconds since epoch.
fn parse_timestamp_to_millis(s: &str) -> Option<i64> {
    if s.len() < 14 { return None; }
    let dt = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S").ok()?;
    let local = chrono::Local.from_local_datetime(&dt).single()?;
    Some(local.timestamp_millis())
}

// --- Send message ---

async fn handle_send_message(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    body: String,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let body = body.trim().to_string();
    if body.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Empty message");
    }

    let instance_dir = state.instances_dir.join(&name);
    if !instance_dir.exists() {
        return json_error(StatusCode::NOT_FOUND, "Instance not found");
    }

    let user_id = state.user_id.clone();
    let ch = match state.get_chat(&name).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let result = tokio::task::spawn_blocking(move || {
        let mut ch = ch.lock().unwrap_or_else(|e| e.into_inner());
        let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        ch.write_user_message(&user_id, &body, &timestamp, "chat")?;
        info!("[MSG] Web: message sent to {}", name);
        Ok::<_, anyhow::Error>(())
    }).await;

    match result {
        Ok(Ok(())) => Json(serde_json::json!({"status": "sent"})).into_response(),
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --- Get replies ---

async fn handle_get_replies(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<RepliesQuery>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let after_id = query.after.unwrap_or(0);
    let ch = match state.get_chat(&name).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let result = tokio::task::spawn_blocking(move || {
        let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
        ch.get_agent_replies_after(after_id)
    }).await;

    match result {
        Ok(Ok(replies)) => {
            let json_replies: Vec<serde_json::Value> = replies.into_iter().map(|(id, content, timestamp)| {
                serde_json::json!({"id": id, "content": content, "timestamp": timestamp})
            }).collect();
            Json(json_replies).into_response()
        },
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --- Get history ---

#[derive(Deserialize)]
struct RepliesQuery {
    after: Option<i64>,
}

#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
    before: Option<i64>,
}

async fn handle_get_history(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let limit = query.limit.unwrap_or(50).max(1).min(500);
    let before = query.before.unwrap_or(0).max(0);

    let ch = match state.get_chat(&name).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let result = tokio::task::spawn_blocking(move || {
        let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
        ch.query(limit, before)
    }).await;

    match result {
        Ok(Ok(qr)) => {
            let messages: Vec<serde_json::Value> = qr.messages.iter().map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "role": m.role,
                    "content": m.content,
                    "timestamp": m.timestamp,
                })
            }).collect();
            Json(serde_json::json!({
                "total": qr.total,
                "startId": qr.start_id,
                "hasMore": qr.has_more,
                "messages": messages,
            })).into_response()
        }
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --- Observe ---

async fn handle_observe(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let ch = match state.get_chat(&name).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let result = tokio::task::spawn_blocking(move || {
        let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
        let status = ch.read_status()?;
        Ok::<_, anyhow::Error>(status)
    }).await;

    match result {
        Ok(Ok(Some(status_json))) => {
            let (engine_online, idle, is_inferring, infer_log_path, born) = parse_engine_status(&status_json);
            let current_doing = extract_json_string_simple(&status_json, "currentDoing");
            let executing_script = extract_json_string_simple(&status_json, "executingScript");

            let infer_output = if let Some(ref path) = infer_log_path {
                std::fs::read_to_string(path).ok()
            } else {
                None
            };

            let recent_actions = extract_json_array_raw(&status_json, "recentDoings")
                .unwrap_or_else(|| "[]".to_string());

            let idle_timeout_secs = extract_json_i64_simple(&status_json, "idleTimeoutSecs");
            let idle_since = extract_json_i64_simple(&status_json, "idleSince");
            let active_model = extract_json_i64_simple(&status_json, "activeModel").unwrap_or(0);
            let model_count = extract_json_i64_simple(&status_json, "modelCount").unwrap_or(1);

            let response = format!(
                r#"{{"inferring":{},"inferOutput":{},"currentAction":{},"executingScript":{},"idle":{},"engineOnline":{},"born":{},"recentActions":{},"idleTimeoutSecs":{},"idleSince":{},"activeModel":{},"modelCount":{}}}"#,
                is_inferring,
                match &infer_output {
                    Some(s) => format!("\"{}\"", escape_json(s)),
                    None => "null".to_string(),
                },
                match &current_doing {
                    Some(s) => format!("\"{}\"", escape_json(s)),
                    None => "null".to_string(),
                },
                match &executing_script {
                    Some(s) => format!("\"{}\"", escape_json(s)),
                    None => "null".to_string(),
                },
                idle,
                engine_online,
                born,
                recent_actions,
                match idle_timeout_secs {
                    Some(v) => v.to_string(),
                    None => "null".to_string(),
                },
                match idle_since {
                    Some(v) => v.to_string(),
                    None => "null".to_string(),
                },
                active_model,
                model_count,
            );

            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "application/json; charset=utf-8".parse().unwrap());
            (headers, response).into_response()
        }
        Ok(Ok(None)) => {
            Json(serde_json::json!({
                "inferring": false,
                "inferOutput": null,
                "currentAction": null,
                "executingScript": null,
                "idle": true,
                "recentActions": [],
                "engineOnline": false,
                "idleTimeoutSecs": null,
                "idleSince": null,
            })).into_response()
        }
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --- Inference ---

async fn handle_inference(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let ch = match state.get_chat(&name).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let result = tokio::task::spawn_blocking(move || {
        let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
        let status = ch.read_status()?;
        Ok::<_, anyhow::Error>(status)
    }).await;

    match result {
        Ok(Ok(Some(status_json))) => {
            let (_, _, is_inferring, infer_log_path, _) = parse_engine_status(&status_json);
            let infer_output = if let Some(ref path) = infer_log_path {
                std::fs::read_to_string(path).ok()
            } else {
                None
            };

            Json(serde_json::json!({
                "inferring": is_inferring,
                "output": infer_output,
            })).into_response()
        }
        Ok(Ok(None)) => Json(serde_json::json!({"inferring": false, "output": null})).into_response(),
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --- Actions ---

async fn handle_actions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let ch = match state.get_chat(&name).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let result = tokio::task::spawn_blocking(move || {
        let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
        let status = ch.read_status()?;
        Ok::<_, anyhow::Error>(status)
    }).await;

    match result {
        Ok(Ok(Some(status_json))) => {
            let (_, _, is_inferring, _, _) = parse_engine_status(&status_json);
            let current_doing = extract_json_string_simple(&status_json, "currentDoing");
            let recent = extract_json_array_raw(&status_json, "recentDoings")
                .unwrap_or_else(|| "[]".to_string());

            let current = match (&current_doing, is_inferring) {
                (Some(d), _) => format!("\"{}\"", escape_json(d)),
                (None, true) => "\"inferring...\"".to_string(),
                _ => "null".to_string(),
            };

            let response = format!(
                r#"{{"current":{},"doings":{}}}"#,
                current, recent
            );

            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "application/json; charset=utf-8".parse().unwrap());
            (headers, response).into_response()
        }
        Ok(Ok(None)) => Json(serde_json::json!({"current": null, "doings": []})).into_response(),
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --- Interrupt ---

async fn handle_get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let instance_dir = state.instances_dir.join(&name);
    let settings_path = instance_dir.join("settings.json");
    if !settings_path.exists() {
        return json_error(StatusCode::NOT_FOUND, "Instance not found");
    }

    let content = match tokio::fs::read_to_string(&settings_path).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to read settings: {}", e)),
    };
    let mut settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to parse settings: {}", e)),
    };

    // Mask api_key: show only last 4 chars
    if let Some(key) = settings.get("api_key").and_then(|v| v.as_str()) {
        if key.len() > 4 {
            settings["api_key"] = serde_json::json!(format!("...{}", &key[key.len()-4..]));
        }
    }
    // Mask extra_models api_keys
    if let Some(extras) = settings.get_mut("extra_models") {
        if let Some(arr) = extras.as_array_mut() {
            for item in arr.iter_mut() {
                if let Some(key) = item.get("api_key").and_then(|v| v.as_str()).map(|s| s.to_string()) {
                    if key.len() > 4 {
                        item["api_key"] = serde_json::json!(format!("...{}", &key[key.len()-4..]));
                    }
                }
            }
        }
    }

    Json(settings).into_response()
}

async fn handle_update_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let instance_dir = state.instances_dir.join(&name);
    let settings_path = instance_dir.join("settings.json");
    if !settings_path.exists() {
        return json_error(StatusCode::NOT_FOUND, "Instance not found");
    }

    // Read current settings
    let content = match tokio::fs::read_to_string(&settings_path).await {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to read settings: {}", e)),
    };
    let mut settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to parse settings: {}", e)),
    };

    // Update allowed fields
    let mut updated = Vec::new();
    let string_fields = ["name", "avatar", "color", "api_key", "model"];
    for field in &string_fields {
        if let Some(val) = body.get(*field).and_then(|v| v.as_str()) {
            let val = val.trim();
            if !val.is_empty() {
                settings[*field] = serde_json::json!(val);
                if *field == "api_key" {
                    updated.push(format!("{}: ...{}", field, &val[val.len().saturating_sub(4)..]));
                } else {
                    updated.push(format!("{}: {}", field, val));
                }
            }
        }
    }
    if let Some(val) = body.get("privileged") {
        if let Some(b) = val.as_bool() {
            settings["privileged"] = serde_json::json!(b);
            updated.push(format!("privileged: {}", b));
        }
    }
    if let Some(val) = body.get("extra_models") {
        if let Some(arr) = val.as_array() {
            settings["extra_models"] = serde_json::json!(arr);
            updated.push(format!("extra_models: {} items", arr.len()));
        }
    }

    if updated.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "No valid fields to update");
    }

    // Write back
    let new_content = match serde_json::to_string_pretty(&settings) {
        Ok(c) => c,
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to serialize: {}", e)),
    };
    if let Err(e) = tokio::fs::write(&settings_path, &new_content).await {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to write settings: {}", e));
    }

    info!("[WEB] Settings updated for {}: {}", name, updated.join(", "));
    Json(serde_json::json!({"status": "ok", "updated": updated})).into_response()
}

async fn handle_interrupt(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let instance_dir = state.instances_dir.join(&name);
    if !instance_dir.exists() {
        return json_error(StatusCode::NOT_FOUND, "Instance not found");
    }

    let signal_file = instance_dir.join("interrupt.signal");
    match tokio::fs::File::create(&signal_file).await {
        Ok(_) => {
            info!("Interrupt signal written for instance: {}", name);
            Json(serde_json::json!({"status": "interrupt_requested"})).into_response()
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn handle_switch_model(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let instance_dir = state.instances_dir.join(&name);
    if !instance_dir.exists() {
        return json_error(StatusCode::NOT_FOUND, "Instance not found");
    }

    let index = match body.get("index").and_then(|v| v.as_u64()) {
        Some(i) => i.to_string(),
        None => return json_error(StatusCode::BAD_REQUEST, "Missing or invalid 'index' field"),
    };

    let signal_file = instance_dir.join("switch-model.signal");
    match tokio::fs::write(&signal_file, &index).await {
        Ok(_) => {
            info!("[WEB] Switch-model signal written for {}: index={}", name, index);
            Json(serde_json::json!({"status": "ok", "index": index})).into_response()
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// --- File serving (workspace) ---

async fn handle_serve_static(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath((name, path)): AxumPath<(String, String)>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let workspace = state.instances_dir.join(&name).join("workspace");
    serve_workspace_file(&workspace, &path).await
}

async fn handle_public_static(
    State(state): State<Arc<AppState>>,
    AxumPath((name, path)): AxumPath<(String, String)>,
) -> Response {
    if !path.starts_with("apps/") {
        return json_error(StatusCode::FORBIDDEN, "Public access only allowed for apps/ directory");
    }

    let workspace = state.instances_dir.join(&name).join("workspace");
    serve_workspace_file(&workspace, &path).await
}

async fn serve_workspace_file(workspace: &Path, rel_path: &str) -> Response {
    if !workspace.exists() {
        return json_error(StatusCode::NOT_FOUND, "Workspace not found");
    }

    let target = match workspace.join(rel_path).canonicalize() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::NOT_FOUND, "File not found"),
    };

    let workspace_canonical = match workspace.canonicalize() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Cannot resolve workspace"),
    };

    if !target.starts_with(&workspace_canonical) {
        return json_error(StatusCode::FORBIDDEN, "Access denied");
    }

    if !target.is_file() {
        return json_error(StatusCode::NOT_FOUND, "File not found");
    }

    let content_type = guess_content_type(&target);
    match tokio::fs::read(&target).await {
        Ok(data) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
            (headers, data).into_response()
        }
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file"),
    }
}

// --- File browser ---

#[derive(Deserialize)]
struct FileQuery {
    path: Option<String>,
}

async fn handle_file_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<FileQuery>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let workspace = state.instances_dir.join(&name).join("workspace");
    if !workspace.exists() {
        return json_error(StatusCode::NOT_FOUND, "Workspace not found");
    }

    let rel_path = query.path.unwrap_or_default();
    let target = match workspace.join(&rel_path).canonicalize() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::NOT_FOUND, "Not a directory"),
    };

    let workspace_canonical = match workspace.canonicalize() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Cannot resolve workspace"),
    };

    if !target.starts_with(&workspace_canonical) {
        return json_error(StatusCode::FORBIDDEN, "Access denied");
    }

    if !target.is_dir() {
        return json_error(StatusCode::NOT_FOUND, "Not a directory");
    }

    let result = tokio::task::spawn_blocking(move || {
        let mut items = Vec::new();
        let entries = std::fs::read_dir(&target)?;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            let metadata = entry.metadata()?;
            let mut item = serde_json::json!({
                "name": name,
                "type": if metadata.is_dir() { "dir" } else { "file" },
            });
            if metadata.is_file() {
                item["size"] = serde_json::json!(metadata.len());
            }
            items.push(item);
        }
        items.sort_by(|a, b| {
            let a_type = a["type"].as_str().unwrap_or("");
            let b_type = b["type"].as_str().unwrap_or("");
            if a_type != b_type {
                if a_type == "dir" { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }
            } else {
                a["name"].as_str().unwrap_or("").to_lowercase()
                    .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
            }
        });
        Ok::<_, anyhow::Error>(serde_json::json!({
            "path": rel_path,
            "items": items,
        }))
    }).await;

    match result {
        Ok(Ok(data)) => Json(data).into_response(),
        Ok(Err(e)) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}


async fn handle_get_knowledge(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let knowledge_path = state.instances_dir.join(&name).join("memory").join("knowledge.md");
    if !knowledge_path.exists() {
        return Json(serde_json::json!({
            "content": "",
            "size": 0
        })).into_response();
    }

    match std::fs::read_to_string(&knowledge_path) {
        Ok(content) => {
            let size = content.len();
            Json(serde_json::json!({
                "content": content,
                "size": size
            })).into_response()
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to read knowledge: {}", e)),
    }
}

async fn handle_file_read(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<FileQuery>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let workspace = state.instances_dir.join(&name).join("workspace");
    if !workspace.exists() {
        return json_error(StatusCode::NOT_FOUND, "Workspace not found");
    }

    let rel_path = query.path.unwrap_or_default();
    let target = match workspace.join(&rel_path).canonicalize() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::NOT_FOUND, "File not found"),
    };

    let workspace_canonical = match workspace.canonicalize() {
        Ok(p) => p,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Cannot resolve workspace"),
    };

    if !target.starts_with(&workspace_canonical) {
        return json_error(StatusCode::FORBIDDEN, "Access denied");
    }

    if !target.is_file() {
        return json_error(StatusCode::NOT_FOUND, "File not found");
    }

    let metadata = match target.metadata() {
        Ok(m) => m,
        Err(_) => return json_error(StatusCode::NOT_FOUND, "File not found"),
    };

    if metadata.len() > 1024 * 1024 {
        return json_error(StatusCode::BAD_REQUEST, "File too large (>1MB)");
    }

    let file_name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
    if is_binary_file(&file_name) {
        return Json(serde_json::json!({
            "binary": true,
            "name": file_name,
            "size": metadata.len(),
            "path": rel_path,
        })).into_response();
    }

    match tokio::fs::read_to_string(&target).await {
        Ok(content) => Json(serde_json::json!({
            "binary": false,
            "name": file_name,
            "path": rel_path,
            "size": metadata.len(),
            "content": content,
        })).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file"),
    }
}

// --- Reverse proxy ---

async fn handle_proxy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    method: axum::http::Method,
    body: axum::body::Bytes,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let path = uri.path();
    let rest = match path.strip_prefix("/proxy/") {
        Some(r) => r,
        None => return json_error(StatusCode::BAD_REQUEST, "Invalid proxy path"),
    };

    let (port_str, target_path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], &rest[pos..]),
        None => (rest, "/"),
    };

    let port: u16 = match port_str.parse() {
        Ok(p) if p >= 1024 => p,
        _ => return json_error(StatusCode::BAD_REQUEST, "Invalid port (must be >= 1024)"),
    };

    let mut target_url = format!("http://localhost:{}{}", port, target_path);
    if let Some(query) = uri.query() {
        target_url.push('?');
        target_url.push_str(query);
    }

    let client = reqwest::Client::new();
    let req_method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        "PATCH" => reqwest::Method::PATCH,
        _ => reqwest::Method::GET,
    };

    let mut req = client.request(req_method, &target_url);

    // Forward request headers (except hop-by-hop and host)
    for (name, value) in headers.iter() {
        match name.as_str() {
            "host" | "connection" | "keep-alive" | "transfer-encoding"
            | "te" | "trailer" | "upgrade" => {} // skip hop-by-hop
            _ => {
                if let Ok(v) = value.to_str() {
                    req = req.header(name.as_str(), v);
                }
            }
        }
    }

    if !body.is_empty() {
        req = req.body(body.to_vec());
    }

    let proxy_prefix = format!("/proxy/{}", port);

    match req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);

            // Collect response headers with transparent rewriting
            let mut out_headers = HeaderMap::new();

            for (name, value) in resp.headers().iter() {
                let name_str = name.as_str().to_lowercase();
                if let Ok(val_str) = value.to_str() {
                    match name_str.as_str() {
                        // Rewrite Location header: /xxx -> /proxy/{port}/xxx
                        "location" => {
                            let rewritten = if val_str.starts_with('/') && !val_str.starts_with(&proxy_prefix) {
                                format!("{}{}", proxy_prefix, val_str)
                            } else {
                                val_str.to_string()
                            };
                            if let Ok(v) = rewritten.parse() {
                                out_headers.insert(name.clone(), v);
                            }
                        }
                        // Rewrite Set-Cookie Path: path=/ -> path=/proxy/{port}/
                        "set-cookie" => {
                            let rewritten = rewrite_cookie_path(val_str, &proxy_prefix);
                            if let Ok(v) = rewritten.parse() {
                                out_headers.append(name.clone(), v);
                            }
                        }
                        // Skip hop-by-hop headers
                        "transfer-encoding" | "connection" | "keep-alive" => {}
                        // Pass through everything else
                        _ => {
                            out_headers.append(name.clone(), value.clone());
                        }
                    }
                }
            }

            // Ensure content-type exists
            if !out_headers.contains_key(header::CONTENT_TYPE) {
                out_headers.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
            }

            match resp.bytes().await {
                Ok(body) => {
                    (status, out_headers, body.to_vec()).into_response()
                }
                Err(e) => json_error(StatusCode::BAD_GATEWAY, &e.to_string()),
            }
        }
        Err(e) => {
            if e.is_connect() {
                json_error(StatusCode::BAD_GATEWAY, &format!("Cannot connect to localhost:{}", port))
            } else if e.is_timeout() {
                json_error(StatusCode::GATEWAY_TIMEOUT, "Proxy timeout")
            } else {
                json_error(StatusCode::BAD_GATEWAY, &e.to_string())
            }
        }
    }
}

/// Rewrite cookie Path attribute for reverse proxy transparency.
/// e.g. "session=abc; Path=/" -> "session=abc; Path=/proxy/9000/"
fn rewrite_cookie_path(cookie: &str, proxy_prefix: &str) -> String {
    // Case-insensitive search for Path=
    let lower = cookie.to_lowercase();
    if let Some(idx) = lower.find("path=") {
        let path_start = idx + 5; // after "path="
        // Find the end of the path value (next ';' or end of string)
        let path_end = cookie[path_start..].find(';')
            .map(|i| path_start + i)
            .unwrap_or(cookie.len());
        let path_val = cookie[path_start..path_end].trim();
        // Only rewrite if path is "/" or doesn't already include proxy prefix
        if path_val == "/" || !path_val.starts_with(proxy_prefix) {
            let new_path = if path_val == "/" {
                format!("{}/", proxy_prefix)
            } else if path_val.starts_with('/') {
                format!("{}{}", proxy_prefix, path_val)
            } else {
                return cookie.to_string(); // relative path, don't touch
            };
            return format!("{}{}{}", &cookie[..path_start], new_path, &cookie[path_end..]);
        }
    }
    cookie.to_string()
}

// ─── Utility ─────────────────────────────────────────────────────

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

pub(crate) fn extract_json_array_raw(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped { escaped = false; continue; }
        if c == '\\' && in_string { escaped = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if in_string { continue; }
        if c == '[' { depth += 1; }
        if c == ']' {
            depth -= 1;
            if depth == 0 {
                return Some(rest[..=i].to_string());
            }
        }
    }
    None
}

fn guess_content_type(path: &Path) -> &'static str {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn is_binary_file(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".jar") || name.ends_with(".class") || name.ends_with(".zip")
        || name.ends_with(".gz") || name.ends_with(".png") || name.ends_with(".jpg")
        || name.ends_with(".jpeg") || name.ends_with(".gif") || name.ends_with(".ico")
        || name.ends_with(".pdf") || name.ends_with(".exe") || name.ends_with(".so")
        || name.ends_with(".wasm")
}

// --- Frontend log ---

/// Request body for frontend log endpoint.
#[derive(Deserialize)]
struct FrontendLogEntry {
    #[serde(default)]
    client_id: String,
    #[serde(default = "default_log_level")]
    level: String,
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    message: String,
}

fn default_log_level() -> String { "INFO".to_string() }

async fn handle_frontend_log(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(entry): Json<FrontendLogEntry>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let client = if entry.client_id.is_empty() { "unknown" } else { &entry.client_id };
    let level = entry.level.to_uppercase();
    let log_type = if entry.r#type.is_empty() { "general" } else { &entry.r#type };

    let line = format!(
        "[{}] [client:{}] [{}] [{}] {}\n",
        timestamp, client, level, log_type, entry.message
    );

    let log_path = state.logs_dir.join("frontend.log");
    // Append to log file (create if not exists)
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Write failed: {}", e));
            }
            Json(serde_json::json!({"status": "ok"})).into_response()
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Open failed: {}", e)),
    }
}

// --- Delete instance ---

async fn handle_delete_instance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if !is_authenticated(&headers, &state) {
        return json_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }

    let instance_dir = state.instances_dir.join(&name);
    if !instance_dir.exists() {
        return json_error(StatusCode::NOT_FOUND, "Instance not found");
    }

    // Safety: refuse to delete if name looks suspicious (path traversal)
    if name.contains('/') || name.contains("..") || name.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Invalid instance name");
    }

    // Move to trash instead of deleting (recycle bin)
    let trash_dir = state.instances_dir.join(".trash");
    if let Err(e) = std::fs::create_dir_all(&trash_dir) {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to create trash dir: {}", e));
    }

    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let trash_name = format!("{}_{}", name, timestamp);
    let trash_path = trash_dir.join(&trash_name);

    match std::fs::rename(&instance_dir, &trash_path) {
        Ok(()) => {
            info!("[WEB] Moved instance to trash: {} -> .trash/{}", name, trash_name);
            Json(serde_json::json!({"status": "deleted", "name": name, "trash": trash_name})).into_response()
        }
        Err(e) => {
            json_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("Failed to move to trash: {}", e))
        }
    }
}

// ─── Router builder ──────────────────────────────────────────────

/// Build the axum router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Auth (no auth required)
        .route("/api/auth", post(handle_auth))
        .route("/api/auth/check", get(handle_auth_check))
        .route("/api/logout", post(handle_logout))
        // Pages
        .route("/", get(handle_index_page))
        .route("/login", get(handle_login_page))
        .route("/backup", get(handle_backup_page))
        .route("/setup", get(handle_setup_page))
        .route("/knowledge", get(handle_knowledge_page))
        // Setup API (no auth required, only works when no instances exist)
        .route("/api/setup", post(handle_setup_api))
        // Instance API
        .route("/api/frontend-log", post(handle_frontend_log))
        .route("/api/instances", get(handle_list_instances).post(handle_create_instance))
        .route("/api/instances/{name}", axum::routing::delete(handle_delete_instance))
        .route("/api/instances/{name}/messages", post(handle_send_message))
        .route("/api/instances/{name}/replies", get(handle_get_replies))
        .route("/api/instances/{name}/history", get(handle_get_history))
        .route("/api/instances/{name}/observe", get(handle_observe))
        .route("/api/instances/{name}/actions", get(handle_actions))
        .route("/api/instances/{name}/inference", get(handle_inference))
        .route("/api/instances/{name}/interrupt", post(handle_interrupt))
        .route("/api/instances/{name}/settings", get(handle_get_settings).post(handle_update_settings))
        .route("/api/instances/{name}/switch-model", post(handle_switch_model))
        // File browser
        .route("/api/instances/{name}/files/list", get(handle_file_list))
        .route("/api/instances/{name}/files/read", get(handle_file_read))
        // Knowledge API
        .route("/api/instances/{name}/knowledge", get(handle_get_knowledge))
        // Static file serving
        .route("/serve/{name}/{*path}", get(handle_serve_static))
        .route("/public/{name}/{*path}", get(handle_public_static))
        // Reverse proxy
        .route("/proxy/{*path}", axum::routing::any(handle_proxy))
        // State
        .with_state(state)
}

/// Start the web server. Returns a future that runs until shutdown.
pub async fn start_server(state: Arc<AppState>, port: u16) -> anyhow::Result<()> {
    let router = build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", state.bind_addr, port)).await?;
    info!("[WEB] Server started on {}:{}", state.bind_addr, port);
    axum::serve(listener, router).await?;
    Ok(())
}