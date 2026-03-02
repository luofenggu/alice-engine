//! RPC Server — 引擎端的tarpc服务实现
//!
//! 通过Unix socket暴露类型安全的RPC接口，供Leptos前端调用。
//! 与HTTP API并行运行，逐步替代前端对HTTP的依赖。

use std::sync::{Arc, Mutex};
use chrono::TimeZone;

use alice_rpc::{
    AliceEngine, ObserveResult, ActionResult,
    InstanceInfo, MessageInfo, MessagesResult,
    FileInfo, FileReadResult,
    RPC_SOCKET_PATH,
};
use tarpc::context::Context;
use tarpc::server::{self, Channel};
use tokio::net::UnixListener;
use tracing::{info, error};

use std::collections::HashMap;
use std::path::PathBuf;

use crate::core::instance::InstanceStore;
use crate::core::signal::SignalHub;

/// 引擎状态（替代原web层的AppState，只保留引擎需要的字段）
pub struct EngineState {
    /// Instance store for path resolution and lifecycle management.
    pub instance_store: InstanceStore,
    /// Logs directory.
    pub logs_dir: PathBuf,
    /// User ID for messages.
    pub user_id: String,
    /// Signal hub for inter-thread communication (interrupt, switch-model).
    pub signal_hub: SignalHub,
}

impl EngineState {
    pub fn new(instances_dir: PathBuf, logs_dir: PathBuf, user_id: String, signal_hub: SignalHub) -> Self {
        Self {
            instance_store: InstanceStore::new(instances_dir),
            logs_dir,
            user_id,
            signal_hub,
        }
    }

}

/// RPC服务实现，持有引擎状态
#[derive(Clone)]
struct AliceEngineServer {
    state: Arc<EngineState>,
}

