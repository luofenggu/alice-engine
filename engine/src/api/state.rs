//! Engine state — shared state for the HTTP API server.
//!
//! EngineState holds all shared resources needed by API handlers.
//! Business methods are defined here, called directly by route handlers.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, error};

use crate::persist::instance::InstanceStore;
use crate::core::signal::SignalHub;
use crate::api::types::*;

/// Engine state shared across the HTTP server and engine thread.
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
    /// Session token for cookie authentication (SHA256 of auth_secret).
    pub session_token: String,
}

impl EngineState {
    pub fn new(
        instances_dir: PathBuf,
        logs_dir: PathBuf,
        user_id: String,
        signal_hub: SignalHub,
        engine_config: crate::policy::EngineConfig,
        env_config: Arc<crate::policy::EnvConfig>,
    ) -> Self {
        let session_token = if env_config.auth_secret.is_empty() {
            String::new()
        } else {
            use sha2::{Sha256, Digest};
            let hash = Sha256::digest(env_config.auth_secret.as_bytes());
            hex::encode(hash)
        };
        Self {
            instance_store: InstanceStore::new(instances_dir),
            logs_dir,
            user_id,
            signal_hub,
            engine_config,
            env_config,
            session_token,
        }
    }

    /// Get all instance info.
    pub async fn get_instances(&self) -> Vec<InstanceInfo> {
        let store = self.instance_store.clone();
        let result = tokio::task::spawn_blocking(move || {
            collect_instances(&store)
        }).await;

        match result {
            Ok(Ok(instances)) => instances,
            Ok(Err(e)) => {
                error!("[API] get_instances error: {}", e);
                vec![]
            }
            Err(e) => {
                error!("[API] get_instances join error: {}", e);
                vec![]
            }
        }
    }

    /// Create a new instance.
    pub async fn create_instance(&self, display_name: String, initial_settings: Option<SettingsUpdate>) -> ActionResult {
        let display_name = display_name.trim().to_string();
        let name_opt = if display_name.is_empty() { None } else { Some(display_name.as_str()) };

        match self.instance_store.create(&self.user_id, name_opt, None, initial_settings.as_ref()) {
            Ok(instance) => {
                info!("[API] Created instance: id={}, name={:?}", instance.id, name_opt);
                ActionResult::ok(instance.id)
            }
            Err(e) => {
                error!("[API] Create instance failed: {}", e);
                ActionResult::err(e.to_string())
            }
        }
    }

    /// Delete an instance (move to trash).
    pub async fn delete_instance(&self, instance_id: String) -> ActionResult {
        match self.instance_store.delete(&instance_id) {
            Ok(trash_name) => {
                info!("[API] Deleted instance: {} -> .trash/{}", instance_id, trash_name);
                ActionResult::ok(crate::policy::messages::instance_deleted(&instance_id))
            }
            Err(e) => {
                error!("[API] Delete instance failed: {}", e);
                ActionResult::err(e.to_string())
            }
        }
    }

