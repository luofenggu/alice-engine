//! RPC Server — 引擎端的tarpc服务实现
//!
//! 通过Unix socket暴露类型安全的RPC接口，供Leptos前端调用。
//! 与HTTP API并行运行，逐步替代前端对HTTP的依赖。

use std::sync::Arc;

use alice_rpc::{
    InstanceSettings, SettingsUpdate,
    AliceEngine, ObserveResult, ActionResult,
    InstanceInfo, MessageInfo, MessagesResult,
    FileInfo, FileReadResult,
    RPC_SOCKET_PATH,
};
use tarpc::context::Context;
use tarpc::server::{self, Channel};
use tokio::net::UnixListener;
use tracing::{info, error};


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
            return ActionResult::err("Empty message");
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
            Ok(Ok(id)) => ActionResult::ok(id.to_string()),
            Ok(Err(e)) => ActionResult::err(e.to_string()),
            Err(e) => ActionResult::err(e.to_string()),
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
        match self.state.signal_hub.get_status(&instance_id) {
            Some(status) => {
                let engine_online = if status.inferring {
                    true
                } else {
                    status.last_beat.elapsed() < std::time::Duration::from_secs(30)
                };

                let infer_output = if status.inferring {
                    status.log_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok())
                } else {
                    None
                };

                ObserveResult {
                    engine_online,
                    inferring: status.inferring,
                    idle: !status.inferring,
                    born: status.born,
                    current_action: None,
                    executing_script: None,
                    infer_output,
                    recent_actions: vec![],
                    idle_timeout_secs: status.idle_timeout_secs.map(|v| v as i64),
                    idle_since: status.idle_since.map(|v| v as i64),
                    active_model: status.active_model as i64,
                    model_count: status.model_count as i64,
                }
            }
            None => ObserveResult::default(),
        }
    }

    async fn interrupt(self, _: Context, instance_id: String) -> ActionResult {
        self.state.signal_hub.set_interrupt(&instance_id);
        info!("[RPC] Interrupt signal set for {}", instance_id);
        ActionResult::ok_empty()
    }

    async fn switch_model(self, _: Context, instance_id: String, model_index: u32) -> ActionResult {
        self.state.signal_hub.set_switch_model(&instance_id, model_index as usize);
        info!("[RPC] Switch model signal set for {}: index={}", instance_id, model_index);
        ActionResult::ok_empty()
    }

    async fn create_instance(self, _: Context, display_name: String) -> ActionResult {
        let display_name = display_name.trim().to_string();
        let name_opt = if display_name.is_empty() { None } else { Some(display_name.as_str()) };

        match self.state.instance_store.create(&self.state.user_id, name_opt, None) {
            Ok(instance) => {
                info!("[RPC] Created instance: id={}, name={:?}", instance.id, name_opt);
                ActionResult::ok(instance.id)
            }
            Err(e) => {
                error!("[RPC] Create instance failed: {}", e);
                ActionResult::err(e.to_string())
            }
        }
    }

    async fn delete_instance(self, _: Context, instance_id: String) -> ActionResult {
        match self.state.instance_store.delete(&instance_id) {
            Ok(trash_name) => {
                info!("[RPC] Deleted instance: {} -> .trash/{}", instance_id, trash_name);
                ActionResult::ok(format!("Deleted: {}", instance_id))
            }
            Err(e) => {
                error!("[RPC] Delete instance failed: {}", e);
                ActionResult::err(e.to_string())
            }
        }
    }

    async fn get_settings(self, _: Context, instance_id: String) -> InstanceSettings {
        let store = self.state.instance_store.clone();
        let id = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&id)?;
            instance.settings.load()
        }).await;

        match result {
            Ok(Ok(settings)) => settings,
            Ok(Err(e)) => {
                error!("[RPC] get_settings error for {}: {}", instance_id, e);
                InstanceSettings::default()
            }
            Err(e) => {
                error!("[RPC] get_settings join error: {}", e);
                InstanceSettings::default()
            }
        }
    }

    async fn update_settings(self, _: Context, instance_id: String, update: SettingsUpdate) -> ActionResult {
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let mut settings = instance.settings.load()?;

            let mut updated = Vec::new();

            if let Some(ref name) = update.name {
                settings.name = Some(name.clone());
                updated.push(format!("name: {}", name));
            }
            if let Some(ref avatar) = update.avatar {
                settings.avatar = Some(avatar.clone());
                updated.push(format!("avatar: {}", avatar));
            }
            if let Some(ref color) = update.color {
                settings.color = Some(color.clone());
                updated.push(format!("color: {}", color));
            }
            if let Some(ref api_key) = update.api_key {
                settings.api_key = api_key.clone();
                updated.push(format!("api_key: ...{}", &api_key[api_key.len().saturating_sub(4)..]));
            }
            if let Some(ref model) = update.model {
                settings.model = model.clone();
                updated.push(format!("model: {}", model));
            }
            if let Some(privileged) = update.privileged {
                settings.privileged = privileged;
                updated.push(format!("privileged: {}", privileged));
            }
            if let Some(ref extra_models) = update.extra_models {
                settings.extra_models = extra_models.clone();
                updated.push(format!("extra_models: {} items", extra_models.len()));
            }

            if updated.is_empty() {
                return Ok(ActionResult::err("No valid fields to update"));
            }

            instance.settings.save(&settings)?;

            info!("[RPC] Settings updated for {}: {}", instance_id, updated.join(", "));
            Ok::<_, anyhow::Error>(ActionResult::ok(updated.join(", ")))
        }).await;

        match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => ActionResult::err(e.to_string()),
            Err(e) => ActionResult::err(e.to_string()),
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
                const HIDDEN_DIRS: &[&str] = &["target", "node_modules"];
                if name.starts_with('.') || HIDDEN_DIRS.contains(&name.as_str()) {
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
        let empty = FileReadResult::error(String::new());

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
                return Ok(FileReadResult::binary(format!("[Binary file: {}, {} bytes]", file_name, size), size));
            }

            let content = std::fs::read_to_string(&target)?;
            Ok::<_, anyhow::Error>(FileReadResult::text(content, size))
        }).await;

        match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                error!("[RPC] read_file error: {}", e);
                FileReadResult::error(e.to_string())
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


/// 二进制文件扩展名列表
const BINARY_EXTENSIONS: &[&str] = &[
    ".jar", ".class", ".zip", ".gz", ".png", ".jpg",
    ".jpeg", ".gif", ".ico", ".pdf", ".exe", ".so", ".wasm",
];

/// 判断文件是否为二进制格式（基于扩展名）
fn is_binary_file(name: &str) -> bool {
    let name = name.to_lowercase();
    BINARY_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
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