impl AliceEngine for AliceEngineServer {
    async fn get_instances(self, _: Context) -> Vec<InstanceInfo> {
        let store = self.state.instance_store.clone();
        let result = tokio::task::spawn_blocking(move || {
            collect_instances(&store)
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
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&instance_id)?;
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

        let user_id = self.state.user_id.clone();
        let store = self.state.instance_store.clone();
        let name = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&name)?;
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
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&instance_id)?;
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
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&instance_id)?;
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            ch.read_status()
        }).await;

        match result {
            Ok(Ok(Some(status_json))) => parse_observe_result(&status_json),
            _ => ObserveResult::default(),
        }
    }

    async fn interrupt(self, _: Context, instance_id: String) -> ActionResult {
        self.state.signal_hub.set_interrupt(&instance_id);
        info!("[RPC] Interrupt signal set for {}", instance_id);
        ActionResult { success: true, message: None }
    }

    async fn switch_model(self, _: Context, instance_id: String, model_index: u32) -> ActionResult {
        self.state.signal_hub.set_switch_model(&instance_id, model_index as usize);
        info!("[RPC] Switch model signal set for {}: index={}", instance_id, model_index);
        ActionResult { success: true, message: None }
    }

    async fn create_instance(self, _: Context, display_name: String) -> ActionResult {
        let display_name = display_name.trim().to_string();
        let name_opt = if display_name.is_empty() { None } else { Some(display_name.as_str()) };

        match self.state.instance_store.create(&self.state.user_id, name_opt, None) {
            Ok(instance) => {
                info!("[RPC] Created instance: id={}, name={:?}", instance.id, name_opt);
                ActionResult { success: true, message: Some(instance.id) }
            }
            Err(e) => {
                error!("[RPC] Create instance failed: {}", e);
                ActionResult { success: false, message: Some(e.to_string()) }
            }
        }
    }

    async fn delete_instance(self, _: Context, instance_id: String) -> ActionResult {
        match self.state.instance_store.delete(&instance_id) {
            Ok(trash_name) => {
                info!("[RPC] Deleted instance: {} -> .trash/{}", instance_id, trash_name);
                ActionResult { success: true, message: Some(format!("Deleted: {}", instance_id)) }
            }
            Err(e) => {
                error!("[RPC] Delete instance failed: {}", e);
                ActionResult { success: false, message: Some(e.to_string()) }
            }
        }
    }

    async fn get_settings(self, _: Context, instance_id: String) -> String {
        let store = self.state.instance_store.clone();
        let id = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&id)?;
            let path = instance.settings.path().to_path_buf();
            std::fs::read_to_string(&path).map_err(anyhow::Error::from)
        }).await;

        match result {
            Ok(Ok(content)) => content,
            Ok(Err(e)) => {
                error!("[RPC] get_settings error for {}: {}", instance_id, e);
                String::new()
            }
            Err(e) => {
                error!("[RPC] get_settings join error: {}", e);
                String::new()
            }
        }
    }

    async fn update_settings(self, _: Context, instance_id: String, settings_json: String) -> ActionResult {
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let settings_path = instance.settings.path().to_path_buf();
            // Read current settings
            let content = std::fs::read_to_string(&settings_path)?;
            let mut settings: serde_json::Value = serde_json::from_str(&content)?;
            let body: serde_json::Value = serde_json::from_str(&settings_json)?;

            // Update allowed fields
            let mut updated = Vec::new();
            let string_fields = ["name", "avatar", "color", "api_key", "model"];
            for field in &string_fields {
                if let Some(val) = body.get(*field).and_then(|v| v.as_str()) {
                    let val = val.trim();
                    if !val.is_empty() {
                        settings[*field] = serde_json::json!(val);
                        if *field == "api_key" {
                            updated.push(format!("{}: ...{}", field, &val[val.len().saturating_sub(4)..]));
                        } else {
                            updated.push(format!("{}: {}", field, val));
                        }
                    }
                }
            }
            if let Some(val) = body.get("privileged") {
                if let Some(b) = val.as_bool() {
                    settings["privileged"] = serde_json::json!(b);
                    updated.push(format!("privileged: {}", b));
                }
            }
            if let Some(val) = body.get("extra_models") {
                if let Some(arr) = val.as_array() {
                    settings["extra_models"] = serde_json::json!(arr);
                    updated.push(format!("extra_models: {} items", arr.len()));
                }
            }

            if updated.is_empty() {
                return Ok(ActionResult { success: false, message: Some("No valid fields to update".to_string()) });
            }

            let new_content = serde_json::to_string_pretty(&settings)?;
            std::fs::write(&settings_path, &new_content)?;

            info!("[RPC] Settings updated for {}: {}", instance_id, updated.join(", "));
            Ok::<_, anyhow::Error>(ActionResult { success: true, message: Some(updated.join(", ")) })
        }).await;

        match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => ActionResult { success: false, message: Some(e.to_string()) },
            Err(e) => ActionResult { success: false, message: Some(e.to_string()) },
        }
    }

    async fn list_files(self, _: Context, instance_id: String, path: String) -> Vec<FileInfo> {
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let workspace = instance.workspace.clone();
            let workspace_canonical = workspace.canonicalize()?;
            let target = if path.is_empty() {
                workspace_canonical.clone()
            } else {
                workspace.join(&path).canonicalize()?
            };

            if !target.starts_with(&workspace_canonical) || !target.is_dir() {
                return Ok::<_, anyhow::Error>(vec![]);
            }

            let mut items = Vec::new();
            for entry in std::fs::read_dir(&target)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                let metadata = entry.metadata()?;
                items.push(FileInfo {
                    name,
                    is_dir: metadata.is_dir(),
                    size: if metadata.is_file() { metadata.len() } else { 0 },
                });
            }
            // Sort: dirs first, then by name
            items.sort_by(|a, b| {
                match (a.is_dir, b.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                }
            });
            Ok(items)
        }).await;

        match result {
            Ok(Ok(items)) => items,
            Ok(Err(e)) => {
                error!("[RPC] list_files error: {}", e);
                vec![]
            }
            Err(e) => {
                error!("[RPC] list_files join error: {}", e);
                vec![]
            }
        }
    }

    async fn read_file(self, _: Context, instance_id: String, path: String) -> FileReadResult {
        let store = self.state.instance_store.clone();
        let empty = FileReadResult { content: String::new(), size: 0, is_binary: false };

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let workspace = instance.workspace.clone();
            let workspace_canonical = workspace.canonicalize()?;
            let target = workspace.join(&path).canonicalize()?;

            if !target.starts_with(&workspace_canonical) || !target.is_file() {
                anyhow::bail!("File not found or access denied");
            }

            let metadata = target.metadata()?;
            let size = metadata.len();

            if size > 1024 * 1024 {
                anyhow::bail!("File too large (>1MB)");
            }

            let file_name = target.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if is_binary_file(&file_name) {
                return Ok(FileReadResult {
                    content: format!("[Binary file: {}, {} bytes]", file_name, size),
                    size,
                    is_binary: true,
                });
            }

            let content = std::fs::read_to_string(&target)?;
            Ok::<_, anyhow::Error>(FileReadResult { content, size, is_binary: false })
        }).await;

        match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                error!("[RPC] read_file error: {}", e);
                FileReadResult { content: e.to_string(), size: 0, is_binary: false }
            }
            Err(e) => {
                error!("[RPC] read_file join error: {}", e);
                empty
            }
        }
    }

    async fn get_knowledge(self, _: Context, instance_id: String) -> String {
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            instance.memory.knowledge.read().map_err(anyhow::Error::from)
        }).await;

        match result {
            Ok(Ok(content)) => content,
            _ => String::new(),
        }
    }

}

