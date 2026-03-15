use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn, error, debug};
use base64::Engine as _;

use mad_hatter::tunnel::TunnelEndpoint;
use crate::hub::tunnel::*;
use crate::hub::extension_impl::SlaveLocalHandler;
use crate::persist::instance::InstanceStore;
use crate::service::extension::ExtensionHandlerDispatcher;
use crate::service::http_proxy::{HttpProxy, HttpProxyRequest, HttpProxyResponse, HttpProxyDispatcher};
use crate::bindings::http::{
    HUB_WS_PATH,
};

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Message,
>;


/// Slave state: maintains WebSocket connection to host (bidirectional tunnel)
/// Supports automatic reconnection on disconnect with exponential backoff
pub struct SlaveState {
    pub host_url: String,
    /// WebSocket sender for sending messages to host
    ws_sender: Arc<Mutex<Option<WsSink>>>,
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
    /// TunnelEndpoint for RPC calls (slave→host direction)
    tunnel_endpoint: Arc<std::sync::Mutex<Option<Arc<TunnelEndpoint>>>>,
    /// Instance store for creating SlaveLocalHandler dispatchers
    instance_store: InstanceStore,
}

impl SlaveState {
    pub fn new(host_url: String, local_port: u16, auth_token: String, join_token: String, engine_id: String, instance_store: InstanceStore) -> Self {
        Self {
            host_url,
            ws_sender: Arc::new(Mutex::new(None)),
            local_port,
            auth_token,
            should_reconnect: Arc::new(AtomicBool::new(true)),
            join_token,
            engine_id,
            reconnect_instances: Arc::new(Mutex::new(Vec::new())),
            engine_endpoint: Mutex::new(None),
            tunnel_endpoint: Arc::new(std::sync::Mutex::new(None)),
            instance_store,
        }
    }

    pub fn stop_reconnect(&self) {
        self.should_reconnect.store(false, Ordering::SeqCst);
    }

    /// Get the tunnel endpoint for RPC proxy calls
    pub fn get_tunnel_endpoint(&self) -> Option<Arc<TunnelEndpoint>> {
        self.tunnel_endpoint.lock().unwrap().clone()
    }

