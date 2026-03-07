//! Engine state — shared state for the HTTP API server.
//!
//! EngineState holds all shared resources needed by API handlers.
//! Business methods are defined here, called directly by route handlers.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{error, info};

use crate::api::types::*;
use crate::core::signal::SignalHub;
use crate::persist::hooks::{HooksCaller, HooksConfig, HooksStore};
use crate::persist::instance::InstanceStore;
use crate::persist::{GlobalSettingsStore, Settings};
use std::sync::atomic::AtomicBool;

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
    /// Session cookie name (includes port to avoid conflicts between engines on same host).
    pub session_cookie_name: String,
    /// Global settings store.
    pub global_settings_store: GlobalSettingsStore,
    /// HTML directory for frontend files (fallback to embedded if not found).
    pub html_dir: PathBuf,

    pub setup_completed: AtomicBool,
    /// Shared HTTP client for outbound requests (vision API, etc.)
    pub http_client: reqwest::Client,
    /// Shared LLM client for channel rotation (used by vision, etc.)
    pub llm_client: Arc<crate::external::llm::LlmClient>,
    /// Hooks store for persistent hook configuration.
    pub hooks_store: HooksStore,
    /// Shared hooks caller for all instances (contains config + cache).
    pub hooks_caller: Arc<HooksCaller>,
}

impl EngineState {
    pub fn new(
        instances_dir: PathBuf,
        logs_dir: PathBuf,
        html_dir: PathBuf,
        user_id: String,
        signal_hub: SignalHub,
        engine_config: crate::policy::EngineConfig,
        env_config: Arc<crate::policy::EnvConfig>,
        global_settings_store: GlobalSettingsStore,
        llm_client: Arc<crate::external::llm::LlmClient>,
    ) -> Self {
        let session_token = if env_config.auth_secret.is_empty() {
            String::new()
        } else {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(env_config.auth_secret.as_bytes());
            hex::encode(hash)
        };
        let session_cookie_name =
            super::http_protocol::build_session_cookie_name(&env_config.http_port.to_string());
        let setup_done = global_settings_store
            .load()
            .map(|s| s.api_key.as_ref().map_or(false, |k| !k.is_empty()))
            .unwrap_or(false);
        Self {
            instance_store: InstanceStore::new(instances_dir.clone()),
            logs_dir,
            html_dir,
            user_id,
            signal_hub,
            engine_config,
            env_config,
            session_token,
            session_cookie_name,
            global_settings_store,
            setup_completed: AtomicBool::new(setup_done),
            http_client: reqwest::Client::new(),
            llm_client,
            hooks_store: HooksStore::open(instances_dir.parent().unwrap_or(&instances_dir).join("hooks.json")).unwrap_or_else(|e| {
                tracing::warn!("[HOOKS] Failed to open hooks.json: {}, using defaults", e);
                HooksStore::open(instances_dir.parent().unwrap_or(&instances_dir).join("hooks.json")).expect("hooks store")
            }),
            hooks_caller: Arc::new(HooksCaller::new(HooksConfig::default())),
        }
    }

    /// Get all instance info.
    pub async fn get_instances(&self) -> Vec<InstanceInfo> {
        let store = self.instance_store.clone();
        let result = tokio::task::spawn_blocking(move || collect_instances(&store)).await;

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

    /// Get a single instance by ID.
    pub async fn get_instance(&self, instance_id: String) -> Option<InstanceInfo> {
        let store = self.instance_store.clone();
        let result = tokio::task::spawn_blocking(move || match store.open(&instance_id) {
            Ok(instance) => Some(build_instance_info(instance_id, &instance)),
            Err(_) => None,
        })
        .await;

        match result {
            Ok(info) => info,
            Err(e) => {
                error!("[API] get_instance join error: {}", e);
                None
            }
        }
    }

    /// Create a new instance.
    pub async fn create_instance(
        &self,
        display_name: String,
        initial_settings: Option<Settings>,
    ) -> ActionResult {
        let display_name = display_name.trim().to_string();
        let name_opt = if display_name.is_empty() {
            None
        } else {
            Some(display_name.as_str())
        };

        match self
            .instance_store
            .create(&self.user_id, name_opt, None, initial_settings.as_ref())
        {
            Ok(instance) => {
                info!(
                    "[API] Created instance: id={}, name={:?}",
                    instance.id, name_opt
                );
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
                info!(
                    "[API] Deleted instance: {} -> .trash/{}",
                    instance_id, trash_name
                );
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
        let limit = limit
            .max(rpc_config.min_page_size)
            .min(rpc_config.max_page_size);
        let store = self.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let ch = instance.chat.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(after) = after_id {
                let rows = ch.get_messages_after(after, limit)?;
                let messages: Vec<MessageInfo> = rows
                    .into_iter()
                    .map(|(id, role, content, timestamp)| MessageInfo {
                        id,
                        role,
                        content,
                        timestamp,
                    })
                    .collect();
                let has_more = messages.len() >= limit as usize;
                Ok::<_, anyhow::Error>(MessagesResult { messages, has_more })
            } else {
                let qr = ch.query(limit, before_id)?;
                let messages: Vec<MessageInfo> = qr
                    .messages
                    .iter()
                    .map(|m| MessageInfo {
                        id: m.id,
                        role: m.role.clone(),
                        content: m.content.clone(),
                        timestamp: m.timestamp.clone(),
                    })
                    .collect();
                Ok(MessagesResult {
                    messages,
                    has_more: qr.has_more,
                })
            }
        })
        .await;

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
            let instance = store.open(&name)?;
            let mut ch = instance.chat.lock().unwrap_or_else(|e| e.into_inner());
            let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
            let id = ch.write_user_message(&user_id, &content, &timestamp)?;
            info!("[MSG] API: message sent to {}, id={}", name, id);
            Ok::<_, anyhow::Error>(id)
        })
        .await;

