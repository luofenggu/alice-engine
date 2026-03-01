//! RPC Server — 引擎端的tarpc服务实现
//!
//! 通过Unix socket暴露类型安全的RPC接口，供Leptos前端调用。
//! 与HTTP API并行运行，逐步替代前端对HTTP的依赖。

use std::sync::Arc;
use std::path::Path;

use alice_rpc::{
    AliceEngine, ObserveResult, ActionResult,
    InstanceInfo, MessageInfo, MessagesResult,
    RPC_SOCKET_PATH,
};
use tarpc::context::Context;
use tarpc::server::{self, Channel};
use tokio::net::UnixListener;
use tracing::{info, error};

use crate::web::AppState;

/// RPC服务实现，持有引擎状态
#[derive(Clone)]
struct AliceEngineServer {
    state: Arc<AppState>,
}

impl AliceEngine for AliceEngineServer {
    async fn get_instances(self, _: Context) -> Vec<InstanceInfo> {
        let instances_dir = self.state.instances_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            collect_instances(&instances_dir)
        }).await;

        match result {
            Ok(Ok(instances)) => instances,
            Ok(Err(e)) => {
                error!("[RPC] get_instances error: {}", e);
                vec![]
            }
            Err(e) => {
                error!("[RPC] get_instances join error: {}", e);
                vec![]
            }
        }
    }

    async fn get_messages(
        self, _: Context,
        instance_id: String,
        before_id: Option<i64>,
        after_id: Option<i64>,
        limit: i64,
    ) -> MessagesResult {
        let limit = limit.max(1).min(500);
        let ch = match self.state.get_chat(&instance_id).await {
            Ok(c) => c,
            Err(e) => {
                error!("[RPC] get_messages: chat open error: {}", e);
                return MessagesResult { messages: vec![], has_more: false };
            }
        };

        let result = tokio::task::spawn_blocking(move || {
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(after) = after_id {
                // 轮询新消息（包含所有角色，支持多端同步）
                let rows = ch.get_messages_after(after)?;
                let messages: Vec<MessageInfo> = rows.into_iter().map(|(id, role, content, timestamp)| {
                    MessageInfo { id, role, content, timestamp }
                }).collect();
                Ok::<_, anyhow::Error>(MessagesResult { messages, has_more: false })
            } else {
                // 分页查询
                let before = before_id.unwrap_or(0).max(0);
                let qr = ch.query(limit, before)?;
                let messages: Vec<MessageInfo> = qr.messages.iter().map(|m| {
                    MessageInfo {
                        id: m.id,
                        role: m.role.clone(),
                        content: m.content.clone(),
                        timestamp: m.timestamp.clone(),
                    }
                }).collect();
                Ok(MessagesResult { messages, has_more: qr.has_more })
            }
        }).await;

        match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                error!("[RPC] get_messages error: {}", e);
                MessagesResult { messages: vec![], has_more: false }
            }
            Err(e) => {
                error!("[RPC] get_messages join error: {}", e);
                MessagesResult { messages: vec![], has_more: false }
            }
        }
    }

    async fn send_message(self, _: Context, instance_id: String, content: String) -> ActionResult {
        let content = content.trim().to_string();
        if content.is_empty() {
            return ActionResult { success: false, message: Some("Empty message".to_string()) };
        }

        let instance_dir = self.state.instances_dir.join(&instance_id);
        if !instance_dir.exists() {
            return ActionResult { success: false, message: Some("Instance not found".to_string()) };
        }

        let user_id = self.state.user_id.clone();
        let ch = match self.state.get_chat(&instance_id).await {
            Ok(c) => c,
            Err(e) => return ActionResult { success: false, message: Some(e.to_string()) },
        };

        let name = instance_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
            let id = ch.write_user_message(&user_id, &content, &timestamp, "chat")?;
            info!("[MSG] RPC: message sent to {}, id={}", name, id);
            Ok::<_, anyhow::Error>(id)
        }).await;

        match result {
            Ok(Ok(id)) => ActionResult { success: true, message: Some(id.to_string()) },
            Ok(Err(e)) => ActionResult { success: false, message: Some(e.to_string()) },
            Err(e) => ActionResult { success: false, message: Some(e.to_string()) },
        }
    }

    async fn get_replies_after(self, _: Context, instance_id: String, after_id: i64) -> Vec<MessageInfo> {
        let ch = match self.state.get_chat(&instance_id).await {
            Ok(c) => c,
            Err(e) => {
                error!("[RPC] get_replies_after error: {}", e);
                return vec![];
            }
        };

        let result = tokio::task::spawn_blocking(move || {
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            ch.get_messages_after(after_id)
        }).await;

        match result {
            Ok(Ok(messages)) => {
                messages.into_iter().map(|(id, role, content, timestamp)| {
                    MessageInfo { id, role, content, timestamp }
                }).collect()
            }
            Ok(Err(e)) => {
                error!("[RPC] get_replies_after error: {}", e);
                vec![]
            }
            Err(e) => {
                error!("[RPC] get_replies_after join error: {}", e);
                vec![]
            }
        }
    }

    async fn observe(self, _: Context, instance_id: String) -> ObserveResult {
        let ch = match self.state.get_chat(&instance_id).await {
            Ok(c) => c,
            Err(_) => return ObserveResult::default(),
        };

        let result = tokio::task::spawn_blocking(move || {
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            ch.read_status()
        }).await;

        match result {
            Ok(Ok(Some(status_json))) => parse_observe_result(&status_json),
            _ => ObserveResult::default(),
        }
    }

    async fn interrupt(self, _: Context, instance_id: String) -> ActionResult {
        let signal_path = self.state.instances_dir
            .join(&instance_id)
            .join("interrupt.signal");

        match std::fs::write(&signal_path, "interrupt") {
            Ok(_) => {
                info!("[RPC] Interrupt signal written for {}", instance_id);
                ActionResult { success: true, message: None }
            }
            Err(e) => ActionResult { success: false, message: Some(e.to_string()) },
        }
    }

    async fn switch_model(self, _: Context, instance_id: String, model_index: u32) -> ActionResult {
        let signal_path = self.state.instances_dir
            .join(&instance_id)
            .join("switch-model.signal");

        match std::fs::write(&signal_path, model_index.to_string()) {
            Ok(_) => {
                info!("[RPC] Switch model signal written for {}: index={}", instance_id, model_index);
                ActionResult { success: true, message: None }
            }
            Err(e) => ActionResult { success: false, message: Some(e.to_string()) },
        }
    }

    async fn create_instance(self, _: Context, display_name: String) -> ActionResult {
        let display_name = display_name.trim().to_string();
        let name_opt = if display_name.is_empty() { None } else { Some(display_name.as_str()) };

        match crate::engine::create_instance_dir(
            &self.state.instances_dir,
            &self.state.user_id,
            name_opt,
        ) {
            Ok((id, _path)) => {
                info!("[RPC] Created instance: id={}, name={:?}", id, name_opt);
                ActionResult { success: true, message: Some(id) }
            }
            Err(e) => {
                error!("[RPC] Create instance failed: {}", e);
                ActionResult { success: false, message: Some(e) }
            }
        }
    }

    async fn delete_instance(self, _: Context, instance_id: String) -> ActionResult {
        // Safety: refuse suspicious names
        if instance_id.contains('/') || instance_id.contains("..") || instance_id.is_empty() {
            return ActionResult { success: false, message: Some("Invalid instance id".to_string()) };
        }

        let instance_dir = self.state.instances_dir.join(&instance_id);
        if !instance_dir.exists() {
            return ActionResult { success: false, message: Some("Instance not found".to_string()) };
        }

        let trash_dir = self.state.instances_dir.join(".trash");
        if let Err(e) = std::fs::create_dir_all(&trash_dir) {
            return ActionResult { success: false, message: Some(format!("Failed to create trash dir: {}", e)) };
        }

        let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        let trash_name = format!("{}_{}", instance_id, timestamp);
        let trash_path = trash_dir.join(&trash_name);

        match std::fs::rename(&instance_dir, &trash_path) {
            Ok(()) => {
                info!("[RPC] Moved instance to trash: {} -> .trash/{}", instance_id, trash_name);
                ActionResult { success: true, message: Some(format!("Deleted: {}", instance_id)) }
            }
            Err(e) => ActionResult { success: false, message: Some(format!("Failed to move to trash: {}", e)) },
        }
    }
}

