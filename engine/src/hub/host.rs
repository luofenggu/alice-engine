use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, oneshot};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn, error, debug};
use uuid::Uuid;
use base64::Engine as _;

use crate::hub::tunnel::*;

/// A connected slave engine
struct SlaveConnection {
    engine_id: String,
    #[allow(dead_code)]
    engine_endpoint: String,
    instances: Vec<TunnelInstanceInfo>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
}

/// Pending request awaiting response from slave
type PendingRequests = Arc<Mutex<HashMap<String, oneshot::Sender<TunnelResponse>>>>;

/// Host state: manages connected slaves and routes requests
pub struct HostState {
    pub join_token: String,
    /// Public endpoint of this host engine (for hook URLs)
    host_endpoint: String,
    /// Local engine port
    #[allow(dead_code)]
    local_port: u16,
    /// engine_id → SlaveConnection
    connections: RwLock<HashMap<String, SlaveConnection>>,
    /// request_id → response sender (shared across all connections)
    pending: PendingRequests,
}

impl HostState {
    pub fn new(join_token: String, host_endpoint: String, local_port: u16) -> Self {
        Self {
            join_token,
            host_endpoint,
            local_port,
            connections: RwLock::new(HashMap::new()),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Handle a new WebSocket connection from a slave
    pub async fn handle_slave_connection(&self, socket: WebSocket, engine_id: String) {
        let (ws_sender, mut ws_receiver) = socket.split();
        let ws_sender = Arc::new(Mutex::new(ws_sender));
        let pending = self.pending.clone();

        // Wait for register message
        let (registered_engine_id, engine_endpoint, instances) = match ws_receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<TunnelMessage>(&text) {
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

        // Store connection
        {
            let conn = SlaveConnection {
                engine_id: engine_id.clone(),
                engine_endpoint,
                instances,
                sender: ws_sender.clone(),
            };
            self.connections.write().await.insert(engine_id.clone(), conn);
        }

        // Register hooks on slave through tunnel
        self.register_hooks_on_slave(&engine_id).await;

        // Process incoming messages (responses from slave)
        while let Some(msg) = ws_receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<TunnelMessage>(&text) {
                        Ok(TunnelMessage::Response(resp)) => {
                            let mut pending_map = pending.lock().await;
                            if let Some(sender) = pending_map.remove(&resp.request_id) {
                                let _ = sender.send(resp);
                            }
                        }
                        Ok(TunnelMessage::Heartbeat) => {
                            let hb = serde_json::to_string(&TunnelMessage::Heartbeat).unwrap();
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
                        _ => {}
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    warn!("[HUB-HOST] WebSocket error from {}: {}", engine_id, e);
                    break;
                }
                _ => {}
            }
        }

        // Cleanup on disconnect
        info!("[HUB-HOST] Slave disconnected: {}", engine_id_clone);
        self.connections.write().await.remove(&engine_id_clone);
    }

    /// Send an HTTP request through the tunnel to a slave (by instance_id)
    pub async fn proxy_request(&self, instance_id: &str, req: TunnelRequest) -> Option<TunnelResponse> {
        let (_engine_id, sender) = {
            let conns = self.connections.read().await;
            let mut found = None;
            for (eid, conn) in conns.iter() {
                if conn.instances.iter().any(|i| i.id == instance_id) {
                    found = Some((eid.clone(), conn.sender.clone()));
                    break;
                }
            }
            found?
        };

        let request_id = req.request_id.clone();

        // Set up response channel
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(request_id.clone(), tx);
        }

        // Send request through tunnel
        let msg = serde_json::to_string(&TunnelMessage::Request(req)).unwrap();
        {
            let mut ws = sender.lock().await;
            if ws.send(Message::Text(msg.into())).await.is_err() {
                error!("[HUB-HOST] Failed to send tunnel request");
                self.pending.lock().await.remove(&request_id);
                return None;
            }
        }

        // Wait for response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(resp)) => Some(resp),
            Ok(Err(_)) => {
                warn!("[HUB-HOST] Response channel dropped for {}", request_id);
                None
            }
            Err(_) => {
                warn!("[HUB-HOST] Tunnel request timeout for {}", request_id);
                self.pending.lock().await.remove(&request_id);
                None
            }
        }
    }

    /// Register hooks on a slave engine through the tunnel
    async fn register_hooks_on_slave(&self, engine_id: &str) {
        let hooks_body = serde_json::json!({
            "contacts_url": format!("{}/api/hub/contacts/{{instance_id}}", self.host_endpoint),
            "send_msg_relay_url": format!("{}/api/hub/relay/{{instance_id}}", self.host_endpoint),
        });

        let body_bytes = serde_json::to_vec(&hooks_body).unwrap();
        let req = TunnelRequest {
            request_id: Uuid::new_v4().to_string(),
            method: "POST".to_string(),
            path: "/api/hooks".to_string(),
            headers: HashMap::from([
                ("content-type".to_string(), "application/json".to_string()),
            ]),
            body: Some(base64::engine::general_purpose::STANDARD.encode(&body_bytes)),
        };

        if let Some(resp) = self.proxy_request_to_engine(engine_id, req).await {
            if resp.status == 200 {
                info!("[HUB-HOST] Hooks registered on slave {}", engine_id);
            } else {
                warn!("[HUB-HOST] Failed to register hooks on slave {}: status {}", engine_id, resp.status);
            }
        }
    }

    /// Send request directly to a specific engine (by engine_id, not instance_id)
    async fn proxy_request_to_engine(&self, engine_id: &str, req: TunnelRequest) -> Option<TunnelResponse> {
        let sender = {
            let conns = self.connections.read().await;
            conns.get(engine_id)?.sender.clone()
        };

        let request_id = req.request_id.clone();

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(request_id.clone(), tx);
        }

        let msg = serde_json::to_string(&TunnelMessage::Request(req)).unwrap();
        {
            let mut ws = sender.lock().await;
            if ws.send(Message::Text(msg.into())).await.is_err() {
                self.pending.lock().await.remove(&request_id);
                return None;
            }
        }

        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(resp)) => Some(resp),
            _ => {
                self.pending.lock().await.remove(&request_id);
                None
            }
        }
    }

    /// Get all connected slave instances (for contacts aggregation)
    pub async fn get_all_remote_instances(&self) -> Vec<(String, Vec<TunnelInstanceInfo>)> {
        let conns = self.connections.read().await;
        conns.iter().map(|(eid, conn)| {
            (eid.clone(), conn.instances.clone())
        }).collect()
    }

    /// Find which engine has a given instance
    pub async fn find_instance_engine(&self, instance_id: &str) -> Option<String> {
        let conns = self.connections.read().await;
        for (_eid, conn) in conns.iter() {
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

