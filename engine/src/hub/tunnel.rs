use serde::{Deserialize, Serialize};

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

    /// Bidirectional: keepalive
    #[serde(rename = "heartbeat")]
    Heartbeat,

    /// Slave → Host: graceful disconnect
    #[serde(rename = "leave")]
    Leave,

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




impl TunnelMessage {
    /// Serialize to JSON string for WebSocket text frame
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap()
    }

    /// Deserialize from JSON string
    pub fn decode(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}
