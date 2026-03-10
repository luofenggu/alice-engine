//! HTTP API routes — all endpoint handlers and router construction.

use std::sync::Arc;

use axum::{
    extract::{Multipart, Path as AxumPath, Query, State},
    extract::ws::WebSocketUpgrade,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use route_macro::*;
use mad_hatter::http_service;
use serde::Deserialize;

use super::http_protocol;
use super::state::EngineState;
use crate::persist::hooks::HooksConfig;
use crate::persist::hooks::RelayRequest;
use crate::persist::Settings;

// Hub API request/response types
#[derive(Deserialize)]
pub struct HubEnableBody {
    pub join_token: String,
}

#[derive(Deserialize)]
pub struct HubJoinBody {
    pub host_url: String,
    pub join_token: String,
}

#[derive(Deserialize)]
pub struct HubWsQuery {
    pub token: Option<String>,
    pub engine_id: Option<String>,
}

// ── Hub instance_id extraction patterns ──

/// Extract instance_id from URL path for hub routing.
/// Matches: /api/instances/{id}/..., /serve/{id}/..., /public/{id}/...
fn extract_instance_id_from_path(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("/api/instances/") {
        // /api/instances/{id} or /api/instances/{id}/...
        let id = rest.split('/').next()?;
        if !id.is_empty() {
            return Some(id);
        }
    }
    if let Some(rest) = path.strip_prefix("/serve/") {
        let id = rest.split('/').next()?;
        if !id.is_empty() {
            return Some(id);
        }
    }
    if let Some(rest) = path.strip_prefix("/public/") {
        let id = rest.split('/').next()?;
        if !id.is_empty() {
            return Some(id);
        }
    }
    None
}

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
pub struct RelayMessageBody {
    pub sender: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct CreateInstanceBody {
    pub name: Option<String>,
    pub settings: Option<Settings>,
}

#[derive(Deserialize)]
pub struct VisionBody {
    pub prompt: String,
    pub image_url: String,
}

#[derive(Deserialize)]
pub struct ChannelSelectBody {
    pub index: usize,
}

// ── Instance Handlers (Mad Hatter) ──

http_service! {
    service InstanceApi {
        GET    "api/instances"      => list() -> Json<Vec<crate::api::types::InstanceInfo>>;
        POST   "api/instances"      => create(body: CreateInstanceBody) -> Json<crate::api::types::ActionResult>;
        GET    "api/instances/{id}" => get_by_id(id: String) -> Json<crate::api::types::InstanceInfo>;
        DELETE "api/instances/{id}" => delete_by_id(id: String) -> Json<crate::api::types::ActionResult>;
    }
}

impl InstanceApiService for EngineState {
    async fn list(&self) -> mad_hatter::Result<Json<Vec<crate::api::types::InstanceInfo>>> {
        let mut instances = self.get_instances().await;

        // Hub host mode: merge remote instances from all slave engines
        if let Some(host) = self.hub.as_host().await {
            let remote = host.get_all_remote_instances().await;
            for (_engine_id, tunnel_instances) in remote {
                for ti in tunnel_instances {
                    instances.push(crate::api::types::InstanceInfo {
                        id: ti.id,
                        name: ti.name,
                        avatar: ti.avatar,
                        color: ti.color,
                        privileged: ti.privileged,
                        last_active: ti.last_active,
                    });
                }
            }
        }

        Ok(Json(instances))
    }

    async fn create(&self, body: CreateInstanceBody) -> mad_hatter::Result<Json<crate::api::types::ActionResult>> {
        let name = body.name.unwrap_or_default();
        let result = self.create_instance(name, body.settings).await;
        // Notify hub of instance list change (no-op if not in joined mode)
        notify_hub_instances(self).await;
        Ok(Json(result))
    }

    async fn get_by_id(&self, id: String) -> mad_hatter::Result<Json<crate::api::types::InstanceInfo>> {
        match self.get_instance(id).await {
            Some(info) => Ok(Json(info)),
            None => Err(mad_hatter::HttpError::not_found("Instance not found")),
        }
    }

    async fn delete_by_id(&self, id: String) -> mad_hatter::Result<Json<crate::api::types::ActionResult>> {
        let result = self.delete_instance(id).await;
        // Notify hub of instance list change (no-op if not in joined mode)
        notify_hub_instances(self).await;
        Ok(Json(result))
    }
}


// ── Control API (Mad Hatter) ──

http_service! {
    service ControlApi {
        GET    "api/instances/{id}/observe"          => observe_instance(id: String) -> Response;
        POST   "api/instances/{id}/interrupt"        => interrupt_instance(id: String) -> Response;
        GET    "api/instances/{id}/channels"         => get_channels(id: String) -> Response;
        POST   "api/instances/{id}/channels/select"  => select_channel(id: String, body: ChannelSelectBody) -> Response;
    }
}

impl ControlApiService for EngineState {
    async fn observe_instance(&self, id: String) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.observe(id).await))
    }

    async fn interrupt_instance(&self, id: String) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.interrupt(id).await))
    }

    async fn get_channels(&self, id: String) -> mad_hatter::Result<Response> {
        let _ = id; // instance id unused — channels are global
        let (channels, counter, current_idx) = self.llm_client.channels_status();
        let channels_json: Vec<serde_json::Value> = channels
            .iter()
            .map(|(idx, name, model)| {
                serde_json::json!({
                    "index": idx,
                    "name": name,
                    "model": model,
                })
            })
            .collect();
        Ok(json_ok(serde_json::json!({
            "channels": channels_json,
            "counter": counter,
            "current_index": current_idx,
            "current_name": crate::external::llm::LlmClient::channel_display_name(current_idx),
        })))
    }

    async fn select_channel(&self, id: String, body: ChannelSelectBody) -> mad_hatter::Result<Response> {
        let _ = id;
        match self.llm_client.select_channel(body.index) {
            Ok(()) => Ok(json_ok(serde_json::json!({
                "success": true,
                "message": format!("switched to {}", crate::external::llm::LlmClient::channel_display_name(body.index)),
            }))),
            Err(e) => Ok(json_error(StatusCode::BAD_REQUEST, &e)),
        }
    }
}