// ObserveResult::default() 由 alice-rpc 的 #[derive(Default)] 提供

/// 从engine_status JSON解析ObserveResult
fn parse_observe_result(status_json: &str) -> ObserveResult {
    use crate::web::{parse_engine_status, extract_json_string_simple, extract_json_array_raw, extract_json_i64_simple};

    let (engine_online, idle, inferring, infer_log_path, born) = parse_engine_status(status_json);

    let current_action = extract_json_string_simple(status_json, "currentDoing");
    let executing_script = extract_json_string_simple(status_json, "executingScript");

    let infer_output = if let Some(ref path) = infer_log_path {
        std::fs::read_to_string(path).ok()
    } else {
        None
    };

    let recent_actions_raw = extract_json_array_raw(status_json, "recentDoings")
        .unwrap_or_else(|| "[]".to_string());
    // Parse JSON array of strings
    let recent_actions: Vec<String> = serde_json::from_str(&recent_actions_raw).unwrap_or_default();

    let idle_timeout_secs = extract_json_i64_simple(status_json, "idleTimeoutSecs");
    let idle_since = extract_json_i64_simple(status_json, "idleSince");
    let active_model = extract_json_i64_simple(status_json, "activeModel").unwrap_or(0);
    let model_count = extract_json_i64_simple(status_json, "modelCount").unwrap_or(1);

    ObserveResult {
        engine_online,
        inferring,
        idle,
        born,
        current_action,
        executing_script,
        infer_output,
        recent_actions,
        idle_timeout_secs,
        idle_since,
        active_model,
        model_count,
    }
}

