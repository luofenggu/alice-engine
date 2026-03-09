use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn, error, debug};
use base64::Engine as _;

use crate::hub::tunnel::*;

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Message,
>;

type PendingRequests = Arc<Mutex<HashMap<String, oneshot::Sender<TunnelResponse>>>>;

/// Slave state: maintains WebSocket connection to host (bidirectional tunnel)
pub struct SlaveState {
    pub host_url: String,
    /// WebSocket sender for sending messages to host
    ws_sender: Arc<Mutex<Option<WsSink>>>,
    /// Pending requests awaiting response from host (slave→host direction)
    pending: PendingRequests,
    local_port: u16,
    auth_token: String,
}

impl SlaveState {
    pub fn new(host_url: String, local_port: u16, auth_token: String) -> Self {
        Self {
            host_url,
            ws_sender: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            local_port,
            auth_token,
        }
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

    /// Connect to host and start processing tunnel messages (bidirectional)
    pub async fn connect(&self, instances: Vec<TunnelInstanceInfo>, engine_id: &str, join_token: &str) -> Result<(), String> {
        // Build WebSocket URL with join_token as query param
        let ws_url = self.host_url
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        let ws_url = format!("{}/api/hub/ws?token={}", ws_url, join_token);

        info!("[HUB-SLAVE] Connecting to host: {}", self.host_url);

        // Connect using tokio-tungstenite for client-side WebSocket
        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {}", e))?;

        let (ws_sender_raw, mut ws_receiver) = ws_stream.split();

        // Store sender in self.ws_sender for proxy_request_to_host
        {
            let mut s = self.ws_sender.lock().await;
            *s = Some(ws_sender_raw);
        }

        // Send register message
        let register = TunnelMessage::Register {
            engine_id: engine_id.to_string(),
            engine_endpoint: format!("http://localhost:{}", self.local_port),
            instances,
        };
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

        // Register hooks locally — URLs point to local tunnel_proxy routes
        self.register_tunnel_hooks().await;

        let local_port = self.local_port;
        let auth_token = self.auth_token.clone();
        let pending = self.pending.clone();
        let ws_sender_shared = self.ws_sender.clone();

        // Process incoming messages (bidirectional: requests from host + responses to our requests)
        tokio::spawn(async move {
            while let Some(msg) = ws_receiver.next().await {
                match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        match serde_json::from_str::<TunnelMessage>(&text) {
                            Ok(TunnelMessage::Request(req)) => {
                                // Host is requesting something from us — handle locally
                                let sender = ws_sender_shared.clone();
                                let token = auth_token.clone();
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
                                // Response to a request we sent to host
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
            info!("[HUB-SLAVE] Disconnected from host");
        });

        Ok(())
    }

    /// Register hooks on local engine pointing to tunnel_proxy routes
    async fn register_tunnel_hooks(&self) {
        let client = reqwest::Client::new();
        let url = format!("http://localhost:{}/api/hooks", self.local_port);
        let session_token = crate::hub::compute_session_token(&self.auth_token);

        // Hook URLs point to local tunnel_proxy — requests will be forwarded through WS tunnel to host
        let hooks_body = serde_json::json!({
            "contacts_url": format!("http://localhost:{}/api/hub/tunnel_proxy/contacts/{{instance_id}}", self.local_port),
            "send_msg_relay_url": format!("http://localhost:{}/api/hub/tunnel_proxy/relay", self.local_port),
        });

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

/// Handle a tunnel request by making a local HTTP request
async fn handle_tunnel_request(req: TunnelRequest, local_port: u16, auth_token: &str) -> TunnelResponse {
    let client = reqwest::Client::new();
    let url = format!("http://localhost:{}{}", local_port, req.path);

    debug!("[HUB-SLAVE] Tunnel request: {} {}", req.method, req.path);

    // Build request with auth cookie
    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);

    let mut http_req = client.request(method, &url);

    // Inject auth cookie (slave computes session token from its own auth_token)
    let session_token = crate::hub::compute_session_token(auth_token);
    http_req = http_req.header("cookie", format!("session_token={}", session_token));

    // Add headers from tunnel request
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

    // Execute request
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

