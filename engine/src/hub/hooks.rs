//! Hub hooks — callback endpoints and startup registration.
//!
//! The hub provides hook callbacks that slave engines call:
//! - contacts: returns aggregated contact list for an instance
//! - relay: routes messages between instances across engines
//!
//! At startup, the hub registers these hooks on each slave engine.

use axum::{

    response::{IntoResponse, Response},
    Json,
};

use super::HubState;
use crate::persist::hooks::{ContactInfo, ContactsResponse, RelayRequest, RelayResponse};

/// Handle contacts hook callback: return all instances except the requesting one.
/// Called from routes.rs handler which extracts State and Path.
pub async fn handle_hub_contacts(
    hub: &HubState,
    instance_id: &str,
) -> Response {
    // Collect all instances except the requesting one
    let all_instances = hub.all_instances();
    let contacts: Vec<ContactInfo> = all_instances
        .iter()
        .filter(|inst| inst.id.as_str() != instance_id)
        .map(|inst| ContactInfo {
            id: inst.id.clone(),
            name: if inst.name != inst.id {
                Some(inst.name.clone())
            } else {
                None
            },
        })
        .collect();

    Json(ContactsResponse { contacts }).into_response()
}

/// Handle relay hook callback: route a message to the target instance.
/// Called from routes.rs handler which extracts State and Json.
pub async fn handle_hub_relay(
    hub: &HubState,
    state: &crate::api::state::EngineState,
    body: RelayRequest,
) -> Response {
    let target_id = &body.to_instance;
    let from_id = &body.from_instance;
    let content = &body.content;

    // Find which engine the target instance is on
    match hub.route(target_id) {
        Some(route) => {
            // Relay to the target engine via /api/instances/{id}/messages/relay
            let url = format!(
                "{}/api/instances/{}/messages/relay",
                route.endpoint, target_id
            );
            let auth_cookie = hub.build_auth_cookie(&route);

            let relay_body = serde_json::json!({
                "sender": from_id,
                "content": content,
            });

            match hub
                .client
                .post(&url)
                .header("Cookie", auth_cookie)
                .json(&relay_body)
                .send()
                .await
            {
                Ok(resp) => {
                    if resp.status().is_success() {
                        tracing::info!(
                            "[HUB] Relayed message from {} to {} on {}",
                            from_id,
                            target_id,
                            route.label
                        );
                        Json(RelayResponse {
                            success: true,
                            message: None,
                        })
                        .into_response()
                    } else {
                        let status = resp.status();
                        let body_text = resp.text().await.unwrap_or_default();
                        tracing::warn!(
                            "[HUB] Relay failed: {} -> {} returned {}: {}",
                            from_id,
                            target_id,
                            status,
                            body_text
                        );
                        Json(RelayResponse {
                            success: false,
                            message: Some(format!("Target engine returned {}", status)),
                        })
                        .into_response()
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[HUB] Relay request failed: {} -> {}: {}",
                        from_id,
                        target_id,
                        e
                    );
                    Json(RelayResponse {
                        success: false,
                        message: Some(format!("Request failed: {}", e)),
                    })
                    .into_response()
                }
            }
        }
        None => {
            // Target instance not found in any engine — try local
            // (This handles the case where the target is on the hub engine itself)
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
                        from_id,
                        target_id,
                        id
                    );
                    Json(RelayResponse {
                        success: true,
                        message: None,
                    })
                    .into_response()
                }
                _ => {
                    tracing::warn!(
                        "[HUB] Relay failed: target {} not found anywhere",
                        target_id
                    );
                    Json(RelayResponse {
                        success: false,
                        message: Some(format!("Instance {} not found", target_id)),
                    })
                    .into_response()
                }
            }
        }
    }
}

/// Register hooks on all slave engines.
/// Called at startup to inject contacts and relay callbacks.
pub async fn register_hooks_on_engines(hub: &HubState) {
    let self_port = hub.self_port;

    for engine in &hub.engines {
        let hooks_url = format!("{}/api/hooks", engine.endpoint);
        let auth_cookie = hub.build_auth_cookie(&InstanceRoute {
            endpoint: engine.endpoint.clone(),
            auth_token: engine.auth_token.clone(),
            label: engine.label.clone(),
        });

        // Register contacts and relay hooks pointing back to this hub
        let hooks_config = serde_json::json!({
            "contacts_url": format!("http://localhost:{}/api/hub/contacts/{{instance_id}}", self_port),
            "send_msg_relay_url": format!("http://localhost:{}/api/hub/relay", self_port),
        });

        match hub
            .client
            .post(&hooks_url)
            .header("Cookie", auth_cookie)
            .json(&hooks_config)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    tracing::info!(
                        "[HUB] Registered hooks on {} ({})",
                        engine.label,
                        engine.endpoint
                    );
                } else {
                    tracing::warn!(
                        "[HUB] Failed to register hooks on {} ({}): HTTP {}",
                        engine.label,
                        engine.endpoint,
                        resp.status()
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "[HUB] Failed to register hooks on {} ({}): {}",
                    engine.label,
                    engine.endpoint,
                    e
                );
            }
        }
    }
}

// Need to import InstanceRoute for the register function
use super::InstanceRoute;