// ── Knowledge API (Mad Hatter) ──

http_service! {
    service KnowledgeApi {
        GET "api/instances/{id}/knowledge" => get_instance_knowledge(id: String) -> Response;
        GET "api/instances/{id}/skill" => get_instance_skill(id: String) -> Response;
        PUT "api/instances/{id}/skill" => update_instance_skill(id: String, body: String) -> Response;
    }
}

impl KnowledgeApiService for EngineState {
    async fn get_instance_knowledge(&self, id: String) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.get_knowledge(id).await))
    }
    async fn get_instance_skill(&self, id: String) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.get_skill(id).await))
    }
    async fn update_instance_skill(&self, id: String, body: String) -> mad_hatter::Result<Response> {
        match self.update_skill(id, body).await {
            Ok(()) => Ok(json_ok(crate::api::types::ActionResult::ok_empty())),
            Err(e) => Ok(json_ok(crate::api::types::ActionResult::err(e.to_string()))),
        }
    }
}

// ── Auth API (Mad Hatter) ──

http_service! {
    service AuthApi {
        GET "api/auth/check" => check() -> Response;
    }
}

impl AuthApiService for EngineState {
    async fn check(&self) -> mad_hatter::Result<Response> {
        Ok(json_ok(serde_json::json!({ "authenticated": true })))
    }
}

// ── Vision API (Mad Hatter) ──

http_service! {
    service VisionApi {
        POST "api/instances/{id}/vision" => vision_analyze(id: String, body: VisionBody) -> Response;
    }
}

impl VisionApiService for EngineState {
    async fn vision_analyze(&self, id: String, body: VisionBody) -> mad_hatter::Result<Response> {
        match self.vision(id, body.prompt, body.image_url).await {
            Ok(text) => Ok(json_ok(serde_json::json!({ "text": text }))),
            Err(e) => Ok(json_error(StatusCode::BAD_GATEWAY, &e)),
        }
    }
}

