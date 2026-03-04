//! HTTP API routes — all endpoint handlers and router construction.

use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, delete, get, post},
    Json, Router,
};
use serde::Deserialize;


use super::state::EngineState;
use super::types::*;

// ── Helper Functions ──

fn json_ok(body: impl serde::Serialize) -> Response {
    Json(body).into_response()
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    let body = serde_json::json!({ "error": msg });
    (status, Json(body)).into_response()
}

// ── Query Types ──

#[derive(Deserialize)]
pub struct MessagesQuery {
    pub before_id: Option<i64>,
    pub after_id: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct RepliesQuery {
    pub after_id: i64,
}

#[derive(Deserialize)]
pub struct FilePathQuery {
    pub path: Option<String>,
}

#[derive(Deserialize)]
pub struct SendMessageBody {
    pub content: String,
}

#[derive(Deserialize)]
pub struct CreateInstanceBody {
    pub name: Option<String>,
    pub settings: Option<SettingsUpdate>,
}

// ── Instance Handlers ──

async fn handle_get_instances(State(state): State<Arc<EngineState>>) -> Response {
    json_ok(state.get_instances().await)
}

async fn handle_create_instance(
    State(state): State<Arc<EngineState>>,
    Json(body): Json<CreateInstanceBody>,
) -> Response {
    let name = body.name.unwrap_or_default();
    json_ok(state.create_instance(name, body.settings).await)
}

async fn handle_delete_instance(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.delete_instance(id).await)
}

// ── Message Handlers ──

async fn handle_get_messages(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<MessagesQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(50);
    match state.get_messages(id, query.before_id, query.after_id, limit).await {
        Ok(result) => json_ok(result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

async fn handle_send_message(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<SendMessageBody>,
) -> Response {
    json_ok(state.send_message(id, body.content).await)
}

async fn handle_get_replies(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<RepliesQuery>,
) -> Response {
    json_ok(state.get_replies_after(id, query.after_id).await)
}

// ── Observe & Control ──

async fn handle_observe(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.observe(id).await)
}

async fn handle_interrupt(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.interrupt(id).await)
}

// ── Settings ──

async fn handle_get_global_settings(
    State(state): State<Arc<EngineState>>,
) -> Response {
    json_ok(state.get_global_settings().await)
}

async fn handle_update_global_settings(
    State(state): State<Arc<EngineState>>,
    Json(update): Json<SettingsUpdate>,
) -> Response {
    json_ok(state.update_global_settings(update).await)
}

async fn handle_get_settings(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let settings = state.get_settings(id).await;
    // Mask API keys for security
    let mut val = serde_json::to_value(&settings).unwrap_or_default();
    mask_api_keys(&mut val);
    json_ok(val)
}

fn mask_api_keys(val: &mut serde_json::Value) {
    if let Some(obj) = val.as_object_mut() {
        if let Some(key) = obj.get_mut("api_key") {
            if let Some(s) = key.as_str() {
                if s.len() > 8 {
                    *key = serde_json::Value::String(format!("{}...{}", &s[..4], &s[s.len()-4..]));
                }
            }
        }
    }
}

async fn handle_update_settings(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Json(update): Json<SettingsUpdate>,
) -> Response {
    json_ok(state.update_settings(id, update).await)
}

// ── Files & Knowledge ──

async fn handle_file_list(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> Response {
    let path = query.path.unwrap_or_default();
    json_ok(state.list_files(id, path).await)
}

async fn handle_file_read(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> Response {
    let path = query.path.unwrap_or_default();
    json_ok(state.read_file(id, path).await)
}

async fn handle_get_knowledge(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.get_knowledge(id).await)
}

async fn handle_get_skill(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.get_skill(id).await)
}

async fn handle_update_skill(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    body: String,
) -> Response {
    match state.update_skill(id, body).await {
        Ok(()) => json_ok("ok"),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Static File Serving ──

async fn handle_serve_static(
    State(state): State<Arc<EngineState>>,
    AxumPath((instance_id, path)): AxumPath<(String, String)>,
) -> Response {
    let workspace = state.instance_store.instances_dir().join(&instance_id).join("workspace");
    serve_workspace_file(&workspace, &path).await
}

/// Public static files — only apps/ directory, no auth required.
pub async fn handle_public_static(
    State(state): State<Arc<EngineState>>,
    AxumPath((instance_id, path)): AxumPath<(String, String)>,
) -> Response {
    if !path.starts_with("apps/") {
        return json_error(StatusCode::FORBIDDEN, "Public access only allowed for apps/ directory");
    }
    let workspace = state.instance_store.instances_dir().join(&instance_id).join("workspace");
    serve_workspace_file(&workspace, &path).await
}

async fn serve_workspace_file(workspace: &std::path::Path, rel_path: &str) -> Response {
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

    // Path traversal protection
    if !target.starts_with(&workspace_canonical) || !target.is_file() {
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

fn guess_content_type(path: &std::path::Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
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
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webp" => "image/webp",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

// ── Reverse Proxy ──

pub async fn handle_proxy(
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
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
        "HEAD" => reqwest::Method::HEAD,
        "OPTIONS" => reqwest::Method::OPTIONS,
        _ => reqwest::Method::GET,
    };

    let mut req = client.request(req_method, &target_url);

    // Forward request headers (skip hop-by-hop)
    for (name, value) in headers.iter() {
        match name.as_str() {
            "host" | "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade" => {}
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
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut out_headers = HeaderMap::new();

            for (name, value) in resp.headers().iter() {
                let name_str = name.as_str().to_lowercase();
                if let Ok(val_str) = value.to_str() {
                    match name_str.as_str() {
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
                        "set-cookie" => {
                            let rewritten = rewrite_cookie_path(val_str, &proxy_prefix);
                            if let Ok(v) = rewritten.parse() {
                                out_headers.append(name.clone(), v);
                            }
                        }
                        "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" => {}
                        _ => {
                            if let Ok(hv) = val_str.parse() {
                                out_headers.append(name.clone(), hv);
                            }
                        }
                    }
                }
            }

            let resp_body = resp.bytes().await.unwrap_or_default();
            (status, out_headers, resp_body).into_response()
        }
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &format!("Proxy error: {}", e)),
    }
}

fn rewrite_cookie_path(cookie: &str, proxy_prefix: &str) -> String {
    let lower = cookie.to_lowercase();
    if let Some(idx) = lower.find("path=/") {
        let path_start = idx + 5;
        let path_end = cookie[path_start..].find(';').map(|i| path_start + i).unwrap_or(cookie.len());
        let original_path = &cookie[path_start..path_end];
        if !original_path.starts_with(proxy_prefix) {
            let new_path = format!("{}{}", proxy_prefix, original_path);
            return format!("{}{}{}", &cookie[..path_start], new_path, &cookie[path_end..]);
        }
    }
    cookie.to_string()
}

// ── Router Builders ──

/// Authenticated API routes — require valid session cookie.
/// Auth check — returns authenticated status (already passed auth middleware).
async fn handle_auth_check() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "authenticated": true }))
}

