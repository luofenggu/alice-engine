use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, oneshot};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn, error, debug};
use base64::Engine as _;

use crate::hub::tunnel::*;
use crate::bindings::http::{
    HUB_WS_PATH, HUB_HOOKS_PATH, HUB_CONTACTS_PATH_PREFIX, HUB_RELAY_PATH,
    HUB_TUNNEL_PROXY_CONTACTS_PATH_PREFIX, HUB_TUNNEL_PROXY_RELAY_PATH,
};

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Message,
>;

type PendingRequests = Arc<Mutex<HashMap<String, oneshot::Sender<TunnelResponse>>>>;

/// Slave state: maintains WebSocket connection to host (bidirectional tunnel)
/// Supports automatic reconnection on disconnect with exponential backoff
pub struct SlaveState {
    pub host_url: String,
    /// WebSocket sender for sending messages to host
    ws_sender: Arc<Mutex<Option<WsSink>>>,
    /// Pending requests awaiting response from host (slave→host direction)
    pending: PendingRequests,
    local_port: u16,
    auth_token: String,
    /// Whether the background task should attempt reconnection after disconnect
    should_reconnect: Arc<AtomicBool>,
    /// Saved parameters for reconnection
    join_token: String,
    engine_id: String,
    reconnect_instances: Arc<Mutex<Vec<TunnelInstanceInfo>>>,
    /// Stored after connect for re-sending Register/Leave messages
    engine_endpoint: Mutex<Option<String>>,
}

impl SlaveState {
    pub fn new(host_url: String, local_port: u16, auth_token: String, join_token: String, engine_id: String) -> Self {
        Self {
            host_url,
            ws_sender: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            local_port,
            auth_token,
            should_reconnect: Arc::new(AtomicBool::new(true)),
            join_token,
            engine_id,
            reconnect_instances: Arc::new(Mutex::new(Vec::new())),
            engine_endpoint: Mutex::new(None),
        }
    }

    /// Stop reconnection attempts (called by leave_host before dropping)
    pub fn stop_reconnect(&self) {
        self.should_reconnect.store(false, Ordering::SeqCst);
    }