// ── Message API (Mad Hatter) ──

http_service! {
    service MessageApi {
        GET    "api/instances/{id}/messages"         => fetch_messages(id: String, query: MessagesQuery) -> Response;
        POST   "api/instances/{id}/messages"         => post_message(id: String, body: SendMessageBody) -> Response;
        POST   "api/instances/{id}/messages/relay"   => post_relay(id: String, body: RelayMessageBody) -> Response;
        POST   "api/instances/{id}/system-messages"  => post_system_message(id: String, body: SendMessageBody) -> Response;
        GET    "api/instances/{id}/replies"           => fetch_replies(id: String, query: RepliesQuery) -> Response;
    }
}

impl MessageApiService for EngineState {
    async fn fetch_messages(&self, id: String, query: MessagesQuery) -> mad_hatter::Result<Response> {
        let limit = query.limit.unwrap_or(http_protocol::DEFAULT_MESSAGE_LIMIT);
        match self.get_messages(id, query.before_id, query.after_id, limit).await {
            Ok(result) => Ok(json_ok(result)),
            Err(e) => Ok(json_error(StatusCode::INTERNAL_SERVER_ERROR, &e)),
        }
    }

    async fn post_message(&self, id: String, body: SendMessageBody) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.send_message(id, body.content).await))
    }

    async fn post_relay(&self, id: String, body: RelayMessageBody) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.send_relay_message(id, body.sender, body.content).await))
    }

    async fn post_system_message(&self, id: String, body: SendMessageBody) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.send_system_message(id, body.content).await))
    }

    async fn fetch_replies(&self, id: String, query: RepliesQuery) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.get_replies_after(id, query.after_id).await))
    }
}

// ── File API (Mad Hatter) ──

http_service! {
    service FileApi {
        GET    "api/instances/{id}/files/list"   => list_instance_files(id: String, query: FilePathQuery) -> Response;
        GET    "api/instances/{id}/files/read"    => read_instance_file(id: String, query: FilePathQuery) -> Response;
        DELETE "api/instances/{id}/files/delete"  => delete_instance_file(id: String, query: FilePathQuery) -> Response;
    }
}

impl FileApiService for EngineState {
    async fn list_instance_files(&self, id: String, query: FilePathQuery) -> mad_hatter::Result<Response> {
        let path = query.path.unwrap_or_default();
        Ok(json_ok(self.list_files(id, path).await))
    }

    async fn read_instance_file(&self, id: String, query: FilePathQuery) -> mad_hatter::Result<Response> {
        let path = query.path.unwrap_or_default();
        Ok(json_ok(self.read_file(id, path).await))
    }

    async fn delete_instance_file(&self, id: String, query: FilePathQuery) -> mad_hatter::Result<Response> {
        let path = query.path.unwrap_or_default();
        Ok(json_ok(self.delete_file(id, path).await))
    }
}

// ── Settings API (Mad Hatter) ──

http_service! {
    service SettingsApi {
        GET    "api/settings"                    => fetch_global_settings() -> Response;
        POST   "api/settings"                    => save_global_settings(body: Settings) -> Response;
        GET    "api/instances/{id}/settings"      => fetch_instance_settings(id: String) -> Response;
        POST   "api/instances/{id}/settings"      => save_instance_settings(id: String, body: Settings) -> Response;
    }
}

impl SettingsApiService for EngineState {
    async fn fetch_global_settings(&self) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.get_global_settings().await))
    }

    async fn save_global_settings(&self, body: Settings) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.update_global_settings(body).await))
    }

    async fn fetch_instance_settings(&self, id: String) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.get_settings(id).await))
    }

    async fn save_instance_settings(&self, id: String, body: Settings) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.update_settings(id, body).await))
    }
}