        match result {
            Ok(Ok(id)) => ActionResult::ok(id.to_string()),
            Ok(Err(e)) => ActionResult::err(e.to_string()),
            Err(e) => ActionResult::err(e.to_string()),
        }
    }

    /// Send a system message to an instance (no auth required for sender identity).
    pub async fn send_system_message(
        &self,
        instance_id: String,
        content: String,
    ) -> ActionResult {
        let content = content.trim().to_string();
        if content.is_empty() {
            return ActionResult::err(crate::policy::messages::empty_message());
        }

        let store = self.instance_store.clone();
        let name = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&name)?;
            let mut ch = instance.chat.lock().unwrap_or_else(|e| e.into_inner());
            let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
            let id = ch.write_system_message(&content, &timestamp)?;
            info!("[MSG] API: system message sent to {}, id={}", name, id);
            Ok::<_, anyhow::Error>(id)
        })
        .await;

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
            let instance = store.open(&instance_id)?;
            let ch = instance.chat.lock().unwrap_or_else(|e| e.into_inner());
            ch.get_messages_after(after_id, 100)
        })
        .await;

        match result {
            Ok(Ok(replies)) => replies
                .into_iter()
                .map(|(id, role, content, timestamp)| MessageInfo {
                    id,
                    role,
                    content,
                    timestamp,
                })
                .collect(),
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
                } else if status.last_beat.elapsed()
                    < std::time::Duration::from_secs(self.engine_config.rpc.heartbeat_timeout_secs)
                {
                    EngineOnlineStatus::Online
                } else {
                    EngineOnlineStatus::Offline
                };

                let infer_output = if status.inferring {
                    status
                        .log_path
                        .as_ref()
                        .and_then(|p| std::fs::read_to_string(p).ok())
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
    pub async fn get_settings(&self, instance_id: String) -> Settings {
        let store = self.instance_store.clone();
        let id = instance_id.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&id)?;
            instance.settings.load()
        })
        .await;

        match result {
            Ok(Ok(settings)) => settings,
            Ok(Err(e)) => {
                error!("[API] get_settings error for {}: {}", instance_id, e);
                Settings::default()
            }
            Err(e) => {
                error!("[API] get_settings join error: {}", e);
                Settings::default()
            }
        }
    }

    /// Update instance settings (merge-update).
    pub async fn update_settings(&self, instance_id: String, update: Settings) -> ActionResult {
        let store = self.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let current = instance.settings.load()?;
            let old = current.clone();

            // Merge: update's non-None fields overwrite current
            let mut merged = update;
            merged.merge_fallback(&current);

            match crate::policy::messages::describe_settings_change(&old, &merged) {
                Some(desc) => {
                    instance.settings.save(&merged)?;
                    info!("[API] Settings updated for {}: {}", instance_id, desc);
                    Ok::<_, anyhow::Error>(ActionResult::ok(desc))
                }
                None => Ok(ActionResult::err(crate::policy::messages::no_valid_fields())),
            }
        })
        .await;

        match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => ActionResult::err(e.to_string()),
            Err(e) => ActionResult::err(e.to_string()),
        }
    }

    /// Get global settings.
    pub async fn get_global_settings(&self) -> Settings {
        let store = self.global_settings_store.clone();
        let result = tokio::task::spawn_blocking(move || store.load()).await;

        match result {
            Ok(Ok(settings)) => settings,
            Ok(Err(e)) => {
                error!("[API] get_global_settings error: {}", e);
                Settings::default()
            }
            Err(e) => {
                error!("[API] get_global_settings join error: {}", e);
                Settings::default()
            }
        }
    }

    /// Update global settings (merge-update).
    pub async fn update_global_settings(&self, update: Settings) -> ActionResult {
        let store = self.global_settings_store.clone();
        let result = tokio::task::spawn_blocking(move || {
            store.merge_update(update)?;
            Ok::<_, anyhow::Error>(ActionResult::ok(
                crate::policy::messages::global_settings_updated(),
            ))
        })
        .await;

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
                // Use std::fs::metadata to follow symlinks (DirEntry::metadata does not on Unix)
                let metadata = std::fs::metadata(entry.path()).or_else(|_| entry.metadata())?;
                items.push(FileInfo {
                    name,
                    is_dir: metadata.is_dir(),
                    size: if metadata.is_file() {
                        Some(metadata.len())
                    } else {
                        None
                    },
                });
            }
            items.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(items)
        })
        .await;

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

    /// Delete a file or directory from instance workspace.
    pub async fn delete_file(&self, instance_id: String, path: String) -> ActionResult {
        if path.is_empty() {
            return ActionResult::err("Cannot delete workspace root");
        }
        let store = self.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            let workspace = instance.workspace.clone();
            let workspace_canonical = workspace.canonicalize()?;
            let target = workspace.join(&path).canonicalize()?;

            if !target.starts_with(&workspace_canonical) {
                return Err(anyhow::anyhow!("Access denied"));
            }
            if target == workspace_canonical {
                return Err(anyhow::anyhow!("Cannot delete workspace root"));
            }

            if target.is_dir() {
                std::fs::remove_dir_all(&target)?;
            } else {
                std::fs::remove_file(&target)?;
            }
            Ok(path)
        })
        .await;

        match result {
            Ok(Ok(deleted_path)) => ActionResult::ok(format!("Deleted: {}", deleted_path)),
            Ok(Err(e)) => ActionResult::err(e.to_string()),
            Err(e) => ActionResult::err(e.to_string()),
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

            let file_name = target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            if engine_config.file_browse.is_binary_file(&file_name) {
                return Ok(FileReadResult::binary(
                    crate::policy::messages::binary_file_description(&file_name, size),
                    size,
                ));
            }

            let content = std::fs::read_to_string(&target)?;
            Ok::<_, anyhow::Error>(FileReadResult::text(content, size))
        })
        .await;

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
            instance
                .memory
                .knowledge
                .read()
                .map_err(anyhow::Error::from)
        })
        .await;

        match result {
            Ok(Ok(content)) => content,
            _ => String::new(),
        }
    }

    /// Get instance skill file content.
    pub async fn get_skill(&self, instance_id: String) -> String {
        let store = self.instance_store.clone();

        let result = tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            instance.skill.read().map_err(anyhow::Error::from)
        })
        .await;

        match result {
            Ok(Ok(content)) => content,
            _ => String::new(),
        }
    }

    /// Update instance skill file content.
    pub async fn update_skill(&self, instance_id: String, content: String) -> anyhow::Result<()> {
        let store = self.instance_store.clone();

        tokio::task::spawn_blocking(move || {
            let instance = store.open(&instance_id)?;
            instance.skill.write(&content)?;
            Ok(())
        })
        .await?
    }

    /// Run vision inference: send an image to the LLM for understanding.
    /// Uses the shared LLM client's current channel (participates in rotation).
    pub async fn vision(
        &self,
        instance_id: String,
        prompt: String,
        image_url: String,
    ) -> Result<String, String> {
        // Use shared LlmClient's current config (participates in channel rotation)
        let config = self.llm_client.current_config();

        let http_client = self.http_client.clone();
        match crate::external::llm::run_vision_inference(
            &config,
            &http_client,
            &prompt,
            &image_url,
            &instance_id,
        )
        .await
        {
            Ok((text, _usage)) => Ok(text),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Build InstanceInfo from an opened Instance.
fn build_instance_info(id: String, instance: &crate::persist::instance::Instance) -> InstanceInfo {
    let mut display_name = id.clone();
    let mut color = String::new();
    let mut avatar = String::new();
    let mut privileged = false;

    if let Ok(settings) = instance.settings.load() {
        if let Some(n) = &settings.name {
            if !n.is_empty() {
                display_name = n.clone();
            }
        }
        color = settings.color.clone().unwrap_or_default();
        avatar = settings.avatar.clone().unwrap_or_default();
        privileged = settings.privileged_or_default();
    }

    let last_active = instance
        .chat
        .lock()
        .ok()
        .and_then(|chat| chat.get_last_message_time().ok())
        .unwrap_or(0);

    InstanceInfo {
        id,
        name: display_name,
        avatar,
        color,
        privileged,
        last_active,
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
        instances.push(build_instance_info(name, &instance));
    }

    Ok(instances)
}