/// 收集所有实例信息
fn collect_instances(instances_dir: &Path) -> anyhow::Result<Vec<InstanceInfo>> {
    let mut instances = Vec::new();
    let entries = std::fs::read_dir(instances_dir)?;

    for entry in entries {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let settings_path = entry.path().join("settings.json");
        if !settings_path.exists() {
            continue;
        }

        let mut display_name = name.clone();
        let mut color = String::new();
        let mut avatar = String::new();

        if let Ok(content) = std::fs::read_to_string(&settings_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(n) = v.get("name").and_then(|v| v.as_str()) {
                    if !n.is_empty() {
                        display_name = n.to_string();
                    }
                }
                color = v.get("color").and_then(|v| v.as_str()).unwrap_or("").to_string();
                avatar = v.get("avatar").and_then(|v| v.as_str()).unwrap_or("").to_string();
            }
        }

        instances.push(InstanceInfo {
            id: name,
            name: display_name,
            avatar,
            color,
        });
    }

    Ok(instances)
}

/// 启动RPC server（Unix socket）
pub async fn start_rpc_server(state: Arc<AppState>) {
    // RPC socket路径：环境变量 > 默认常量
    let socket_path = std::env::var("ALICE_RPC_SOCKET")
        .unwrap_or_else(|_| RPC_SOCKET_PATH.to_string());

    // 清理旧socket文件
    let _ = std::fs::remove_file(&socket_path);

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            error!("[RPC] Failed to bind Unix socket {}: {}", socket_path, e);
            return;
        }
    };

    info!("[RPC] Server listening on {}", socket_path);

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    // 用JSON codec（兼容性好，调试方便）
                    let transport = tarpc::serde_transport::Transport::from((stream, tarpc::tokio_serde::formats::Json::default()));
                    let server = AliceEngineServer { state };
                    let channel = server::BaseChannel::with_defaults(transport);
                    use futures_util::StreamExt;
                    channel.execute(server.serve())
                        .for_each_concurrent(None, |response| async move {
                            tokio::spawn(response);
                        })
                        .await;
                });
            }
            Err(e) => {
                error!("[RPC] Accept error: {}", e);
            }
        }
    }
}