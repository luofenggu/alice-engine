//! API shared types — data contracts for HTTP API.
//!
//! These types are the declarative contract between engine and frontend.
//! All fields use serde for serialization — no manual JSON parsing.

use serde::{Deserialize, Serialize};

// ============================================================
// Instance Info
// ============================================================

/// 实例基本信息
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub color: String,
    #[serde(default)]
    pub privileged: bool,
    #[serde(rename = "lastActive")]
    pub last_active: i64,
}

// ============================================================
// Messages
// ============================================================

/// 消息
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageInfo {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub sender: String,
    pub recipient: String,
}

/// 消息查询结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessagesResult {
    pub messages: Vec<MessageInfo>,
    pub has_more: bool,
}

// ============================================================
// Observe / Status
// ============================================================

/// 引擎在线状态
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum EngineOnlineStatus {
    Inferring,
    Online,
    #[default]
    Offline,
}

/// 实例推理状态
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObserveResult {
    pub engine_online: EngineOnlineStatus,
    pub inferring: bool,
    pub idle: bool,
    pub born: bool,
    pub current_action: Option<String>,
    pub executing_script: Option<String>,
    pub infer_output: Option<String>,
    pub recent_actions: Vec<String>,
    pub idle_timeout_secs: Option<i64>,
    pub idle_since: Option<i64>,
    #[serde(default)]
    pub user_id: String,
}

// ============================================================
// Action Result
// ============================================================

/// 操作结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionResult {
    pub success: bool,
    pub message: Option<String>,
}

impl ActionResult {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
        }
    }
    pub fn ok_empty() -> Self {
        Self {
            success: true,
            message: None,
        }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: Some(message.into()),
        }
    }
}

// ============================================================
// File Operations
// ============================================================

/// 文件信息
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

/// 文件读取结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileReadResult {
    pub content: String,
    pub size: u64,
    pub is_binary: bool,
}

impl FileReadResult {
    pub fn text(content: String, size: u64) -> Self {
        Self {
            content,
            size,
            is_binary: false,
        }
    }
    pub fn binary(description: String, size: u64) -> Self {
        Self {
            content: description,
            size,
            is_binary: true,
        }
    }
    pub fn error(message: String) -> Self {
        Self {
            content: message,
            size: 0,
            is_binary: false,
        }
    }
}
