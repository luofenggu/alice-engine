//! API shared types — data contracts for HTTP API.
//!
//! These types are the declarative contract between engine and frontend.
//! All fields use serde for serialization — no manual JSON parsing.

use serde::{Deserialize, Serialize};

// ============================================================
// Instance Settings
// ============================================================

/// Per-instance settings loaded from instance root settings.json.
///
/// This is the declarative contract for settings.json structure.
/// All fields use serde for serialization — no manual JSON parsing.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
    pub session_blocks_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_block_kb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_kb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_max_consecutive_beats: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_cooldown_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// Settings update request — all fields Optional for merge-update semantics.
/// Only Some fields will be applied to the current settings.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privileged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_beats: Option<u32>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

impl SettingsUpdate {
    /// Apply non-None fields to the given settings.
    pub fn apply_to(&self, s: &mut InstanceSettings) {
        if let Some(ref v) = self.api_key { s.api_key = v.clone(); }
        if let Some(ref v) = self.model { s.model = v.clone(); }
        if let Some(v) = self.privileged { s.privileged = v; }
        if let Some(v) = self.max_beats { s.max_beats = Some(v); }
        if let Some(v) = self.session_blocks_limit { s.session_blocks_limit = Some(v); }
        if let Some(v) = self.session_block_kb { s.session_block_kb = Some(v); }
        if let Some(v) = self.history_kb { s.history_kb = Some(v); }
        if let Some(v) = self.safety_max_consecutive_beats { s.safety_max_consecutive_beats = Some(v); }
        if let Some(v) = self.safety_cooldown_secs { s.safety_cooldown_secs = Some(v); }
        if let Some(ref v) = self.name { s.name = Some(v.clone()); }
        if let Some(ref v) = self.color { s.color = Some(v.clone()); }
        if let Some(ref v) = self.avatar { s.avatar = Some(v.clone()); }
    }
}

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
        Self { success: true, message: Some(message.into()) }
    }
    pub fn ok_empty() -> Self {
        Self { success: true, message: None }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self { success: false, message: Some(message.into()) }
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
        Self { content, size, is_binary: false }
    }
    pub fn binary(description: String, size: u64) -> Self {
        Self { content: description, size, is_binary: true }
    }
    pub fn error(message: String) -> Self {
        Self { content: message, size: 0, is_binary: false }
    }
}