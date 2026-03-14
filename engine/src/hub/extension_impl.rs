//! Hub implementation of ExtensionHandler trait.
//!
//! Provides cross-instance communication through the hub module:
//! - Off mode: local instance discovery and relay
//! - Host mode: aggregated remote + local instances
//! - Joined mode: tunnel proxy to host via ExtensionHandlerProxy

use std::sync::Arc;
use std::collections::HashSet;
use tracing::{info, warn};

use crate::persist::hooks::ContactInfo;
use crate::persist::instance::InstanceStore;
use crate::service::extension::{ExtensionHandler, ExtensionHandlerProxy};
use super::HubState;


/// Hub-backed extension handler.
pub struct HubExtensionHandler {
    hub: Arc<HubState>,
    instance_store: InstanceStore,
}

impl HubExtensionHandler {
    pub fn new(hub: Arc<HubState>, instance_store: InstanceStore) -> Self {
        Self { hub, instance_store }
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

#[async_trait::async_trait]
impl ExtensionHandler for HubExtensionHandler {
    async fn relay_message(&self, from: String, to: String, content: String) -> Result<(), String> {
        let mode = self.hub.mode.read().await;

        match &*mode {
            super::HubMode::Host(host) => {
                let host = host.clone();
                drop(mode);

                // Check if target is on a remote slave
                if let Some(_engine_id) = host.find_instance_engine(&to).await {
                    // Remote: use tunnel proxy via slave's TunnelEndpoint
                    if let Some(ep) = host.get_slave_tunnel_endpoint(&to).await {
                        let proxy = ExtensionHandlerProxy::new(ep);
                        match proxy.relay_message(from.clone(), to.clone(), content).await {
                            Ok(()) => {
                                info!("[EXT] Relayed message from {} to {} via tunnel proxy", from, to);
                                Ok(())
                            }
                            Err(e) => Err(format!("Tunnel relay failed: {}", e)),
                        }
                    } else {
                        Err("Slave tunnel endpoint not available".to_string())
                    }
                } else {
                    // Local delivery
                    self.local_relay(&from, &to, &content).map(|()| {
                        info!("[EXT] Relayed message from {} to {} (local)", from, to);
                    })
                }
            }

            super::HubMode::Joined(slave) => {
                let slave = slave.clone();
                drop(mode);

                // Use ExtensionHandlerProxy through tunnel to host
                let ep = slave.get_tunnel_endpoint()
                    .ok_or_else(|| "Not connected to host".to_string())?;
                let proxy = ExtensionHandlerProxy::new(ep);
                proxy.relay_message(from.clone(), to.clone(), content).await.map(|()| {
                    info!("[EXT] Relayed message from {} to {} via host proxy", from, to);
                })
            }

            super::HubMode::Off => {
                drop(mode);
                self.local_relay(&from, &to, &content)
            }
        }
    }

    async fn fetch_contacts(&self, instance_id: String) -> Result<Vec<ContactInfo>, String> {
        let mode = self.hub.mode.read().await;

        match &*mode {
            super::HubMode::Host(host) => {
                let host = host.clone();
                let local_contacts = self.local_contacts(&instance_id);
                drop(mode);

                let remote = host.get_all_remote_instances().await;

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

                Ok(contacts)
            }

            super::HubMode::Joined(slave) => {
                let slave = slave.clone();
                drop(mode);

                // Use ExtensionHandlerProxy through tunnel to host
                let ep = slave.get_tunnel_endpoint()
                    .ok_or_else(|| "Not connected to host".to_string())?;
                let proxy = ExtensionHandlerProxy::new(ep);
                proxy.fetch_contacts(instance_id).await
            }

            super::HubMode::Off => {
                let contacts = self.local_contacts(&instance_id);
                drop(mode);
                Ok(contacts)
            }
        }
    }
}

/// Slave-side local handler for host→slave RPC requests.
/// Handles relay_message and fetch_contacts for local instances only.
pub struct SlaveLocalHandler {
    instance_store: InstanceStore,
}

impl SlaveLocalHandler {
    pub fn new(instance_store: InstanceStore) -> Self {
        Self { instance_store }
    }

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
                warn!("[EXT-SLAVE] Failed to list local instances: {}", e);
                vec![]
            }
        }
    }

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

#[async_trait::async_trait]
impl ExtensionHandler for SlaveLocalHandler {
    async fn relay_message(&self, from: String, to: String, content: String) -> Result<(), String> {
        self.local_relay(&from, &to, &content).map(|()| {
            info!("[EXT-SLAVE] Relayed message from {} to {} (local)", from, to);
        })
    }

    async fn fetch_contacts(&self, instance_id: String) -> Result<Vec<ContactInfo>, String> {
        Ok(self.local_contacts(&instance_id))
    }
}