    /// Build dispatchers for slave-side TunnelEndpoint (handles host→slave RPC)
    fn build_slave_dispatchers(&self) -> Vec<Box<dyn mad_hatter::tunnel::Dispatch>> {
        let ext_handler = Arc::new(SlaveLocalHandler::new(self.instance_store.clone()));
        let http_handler = Arc::new(SlaveHttpProxyHandler::new(self.local_port, self.auth_token.clone()));
        vec![
            ExtensionHandlerDispatcher::boxed(ext_handler),
            HttpProxyDispatcher::boxed(http_handler),
        ]
    }

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
        let msg = register.encode();
        {
            let mut s = self.ws_sender.lock().await;
            if let Some(ws) = s.as_mut() {
                ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into()))
                    .await
                    .map_err(|e| format!("Failed to send register: {}", e))?;
            }
        }

        info!("[HUB-SLAVE] Registered with host");

        Ok(ws_receiver)
    }

    /// Create channel pair and TunnelEndpoint for RPC communication
    fn create_tunnel_endpoint(&self) -> (mpsc::UnboundedSender<String>, mpsc::UnboundedReceiver<String>, Arc<TunnelEndpoint>) {
        let (ws_to_tunnel_tx, ws_to_tunnel_rx) = mpsc::unbounded_channel::<String>();
        let (tunnel_to_ws_tx, tunnel_to_ws_rx) = mpsc::unbounded_channel::<String>();
        let dispatchers = self.build_slave_dispatchers();
        let endpoint = TunnelEndpoint::new(
            ws_to_tunnel_rx,
            tunnel_to_ws_tx,
            dispatchers,
            Duration::from_secs(30),
        );
        (ws_to_tunnel_tx, tunnel_to_ws_rx, endpoint)
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

        // Create channel pair + TunnelEndpoint for initial connection
        let (ws_to_tunnel_tx, tunnel_to_ws_rx, endpoint) = self.create_tunnel_endpoint();
        {
            let mut ep = self.tunnel_endpoint.lock().unwrap();
            *ep = Some(endpoint);
        }

        // Spawn message processing + auto-reconnect loop

        let ws_sender_shared = self.ws_sender.clone();
        let should_reconnect = self.should_reconnect.clone();
        let host_url = self.host_url.clone();
        let join_token = self.join_token.clone();
        let engine_id = self.engine_id.clone();
        let reconnect_instances = self.reconnect_instances.clone();
        let self_local_port = self.local_port;
        let self_auth_token = self.auth_token.clone();
        let tunnel_endpoint_shared = self.tunnel_endpoint.clone();
        let instance_store_clone = self.instance_store.clone();

        tokio::spawn(async move {
            let mut current_receiver = ws_receiver;
            let mut current_ws_to_tunnel_tx = ws_to_tunnel_tx;
            let mut current_tunnel_to_ws_rx = tunnel_to_ws_rx;

            loop {
                // Process messages until disconnect
                Self::run_message_loop(
                    &mut current_receiver,
                    &ws_sender_shared,
                    &current_ws_to_tunnel_tx,
                    &mut current_tunnel_to_ws_rx,
                ).await;

                // Disconnected — clear ws_sender and tunnel_endpoint immediately
                {
                    let mut s = ws_sender_shared.lock().await;
                    *s = None;
                }
                {
                    let mut ep = tunnel_endpoint_shared.lock().unwrap();
                    *ep = None;
                }
                // Drop the old channel sender to signal TunnelEndpoint reader to stop
                drop(current_ws_to_tunnel_tx);
                info!("[HUB-SLAVE] Disconnected from host, ws_sender and tunnel_endpoint cleared");

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
                            let msg = register.encode();
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

                            // Create new channel pair + TunnelEndpoint for reconnection
                            let (ws_to_tunnel_tx, tunnel_to_ws_rx) = mpsc::unbounded_channel::<String>();
                            let (tunnel_to_ws_tx_new, tunnel_to_ws_rx_new) = mpsc::unbounded_channel::<String>();
                            let slave_handler = Arc::new(SlaveLocalHandler::new(instance_store_clone.clone()));
                            let http_handler = Arc::new(SlaveHttpProxyHandler::new(self_local_port, self_auth_token.clone()));
                            let dispatchers: Vec<Box<dyn mad_hatter::tunnel::Dispatch>> = vec![ExtensionHandlerDispatcher::boxed(slave_handler), HttpProxyDispatcher::boxed(http_handler)];
                            let endpoint = TunnelEndpoint::new(
                                tunnel_to_ws_rx,
                                tunnel_to_ws_tx_new,
                                dispatchers,
                                Duration::from_secs(30),
                            );
                            {
                                let mut ep = tunnel_endpoint_shared.lock().unwrap();
                                *ep = Some(endpoint);
                            }

                            current_receiver = ws_receiver;
                            current_ws_to_tunnel_tx = ws_to_tunnel_tx;
                            current_tunnel_to_ws_rx = tunnel_to_ws_rx_new;
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

    /// Process WebSocket messages until disconnect.
    /// Uses tokio::select! to handle both WebSocket messages and tunnel RPC responses.
    async fn run_message_loop(
        ws_receiver: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
        >,
        ws_sender_shared: &Arc<Mutex<Option<WsSink>>>,
        ws_to_tunnel_tx: &mpsc::UnboundedSender<String>,
        tunnel_to_ws_rx: &mut mpsc::UnboundedReceiver<String>,
    ) {
        loop {
            tokio::select! {
                msg = ws_receiver.next() => {
                    match msg {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            match TunnelMessage::decode(&text) {
                                Ok(TunnelMessage::Heartbeat) => {
                                    let hb = TunnelMessage::Heartbeat.encode();
                                    let mut s = ws_sender_shared.lock().await;
                                    if let Some(ws) = s.as_mut() {
                                        let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(hb.into())).await;
                                    }
                                }
                                Ok(TunnelMessage::Rpc { payload }) => {
                                    // Forward RPC message to TunnelEndpoint via channel
                                    if ws_to_tunnel_tx.send(payload).is_err() {
                                        warn!("[HUB-SLAVE] Tunnel channel closed, cannot forward RPC");
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                            info!("[HUB-SLAVE] Host closed connection");
                            break;
                        }
                        Some(Err(e)) => {
                            warn!("[HUB-SLAVE] WebSocket error: {}", e);
                            break;
                        }
                        None => {
                            info!("[HUB-SLAVE] WebSocket stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
                Some(rpc_payload) = tunnel_to_ws_rx.recv() => {
                    // TunnelEndpoint wants to send an RPC message → forward to WebSocket
                    let rpc_msg = TunnelMessage::Rpc { payload: rpc_payload };
                    let text = rpc_msg.encode();
                    let mut s = ws_sender_shared.lock().await;
                    if let Some(ws) = s.as_mut() {
                        if ws.send(tokio_tungstenite::tungstenite::Message::Text(text.into())).await.is_err() {
                            warn!("[HUB-SLAVE] Failed to send RPC to WebSocket");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Gracefully disconnect from host by sending Leave message
    pub async fn disconnect(&self) {
        let mut s = self.ws_sender.lock().await;
        if let Some(ws) = s.as_mut() {
            let msg = TunnelMessage::Leave.encode();
            let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await;
            let _ = ws.close().await;
            info!("[HUB-SLAVE] Sent Leave message and closed WebSocket");
        }
        *s = None;
        // Clear tunnel endpoint
        let mut ep = self.tunnel_endpoint.lock().unwrap();
        *ep = None;
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
        let msg = register.encode();
        let mut s = self.ws_sender.lock().await;
        if let Some(ws) = s.as_mut() {
            match ws.send(tokio_tungstenite::tungstenite::Message::Text(msg.into())).await {
                Ok(_) => info!("[HUB-SLAVE] Sent instances update to host"),
                Err(e) => warn!("[HUB-SLAVE] Failed to send instances update: {}", e),
            }
        }
    }
}

/// Slave-side HTTP proxy handler — forwards proxied HTTP requests to local instances
struct SlaveHttpProxyHandler {
    local_port: u16,
    auth_token: String,
}

impl SlaveHttpProxyHandler {
    fn new(local_port: u16, auth_token: String) -> Self {
        Self { local_port, auth_token }
    }
}

#[async_trait::async_trait]
impl HttpProxy for SlaveHttpProxyHandler {
    async fn proxy_http(&self, request: HttpProxyRequest) -> Result<HttpProxyResponse, String> {
        let client = reqwest::Client::new();
        let url = format!("http://localhost:{}{}", self.local_port, request.path);

        debug!("[HUB-SLAVE] Proxy request: {} {}", request.method, request.path);

        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .unwrap_or(reqwest::Method::GET);

        let mut http_req = client.request(method, &url);

        let session_token = crate::hub::compute_session_token(&self.auth_token);
        http_req = http_req.header("cookie", format!("session_token={}", session_token));

        for (k, v) in &request.headers {
            let k_lower = k.to_lowercase();
            if k_lower != "cookie" && k_lower != "host" {
                http_req = http_req.header(k.as_str(), v.as_str());
            }
        }

        if let Some(body_b64) = &request.body {
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

                Ok(HttpProxyResponse {
                    status,
                    headers,
                    body,
                })
            }
            Err(e) => {
                error!("[HUB-SLAVE] Local request failed: {}", e);
                Ok(HttpProxyResponse {
                    status: 502,
                    headers: HashMap::new(),
                    body: Some(base64::engine::general_purpose::STANDARD.encode(
                        format!("Tunnel error: {}", e).as_bytes(),
                    )),
                })
            }
        }
    }
}