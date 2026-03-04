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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_env: Option<String>,
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
    pub user_id: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_env: Option<String>,
}

impl SettingsUpdate {
    /// Apply non-None fields to the given settings.
    pub fn apply_to(&self, s: &mut InstanceSettings) {
        if let Some(ref v) = self.api_key { s.api_key = v.clone(); }
        if let Some(ref v) = self.model { s.model = v.clone(); }
        if let Some(ref v) = self.user_id { s.user_id = v.clone(); }
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
        if let Some(v) = self.temperature { s.temperature = Some(v); }
        if let Some(v) = self.max_tokens { s.max_tokens = Some(v); }
        if let Some(ref v) = self.host { s.host = Some(v.clone()); }
        if let Some(ref v) = self.shell_env { s.shell_env = Some(v.clone()); }
    }

    /// Fill None fields from fallback. Self takes priority.
    pub fn merge_fallback(&mut self, fallback: &SettingsUpdate) {
        if self.api_key.is_none() { self.api_key = fallback.api_key.clone(); }
        if self.model.is_none() { self.model = fallback.model.clone(); }
        if self.user_id.is_none() { self.user_id = fallback.user_id.clone(); }
        if self.privileged.is_none() { self.privileged = fallback.privileged; }
        if self.max_beats.is_none() { self.max_beats = fallback.max_beats; }
        if self.session_blocks_limit.is_none() { self.session_blocks_limit = fallback.session_blocks_limit; }
        if self.session_block_kb.is_none() { self.session_block_kb = fallback.session_block_kb; }
        if self.history_kb.is_none() { self.history_kb = fallback.history_kb; }
        if self.safety_max_consecutive_beats.is_none() { self.safety_max_consecutive_beats = fallback.safety_max_consecutive_beats; }
        if self.safety_cooldown_secs.is_none() { self.safety_cooldown_secs = fallback.safety_cooldown_secs; }
        if self.name.is_none() { self.name = fallback.name.clone(); }
        if self.color.is_none() { self.color = fallback.color.clone(); }
        if self.avatar.is_none() { self.avatar = fallback.avatar.clone(); }
        if self.temperature.is_none() { self.temperature = fallback.temperature; }
        if self.max_tokens.is_none() { self.max_tokens = fallback.max_tokens; }
        if self.host.is_none() { self.host = fallback.host.clone(); }
        if self.shell_env.is_none() { self.shell_env = fallback.shell_env.clone(); }
    }

    /// Build seed settings from environment variables and engine.toml defaults.
    pub fn from_env_and_defaults(env: &crate::policy::EnvConfig) -> Self {
        let llm = &crate::policy::EngineConfig::get().llm;
        let mem = &crate::policy::EngineConfig::get().memory;
        Self {
            api_key: if env.default_api_key.is_empty() { None } else { Some(env.default_api_key.clone()) },
            model: env.default_model.clone().or_else(|| Some(llm.default_model.clone())),
            user_id: Some(env.user_id.clone()),
            privileged: None,
            max_beats: None,
            session_blocks_limit: Some(mem.session_blocks_limit),
            session_block_kb: Some(mem.session_block_kb),
            history_kb: Some(mem.history_kb),
            safety_max_consecutive_beats: Some(mem.safety_max_consecutive_beats),
            safety_cooldown_secs: Some(mem.safety_cooldown_secs),
            name: None,
            color: None,
            avatar: None,
            temperature: Some(llm.temperature),
            max_tokens: Some(llm.max_tokens),
            host: env.host.clone(),
            shell_env: if env.shell_env.is_empty() { None } else { Some(env.shell_env.clone()) },
        }
    }

    /// Initialize global settings: load from file, merge with seed, write back.
    /// Returns (global_settings, path).
    pub fn init_global(base_dir: &std::path::Path, env: &crate::policy::EnvConfig) -> (Self, std::path::PathBuf) {
        let path = base_dir.join(crate::persist::GLOBAL_SETTINGS_FILE);
        let seed = Self::from_env_and_defaults(env);
        let mut gs = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str::<Self>(&c).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        };
        gs.merge_fallback(&seed);
        if let Ok(json) = serde_json::to_string_pretty(&gs) {
            std::fs::write(&path, json).ok();
        }
        (gs, path)
    }

    /// Resolve into a complete InstanceSettings, using defaults for missing fields.
    pub fn resolve(&self) -> InstanceSettings {
        InstanceSettings {
            api_key: self.api_key.clone().unwrap_or_default(),
            model: self.model.clone().unwrap_or_default(),
            user_id: self.user_id.clone().unwrap_or_default(),
            privileged: self.privileged.unwrap_or(false),
            max_beats: self.max_beats,
            session_blocks_limit: self.session_blocks_limit,
            session_block_kb: self.session_block_kb,
            history_kb: self.history_kb,
            safety_max_consecutive_beats: self.safety_max_consecutive_beats,
            safety_cooldown_secs: self.safety_cooldown_secs,
            name: self.name.clone(),
            color: self.color.clone(),
            avatar: self.avatar.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            host: self.host.clone(),
            shell_env: self.shell_env.clone(),
        }
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