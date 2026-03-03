//! Alice RPC — 引擎与前端的类型安全通信层
//!
//! 这是隔离仓：序列化/反序列化的脏活在这里完成，
//! 业务两端只看到干净的Rust类型。
//!
//! 传输层：Unix socket（同机通信，低延迟）
//! 序列化：serde + bincode（性能优先）

use serde::{Deserialize, Serialize};

/// Unix socket 路径（引擎启动时监听，前端连接）
pub const RPC_SOCKET_PATH: &str = "/opt/alice/engine/alice-rpc.sock";

// ============================================================
// 共享类型 — 改了字段两边编译不过
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

/// 实例基本信息
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub color: String,
}

/// 消息
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageInfo {
    pub id: i64,
    pub role: String, // "user" | "agent"
    pub content: String,
    pub timestamp: String,
}

/// 消息查询结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessagesResult {
    pub messages: Vec<MessageInfo>,
    pub has_more: bool,
}

/// 引擎在线状态
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum EngineOnlineStatus {
    /// 正在推理
    Inferring,
    /// 在线（心跳存活）
    Online,
    /// 离线（心跳超时或未知）
    #[default]
    Offline,
}

/// 实例推理状态
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObserveResult {
    /// 引擎在线状态
    pub engine_online: EngineOnlineStatus,
    /// 是否正在推理
    pub inferring: bool,
    /// 是否空闲
    pub idle: bool,
    /// 是否已初始化（born）
    pub born: bool,
    /// 当前正在执行的action描述
    pub current_action: Option<String>,
    /// 正在执行的脚本内容
    pub executing_script: Option<String>,
    /// 推理输出日志（实时内容）
    pub infer_output: Option<String>,
    /// 最近的action列表
    pub recent_actions: Vec<String>,
    /// 空闲超时秒数
    pub idle_timeout_secs: Option<i64>,
    /// 空闲开始时间戳
    pub idle_since: Option<i64>,
}

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
    /// 文件内容（文本文件）或元信息描述（二进制文件）
    pub content: String,
    /// 文件大小（字节）
    pub size: u64,
    /// 是否为二进制文件
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

// ============================================================
// RPC Service — trait定义即契约
// ============================================================

#[tarpc::service]
pub trait AliceEngine {
    // --- 实例管理 ---
    /// 获取所有实例列表
    async fn get_instances() -> Vec<InstanceInfo>;

    /// Create a new instance with optional display name
    async fn create_instance(display_name: String) -> ActionResult;

    /// Delete an instance (move to trash)
    async fn delete_instance(instance_id: String) -> ActionResult;

    // --- 消息 ---
    /// 查询消息（支持分页：before_id向上翻页，after_id轮询新消息）
    async fn get_messages(
        instance_id: String,
        before_id: Option<i64>,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<MessagesResult, String>;

    /// 发送消息到实例
    async fn send_message(instance_id: String, content: String) -> ActionResult;

    /// 获取指定ID之后的新回复（轮询用）
    async fn get_replies_after(instance_id: String, after_id: i64) -> Vec<MessageInfo>;

    // --- 推理观察与控制 ---
    /// 观察实例推理状态
    async fn observe(instance_id: String) -> ObserveResult;

    /// 中断当前推理
    async fn interrupt(instance_id: String) -> ActionResult;

    // --- 设置 ---
    /// 获取实例设置
    async fn get_settings(instance_id: String) -> InstanceSettings;

    /// 更新实例设置（合并更新，只有Some的字段会被更新）
    async fn update_settings(instance_id: String, update: SettingsUpdate) -> ActionResult;

    // --- 文件与知识 ---
    /// 列出实例工作空间中的文件
    async fn list_files(instance_id: String, path: String) -> Vec<FileInfo>;

    /// 读取实例工作空间中的文件
    async fn read_file(instance_id: String, path: String) -> FileReadResult;

    /// 获取实例的知识文件内容
    async fn get_knowledge(instance_id: String) -> String;
}
