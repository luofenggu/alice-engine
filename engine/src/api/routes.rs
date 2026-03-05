//! HTTP API routes — all endpoint handlers and router construction.

use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, delete, get, post},
    Json, Router,
};
use route_macro::*;
use serde::Deserialize;

use super::http_protocol;
use super::state::EngineState;
use crate::persist::Settings;

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
    pub settings: Option<Settings>,
}

// ── Instance Handlers ──

#[get("/api/instances")]
async fn handle_get_instances(State(state): State<Arc<EngineState>>) -> Response {
    json_ok(state.get_instances().await)
}

#[post("/api/instances")]
async fn handle_create_instance(
    State(state): State<Arc<EngineState>>,
    Json(body): Json<CreateInstanceBody>,
) -> Response {
    let name = body.name.unwrap_or_default();
    json_ok(state.create_instance(name, body.settings).await)
}

#[delete("/api/instances/{id}")]
async fn handle_delete_instance(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.delete_instance(id).await)
}

// ── Message Handlers ──

#[get("/api/instances/{id}/messages")]
async fn handle_get_messages(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<MessagesQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(http_protocol::DEFAULT_MESSAGE_LIMIT);
    match state.get_messages(id, query.before_id, query.after_id, limit).await {
        Ok(result) => json_ok(result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

#[post("/api/instances/{id}/messages")]
async fn handle_send_message(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<SendMessageBody>,
) -> Response {
    json_ok(state.send_message(id, body.content).await)
}

#[get("/api/instances/{id}/replies")]
async fn handle_get_replies(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<RepliesQuery>,
) -> Response {
    json_ok(state.get_replies_after(id, query.after_id).await)
}

// ── Observe & Control ──

#[get("/api/instances/{id}/observe")]
async fn handle_observe(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.observe(id).await)
}

#[post("/api/instances/{id}/interrupt")]
async fn handle_interrupt(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.interrupt(id).await)
}

// ── Settings ──

#[get("/api/settings")]
async fn handle_get_global_settings(
    State(state): State<Arc<EngineState>>,
) -> Response {
    json_ok(state.get_global_settings().await)
}

#[post("/api/settings")]
async fn handle_update_global_settings(
    State(state): State<Arc<EngineState>>,
    Json(update): Json<Settings>,
) -> Response {
    json_ok(state.update_global_settings(update).await)
}

#[get("/api/instances/{id}/settings")]
async fn handle_get_settings(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let settings = state.get_settings(id).await;
    // Mask API keys for security
    let mut val = serde_json::to_value(&settings).unwrap_or_default();
    if let Some(obj) = val.as_object_mut() {
        if let Some(key_val) = obj.get_mut(http_protocol::API_KEY_FIELD_NAME) {
            if let Some(s) = key_val.as_str() {
                if s.len() > http_protocol::API_KEY_MASK_MIN_LEN {
                    *key_val = serde_json::Value::String(http_protocol::mask_api_key(s));
                }
            }
        }
    }
    json_ok(val)
}

#[post("/api/instances/{id}/settings")]
async fn handle_update_settings(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Json(update): Json<Settings>,
) -> Response {
    json_ok(state.update_settings(id, update).await)
}

// ── Files & Knowledge ──

#[get("/api/instances/{id}/files/list")]
async fn handle_file_list(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> Response {
    let path = query.path.unwrap_or_default();
    json_ok(state.list_files(id, path).await)
}

#[get("/api/instances/{id}/files/read")]
async fn handle_file_read(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilePathQuery>,
) -> Response {
    let path = query.path.unwrap_or_default();
    json_ok(state.read_file(id, path).await)
}

#[get("/api/instances/{id}/knowledge")]
async fn handle_get_knowledge(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.get_knowledge(id).await)
}

#[get("/api/instances/{id}/skill")]
async fn handle_get_skill(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    json_ok(state.get_skill(id).await)
}

#[put("/api/instances/{id}/skill")]
async fn handle_update_skill(
    State(state): State<Arc<EngineState>>,
    AxumPath(id): AxumPath<String>,
    body: String,
) -> Response {
    match state.update_skill(id, body).await {
        Ok(()) => json_ok(serde_json::json!({"status": "ok"})),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Static File Serving ──

#[get("/serve/{id}/{*path}")]
async fn handle_serve_static(
    State(state): State<Arc<EngineState>>,
    AxumPath((instance_id, path)): AxumPath<(String, String)>,
) -> Response {
    let workspace = state.instance_store.workspace_dir(&instance_id);
    serve_workspace_file(&workspace, &path).await
}

/// Public static files — only apps/ directory, no auth required.
#[get("/public/{id}/{*path}")]
pub async fn handle_public_static(
    State(state): State<Arc<EngineState>>,
    AxumPath((instance_id, path)): AxumPath<(String, String)>,
) -> Response {
    if !path.starts_with(http_protocol::PUBLIC_DIR_PREFIX) {
        return json_error(StatusCode::FORBIDDEN, "Public access only allowed for apps/ directory");
    }
    let workspace = state.instance_store.workspace_dir(&instance_id);
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

    let ext = target.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let content_type = http_protocol::content_type_for_extension(&ext);
    match tokio::fs::read(&target).await {
        Ok(data) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
            (headers, data).into_response()
        }
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file"),
    }
}

// ── Reverse Proxy ──

#[any_method("/proxy/{*path}")]
pub async fn handle_proxy(
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    method: axum::http::Method,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let path = uri.path();
    let rest = match path.strip_prefix(http_protocol::PROXY_PATH_PREFIX) {
        Some(r) => r,
        None => return json_error(StatusCode::BAD_REQUEST, "Invalid proxy path"),
    };

    let (port, target_path) = match http_protocol::parse_proxy_target(rest) {
        Some(result) => result,
        None => return json_error(StatusCode::BAD_REQUEST, "Invalid port (must be >= 1024)"),
    };

    let target_url = http_protocol::build_proxy_url(port, target_path, uri.query());

    let client = reqwest::Client::new();
    let req_method = http_protocol::to_reqwest_method(&method);

    let mut req = client.request(req_method, &target_url);
    req = http_protocol::forward_request_headers(&headers, req);

    if !body.is_empty() {
        req = req.body(body.to_vec());
    }

    let proxy_prefix = http_protocol::build_proxy_prefix(port);

    match req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut out_headers = HeaderMap::new();

            for (name, value) in resp.headers().iter() {
                if let Ok(val_str) = value.to_str() {
                    match http_protocol::process_proxy_response_header(name.as_str(), val_str, &proxy_prefix) {
                        None => {} // hop-by-hop header, strip it
                        Some(rewritten) => {
                            if let Ok(hv) = rewritten.parse() {
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

// ── Router Builders ──

/// Auth check — returns authenticated status (already passed auth middleware).
#[get("/api/auth/check")]
async fn handle_auth_check() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "authenticated": true }))
}

/// Authenticated API routes — require valid session cookie.
pub fn authenticated_api_routes() -> Router<Arc<EngineState>> {
    Router::new()
        .route(ROUTE_HANDLE_GET_INSTANCES, get(handle_get_instances).post(handle_create_instance))
        .route(ROUTE_HANDLE_DELETE_INSTANCE, delete(handle_delete_instance))
        .route(ROUTE_HANDLE_GET_MESSAGES, get(handle_get_messages).post(handle_send_message))
        .route(ROUTE_HANDLE_GET_REPLIES, get(handle_get_replies))
        .route(ROUTE_HANDLE_OBSERVE, get(handle_observe))
        .route(ROUTE_HANDLE_INTERRUPT, post(handle_interrupt))
        .route(ROUTE_HANDLE_GET_GLOBAL_SETTINGS, get(handle_get_global_settings).post(handle_update_global_settings))
        .route(ROUTE_HANDLE_GET_SETTINGS, get(handle_get_settings).post(handle_update_settings))
        .route(ROUTE_HANDLE_FILE_LIST, get(handle_file_list))
        .route(ROUTE_HANDLE_FILE_READ, get(handle_file_read))
        .route(ROUTE_HANDLE_GET_KNOWLEDGE, get(handle_get_knowledge))
        .route(ROUTE_HANDLE_GET_SKILL, get(handle_get_skill).put(handle_update_skill))
        .route(ROUTE_HANDLE_SERVE_STATIC, get(handle_serve_static))
        .route(ROUTE_HANDLE_PROXY, any(handle_proxy))
        .route(ROUTE_HANDLE_AUTH_CHECK, get(handle_auth_check))
}

/// Public API routes — no auth required.
pub fn public_api_routes() -> Router<Arc<EngineState>> {
    Router::new()
        .route(ROUTE_HANDLE_PUBLIC_STATIC, get(handle_public_static))
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
        .route(auth::ROUTE_HANDLE_LOGIN_PAGE, get(auth::handle_login_page).post(auth::handle_login_post))
        .route(auth::ROUTE_HANDLE_LOGOUT, get(auth::handle_logout))
        .route(auth::ROUTE_HANDLE_FRONTEND_ERROR, post(auth::handle_frontend_error))
        .route(auth::ROUTE_HANDLE_LEGACY_LOGIN, post(auth::handle_legacy_login))
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