/// Notify hub host of local instance list changes (used after create/delete)
async fn notify_hub_instances(state: &EngineState) {
    let instances: Vec<crate::hub::tunnel::TunnelInstanceInfo> = state.get_instances().await
        .iter()
        .map(|inst| crate::hub::tunnel::TunnelInstanceInfo {
            id: inst.id.clone(),
            name: inst.name.clone(),
            avatar: inst.avatar.clone(),
            color: inst.color.clone(),
            privileged: inst.privileged,
            last_active: inst.last_active,
        })
        .collect();
    state.hub.notify_instances_changed(instances).await;
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
        return json_error(
            StatusCode::FORBIDDEN,
            "Public access only allowed for apps/ directory",
        );
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
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Cannot resolve workspace",
            )
        }
    };

    // Path traversal protection: must be within workspace
    if !target.starts_with(&workspace_canonical) || !target.is_file() {
        return json_error(StatusCode::NOT_FOUND, "File not found");
    }

    let ext = target
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
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
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut out_headers = HeaderMap::new();

            for (name, value) in resp.headers().iter() {
                if let Ok(val_str) = value.to_str() {
                    match http_protocol::process_proxy_response_header(
                        name.as_str(),
                        val_str,
                        &proxy_prefix,
                    ) {
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

// ── Vision ──


// ── File Upload ──

/// Upload files to an instance's workspace uploads directory.
#[post("/api/instances/{id}/upload")]
async fn handle_upload(
    State(state): State<Arc<EngineState>>,
    AxumPath(instance_id): AxumPath<String>,
    mut multipart: Multipart,
) -> Response {
    let workspace = state.instance_store.workspace_dir(&instance_id);
    let today = chrono::Local::now().format("%Y%m%d").to_string();
    let day_dir = workspace.join("uploads").join(&today);
    if let Err(e) = std::fs::create_dir_all(&day_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create upload directory: {}", e),
        )
            .into_response();
    }

    let mut uploaded: Vec<serde_json::Value> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let file_name = match field.file_name() {
            Some(name) => name.to_string(),
            None => continue,
        };
        let data = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("Failed to read field: {}", e),
                )
                    .into_response();
            }
        };

        // Handle name conflicts: file.txt -> file_1.txt -> file_2.txt
        let (stem, ext) = match file_name.rfind('.') {
            Some(pos) => (&file_name[..pos], &file_name[pos..]),
            None => (file_name.as_str(), ""),
        };
        let mut target = day_dir.join(&file_name);
        let mut counter = 1u32;
        while target.exists() {
            target = day_dir.join(format!("{stem}_{counter}{ext}"));
            counter += 1;
        }
        let actual_name = target.file_name().unwrap().to_string_lossy().to_string();

        if let Err(e) = std::fs::write(&target, &data) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to write file: {}", e),
            )
                .into_response();
        }

        let relative_path = format!("uploads/{today}/{actual_name}");
        uploaded.push(serde_json::json!({
            "name": actual_name,
            "path": relative_path,
            "size": data.len()
        }));
    }

    Json(serde_json::json!({ "files": uploaded })).into_response()
}

// ── Hub Hook Callbacks ──

// ── Hub Public API (Mad Hatter) ──

http_service! {
    service HubPublicApi {
        GET "api/hub/contacts/{id}" => hub_contacts(id: String) -> Response;
        POST "api/hub/relay" => hub_relay(body: RelayRequest) -> Response;
        GET "api/hub/tunnel_proxy/contacts/{id}" => tunnel_proxy_contacts(id: String) -> Response;
    }
}

impl HubPublicApiService for EngineState {
    async fn hub_contacts(&self, id: String) -> mad_hatter::Result<Response> {
        match self.hub.as_host().await {
            Some(host) => Ok(crate::hub::hooks::handle_hub_contacts(&host, self, &id).await.into_response()),
            None => Ok(json_error(StatusCode::NOT_FOUND, "Hub not in host mode")),
        }
    }

