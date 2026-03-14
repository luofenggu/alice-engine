//! Hub implementation of ExtensionHandler trait.
//!
//! Provides cross-instance communication through the hub module:
//! - Off mode: local instance discovery and relay
//! - Host mode: aggregated remote + local instances
//! - Joined mode: tunnel proxy to host

use std::sync::Arc;
use std::collections::HashSet;
use tracing::{info, warn};
use tokio::runtime::Handle;

use crate::persist::hooks::{ContactInfo, ContactsResponse};
use crate::persist::instance::InstanceStore;
use crate::service::extension::ExtensionHandler;
use super::HubState;
use super::tunnel::TunnelRequest;

/// Hub-backed extension handler.
pub struct HubExtensionHandler {
    hub: Arc<HubState>,
    instance_store: InstanceStore,
    handle: Handle,
}

impl HubExtensionHandler {
    pub fn new(hub: Arc<HubState>, instance_store: InstanceStore, handle: Handle) -> Self {
        Self { hub, instance_store, handle }
    }

    /// Collect local instance contacts (id + name), excluding the requester.
    fn local_contacts(&self, exclude_id: &str) -> Vec<ContactInfo> {
        match self.instance_store.list_ids() {
            Ok(ids) => {
                ids.into_iter()
                    .filter(|id| id != exclude_id)
                    .map(|id| {
                        let name = self.instance_store.open(&id)
                            .ok()
                            .and_then(|inst| {
                                let n = inst.settings.load().ok()
                                    .and_then(|s| s.name.clone())
                                    .unwrap_or_else(|| id.clone());
                                if n != id { Some(n) } else { None }
                            });
                        ContactInfo { id, name }
                    })
                    .collect()
            }
            Err(e) => {
                warn!("[EXT] Failed to list local instances: {}", e);
                vec![]
            }
        }
    }

    /// Local relay: deliver message to a local instance.
    fn local_relay(&self, from: &str, to: &str, content: &str) -> Result<(), String> {
        let instance = self.instance_store.open(to)
            .map_err(|e| format!("Instance {} not found: {}", to, e))?;
        let mut ch = instance.chat.lock()
            .unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
        let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
        ch.write_agent_reply(from, content, &timestamp, "")
            .map(|_| ())
            .map_err(|e| format!("Failed to write relay message: {}", e))
    }
}

