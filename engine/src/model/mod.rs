//! # Concept Model — the .proto of Alice Engine
//!
//! All domain concepts live here as declarative definitions.
//! This is the single source of truth for the engine's vocabulary.
//!
//! Rules:
//! - Literals are legal here (this is the contract definition layer)
//! - Business code imports types from here, never defines its own
//! - Changes here = schema migration (treat with care)

use alice_persist::{Persist, Column, Value};
use anyhow::Result;

// ---------------------------------------------------------------------------
// Message — the core chat record
// ---------------------------------------------------------------------------

/// A message in the chat history.
///
/// This is the single source of truth for message structure.
/// All message-related queries and mutations operate on this type.
#[derive(Debug, Clone)]
pub struct Message {
    pub id: i64,
    pub sender: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub read_status: String,
    pub msg_type: String,
}

impl Message {
    /// Role value: user message
    pub const ROLE_USER: &'static str = "user";
    /// Role value: agent/assistant message
    pub const ROLE_AGENT: &'static str = "agent";
    /// Read status: already consumed
    pub const STATUS_READ: &'static str = "read";
    /// Read status: waiting to be consumed
    pub const STATUS_UNREAD: &'static str = "unread";
    /// Message type: normal chat
    pub const TYPE_CHAT: &'static str = "chat";
}

impl Persist for Message {
    fn collection_name() -> &'static str { "messages" }

    fn id(&self) -> i64 { self.id }

    fn schema() -> Vec<Column> {
        vec![
            Column::id("id"),
            Column::text("sender"),
            Column::text("role"),
            Column::text("content"),
            Column::text("timestamp"),
            Column::text("read_status"),
            Column::text("msg_type"),
        ]
    }

    fn to_row(&self) -> Vec<Value> {
        vec![
            Value::from(self.sender.clone()),
            Value::from(self.role.clone()),
            Value::from(self.content.clone()),
            Value::from(self.timestamp.clone()),
            Value::from(self.read_status.clone()),
            Value::from(self.msg_type.clone()),
        ]
    }

    fn from_row(values: &[Value]) -> Result<Self> {
        Ok(Message {
            id: values[0].as_i64().unwrap_or(0),
            sender: values[1].as_str().unwrap_or("").to_string(),
            role: values[2].as_str().unwrap_or("").to_string(),
            content: values[3].as_str().unwrap_or("").to_string(),
            timestamp: values[4].as_str().unwrap_or("").to_string(),
            read_status: values[5].as_str().unwrap_or("").to_string(),
            msg_type: values[6].as_str().unwrap_or("").to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Query views — projections of Message for specific use cases
// ---------------------------------------------------------------------------

/// A message for display (chat history view).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

impl From<&Message> for ChatMessage {
    fn from(m: &Message) -> Self {
        ChatMessage {
            id: m.id,
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
        }
    }
}

/// A message within a time range (for summary).
#[derive(Debug, Clone)]
pub struct RangeMessage {
    pub sender: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

impl From<&Message> for RangeMessage {
    fn from(m: &Message) -> Self {
        RangeMessage {
            sender: m.sender.clone(),
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
        }
    }
}

/// An unread inbox message from a user.
#[derive(Debug, Clone)]
pub struct InboxMessage {
    pub id: i64,
    pub sender: String,
    pub content: String,
    pub timestamp: String,
    pub msg_type: String,
}

impl From<&Message> for InboxMessage {
    fn from(m: &Message) -> Self {
        InboxMessage {
            id: m.id,
            sender: m.sender.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
            msg_type: m.msg_type.clone(),
        }
    }
}

/// Paginated query result.
#[derive(Debug)]
pub struct QueryResult {
    pub messages: Vec<ChatMessage>,
    pub total: i64,
    pub start_id: i64,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// InstanceSettings — per-instance configuration (.proto for settings.json)
// ---------------------------------------------------------------------------

/// A model entry in extra_models array.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ExtraModel {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
}

/// Per-instance settings loaded from instance root settings.json.
///
/// This is the declarative contract for settings.json structure.
/// All fields use serde for serialization — no manual JSON parsing.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct InstanceSettings {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub privileged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_beats: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_separator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_blocks_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_block_kb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_kb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_max_consecutive_beats: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_cooldown_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_models: Vec<ExtraModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

impl InstanceSettings {
    /// Default model when not specified in settings.json or env.
    pub const DEFAULT_MODEL: &str = "openrouter@anthropic/claude-opus-4.6";

    /// Apply environment variable fallbacks for api_key, model, and user_id.
    /// Call this after loading from file to fill in missing values.
    pub fn apply_env_fallbacks(&mut self) {
        if self.api_key.is_empty() {
            self.api_key = std::env::var("ALICE_DEFAULT_API_KEY").ok().unwrap_or_default();
        }
        if self.model.is_empty() {
            self.model = std::env::var("ALICE_DEFAULT_MODEL").ok()
                .unwrap_or_else(|| Self::DEFAULT_MODEL.to_string());
        }
        if self.user_id.is_empty() {
            self.user_id = std::env::var("ALICE_USER_ID").ok()
                .unwrap_or_else(|| "default".to_string());
        }
    }

    /// Check that required fields are present. Call after apply_env_fallbacks().
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.api_key.is_empty() {
            anyhow::bail!("Missing api_key: set in settings.json or ALICE_DEFAULT_API_KEY env var");
        }
        Ok(())
    }

    /// Parse the model field "provider@model_id" into (api_url, model_id).
    pub fn parse_model(&self) -> (String, String) {
        Self::parse_model_str(&self.model)
    }

    /// Parse a model string "provider@model_id" into (api_url, model_id).
    pub fn parse_model_str(model: &str) -> (String, String) {
        if let Some(pos) = model.find('@') {
            let provider = &model[..pos];
            let model_id = &model[pos + 1..];
            let api_url = match provider {
                "openrouter" => "https://openrouter.ai/api/v1/chat/completions".to_string(),
                "openai" => "https://api.openai.com/v1/chat/completions".to_string(),
                "zenmux" => "https://zenmux.ai/api/v1/chat/completions".to_string(),
                other => {
                    tracing::warn!("Unknown provider '{}', using as direct URL", other);
                    other.to_string()
                }
            };
            (api_url, model_id.to_string())
        } else {
            (
                "https://openrouter.ai/api/v1/chat/completions".to_string(),
                model.to_string(),
            )
        }
    }
}