// ObserveResult::default() 由 alice-rpc 的 #[derive(Default)] 提供

/// 从engine_status JSON解析ObserveResult
fn parse_observe_result(status_json: &str) -> ObserveResult {
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
fn collect_instances(store: &InstanceStore) -> anyhow::Result<Vec<InstanceInfo>> {
    let mut instances = Vec::new();
    let ids = store.list_ids()?;

    for name in ids {
        let instance = match store.open(&name) {
            Ok(i) => i,
            Err(_) => continue,
        };

        let mut display_name = name.clone();
        let mut color = String::new();
        let mut avatar = String::new();

        if let Ok(settings) = instance.settings.load() {
            if let Some(n) = settings.name {
                if !n.is_empty() {
                    display_name = n;
                }
            }
            color = settings.color.unwrap_or_default();
            avatar = settings.avatar.unwrap_or_default();
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


/// 判断文件是否为二进制格式（基于扩展名）
fn is_binary_file(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".jar") || name.ends_with(".class") || name.ends_with(".zip")
        || name.ends_with(".gz") || name.ends_with(".png") || name.ends_with(".jpg")
        || name.ends_with(".jpeg") || name.ends_with(".gif") || name.ends_with(".ico")
        || name.ends_with(".pdf") || name.ends_with(".exe") || name.ends_with(".so")
        || name.ends_with(".wasm")
}


// ─── JSON辅助函数（从web层迁移）────────────────────────────────

fn extract_json_bool_simple(json: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if rest.starts_with("true") { Some(true) }
    else if rest.starts_with("false") { Some(false) }
    else { None }
}

fn extract_json_i64_simple(json: &str, key: &str) -> Option<i64> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_json_string_simple(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if rest.starts_with("null") { return None; }
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_json_array_raw(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if !rest.starts_with('[') {
        return None;
    }
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped { escaped = false; continue; }
        if c == '\\' && in_string { escaped = true; continue; }
        if c == '"' { in_string = !in_string; continue; }
        if in_string { continue; }
        if c == '[' { depth += 1; }
        if c == ']' {
            depth -= 1;
            if depth == 0 {
                return Some(rest[..=i].to_string());
            }
        }
    }
    None
}

/// Parse engine status JSON to determine if engine is online and idle.
fn parse_engine_status(status_json: &str) -> (bool, bool, bool, Option<String>, bool) {
    let status_str = extract_json_string_simple(status_json, "status")
        .unwrap_or_default();
    let is_inferring = status_str == "inferring";
    let idle = !is_inferring;

    let last_beat_ms = extract_json_string_simple(status_json, "lastBeat")
        .and_then(|s| parse_timestamp_to_millis(&s))
        .unwrap_or(0);

    let engine_online = if is_inferring {
        true
    } else {
        let now_ms = chrono::Utc::now().timestamp_millis();
        (now_ms - last_beat_ms) < 30000
    };

    let infer_log_path = extract_json_string_simple(status_json, "logPath");
    let born = extract_json_bool_simple(status_json, "born").unwrap_or(false);

    (engine_online, idle, is_inferring, infer_log_path, born)
}

/// Parse "20260222083246" format to milliseconds since epoch.
fn parse_timestamp_to_millis(s: &str) -> Option<i64> {
    if s.len() < 14 { return None; }
    let dt = chrono::NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M%S").ok()?;
    let local = chrono::Local.from_local_datetime(&dt).single()?;
    Some(local.timestamp_millis())
}

/// 启动RPC server（Unix socket）
pub async fn start_rpc_server(state: Arc<EngineState>) {
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