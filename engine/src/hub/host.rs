use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, RwLock, oneshot};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn, error, debug};

use base64::Engine as _;

use crate::hub::tunnel::*;

/// A connected slave engine
struct SlaveConnection {
    engine_id: String,
    #[allow(dead_code)]
    engine_endpoint: String,
    instances: Vec<TunnelInstanceInfo>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    /// Whether this connection is still alive (set to false on disconnect or heartbeat failure)
    connected: Arc<AtomicBool>,
}

/// Pending request awaiting response from slave
type PendingRequests = Arc<Mutex<HashMap<String, oneshot::Sender<TunnelResponse>>>>;

/// Host state: manages connected slaves and routes requests
pub struct HostState {
    pub join_token: String,
    /// Local engine port
    local_port: u16,
    /// Auth secret for local API requests
    auth_secret: String,
    /// engine_id → SlaveConnection
    connections: RwLock<HashMap<String, SlaveConnection>>,
    /// request_id → response sender (shared across all connections)
    pending: PendingRequests,
}

impl HostState {
    pub fn new(join_token: String, local_port: u16, auth_secret: String) -> Self {
        Self {
            join_token,
            local_port,
            auth_secret,
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

        // Connection health flag
        let connected = Arc::new(AtomicBool::new(true));

        // Store connection
        {
            let conn = SlaveConnection {
                engine_id: engine_id.clone(),
                engine_endpoint,
                instances,
                sender: ws_sender.clone(),
                connected: connected.clone(),
            };
            self.connections.write().await.insert(engine_id.clone(), conn);
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
                let hb = serde_json::to_string(&TunnelMessage::Heartbeat).unwrap();
                let mut sender = hb_sender.lock().await;
                if sender.send(Message::Text(hb.into())).await.is_err() {
                    warn!("[HUB-HOST] Heartbeat send failed for {}, marking disconnected", hb_engine_id);
                    hb_connected.store(false, Ordering::Relaxed);
                    break;
                }
            }
        });

        // Process incoming messages (responses from slave, or requests from slave)
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
                        Ok(TunnelMessage::Request(req)) => {
                            // Slave is requesting something from host — handle locally
                            let sender = ws_sender.clone();
                            let port = self.local_port;
                            let auth_secret = self.auth_secret.clone();
                            tokio::spawn(async move {
                                let resp = handle_host_local_request(req, port, &auth_secret).await;
                                let msg = serde_json::to_string(&TunnelMessage::Response(resp)).unwrap();
                                let mut s = sender.lock().await;
                                let _ = s.send(Message::Text(msg.into())).await;
                            });
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
        connected.store(false, Ordering::Relaxed);
        heartbeat_task.abort();
        info!("[HUB-HOST] Slave disconnected: {}", engine_id_clone);
        self.connections.write().await.remove(&engine_id_clone);
    }

    /// Send an HTTP request through the tunnel to a slave (by instance_id)
    pub async fn proxy_request(&self, instance_id: &str, req: TunnelRequest) -> Option<TunnelResponse> {
        let (engine_id, sender, connected) = {
            let conns = self.connections.read().await;
            let mut found = None;
            for (eid, conn) in conns.iter() {
                if conn.instances.iter().any(|i| i.id == instance_id) {
                    found = Some((eid.clone(), conn.sender.clone(), conn.connected.clone()));
                    break;
                }
            }
            found?
        };

        // Fast fail: check if connection is still alive
        if !connected.load(Ordering::Relaxed) {
            warn!("[HUB-HOST] Connection already dead for instance {}, cleaning up", instance_id);
            self.connections.write().await.remove(&engine_id);
            return None;
        }

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
                error!("[HUB-HOST] Failed to send tunnel request, marking disconnected");
                connected.store(false, Ordering::Relaxed);
                self.pending.lock().await.remove(&request_id);
                self.connections.write().await.remove(&engine_id);
                return None;
            }
        }

        // Wait for response with timeout (10s — fail fast)
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(resp)) => Some(resp),
            Ok(Err(_)) => {
                warn!("[HUB-HOST] Response channel dropped for {}", request_id);
                None
            }
            Err(_) => {
                warn!("[HUB-HOST] Tunnel request timeout for {} (10s), marking disconnected", request_id);
                connected.store(false, Ordering::Relaxed);
                self.pending.lock().await.remove(&request_id);
                self.connections.write().await.remove(&engine_id);
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

/// Handle a tunnel request from slave by making a local HTTP request on the host
async fn handle_host_local_request(req: TunnelRequest, local_port: u16, auth_secret: &str) -> TunnelResponse {
    let client = reqwest::Client::new();
    let url = format!("http://localhost:{}{}", local_port, req.path);

    debug!("[HUB-HOST] Tunnel request from slave: {} {}", req.method, req.path);

    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);

    let mut http_req = client.request(method, &url);

    // Inject auth cookie (host uses its own auth_secret)
    let session_token = crate::hub::compute_session_token(auth_secret);
    http_req = http_req.header("cookie", format!("session_token={}", session_token));

    // Add headers from tunnel request (skip cookie/host)
    for (k, v) in &req.headers {
        let k_lower = k.to_lowercase();
        if k_lower != "cookie" && k_lower != "host" {
            http_req = http_req.header(k.as_str(), v.as_str());
        }
    }

    // Add body if present (base64 decoded)
    if let Some(body_b64) = &req.body {
        if let Ok(body_bytes) = base64::engine::general_purpose::STANDARD.decode(body_b64) {
            http_req = http_req.body(body_bytes);
        }
    }

    match http_req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let mut headers = HashMap::new();
            for (k, v) in resp.headers() {
                if let Ok(v_str) = v.to_str() {
                    headers.insert(k.to_string(), v_str.to_string());
                }
            }
            let body_bytes = resp.bytes().await.unwrap_or_default();
            let body = if body_bytes.is_empty() {
                None
            } else {
                Some(base64::engine::general_purpose::STANDARD.encode(&body_bytes))
            };

            TunnelResponse {
                request_id: req.request_id,
                status,
                headers,
                body,
            }
        }
        Err(e) => {
            error!("[HUB-HOST] Local request failed: {}", e);
            TunnelResponse {
                request_id: req.request_id,
                status: 502,
                headers: HashMap::new(),
                body: Some(base64::engine::general_purpose::STANDARD.encode(
                    format!("Host tunnel error: {}", e).as_bytes(),
                )),
            }
        }
    }
}

