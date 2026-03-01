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

/// 实例推理状态
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObserveResult {
    /// 引擎是否在线（心跳是否存活）
    pub engine_online: bool,
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
    /// 当前活跃模型索引
    #[serde(default)]
    pub active_model: i64,
    /// 可用模型总数
    #[serde(default)]
    pub model_count: i64,
}

/// 操作结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionResult {
    pub success: bool,
    pub message: Option<String>,
}

// ============================================================
// RPC Service — trait定义即契约
// ============================================================

#[tarpc::service]
pub trait AliceEngine {
    // --- 实例 ---
    /// 获取所有实例列表
    async fn get_instances() -> Vec<InstanceInfo>;

    // --- 消息 ---
    /// 查询消息（支持分页：before_id向上翻页，after_id轮询新消息）
    async fn get_messages(
        instance_id: String,
        before_id: Option<i64>,
        after_id: Option<i64>,
        limit: i64,
    ) -> MessagesResult;

    /// 发送消息到实例
    async fn send_message(instance_id: String, content: String) -> ActionResult;

    /// 获取指定ID之后的新回复（轮询用）
    async fn get_replies_after(instance_id: String, after_id: i64) -> Vec<MessageInfo>;

    // --- 推理观察与控制 ---
    /// 观察实例推理状态
    async fn observe(instance_id: String) -> ObserveResult;

    /// 中断当前推理
    async fn interrupt(instance_id: String) -> ActionResult;

    /// 切换LLM模型
    async fn switch_model(instance_id: String, model_index: u32) -> ActionResult;

    /// Create a new instance with optional display name
    async fn create_instance(display_name: String) -> ActionResult;

    /// Delete an instance (move to trash)
    async fn delete_instance(instance_id: String) -> ActionResult;
}