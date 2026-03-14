use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc;

use mad_hatter::tunnel::Transport;

/// WebSocket tunnel protocol messages
/// All messages are JSON-encoded text frames

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TunnelMessage {
    /// Slave → Host: register this engine's instances
    #[serde(rename = "register")]
    Register {
        engine_id: String,
        engine_endpoint: String,
        instances: Vec<TunnelInstanceInfo>,
    },

    /// Host → Slave: HTTP request to be processed locally
    #[serde(rename = "request")]
    Request(TunnelRequest),

    /// Slave → Host: HTTP response from local processing
    #[serde(rename = "response")]
    Response(TunnelResponse),

    /// Bidirectional: keepalive
    #[serde(rename = "heartbeat")]
    Heartbeat,

    /// Slave → Host: graceful disconnect
    #[serde(rename = "leave")]
    Leave,

    /// Host → Slave: register hooks on the slave engine
    #[serde(rename = "hook_register")]
    HookRegister {
        hooks: HashMap<String, String>,
    },

    /// Bidirectional: tunnel_service RPC message
    #[serde(rename = "rpc")]
    Rpc {
        payload: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelInstanceInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub avatar: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub last_active: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelRequest {
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    /// Base64-encoded body
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelResponse {
    pub request_id: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    /// Base64-encoded body
    pub body: Option<String>,
}

/// Channel-based Transport bridge for async WebSocket ↔ sync TunnelEndpoint.
///
/// Architecture:
///   async WebSocket loop ←→ channels ←→ sync TunnelEndpoint (reader thread)
///
/// - ws_to_tunnel: std::sync::mpsc (blocking recv for TunnelEndpoint reader thread)
/// - tunnel_to_ws: tokio::sync::mpsc (async recv for WebSocket select loop)
///
/// Usage:
///   let (reader, writer, ws_sender, ws_receiver) = create_ws_bridge();
///   - Pass reader + writer to TunnelEndpoint::new()
///   - WebSocket loop: send RPC payloads via ws_sender, select on ws_receiver to send back
///   - Drop ws_sender when WebSocket closes → reader.recv() returns Ok(None) → TunnelEndpoint exits
pub struct WsBridgeReader {
    rx: mpsc::Receiver<String>,
}

pub struct WsBridgeWriter {
    tx: tokio::sync::mpsc::Sender<String>,
}

impl Transport for WsBridgeReader {
    fn send(&mut self, _text: &str) -> Result<(), String> {
        Err("WsBridgeReader does not support send".to_string())
    }

    fn recv(&mut self) -> Result<Option<String>, String> {
        match self.rx.recv() {
            Ok(text) => Ok(Some(text)),
            Err(_) => Ok(None), // channel closed = connection closed
        }
    }
}

impl Transport for WsBridgeWriter {
    fn send(&mut self, text: &str) -> Result<(), String> {
        self.tx
            .blocking_send(text.to_string())
            .map_err(|_| "WsBridge channel closed".to_string())
    }

    fn recv(&mut self) -> Result<Option<String>, String> {
        Err("WsBridgeWriter does not support recv".to_string())
    }
}

/// Create a channel-bridged Transport pair for connecting async WebSocket to sync TunnelEndpoint.
///
/// Returns:
///   - reader: Transport impl for TunnelEndpoint (blocking recv from ws_sender)
///   - writer: Transport impl for TunnelEndpoint (blocking_send to ws_receiver)
///   - ws_sender: async side feeds RPC payloads here → reader.recv() returns them
///   - ws_receiver: async side reads from here → sends as WebSocket Rpc messages
pub fn create_ws_bridge() -> (
    WsBridgeReader,
    WsBridgeWriter,
    mpsc::Sender<String>,
    tokio::sync::mpsc::Receiver<String>,
) {
    let (ws_to_tunnel_tx, ws_to_tunnel_rx) = mpsc::channel::<String>();
    let (tunnel_to_ws_tx, tunnel_to_ws_rx) = tokio::sync::mpsc::channel::<String>(64);

    let reader = WsBridgeReader { rx: ws_to_tunnel_rx };
    let writer = WsBridgeWriter { tx: tunnel_to_ws_tx };

    (reader, writer, ws_to_tunnel_tx, tunnel_to_ws_rx)
}