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

use crate::persist::instance::InstanceStore;
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
    /// Engine configuration (file browse rules, LLM policy, etc.)
    pub engine_config: crate::policy::EngineConfig,
    /// Environment configuration (all ALICE_* env vars).
    pub env_config: Arc<crate::policy::EnvConfig>,
}

impl EngineState {
    pub fn new(instances_dir: PathBuf, logs_dir: PathBuf, user_id: String, signal_hub: SignalHub, engine_config: crate::policy::EngineConfig, env_config: Arc<crate::policy::EnvConfig>) -> Self {
        Self {
            instance_store: InstanceStore::new(instances_dir),
            logs_dir,
            user_id,
            signal_hub,
            engine_config,
            env_config,
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
    ) -> Result<MessagesResult, String> {
        let rpc_config = &self.state.engine_config.rpc;
        let limit = limit.max(rpc_config.min_page_size).min(rpc_config.max_page_size);
        let store = self.state.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&instance_id)?;
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(after) = after_id {
                // 轮询新消息（包含所有角色，支持多端同步）
                let rows = ch.get_messages_after(after, limit)?;
                let messages: Vec<MessageInfo> = rows.into_iter().map(|(id, role, content, timestamp)| {
                    MessageInfo { id, role, content, timestamp }
                }).collect();
                let has_more = messages.len() >= limit as usize;
                Ok::<_, anyhow::Error>(MessagesResult { messages, has_more })
            } else {
                // 分页查询
                let qr = ch.query(limit, before_id)?;
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
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => {
                error!("[RPC] get_messages error: {}", e);
                Err(e.to_string())
            }
            Err(e) => {
                error!("[RPC] get_messages join error: {}", e);
                Err(e.to_string())
            }
        }
    }

    async fn send_message(self, _: Context, instance_id: String, content: String) -> ActionResult {
        let content = content.trim().to_string();
        if content.is_empty() {
            return ActionResult::err(crate::policy::messages::empty_message());
        }

        let user_id = self.state.user_id.clone();
        let store = self.state.instance_store.clone();
        let name = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&name)?;
            let mut ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
            let id = ch.write_user_message(&user_id, &content, &timestamp)?;
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
        let max_page = self.state.engine_config.rpc.max_page_size;

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&instance_id)?;
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            ch.get_messages_after(after_id, max_page)
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
                    alice_rpc::EngineOnlineStatus::Inferring
                } else if status.last_beat.elapsed() < std::time::Duration::from_secs(self.state.engine_config.rpc.heartbeat_timeout_secs) {
                    alice_rpc::EngineOnlineStatus::Online
                } else {
                    alice_rpc::EngineOnlineStatus::Offline
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


    async fn create_instance(self, _: Context, display_name: String, initial_settings: Option<SettingsUpdate>) -> ActionResult {
        let display_name = display_name.trim().to_string();
        let name_opt = if display_name.is_empty() { None } else { Some(display_name.as_str()) };

        match self.state.instance_store.create(&self.state.user_id, name_opt, None, initial_settings.as_ref()) {
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
                ActionResult::ok(crate::policy::messages::instance_deleted(&instance_id))
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
            let old = settings.clone();

            update.apply_to(&mut settings);

            match crate::policy::messages::describe_settings_change(&old, &settings) {
                Some(desc) => {
                    instance.settings.save(&settings)?;
                    info!("[RPC] Settings updated for {}: {}", instance_id, desc);
                    Ok::<_, anyhow::Error>(ActionResult::ok(desc))
                }
                None => Ok(ActionResult::err(crate::policy::messages::no_valid_fields())),
            }
        }).await;

        match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => ActionResult::err(e.to_string()),
            Err(e) => ActionResult::err(e.to_string()),
        }
    }

    async fn list_files(self, _: Context, instance_id: String, path: String) -> Vec<FileInfo> {
        let store = self.state.instance_store.clone();
        let file_browse = self.state.engine_config.file_browse.clone();

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
                if file_browse.is_hidden_file(&name) || file_browse.is_hidden_dir(&name) {
                    continue;
                }
                let metadata = entry.metadata()?;
                items.push(FileInfo {
                    name,
                    is_dir: metadata.is_dir(),
                    size: if metadata.is_file() { Some(metadata.len()) } else { None },
                });
            }
            // Sort: dirs first, then by name
            items.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
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
        let engine_config = self.state.engine_config.clone();
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

            if size > engine_config.file_browse.max_file_size {
                anyhow::bail!("File too large (>1MB)");
            }

            let file_name = target.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if engine_config.file_browse.is_binary_file(&file_name) {
                return Ok(FileReadResult::binary(crate::policy::messages::binary_file_description(&file_name, size), size));
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






/// 启动RPC server（Unix socket）
pub async fn start_rpc_server(state: Arc<EngineState>) {
    // RPC socket路径：env_config > 默认常量
    let socket_path = state.env_config.rpc_socket.clone()
        .unwrap_or_else(|| RPC_SOCKET_PATH.to_string());

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