    async fn hub_relay(&self, body: RelayRequest) -> mad_hatter::Result<Response> {
        match self.hub.as_host().await {
            Some(host) => Ok(crate::hub::hooks::handle_hub_relay(&host, self, body).await.into_response()),
            None => Ok(json_error(StatusCode::NOT_FOUND, "Hub not in host mode")),
        }
    }

    async fn tunnel_proxy_contacts(&self, id: String) -> mad_hatter::Result<Response> {
        let slave = match self.hub.as_slave().await {
            Some(s) => s,
            None => return Ok(json_error(StatusCode::NOT_FOUND, "Not in joined mode")),
        };

        let req = crate::hub::tunnel::TunnelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            method: "GET".to_string(),
            path: format!("/api/hub/contacts/{}", id),
            headers: std::collections::HashMap::new(),
            body: None,
        };

        match slave.proxy_request_to_host(req).await {
            Some(resp) => Ok(tunnel_response_to_http(resp)),
            None => Ok(json_error(StatusCode::BAD_GATEWAY, "Tunnel request to host failed")),
        }
    }
}

/// Tunnel proxy for relay: slave forwards to host via WebSocket tunnel
#[post("/api/hub/tunnel_proxy/relay")]
async fn handle_tunnel_proxy_relay(
    State(state): State<Arc<EngineState>>,
    body: axum::body::Bytes,
) -> Response {
    let slave = match state.hub.as_slave().await {
        Some(s) => s,
        None => return json_error(StatusCode::NOT_FOUND, "Not in joined mode"),
    };

    use base64::Engine as _;
    let req = crate::hub::tunnel::TunnelRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        method: "POST".to_string(),
        path: "/api/hub/relay".to_string(),
        headers: {
            let mut h = std::collections::HashMap::new();
            h.insert("content-type".to_string(), "application/json".to_string());
            h
        },
        body: if body.is_empty() {
            None
        } else {
            Some(base64::engine::general_purpose::STANDARD.encode(&body))
        },
    };

    match slave.proxy_request_to_host(req).await {
        Some(resp) => tunnel_response_to_http(resp),
        None => json_error(StatusCode::BAD_GATEWAY, "Tunnel request to host failed"),
    }
}

/// Convert a TunnelResponse to an HTTP Response
fn tunnel_response_to_http(resp: crate::hub::tunnel::TunnelResponse) -> Response {
    use base64::Engine as _;
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for (k, v) in &resp.headers {
        if let (Ok(name), Ok(val)) = (
            axum::http::header::HeaderName::from_bytes(k.as_bytes()),
            axum::http::header::HeaderValue::from_str(v),
        ) {
            headers.insert(name, val);
        }
    }
    let body_bytes = resp.body
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(&b).ok())
        .unwrap_or_default();
    (status, headers, body_bytes).into_response()
}

// ── Hub API (Mad Hatter) ──

http_service! {
    service HubApi {
        POST "api/hub/enable" => enable_hub(body: HubEnableBody) -> Response;
        POST "api/hub/disable" => disable_hub() -> Response;
        POST "api/hub/join" => join_hub(body: HubJoinBody) -> Response;
        POST "api/hub/leave" => leave_hub() -> Response;
        GET "api/hub/status" => hub_status() -> Response;
        GET "api/hub/endpoints" => hub_endpoints() -> Response;
        POST "api/hooks" => register_hooks(body: HooksConfig) -> Response;
    }
}

impl HubApiService for EngineState {
    async fn enable_hub(&self, body: HubEnableBody) -> mad_hatter::Result<Response> {
        let local_port = self.env_config.http_port;
        let auth_secret = self.env_config.auth_secret.clone();
        match self.hub.enable_host(body.join_token, local_port, auth_secret).await {
            Ok(()) => {
                // Register hooks on self so local instances can use hub contacts/relay
                let port = local_port;
                let hooks_body = serde_json::json!({
                    "contacts_url": format!("http://localhost:{}/api/hub/contacts/{{instance_id}}", port),
                    "send_msg_relay_url": format!("http://localhost:{}/api/hub/relay", port)
                });
                let cookie = format!("{}={}", self.session_cookie_name, self.session_token);
                let url = format!("http://localhost:{}/api/hooks", port);
                match self.http_client.post(&url)
                    .header("Cookie", cookie)
                    .json(&hooks_body)
                    .send()
                    .await
                {
                    Ok(resp) => tracing::info!("[HUB] Self hooks registration: {}", resp.status()),
                    Err(e) => tracing::warn!("[HUB] Failed to register hooks on self: {}", e),
                }
                Ok(json_ok(serde_json::json!({"status": "host mode enabled"})))
            }
            Err(e) => Ok(json_error(StatusCode::BAD_REQUEST, &e)),
        }
    }

