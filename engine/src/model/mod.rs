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