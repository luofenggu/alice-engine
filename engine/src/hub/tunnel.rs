use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

    /// Host → Slave: register hooks on the slave engine
    #[serde(rename = "hook_register")]
    HookRegister {
        hooks: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelInstanceInfo {
    pub id: String,
    pub name: String,
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