    async fn disable_hub(&self) -> mad_hatter::Result<Response> {
        match self.hub.disable_host().await {
            Ok(()) => Ok(json_ok(serde_json::json!({"status": "host mode disabled"}))),
            Err(e) => Ok(json_error(StatusCode::BAD_REQUEST, &e)),
        }
    }

    async fn join_hub(&self, body: HubJoinBody) -> mad_hatter::Result<Response> {
        let local_instances = self.get_instances().await;
        let tunnel_instances: Vec<crate::hub::tunnel::TunnelInstanceInfo> = local_instances
            .iter()
            .map(|inst| crate::hub::tunnel::TunnelInstanceInfo {
                id: inst.id.clone(),
                name: inst.name.clone(),
                avatar: inst.avatar.clone(),
                color: inst.color.clone(),
                privileged: inst.privileged,
                last_active: inst.last_active,
            })
            .collect();

        let engine_id = tunnel_instances.first()
            .map(|i| i.id.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let local_port = self.env_config.http_port;
        let auth_token = self.env_config.auth_secret.clone();
        match self.hub.join_host(body.host_url, body.join_token, tunnel_instances, &engine_id, local_port, auth_token).await {
            Ok(()) => Ok(json_ok(serde_json::json!({"status": "joined host"}))),
            Err(e) => Ok(json_error(StatusCode::BAD_REQUEST, &e)),
        }
    }

    async fn leave_hub(&self) -> mad_hatter::Result<Response> {
        match self.hub.leave_host().await {
            Ok(()) => Ok(json_ok(serde_json::json!({"status": "left host"}))),
            Err(e) => Ok(json_error(StatusCode::BAD_REQUEST, &e)),
        }
    }

    async fn hub_status(&self) -> mad_hatter::Result<Response> {
        Ok(json_ok(self.hub.status().await))
    }

    async fn hub_endpoints(&self) -> mad_hatter::Result<Response> {
        let local_instances: Vec<crate::api::types::InstanceInfo> = self.get_instances().await;

        let mut groups = vec![crate::api::types::EndpointGroup {
            endpoint: "local".to_string(),
            instances: local_instances,
        }];

        if let Some(host) = self.hub.as_host().await {
            let remote = host.get_remote_endpoints().await;
            for (endpoint, tunnel_instances) in remote {
                groups.push(crate::api::types::EndpointGroup {
                    endpoint,
                    instances: tunnel_instances.into_iter().map(|ti| {
                        crate::api::types::InstanceInfo {
                            id: ti.id,
                            name: ti.name,
                            avatar: ti.avatar,
                            color: ti.color,
                            privileged: ti.privileged,
                            last_active: ti.last_active,
                        }
                    }).collect(),
                });
            }
        }

        Ok(json_ok(groups))
    }

    async fn register_hooks(&self, body: HooksConfig) -> mad_hatter::Result<Response> {
        match self.hooks_store.register(&body) {
            Ok(merged) => {
                self.hooks_caller.update_config(merged);
                tracing::info!("[HOOKS] Hooks registered/updated successfully");
                Ok(json_ok(crate::api::types::ActionResult::ok_empty()))
            }
            Err(e) => {
                tracing::warn!("[HOOKS] Failed to register hooks: {}", e);
                Ok(json_ok(crate::api::types::ActionResult::err(e.to_string())))
            }
        }
    }
}

/// WebSocket endpoint for slave engines to connect.
/// Auth via join_token query parameter (not cookie).
#[get("/api/hub/ws")]
async fn handle_hub_ws(
    State(state): State<Arc<EngineState>>,
    Query(query): Query<HubWsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let host = match state.hub.as_host().await {
        Some(h) => h,
        None => return json_error(StatusCode::BAD_REQUEST, "Not in host mode"),
    };

    // Verify join token
    let token = query.token.unwrap_or_default();
    if token != host.join_token {
        return json_error(StatusCode::UNAUTHORIZED, "Invalid join token");
    }

    let engine_id = query.engine_id.unwrap_or_else(|| "unknown".to_string());

    ws.on_upgrade(move |socket| async move {
        host.handle_slave_connection(socket, engine_id).await;
    })
}

// ── Router Builders ──


/// Authenticated API routes — require valid session cookie.
pub fn authenticated_api_routes() -> Router<Arc<EngineState>> {
    Router::new()
        .merge(InstanceApi::router::<EngineState>())
        .merge(MessageApi::router::<EngineState>())
        .merge(ControlApi::router::<EngineState>())
        .merge(SettingsApi::router::<EngineState>())
        .merge(FileApi::router::<EngineState>())
        .merge(KnowledgeApi::router::<EngineState>())
        .merge(HubApi::router::<EngineState>())
        .route(ROUTE_HANDLE_SERVE_STATIC, get(handle_serve_static))
        .route(ROUTE_HANDLE_PROXY, any(handle_proxy))
        .route(ROUTE_HANDLE_UPLOAD, post(handle_upload))
        .merge(VisionApi::router::<EngineState>())
        .merge(AuthApi::router::<EngineState>())
}

/// Public API routes — no auth required.
pub fn public_api_routes() -> Router<Arc<EngineState>> {
    Router::new()
        .route(ROUTE_HANDLE_PUBLIC_STATIC, get(handle_public_static))
        .merge(HubPublicApi::router::<EngineState>())
        .route(ROUTE_HANDLE_HUB_WS, get(handle_hub_ws))
        .route(ROUTE_HANDLE_TUNNEL_PROXY_RELAY, post(handle_tunnel_proxy_relay))
}

// ── Embedded HTML (compiled into binary) ──

const EMBEDDED_INDEX: &str = include_str!("../../../html-frontend/index.html");
const EMBEDDED_SETUP: &str = include_str!("../../../html-frontend/setup.html");
const EMBEDDED_LOGIN: &str = include_str!("../../../html-frontend/login.html");
const EMBEDDED_KNOWLEDGE: &str = include_str!("../../../html-frontend/knowledge.html");
const EMBEDDED_FILES: &str = include_str!("../../../html-frontend/files.html");

/// Fallback handler: serve HTML from disk (dev mode) or embedded (release).
async fn html_fallback(
    State(state): State<Arc<EngineState>>,
    uri: axum::http::Uri,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    let path = uri.path().trim_start_matches('/');
    let filename = if path.is_empty() || path == "index" {
        "index.html"
    } else {
        path
    };

    // Try disk first (dev mode: html_dir has files)
    let disk_path = state.html_dir.join(filename);
    if disk_path.is_file() {
        if let Ok(content) = tokio::fs::read_to_string(&disk_path).await {
            return (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                content,
            )
                .into_response();
        }
    }

    // Fallback to embedded
    match filename {
        "index.html" => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            EMBEDDED_INDEX,
        )
            .into_response(),
        "setup.html" => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            EMBEDDED_SETUP,
        )
            .into_response(),
        "login.html" => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            EMBEDDED_LOGIN,
        )
            .into_response(),
        "knowledge.html" => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            EMBEDDED_KNOWLEDGE,
        )
            .into_response(),
        "files.html" => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            EMBEDDED_FILES,
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Hub proxy middleware — intercepts requests for remote instances and proxies them.
///
/// When hub mode is active (host), this middleware checks if the target instance_id
/// lives on a slave engine. If so, the request is forwarded through the WebSocket tunnel.
/// Local instances and non-instance routes pass through normally.
async fn hub_proxy_middleware(
    State(state): State<Arc<EngineState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // Only active in host mode
    let host = match state.hub.as_host().await {
        Some(h) => h,
        None => return next.run(req).await,
    };

    let path = req.uri().path().to_string();

    // Check if this request targets a specific instance
    if let Some(instance_id) = extract_instance_id_from_path(&path) {
        // Check if this instance is on a connected slave
        if let Some(_engine_id) = host.find_instance_engine(instance_id).await {
            // Decompose request into parts for tunnel forwarding
            let method = req.method().to_string();
            let path_str = req.uri().path().to_string();
            let query_str = req.uri().query().map(|q| q.to_string());

            // Collect headers
            let mut header_map = std::collections::HashMap::new();
            for (name, value) in req.headers().iter() {
                if let Ok(v) = value.to_str() {
                    header_map.insert(name.to_string(), v.to_string());
                }
            }

            let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
                .await
                .unwrap_or_default();

            use base64::Engine as _;
            let tunnel_req = crate::hub::tunnel::TunnelRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                method,
                path: if let Some(q) = &query_str {
                    format!("{}?{}", path_str, q)
                } else {
                    path_str
                },
                headers: header_map,
                body: if body_bytes.is_empty() {
                    None
                } else {
                    Some(base64::engine::general_purpose::STANDARD.encode(&body_bytes))
                },
            };

            match host.proxy_request(instance_id, tunnel_req).await {
                Some(resp) => {
                    let status = StatusCode::from_u16(resp.status)
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                    let mut headers = HeaderMap::new();
                    for (k, v) in &resp.headers {
                        if let (Ok(name), Ok(val)) = (
                            axum::http::header::HeaderName::from_bytes(k.as_bytes()),
                            axum::http::header::HeaderValue::from_str(v),
                        ) {
                            headers.insert(name, val);
                        }
                    }
                    let body_bytes = resp.body
                        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(&b).ok())
                        .unwrap_or_default();
                    (status, headers, body_bytes).into_response()
                }
                None => {
                    tracing::warn!("[HUB] Tunnel proxy failed for {}", instance_id);
                    json_error(StatusCode::BAD_GATEWAY, "Tunnel proxy error")
                }
            }
        } else {
            // Not a remote instance — proceed normally
            next.run(req).await
        }
    } else {
        // Not an instance request — proceed normally
        next.run(req).await
    }
}