pub fn authenticated_api_routes() -> Router<Arc<EngineState>> {
    Router::new()
        .route("/api/instances", get(handle_get_instances).post(handle_create_instance))
        .route("/api/instances/{id}", delete(handle_delete_instance))
        .route("/api/instances/{id}/messages", get(handle_get_messages).post(handle_send_message))
        .route("/api/instances/{id}/replies", get(handle_get_replies))
        .route("/api/instances/{id}/observe", get(handle_observe))
        .route("/api/instances/{id}/interrupt", post(handle_interrupt))
        .route("/api/settings", get(handle_get_global_settings).post(handle_update_global_settings))
        .route("/api/instances/{id}/settings", get(handle_get_settings).post(handle_update_settings))
        .route("/api/instances/{id}/files/list", get(handle_file_list))
        .route("/api/instances/{id}/files/read", get(handle_file_read))
        .route("/api/instances/{id}/knowledge", get(handle_get_knowledge))
        .route("/api/instances/{id}/skill", get(handle_get_skill).put(handle_update_skill))
        .route("/serve/{id}/{*path}", get(handle_serve_static))
        .route("/proxy/{*path}", any(handle_proxy))
        .route("/api/auth/check", get(handle_auth_check))
}

/// Public API routes — no auth required.
pub fn public_api_routes() -> Router<Arc<EngineState>> {
    Router::new()
        .route("/public/{id}/{*path}", get(handle_public_static))
}
/// Build the complete application router.
///
/// Combines public routes, authenticated API routes, login/logout,
/// static file serving, and auth middleware.
pub fn build_router(
    engine_state: Arc<EngineState>,
    html_dir: &std::path::Path,
) -> Router {
    use axum::routing::{get, post};
    use tower_http::services::ServeDir;
    use crate::api::auth;

    Router::new()
        // Login/logout (auth middleware whitelist covers /login)
        .route("/login", get(auth::handle_login_page).post(auth::handle_login_post))
        .route("/api/logout", get(auth::handle_logout))
        .route("/api/frontend-error", post(auth::handle_frontend_error))
        .route("/api/auth", post(auth::handle_legacy_login))
        // Authenticated API routes
        .merge(authenticated_api_routes())
        // Public API routes (auth middleware whitelist covers /public/)
        .merge(public_api_routes())
        // Static HTML files (fallback for non-API paths)
        .fallback_service(ServeDir::new(html_dir))
        // Auth middleware (applied to all routes, whitelist inside)
        .layer(axum::middleware::from_fn_with_state(
            engine_state.clone(),
            auth::check_auth,
        ))
        .with_state(engine_state)
}
