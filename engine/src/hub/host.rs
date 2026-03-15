use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, mpsc};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn, error};

use mad_hatter::tunnel::{Dispatch, TunnelEndpoint};
use crate::hub::tunnel::*;
use crate::persist::instance::InstanceStore;
use crate::service::http_proxy::{HttpProxy, HttpProxyProxy, HttpProxyRequest, HttpProxyResponse};

/// A connected slave engine
struct SlaveConnection {
    engine_id: String,
    #[allow(dead_code)]
    engine_endpoint: String,
    instances: Vec<TunnelInstanceInfo>,
    /// Whether this connection is still alive (set to false on disconnect or heartbeat failure)
    connected: Arc<AtomicBool>,
    /// TunnelEndpoint for RPC calls to this slave
    tunnel_endpoint: Option<Arc<TunnelEndpoint>>,
}

/// Host state: manages connected slaves and routes requests
pub struct HostState {
    pub join_token: String,
    /// engine_id → SlaveConnection
    connections: RwLock<HashMap<String, SlaveConnection>>,
    /// Instance store for local operations
    instance_store: InstanceStore,
}

impl HostState {
    pub fn new(join_token: String, instance_store: InstanceStore) -> Self {
        Self {
            join_token,
            connections: RwLock::new(HashMap::new()),
            instance_store,
        }
    }