/// Build the complete application router.
///
/// Combines public routes, authenticated API routes, login/logout,
/// embedded HTML serving, and auth middleware.
pub fn build_router(engine_state: Arc<EngineState>) -> Router {
    use crate::api::auth;
    use axum::routing::{get, post};

    let mut router = Router::new()
        // Login/logout (auth middleware whitelist covers /login)
        .route(
            auth::ROUTE_HANDLE_LOGIN_PAGE,
            get(auth::handle_login_page).post(auth::handle_login_post),
        )
        .route(auth::ROUTE_HANDLE_LOGOUT, get(auth::handle_logout))
        .route(
            auth::ROUTE_HANDLE_FRONTEND_ERROR,
            post(auth::handle_frontend_error),
        )
        .route(auth::ROUTE_HANDLE_SETUP, post(auth::handle_setup))
        // Authenticated API routes
        .merge(authenticated_api_routes())
        // Public API routes (auth middleware whitelist covers /public/)
        .merge(public_api_routes())
        // HTML fallback: disk (dev) → embedded (release)
        .fallback(html_fallback);

    // Hub proxy middleware: always applied (hub can be enabled at runtime)
    router = router.layer(axum::middleware::from_fn_with_state(
        engine_state.clone(),
        hub_proxy_middleware,
    ));

    router
        // Auth middleware (applied to all routes, whitelist inside)
        .layer(axum::middleware::from_fn_with_state(
            engine_state.clone(),
            auth::check_auth,
        ))
        .with_state(engine_state)
}