    /// Send an HTTP request through the tunnel to the host
    pub async fn proxy_request_to_host(&self, req: TunnelRequest) -> Option<TunnelResponse> {
        let sender = {
            let s = self.ws_sender.lock().await;
            match s.as_ref() {
                Some(_) => self.ws_sender.clone(),
                None => {
                    warn!("[HUB-SLAVE] No WebSocket connection to host");
                    return None;
                }
            }
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
            let mut s = sender.lock().await;
            if let Some(ws) = s.as_mut() {
                if ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await.is_err() {
                    error!("[HUB-SLAVE] Failed to send tunnel request to host");
                    self.pending.lock().await.remove(&request_id);
                    return None;
                }
            } else {
                self.pending.lock().await.remove(&request_id);
                return None;
            }
        }

        // Wait for response with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(resp)) => Some(resp),
            Ok(Err(_)) => {
                warn!("[HUB-SLAVE] Response channel dropped for {}", request_id);
                None
            }
            Err(_) => {
                warn!("[HUB-SLAVE] Tunnel request to host timeout for {}", request_id);
                self.pending.lock().await.remove(&request_id);
                None
            }
        }
    }

    /// Establish a WebSocket connection to host, register, and return the receiver stream
    async fn establish_connection(&self) -> Result<
        futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
        >,
        String,
    > {
        let ws_url = self.host_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        let ws_url = format!("{}{}?token={}", ws_url, HUB_WS_PATH, self.join_token);

        info!("[HUB-SLAVE] Connecting to host: {}", self.host_url);

        // Connect with 5s timeout
        let (ws_stream, _) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio_tungstenite::connect_async(&ws_url),
        )
            .await
            .map_err(|_| format!("Connection timeout (5s) to {}", self.host_url))?
            .map_err(|e| format!("WebSocket connect failed: {}", e))?;

        let (ws_sender_raw, ws_receiver) = ws_stream.split();

        // Store sender
        {
            let mut s = self.ws_sender.lock().await;
            *s = Some(ws_sender_raw);
        }

        // Send register message
        let instances = self.reconnect_instances.lock().await.clone();
        let endpoint = std::env::var("ALICE_HOST").ok()
            .filter(|s| !s.is_empty())
            .map(|h| if h.starts_with("http") { h } else { format!("http://{}", h) })
            .unwrap_or_else(|| format!("http://localhost:{}", self.local_port));
        let register = TunnelMessage::Register {
            engine_id: self.engine_id.clone(),
            engine_endpoint: endpoint.clone(),
            instances,
        };

        // Store engine_endpoint for later use (disconnect, instances update)
        *self.engine_endpoint.lock().await = Some(endpoint);
        let msg = serde_json::to_string(&register).unwrap();
        {
            let mut s = self.ws_sender.lock().await;
            if let Some(ws) = s.as_mut() {
                ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into()))
                    .await
                    .map_err(|e| format!("Failed to send register: {}", e))?;
            }
        }

        info!("[HUB-SLAVE] Registered with host");

        // Register hooks locally
        self.register_tunnel_hooks().await;

        Ok(ws_receiver)
    }

    /// Connect to host and start processing tunnel messages (bidirectional)
    /// Spawns a background task that auto-reconnects on disconnect
    pub async fn connect(&self, instances: Vec<TunnelInstanceInfo>, _engine_id: &str, _join_token: &str) -> Result<(), String> {
        // Save instances for reconnection (join_token and engine_id already in struct)
        {
            let mut inst = self.reconnect_instances.lock().await;
            *inst = instances;
        }

        // Initial connection
        let ws_receiver = self.establish_connection().await?;

        // Spawn message processing + auto-reconnect loop
        let local_port = self.local_port;
        let auth_token = self.auth_token.clone();
        let pending = self.pending.clone();
        let ws_sender_shared = self.ws_sender.clone();
        let should_reconnect = self.should_reconnect.clone();
        let host_url = self.host_url.clone();
        let join_token = self.join_token.clone();
        let engine_id = self.engine_id.clone();
        let reconnect_instances = self.reconnect_instances.clone();
        let self_local_port = self.local_port;
        let self_auth_token = self.auth_token.clone();

        tokio::spawn(async move {
            let mut current_receiver = ws_receiver;

            loop {
                // Process messages until disconnect
                Self::run_message_loop(
                    &mut current_receiver,
                    &ws_sender_shared,
                    &pending,
                    local_port,
                    &auth_token,
                ).await;

                // Disconnected — clear ws_sender immediately
                {
                    let mut s = ws_sender_shared.lock().await;
                    *s = None;
                }
                info!("[HUB-SLAVE] Disconnected from host, ws_sender cleared");

                // Check if we should reconnect
                if !should_reconnect.load(Ordering::SeqCst) {
                    info!("[HUB-SLAVE] Reconnection disabled, exiting");
                    break;
                }

                // Exponential backoff reconnection
                let mut delay_secs = 2u64;
                loop {
                    if !should_reconnect.load(Ordering::SeqCst) {
                        info!("[HUB-SLAVE] Reconnection disabled during backoff, exiting");
                        return;
                    }

                    info!("[HUB-SLAVE] Reconnecting in {}s...", delay_secs);
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;

                    if !should_reconnect.load(Ordering::SeqCst) {
                        info!("[HUB-SLAVE] Reconnection disabled after sleep, exiting");
                        return;
                    }

                    // Try to reconnect
                    let ws_url = host_url
                        .replace("http://", "ws://")
                        .replace("https://", "wss://");
                    let ws_url = format!("{}{}?token={}", ws_url, HUB_WS_PATH, join_token);

                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        tokio_tungstenite::connect_async(&ws_url),
                    ).await {
                        Ok(Ok((ws_stream, _))) => {
                            let (ws_sender_raw, ws_receiver) = ws_stream.split();

                            // Restore ws_sender
                            {
                                let mut s = ws_sender_shared.lock().await;
                                *s = Some(ws_sender_raw);
                            }

                            // Re-register
                            let instances: Vec<TunnelInstanceInfo> = reconnect_instances.lock().await.clone();
                            let register = TunnelMessage::Register {
                                engine_id: engine_id.clone(),
                                engine_endpoint: std::env::var("ALICE_HOST").ok()
                                    .filter(|s| !s.is_empty())
                                    .map(|h| if h.starts_with("http") { h } else { format!("http://{}", h) })
                                    .unwrap_or_else(|| format!("http://localhost:{}", self_local_port)),
                                instances,
                            };
                            let msg = serde_json::to_string(&register).unwrap();
                            {
                                let mut s = ws_sender_shared.lock().await;
                                if let Some(ws) = s.as_mut() {
                                    if ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await.is_err() {
                                        error!("[HUB-SLAVE] Failed to send register on reconnect");
                                        let mut s2 = ws_sender_shared.lock().await;
                                        *s2 = None;
                                        delay_secs = (delay_secs * 2).min(60);
                                        continue;
                                    }
                                }
                            }

                            info!("[HUB-SLAVE] Reconnected to host successfully");

                            // Re-register hooks
                            let session_token = crate::hub::compute_session_token(&self_auth_token);
                            let client = reqwest::Client::new();
                            let hooks_body = build_hooks_json(self_local_port);
                            let _ = client.post(format!("http://localhost:{}{}", self_local_port, HUB_HOOKS_PATH))
                                .header("cookie", format!("session_token={}", session_token))
                                .json(&hooks_body)
                                .send()
                                .await;

                            current_receiver = ws_receiver;
                            break;
                        }
                        Ok(Err(e)) => {
                            warn!("[HUB-SLAVE] Reconnect failed: {}", e);
                            delay_secs = (delay_secs * 2).min(60);
                        }
                        Err(_) => {
                            warn!("[HUB-SLAVE] Reconnect timeout (5s)");
                            delay_secs = (delay_secs * 2).min(60);
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Process WebSocket messages until disconnect
    async fn run_message_loop(
        ws_receiver: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
        >,
        ws_sender_shared: &Arc<Mutex<Option<WsSink>>>,
        pending: &PendingRequests,
        local_port: u16,
        auth_token: &str,
    ) {
        while let Some(msg) = ws_receiver.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                    match serde_json::from_str::<TunnelMessage>(&text) {
                        Ok(TunnelMessage::Request(req)) => {
                            let sender = ws_sender_shared.clone();
                            let token = auth_token.to_string();
                            tokio::spawn(async move {
                                let resp = handle_tunnel_request(req, local_port, &token).await;
                                let msg = serde_json::to_string(&TunnelMessage::Response(resp)).unwrap();
                                let mut s = sender.lock().await;
                                if let Some(ws) = s.as_mut() {
                                    let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await;
                                }
                            });
                        }
                        Ok(TunnelMessage::Response(resp)) => {
                            let mut pending_map = pending.lock().await;
                            if let Some(sender) = pending_map.remove(&resp.request_id) {
                                let _ = sender.send(resp);
                            }
                        }
                        Ok(TunnelMessage::Heartbeat) => {
                            let hb = serde_json::to_string(&TunnelMessage::Heartbeat).unwrap();
                            let mut s = ws_sender_shared.lock().await;
                            if let Some(ws) = s.as_mut() {
                                let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(hb.into())).await;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                    info!("[HUB-SLAVE] Host closed connection");
                    break;
                }
                Err(e) => {
                    warn!("[HUB-SLAVE] WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }
    }

    /// Gracefully disconnect from host by sending Leave message
    pub async fn disconnect(&self) {
        let mut s = self.ws_sender.lock().await;
        if let Some(ws) = s.as_mut() {
            let msg = serde_json::to_string(&TunnelMessage::Leave).unwrap();
            let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await;
            let _ = ws.close().await;
            info!("[HUB-SLAVE] Sent Leave message and closed WebSocket");
        }
        *s = None;
    }

    /// Send updated instances list to host (for when instances are created/deleted)
    pub async fn send_instances_update(&self, instances: Vec<TunnelInstanceInfo>) {
        let engine_id = self.engine_id.clone();
        let engine_endpoint = self.engine_endpoint.lock().await.clone().unwrap_or_default();
        if engine_id.is_empty() {
            warn!("[HUB-SLAVE] Cannot send instances update: not connected");
            return;
        }
        let register = TunnelMessage::Register {
            engine_id,
            engine_endpoint,
            instances,
        };
        let msg = serde_json::to_string(&register).unwrap();
        let mut s = self.ws_sender.lock().await;
        if let Some(ws) = s.as_mut() {
            match ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await {
                Ok(_) => info!("[HUB-SLAVE] Sent instances update to host"),
                Err(e) => warn!("[HUB-SLAVE] Failed to send instances update: {}", e),
            }
        }
    }

    /// Register hooks on local engine pointing to tunnel_proxy routes
    async fn register_tunnel_hooks(&self) {
        let client = reqwest::Client::new();
        let url = format!("http://localhost:{}{}", self.local_port, HUB_HOOKS_PATH);
        let session_token = crate::hub::compute_session_token(&self.auth_token);
        let hooks_body = build_hooks_json(self.local_port);

        match client.post(&url)
            .header("cookie", format!("session_token={}", session_token))
            .json(&hooks_body)
            .send()
            .await
        {
            Ok(resp) => {
                info!("[HUB-SLAVE] Tunnel hooks registered locally: status {}", resp.status());
            }
            Err(e) => {
                error!("[HUB-SLAVE] Failed to register tunnel hooks: {}", e);
            }
        }
    }
}

/// Build the hooks registration JSON body using centralized path constants.
fn build_hooks_json(local_port: u16) -> serde_json::Value {
    serde_json::json!({
        "contacts_url": format!("http://localhost:{}{}{{instance_id}}", local_port, HUB_TUNNEL_PROXY_CONTACTS_PATH_PREFIX),
        "send_msg_relay_url": format!("http://localhost:{}{}", local_port, HUB_TUNNEL_PROXY_RELAY_PATH),
    })
}

/// Handle a tunnel request by making a local HTTP request
async fn handle_tunnel_request(req: TunnelRequest, local_port: u16, auth_token: &str) -> TunnelResponse {
    let client = reqwest::Client::new();
    let url = format!("http://localhost:{}{}", local_port, req.path);

    debug!("[HUB-SLAVE] Tunnel request: {} {}", req.method, req.path);

    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);

    let mut http_req = client.request(method, &url);

    let session_token = crate::hub::compute_session_token(auth_token);
    http_req = http_req.header("cookie", format!("session_token={}", session_token));

    for (k, v) in &req.headers {
        let k_lower = k.to_lowercase();
        if k_lower != "cookie" && k_lower != "host" {
            http_req = http_req.header(k.as_str(), v.as_str());
        }
    }

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
            error!("[HUB-SLAVE] Local request failed: {}", e);
            TunnelResponse {
                request_id: req.request_id,
                status: 502,
                headers: HashMap::new(),
                body: Some(base64::engine::general_purpose::STANDARD.encode(
                    format!("Tunnel error: {}", e).as_bytes(),
                )),
            }
        }
    }
}