impl ExtensionHandler for HubExtensionHandler {
    fn relay_message(&self, from: &str, to: &str, content: &str) -> Result<(), String> {
        let mode = self.handle.block_on(self.hub.mode.read());

        match &*mode {
            super::HubMode::Host(host) => {
                let host = host.clone();
                let from = from.to_string();
                let to = to.to_string();
                let content = content.to_string();

                drop(mode);

                self.handle.block_on(async {
                    // Check if target is on a remote slave
                    if let Some(_engine_id) = host.find_instance_engine(&to).await {
                        let relay_body = serde_json::json!({
                            "sender": &from,
                            "content": &content,
                        });
                        let body_bytes = serde_json::to_vec(&relay_body).unwrap_or_default();

                        let tunnel_req = TunnelRequest {
                            request_id: uuid::Uuid::new_v4().to_string(),
                            method: "POST".to_string(),
                            path: format!("/api/instances/{}/messages/relay", to),
                            headers: {
                                let mut h = std::collections::HashMap::new();
                                h.insert("Content-Type".to_string(), "application/json".to_string());
                                h
                            },
                            body: Some(base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &body_bytes,
                            )),
                        };

                        match host.proxy_request(&to, tunnel_req).await {
                            Some(resp) if resp.status < 300 => {
                                info!("[EXT] Relayed message from {} to {} via tunnel", from, to);
                                Ok(())
                            }
                            Some(resp) => {
                                Err(format!("Target engine returned {}", resp.status))
                            }
                            None => Err("Tunnel request failed".to_string()),
                        }
                    } else {
                        // Local delivery
                        let instance = self.instance_store.open(&to)
                            .map_err(|e| format!("Instance {} not found: {}", to, e))?;
                        let mut ch = instance.chat.lock()
                            .unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
                        let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
                        ch.write_agent_reply(&from, &content, &timestamp, "")
                            .map(|_| {
                                info!("[EXT] Relayed message from {} to {} (local)", from, to);
                            })
                            .map_err(|e| format!("Failed to write relay message: {}", e))
                    }
                })
            }

            super::HubMode::Joined(slave) => {
                let slave = slave.clone();
                let from = from.to_string();
                let to = to.to_string();
                let content = content.to_string();

                drop(mode);

                self.handle.block_on(async {
                    let relay_body = serde_json::json!({
                        "from_instance": &from,
                        "to_instance": &to,
                        "content": &content,
                    });
                    let body_bytes = serde_json::to_vec(&relay_body).unwrap_or_default();

                    let tunnel_req = TunnelRequest {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        method: "POST".to_string(),
                        path: crate::bindings::http::HUB_RELAY_PATH.to_string(),
                        headers: {
                            let mut h = std::collections::HashMap::new();
                            h.insert("Content-Type".to_string(), "application/json".to_string());
                            h
                        },
                        body: Some(base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            &body_bytes,
                        )),
                    };

                    match slave.proxy_request_to_host(tunnel_req).await {
                        Some(resp) if resp.status < 300 => {
                            info!("[EXT] Relayed message from {} to {} via host", from, to);
                            Ok(())
                        }
                        Some(resp) => Err(format!("Host returned {}", resp.status)),
                        None => Err("Host request failed".to_string()),
                    }
                })
            }

            super::HubMode::Off => {
                drop(mode);
                self.local_relay(from, to, content)
            }
        }
    }

    fn fetch_contacts(&self, instance_id: &str) -> Vec<ContactInfo> {
        let mode = self.handle.block_on(self.hub.mode.read());

        match &*mode {
            super::HubMode::Host(host) => {
                let host = host.clone();
                let instance_id = instance_id.to_string();
                let local_contacts = self.local_contacts(&instance_id);

                drop(mode);

                let remote = self.handle.block_on(host.get_all_remote_instances());

                let mut seen: HashSet<String> = HashSet::new();
                let mut contacts: Vec<ContactInfo> = Vec::new();

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

                for c in local_contacts {
                    if seen.insert(c.id.clone()) {
                        contacts.push(c);
                    }
                }

                contacts
            }

            super::HubMode::Joined(slave) => {
                let slave = slave.clone();
                let instance_id = instance_id.to_string();

                drop(mode);

                self.handle.block_on(async {
                    let contacts_path = format!("{}{}", crate::bindings::http::HUB_CONTACTS_PATH_PREFIX, instance_id);
                    let tunnel_req = TunnelRequest {
                        request_id: uuid::Uuid::new_v4().to_string(),
                        method: "GET".to_string(),
                        path: contacts_path,
                        headers: std::collections::HashMap::new(),
                        body: None,
                    };

                    match slave.proxy_request_to_host(tunnel_req).await {
                        Some(resp) if resp.status < 300 => {
                            if let Some(body) = resp.body {
                                if let Ok(decoded) = base64::Engine::decode(
                                    &base64::engine::general_purpose::STANDARD,
                                    &body,
                                ) {
                                    if let Ok(contacts_resp) = serde_json::from_slice::<ContactsResponse>(&decoded) {
                                        return contacts_resp.contacts;
                                    }
                                }
                            }
                            warn!("[EXT] Failed to parse contacts response from host");
                            vec![]
                        }
                        _ => {
                            warn!("[EXT] Failed to fetch contacts from host");
                            vec![]
                        }
                    }
                })
            }

            super::HubMode::Off => {
                let contacts = self.local_contacts(instance_id);
                drop(mode);
                contacts
            }
        }
    }
}
