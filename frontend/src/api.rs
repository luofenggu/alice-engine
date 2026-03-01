//! API layer — HTTP endpoints that proxy to the engine via RPC.
//!
//! This module implements the external API surface. All engine operations
//! go through tarpc RPC; static file serving and reverse proxy are handled
//! directly by this process.

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ── State ──

#[derive(Clone)]
pub struct ApiState {
    pub instances_dir: PathBuf,
    pub rpc_socket: String,
}

// ── RPC Helper ──

async fn rpc_client(state: &ApiState) -> Result<alice_rpc::AliceEngineClient, StatusCode> {
    use tarpc::serde_transport::unix;
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(&state.rpc_socket)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let transport = tarpc::serde_transport::Transport::from((
        stream,
        tarpc::tokio_serde::formats::Json::default(),
    ));
    let client =
        alice_rpc::AliceEngineClient::new(tarpc::client::Config::default(), transport).spawn();
    Ok(client)
}

fn rpc_ctx() -> tarpc::context::Context {
    tarpc::context::current()
}

fn json_ok(body: impl serde::Serialize) -> Response {
    let json = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json,
    )
        .into_response()
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    let body = serde_json::json!({ "error": msg });
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

// ── Settings ──

async fn handle_get_settings(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client.get_settings(rpc_ctx(), instance_id).await {
        Ok(json_str) => {
            // Mask sensitive fields (api_key)
            if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&json_str) {
                mask_api_keys(&mut val);
                json_ok(&val)
            } else {
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                    json_str,
                )
                    .into_response()
            }
        }
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn mask_api_keys(val: &mut serde_json::Value) {
    if let Some(obj) = val.as_object_mut() {
        if let Some(key) = obj.get("api_key").and_then(|v| v.as_str()) {
            if key.len() > 8 {
                let masked = format!("{}...{}", &key[..4], &key[key.len() - 4..]);
                obj.insert("api_key".to_string(), serde_json::Value::String(masked));
            }
        }
        if let Some(extras) = obj.get_mut("extra_models") {
            if let Some(arr) = extras.as_array_mut() {
                for item in arr.iter_mut() {
                    if let Some(obj) = item.as_object_mut() {
                        if let Some(key) =
                            obj.get("api_key").and_then(|v| v.as_str()).map(|s| s.to_string())
                        {
                            if key.len() > 8 {
                                let masked =
                                    format!("{}...{}", &key[..4], &key[key.len() - 4..]);
                                obj.insert(
                                    "api_key".to_string(),
                                    serde_json::Value::String(masked),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn handle_update_settings(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    let json_str = body.to_string();
    match client.update_settings(rpc_ctx(), instance_id, json_str).await {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Files ──

#[derive(serde::Deserialize)]
struct PathQuery {
    path: Option<String>,
}

async fn handle_file_list(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
    Query(query): Query<PathQuery>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    let rel_path = query.path.unwrap_or_default();
    match client.list_files(rpc_ctx(), instance_id, rel_path).await {
        Ok(files) => json_ok(&files),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn handle_file_read(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
    Query(query): Query<PathQuery>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    let rel_path = query.path.unwrap_or_default();
    match client.read_file(rpc_ctx(), instance_id, rel_path).await {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Knowledge ──

async fn handle_get_knowledge(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client.get_knowledge(rpc_ctx(), instance_id).await {
        Ok(content) => json_ok(&serde_json::json!({ "content": content })),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Static File Serving ──

async fn handle_serve_static(
    State(state): State<Arc<ApiState>>,
    AxumPath((instance_id, path)): AxumPath<(String, String)>,
) -> Response {
    let workspace = state.instances_dir.join(&instance_id).join("workspace");
    serve_workspace_file(&workspace, &path).await
}

/// Public static files — only apps/ directory, no auth required.
pub async fn handle_public_static(
    State(state): State<Arc<ApiState>>,
    AxumPath((instance_id, path)): AxumPath<(String, String)>,
) -> Response {
    if !path.starts_with("apps/") {
        return json_error(
            StatusCode::FORBIDDEN,
            "Public access only allowed for apps/ directory",
        );
    }

    let workspace = state.instances_dir.join(&instance_id).join("workspace");
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
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot resolve workspace",
            )
        }
    };

    // Path traversal protection
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

fn guess_content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
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
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webp" => "image/webp",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

// ── Reverse Proxy ──

pub async fn handle_proxy(
    State(_state): State<Arc<ApiState>>,
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
            "host" | "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer"
            | "upgrade" => {}
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

            let mut out_headers = HeaderMap::new();
            for (name, value) in resp.headers().iter() {
                let name_str = name.as_str().to_lowercase();
                if let Ok(val_str) = value.to_str() {
                    match name_str.as_str() {
                        // Rewrite Location header for redirects
                        "location" => {
                            let rewritten = if val_str.starts_with('/')
                                && !val_str.starts_with(&proxy_prefix)
                            {
                                format!("{}{}", proxy_prefix, val_str)
                            } else {
                                val_str.to_string()
                            };
                            if let Ok(v) = rewritten.parse() {
                                out_headers.insert(name.clone(), v);
                            }
                        }
                        // Rewrite Set-Cookie path
                        "set-cookie" => {
                            let rewritten = rewrite_cookie_path(val_str, &proxy_prefix);
                            if let Ok(v) = rewritten.parse() {
                                out_headers.append(name.clone(), v);
                            }
                        }
                        // Skip hop-by-hop headers
                        "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" => {}
                        _ => {
                            if let Ok(v) = value.to_str() {
                                if let Ok(hv) = v.parse() {
                                    out_headers.append(name.clone(), hv);
                                }
                            }
                        }
                    }
                }
            }

            let resp_body = resp.bytes().await.unwrap_or_default();
            (status, out_headers, resp_body).into_response()
        }
        Err(e) => json_error(
            StatusCode::BAD_GATEWAY,
            &format!("Proxy error: {}", e),
        ),
    }
}

fn rewrite_cookie_path(cookie: &str, proxy_prefix: &str) -> String {
    // Rewrite "path=/" to "path=/proxy/{port}/" in Set-Cookie
    let lower = cookie.to_lowercase();
    if let Some(idx) = lower.find("path=/") {
        let path_start = idx + 5; // position of '/' after 'path='
        let path_end = cookie[path_start..]
            .find(';')
            .map(|i| path_start + i)
            .unwrap_or(cookie.len());
        let original_path = &cookie[path_start..path_end];
        if !original_path.starts_with(proxy_prefix) {
            let new_path = format!("{}{}", proxy_prefix, original_path);
            return format!("{}{}{}", &cookie[..path_start], new_path, &cookie[path_end..]);
        }
    }
    cookie.to_string()
}

// ── Instances ──

async fn handle_get_instances(State(state): State<Arc<ApiState>>) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client.get_instances(rpc_ctx()).await {
        Ok(instances) => json_ok(&instances),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct CreateInstanceBody {
    name: Option<String>,
}

async fn handle_create_instance(
    State(state): State<Arc<ApiState>>,
    axum::Json(body): axum::Json<CreateInstanceBody>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    let name = body.name.unwrap_or_default();
    match client.create_instance(rpc_ctx(), name).await {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn handle_delete_instance(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client.delete_instance(rpc_ctx(), instance_id).await {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Messages ──

#[derive(serde::Deserialize)]
struct MessagesQuery {
    before_id: Option<i64>,
    after_id: Option<i64>,
    limit: Option<i64>,
}

async fn handle_get_messages(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
    Query(query): Query<MessagesQuery>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    let limit = query.limit.unwrap_or(50);
    match client
        .get_messages(rpc_ctx(), instance_id, query.before_id, query.after_id, limit)
        .await
    {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SendMessageBody {
    content: String,
}

async fn handle_send_message(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
    axum::Json(body): axum::Json<SendMessageBody>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client.send_message(rpc_ctx(), instance_id, body.content).await {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct RepliesQuery {
    after_id: i64,
}

async fn handle_get_replies(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
    Query(query): Query<RepliesQuery>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client
        .get_replies_after(rpc_ctx(), instance_id, query.after_id)
        .await
    {
        Ok(replies) => json_ok(&replies),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Observe & Control ──

async fn handle_observe(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client.observe(rpc_ctx(), instance_id).await {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn handle_interrupt(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client.interrupt(rpc_ctx(), instance_id).await {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SwitchModelBody {
    model_index: u32,
}

async fn handle_switch_model(
    State(state): State<Arc<ApiState>>,
    AxumPath(instance_id): AxumPath<String>,
    axum::Json(body): axum::Json<SwitchModelBody>,
) -> Response {
    let client = match rpc_client(&state).await {
        Ok(c) => c,
        Err(s) => return json_error(s, "RPC connection failed"),
    };

    match client
        .switch_model(rpc_ctx(), instance_id, body.model_index)
        .await
    {
        Ok(result) => json_ok(&result),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ── Router Builder ──

/// Build API routes that require authentication.
/// These should be mounted inside the auth middleware.
pub fn authenticated_api_routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Instance management
        .route(
            "/api/instances",
            get(handle_get_instances).post(handle_create_instance),
        )
        .route("/api/instances/{id}", axum::routing::delete(handle_delete_instance))
        // Messages
        .route(
            "/api/instances/{id}/messages",
            get(handle_get_messages).post(handle_send_message),
        )
        .route("/api/instances/{id}/replies", get(handle_get_replies))
        // Observe & control
        .route("/api/instances/{id}/observe", get(handle_observe))
        .route("/api/instances/{id}/interrupt", post(handle_interrupt))
        .route("/api/instances/{id}/switch_model", post(handle_switch_model))
        // Instance settings
        .route(
            "/api/instances/{id}/settings",
            get(handle_get_settings).post(handle_update_settings),
        )
        // File browser
        .route("/api/instances/{id}/files/list", get(handle_file_list))
        .route("/api/instances/{id}/files/read", get(handle_file_read))
        // Knowledge
        .route("/api/instances/{id}/knowledge", get(handle_get_knowledge))
        // Static file serving (authenticated)
        .route("/serve/{id}/{*path}", get(handle_serve_static))
        // Reverse proxy (authenticated)
        .route("/proxy/{*path}", any(handle_proxy))
}

/// Build public API routes (no auth required).
pub fn public_api_routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Public static files
        .route("/public/{id}/{*path}", get(handle_public_static))
}