    /// Handle a new WebSocket connection from a slave
    pub async fn handle_slave_connection(self: &Arc<Self>, socket: WebSocket, engine_id: String) {
        let (ws_sender, mut ws_receiver) = socket.split();
        let ws_sender = Arc::new(Mutex::new(ws_sender));

        // Wait for register message
        let (registered_engine_id, engine_endpoint, instances) = match ws_receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                match TunnelMessage::decode(&text) {
                    Ok(TunnelMessage::Register { engine_id: eid, engine_endpoint, instances }) => {
                        info!("[HUB-HOST] Slave registered: engine_id={}, instances={:?}",
                            eid, instances.iter().map(|i| &i.id).collect::<Vec<_>>());
                        (eid, engine_endpoint, instances)
                    }
                    _ => {
                        warn!("[HUB-HOST] Expected register message, got something else");
                        return;
                    }
                }
            }
            _ => {
                warn!("[HUB-HOST] WebSocket closed before register");
                return;
            }
        };

        // Use the engine_id from register message (more authoritative than query param)
        let engine_id = if registered_engine_id.is_empty() { engine_id } else { registered_engine_id };
        let engine_id_clone = engine_id.clone();

        // Connection health flag
        let connected = Arc::new(AtomicBool::new(true));

        // Create channel pair + TunnelEndpoint for RPC (replaces WsBridge)
        let (ws_to_tunnel_tx, ws_to_tunnel_rx) = mpsc::unbounded_channel::<String>();
        let (tunnel_to_ws_tx, mut tunnel_to_ws_rx) = mpsc::unbounded_channel::<String>();
        let dispatchers = self.build_host_dispatchers();
        let tunnel_endpoint = TunnelEndpoint::new(
            ws_to_tunnel_rx,
            tunnel_to_ws_tx,
            dispatchers,
            Duration::from_secs(10),
        );

        // Store connection (clean stale connections with same endpoint first — Bug fix: zombie endpoint)
        {
            let conn = SlaveConnection {
                engine_id: engine_id.clone(),
                engine_endpoint: engine_endpoint.clone(),
                instances,
                connected: connected.clone(),
                tunnel_endpoint: Some(tunnel_endpoint),
            };
            let mut conns = self.connections.write().await;
            // Remove any existing connection from the same endpoint (handles engine_id changes on re-join)
            conns.retain(|eid, old_conn| {
                if old_conn.engine_endpoint == engine_endpoint && *eid != engine_id {
                    info!("[HUB-HOST] Cleaning stale connection: {} (same endpoint {})", eid, engine_endpoint);
                    old_conn.connected.store(false, Ordering::Relaxed);
                    false
                } else {
                    true
                }
            });
            conns.insert(engine_id.clone(), conn);
        }

        // Spawn heartbeat sender: periodically send heartbeat to detect half-open connections
        let hb_sender = ws_sender.clone();
        let hb_connected = connected.clone();
        let hb_engine_id = engine_id.clone();
        let heartbeat_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(20));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                if !hb_connected.load(Ordering::Relaxed) {
                    break;
                }
                let hb = TunnelMessage::Heartbeat.encode();
                let mut sender = hb_sender.lock().await;
                if sender.send(Message::Text(hb.into())).await.is_err() {
                    warn!("[HUB-HOST] Heartbeat send failed for {}, marking disconnected", hb_engine_id);
                    hb_connected.store(false, Ordering::Relaxed);
                    break;
                }
            }
        });

        // Process incoming messages with tokio::select! for bidirectional RPC
        loop {
            tokio::select! {
                msg = ws_receiver.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            match TunnelMessage::decode(&text) {
                                Ok(TunnelMessage::Heartbeat) => {
                                    let hb = TunnelMessage::Heartbeat.encode();
                                    let mut sender = ws_sender.lock().await;
                                    let _ = sender.send(Message::Text(hb.into())).await;
                                }
                                Ok(TunnelMessage::Register { instances, .. }) => {
                                    // Update instances list
                                    let mut conns = self.connections.write().await;
                                    if let Some(conn) = conns.get_mut(&engine_id) {
                                        info!("[HUB-HOST] Slave {} updated instances: {:?}",
                                            engine_id, instances.iter().map(|i| &i.id).collect::<Vec<_>>());
                                        conn.instances = instances;
                                    }
                                }
                                Ok(TunnelMessage::Leave) => {
                                    info!("[HUB-HOST] Slave {} sent Leave message, disconnecting", engine_id);
                                    break;
                                }
                                Ok(TunnelMessage::Rpc { payload }) => {
                                    // Forward RPC message to TunnelEndpoint via channel
                                    if ws_to_tunnel_tx.send(payload).is_err() {
                                        warn!("[HUB-HOST] Tunnel channel closed, cannot forward RPC");
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(Message::Close(_))) => break,
                        Some(Err(e)) => {
                            warn!("[HUB-HOST] WebSocket error from {}: {}", engine_id, e);
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
                Some(rpc_payload) = tunnel_to_ws_rx.recv() => {
                    // TunnelEndpoint wants to send an RPC message → forward to WebSocket
                    let rpc_msg = TunnelMessage::Rpc { payload: rpc_payload };
                    let text = rpc_msg.encode();
                    let mut sender = ws_sender.lock().await;
                    if sender.send(Message::Text(text.into())).await.is_err() {
                        warn!("[HUB-HOST] Failed to send RPC to WebSocket");
                        break;
                    }
                }
            }
        }

        // Cleanup on disconnect
        connected.store(false, Ordering::Relaxed);
        heartbeat_task.abort();
        // ws_to_tunnel_tx is dropped here → TunnelEndpoint reader task exits
        info!("[HUB-HOST] Slave disconnected: {}", engine_id_clone);
        self.connections.write().await.remove(&engine_id_clone);
    }

    /// Build dispatchers for host-side TunnelEndpoint (handles slave→host RPC)
    fn build_host_dispatchers(self: &Arc<Self>) -> Vec<Box<dyn Dispatch>> {
        use crate::service::extension::ExtensionHandlerDispatcher;
        let handler = HostLocalHandler {
            instance_store: self.instance_store.clone(),
            host: Arc::downgrade(self),
        };
        vec![ExtensionHandlerDispatcher::boxed(Arc::new(handler))]
    }

    /// Get the TunnelEndpoint for a slave that hosts the given instance
    pub async fn get_slave_tunnel_endpoint(&self, instance_id: &str) -> Option<Arc<TunnelEndpoint>> {
        let conns = self.connections.read().await;
        for (_eid, conn) in conns.iter() {
            if !conn.connected.load(Ordering::Relaxed) {
                continue;
            }
            if conn.instances.iter().any(|i| i.id == instance_id) {
                return conn.tunnel_endpoint.clone();
            }
        }
        None
    }

    /// Send an HTTP request through the tunnel to a slave (by instance_id)
    /// Uses HttpProxy RPC via tunnel_service — no manual serialization needed
    pub async fn proxy_request(&self, instance_id: &str, req: HttpProxyRequest) -> Option<HttpProxyResponse> {
        let endpoint = self.get_slave_tunnel_endpoint(instance_id).await?;
        let proxy = HttpProxyProxy::new(endpoint);
        match proxy.proxy_http(req).await {
            Ok(resp) => Some(resp),
            Err(e) => {
                error!("[HUB-HOST] Proxy request failed for instance {}: {}", instance_id, e);
                None
            }
        }
    }

    /// Get all connected slave instances (for contacts aggregation)
    /// Filters out disconnected slaves
    pub async fn get_all_remote_instances(&self) -> Vec<(String, Vec<TunnelInstanceInfo>)> {
        let conns = self.connections.read().await;
        conns.iter()
            .filter(|(_, conn)| conn.connected.load(Ordering::Relaxed))
            .map(|(eid, conn)| {
                (eid.clone(), conn.instances.clone())
            }).collect()
    }

    /// Get remote instances grouped by endpoint URL (for frontend display)
    pub async fn get_remote_endpoints(&self) -> Vec<(String, Vec<TunnelInstanceInfo>)> {
        let conns = self.connections.read().await;
        conns.iter()
            .filter(|(_, conn)| conn.connected.load(Ordering::Relaxed))
            .map(|(_, conn)| {
                (conn.engine_endpoint.clone(), conn.instances.clone())
            }).collect()
    }

    /// Find which engine has a given instance (only among connected slaves)
    pub async fn find_instance_engine(&self, instance_id: &str) -> Option<String> {
        let conns = self.connections.read().await;
        for (_eid, conn) in conns.iter() {
            if !conn.connected.load(Ordering::Relaxed) {
                continue;
            }
            if conn.instances.iter().any(|i| i.id == instance_id) {
                return Some(conn.engine_id.clone());
            }
        }
        None
    }

    /// Get number of connected slaves
    pub async fn slave_count(&self) -> usize {
        self.connections.read().await.len()
    }
}

/// Host-local handler for slave→host RPC requests.
/// Registered as Dispatcher on each slave's TunnelEndpoint.
struct HostLocalHandler {
    instance_store: InstanceStore,
    host: Weak<HostState>,
}

use crate::service::extension::ExtensionHandler;
use crate::persist::hooks::ContactInfo;

#[async_trait::async_trait]
impl ExtensionHandler for HostLocalHandler {
    async fn relay_message(
        &self,
        from_instance_id: String,
        to_instance_id: String,
        content: String,
    ) -> Result<(), String> {
        // Try local delivery first
        if let Ok(instance) = self.instance_store.open(&to_instance_id) {
            let ch = &instance.chat;
            let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
            ch.write_agent_reply(&from_instance_id, &content, &timestamp, "")
                .map(|_| {
                    info!("[HUB-HOST-RPC] Relayed message from {} to {} (local)", from_instance_id, to_instance_id);
                })
                .map_err(|e| format!("Failed to write relay message: {}", e))
        } else {
            // Target not local — try routing to another slave via RPC
            let host = self.host.upgrade()
                .ok_or_else(|| "Host state no longer available".to_string())?;

            if let Some(endpoint) = host.get_slave_tunnel_endpoint(&to_instance_id).await {
                let proxy = crate::service::extension::ExtensionHandlerProxy::new(endpoint);
                proxy.relay_message(from_instance_id.clone(), to_instance_id.clone(), content.clone()).await
                    .map(|_| {
                        info!("[HUB-HOST-RPC] Relayed message from {} to {} via slave RPC", from_instance_id, to_instance_id);
                    })
            } else {
                Err(format!("Instance {} not found on any connected engine", to_instance_id))
            }
        }
    }

    async fn fetch_contacts(&self, instance_id: String) -> Result<Vec<ContactInfo>, String> {
        let mut seen = std::collections::HashSet::new();
        let mut contacts: Vec<ContactInfo> = Vec::new();

        // Collect remote contacts from all slaves
        if let Some(host) = self.host.upgrade() {
            let remote = host.get_all_remote_instances().await;
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
        }

        // Collect local contacts
        if let Ok(ids) = self.instance_store.list_ids() {
            for id in ids {
                if id != instance_id && seen.insert(id.clone()) {
                    let name = self.instance_store.open(&id)
                        .ok()
                        .and_then(|inst| {
                            let n = inst.settings.load().ok()
                                .and_then(|s| s.name.clone())
                                .unwrap_or_else(|| id.clone());
                            if n != id { Some(n) } else { None }
                        });
                    contacts.push(ContactInfo { id, name });
                }
            }
        }

        Ok(contacts)
    }
}