    /// Get messages with pagination.
    pub async fn get_messages(
        &self,
        instance_id: String,
        before_id: Option<i64>,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<MessagesResult, String> {
        let rpc_config = &self.engine_config.rpc;
        let limit = limit.max(rpc_config.min_page_size).min(rpc_config.max_page_size);
        let store = self.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&instance_id)?;
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(after) = after_id {
                let rows = ch.get_messages_after(after, limit)?;
                let messages: Vec<MessageInfo> = rows.into_iter().map(|(id, role, content, timestamp)| {
                    MessageInfo { id, role, content, timestamp }
                }).collect();
                let has_more = messages.len() >= limit as usize;
                Ok::<_, anyhow::Error>(MessagesResult { messages, has_more })
            } else {
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
                error!("[API] get_messages error: {}", e);
                Err(e.to_string())
            }
            Err(e) => {
                error!("[API] get_messages join error: {}", e);
                Err(e.to_string())
            }
        }
    }

    /// Send a user message to an instance.
    pub async fn send_message(&self, instance_id: String, content: String) -> ActionResult {
        let content = content.trim().to_string();
        if content.is_empty() {
            return ActionResult::err(crate::policy::messages::empty_message());
        }

        let user_id = self.user_id.clone();
        let store = self.instance_store.clone();
        let name = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&name)?;
            let mut ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
            let id = ch.write_user_message(&user_id, &content, &timestamp)?;
            info!("[MSG] API: message sent to {}, id={}", name, id);
            Ok::<_, anyhow::Error>(id)
        }).await;

        match result {
            Ok(Ok(id)) => ActionResult::ok(id.to_string()),
            Ok(Err(e)) => ActionResult::err(e.to_string()),
            Err(e) => ActionResult::err(e.to_string()),
        }
    }

    /// Get agent replies after a given message ID (polling).
    pub async fn get_replies_after(&self, instance_id: String, after_id: i64) -> Vec<MessageInfo> {
        let store = self.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let ch = store.get_chat(&instance_id)?;
            let ch = ch.lock().unwrap_or_else(|e| e.into_inner());
            ch.get_agent_replies_after(after_id)
        }).await;

        match result {
            Ok(Ok(replies)) => {
                replies.into_iter().map(|(id, content, timestamp)| {
                    MessageInfo { id, role: "agent".to_string(), content, timestamp }
                }).collect()
            }
            Ok(Err(e)) => {
                error!("[API] get_replies_after error: {}", e);
                vec![]
            }
            Err(e) => {
                error!("[API] get_replies_after join error: {}", e);
                vec![]
            }
        }
    }

    /// Observe instance inference status.
    pub async fn observe(&self, instance_id: String) -> ObserveResult {
        match self.signal_hub.get_status(&instance_id) {
            Some(status) => {
                let engine_online = if status.inferring {
                    EngineOnlineStatus::Inferring
                } else if status.last_beat.elapsed() < std::time::Duration::from_secs(self.engine_config.rpc.heartbeat_timeout_secs) {
                    EngineOnlineStatus::Online
                } else {
                    EngineOnlineStatus::Offline
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

    /// Set interrupt signal for an instance.
    pub async fn interrupt(&self, instance_id: String) -> ActionResult {
        self.signal_hub.set_interrupt(&instance_id);
        info!("[API] Interrupt signal set for {}", instance_id);
        ActionResult::ok_empty()
    }

    /// Get instance settings.
    pub async fn get_settings(&self, instance_id: String) -> InstanceSettings {
        let store = self.instance_store.clone();
        let id = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&id)?;
            instance.settings.load()
        }).await;

        match result {
            Ok(Ok(settings)) => settings,
            Ok(Err(e)) => {
                error!("[API] get_settings error for {}: {}", instance_id, e);
                InstanceSettings::default()
            }
            Err(e) => {
                error!("[API] get_settings join error: {}", e);
                InstanceSettings::default()
            }
        }
    }

    /// Update instance settings (merge-update).
    pub async fn update_settings(&self, instance_id: String, update: SettingsUpdate) -> ActionResult {
        let store = self.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let mut settings = instance.settings.load()?;
            let old = settings.clone();

            update.apply_to(&mut settings);

            match crate::policy::messages::describe_settings_change(&old, &settings) {
                Some(desc) => {
                    instance.settings.save(&settings)?;
                    info!("[API] Settings updated for {}: {}", instance_id, desc);
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

    /// List files in instance workspace.
    pub async fn list_files(&self, instance_id: String, path: String) -> Vec<FileInfo> {
        let store = self.instance_store.clone();
        let file_browse = self.engine_config.file_browse.clone();

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
            items.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(items)
        }).await;

        match result {
            Ok(Ok(items)) => items,
            Ok(Err(e)) => {
                error!("[API] list_files error: {}", e);
                vec![]
            }
            Err(e) => {
                error!("[API] list_files join error: {}", e);
                vec![]
            }
        }
    }

    /// Read a file from instance workspace.
    pub async fn read_file(&self, instance_id: String, path: String) -> FileReadResult {
        let store = self.instance_store.clone();
        let engine_config = self.engine_config.clone();

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
                error!("[API] read_file error: {}", e);
                FileReadResult::error(e.to_string())
            }
            Err(e) => {
                error!("[API] read_file join error: {}", e);
                FileReadResult::error(e.to_string())
            }
        }
    }

    /// Get instance knowledge file content.
    pub async fn get_knowledge(&self, instance_id: String) -> String {
        let store = self.instance_store.clone();

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

/// Collect all instance info from the store.
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