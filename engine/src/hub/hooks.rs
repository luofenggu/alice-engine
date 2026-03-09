//! Hub hooks — callback endpoints for host mode.
//!
//! When this engine is in host mode, it provides hook callbacks:
//! - contacts: returns aggregated contact list from connected slaves
//! - relay: routes messages between instances across engines via WebSocket tunnel

use axum::{
    response::{IntoResponse, Response},
    Json,
};

use super::host::HostState;
use super::tunnel::{TunnelRequest, TunnelInstanceInfo};
use crate::persist::hooks::{ContactInfo, ContactsResponse, RelayRequest, RelayResponse};
use std::sync::Arc;

/// Handle contacts hook callback: return all instances except the requesting one.
pub async fn handle_hub_contacts(
    host: &HostState,
    state: &crate::api::state::EngineState,
    instance_id: &str,
) -> Response {
    // Collect remote instances from connected slaves
    let remote = host.get_all_remote_instances().await;

    // Collect local instances
    let local_instances = state.get_instances().await;

    let mut seen = std::collections::HashSet::new();
    let mut contacts: Vec<ContactInfo> = Vec::new();

    // Add remote instances
    for (_engine_id, instances) in &remote {
        for inst in instances {
            if inst.id != instance_id && seen.insert(inst.id.clone()) {
                contacts.push(ContactInfo {
                    id: inst.id.clone(),
                    name: if inst.name != inst.id { Some(inst.name.clone()) } else { None },
                });
            }
        }
    }

    // Add local instances
    for inst in &local_instances {
        if inst.id.as_str() != instance_id && seen.insert(inst.id.clone()) {
            contacts.push(ContactInfo {
                id: inst.id.clone(),
                name: if inst.name != inst.id { Some(inst.name.clone()) } else { None },
            });
        }
    }

    Json(ContactsResponse { contacts }).into_response()
}

/// Handle relay hook callback: route a message to the target instance via tunnel.
pub async fn handle_hub_relay(
    host: &HostState,
    state: &crate::api::state::EngineState,
    body: RelayRequest,
) -> Response {
    let target_id = &body.to_instance;
    let from_id = &body.from_instance;
    let content = &body.content;

    // Check if target is on a connected slave
    if let Some(_engine_id) = host.find_instance_engine(target_id).await {
        // Route through WebSocket tunnel
        let relay_body = serde_json::json!({
            "sender": from_id,
            "content": content,
        });
        let body_bytes = serde_json::to_vec(&relay_body).unwrap_or_default();

        let tunnel_req = TunnelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            method: "POST".to_string(),
            path: format!("/api/instances/{}/messages/relay", target_id),
            headers: {
                let mut h = std::collections::HashMap::new();
                h.insert("Content-Type".to_string(), "application/json".to_string());
                h
            },
            body: Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &body_bytes)),
        };

        match host.proxy_request(target_id, tunnel_req).await {
            Some(resp) if resp.status < 300 => {
                tracing::info!(
                    "[HUB] Relayed message from {} to {} via tunnel",
                    from_id, target_id
                );
                Json(RelayResponse { success: true, message: None }).into_response()
            }
            Some(resp) => {
                tracing::warn!(
                    "[HUB] Relay via tunnel failed: {} -> {} returned {}",
                    from_id, target_id, resp.status
                );
                Json(RelayResponse {
                    success: false,
                    message: Some(format!("Target engine returned {}", resp.status)),
                }).into_response()
            }
            None => {
                tracing::warn!(
                    "[HUB] Relay via tunnel failed: {} -> {} (no response)",
                    from_id, target_id
                );
                Json(RelayResponse {
                    success: false,
                    message: Some("Tunnel request failed".to_string()),
                }).into_response()
            }
        }
    } else {
        // Try local delivery
        let store = state.instance_store.clone();
        let target = target_id.clone();
        let sender = from_id.clone();
        let msg_content = content.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&target)?;
            let mut ch = instance.chat.lock().unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
            let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
            ch.write_agent_reply(&sender, &msg_content, &timestamp, "")
        })
        .await;

        match result {
            Ok(Ok(id)) => {
                tracing::info!(
                    "[HUB] Relayed message from {} to {} (local), id={}",
                    from_id, target_id, id
                );
                Json(RelayResponse { success: true, message: None }).into_response()
            }
            _ => {
                tracing::warn!(
                    "[HUB] Relay failed: target {} not found anywhere",
                    target_id
                );
                Json(RelayResponse {
                    success: false,
                    message: Some(format!("Instance {} not found", target_id)),
                }).into_response()
            }
        }
    }
}