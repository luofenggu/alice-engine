//! # Core Module
//!
//! Contains the central types: Alice (agent instance) and Transaction (runtime context).
//!
//! @HUB - Core types for the Alice engine.
//!
//! ## Architecture
//!
//! - [`Alice`] — The agent instance. Owns workspace, ChatHistory, config.
//!   One Alice = one agent. @TRACE: BEAT, INSTANCE
//!
//! - [`Transaction`] — Runtime context for a single beat. Tracks action records,
//!   timing, and the action separator token. Created at beat start, consumed at beat end.
//!   @TRACE: ACTION
//!
//! - [`AliceConfig`] — Instance configuration (LLM model, API key, etc.)
//!
//! ## Directory Layout
//!
//! ```text
//! {instances_base}/{name}/          ← instance_dir
//!   ├── settings.json               ← instance settings (root level)
//!   ├── memory/                     ← memory_dir
//!   │   ├── sessions/               ← time-series session files
//!   │   │   ├── history.txt         ← long-range narrative (plain text)
//!   │   │   ├── 20260223172500.jsonl ← session block (JSONL, timestamp-named)
//!   │   │   └── current.txt         ← current session (raw action records)
//!   │   ├── knowledge/              ← topic files
//!   │   │   ├── deploy.md
//!   │   │   └── ...
//!   │   ├── snapshots/
//!   │   └── timed_backups/
//!   ├── workspace/                  ← workspace (agent's working directory)
//!   │   ├── chat.db
//!   │   ├── notebook/
//!   │   └── alice-sourcecode/
//!   └── ...
//! ```

use anyhow::Result;
use chrono::Local;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info, warn};

use crate::action::execute::execute_action;
use mad_hatter::llm::{stream_infer_with_on_text, infer_with_on_text, OpenAiChannel, ToMarkdown};
use crate::external::llm::LlmConfig;
use std::sync::atomic::AtomicU64;
use std::sync::RwLock;
use crate::inference::Action;
use crate::inference::output::ActionOutput;
use crate::persist::instance;
use crate::service::extension::ExtensionHandler;
use crate::persist::settings::GlobalSettingsStore;
use crate::policy::action_output;
use crate::prompt::build_beat_request;
use crate::util::Counter;

// ─── Sequence Guard ──────────────────────────────────────────────

/// Sequence guard state for action execution within a single beat.
///
/// Prevents梦游 (sleepwalking) by enforcing action sequence rules:
/// - After a blocking action: only blocking actions allowed, idle is ignored
/// - After idle: zero tolerance, any non-idle action is rejected
///
/// @TRACE: ACTION
#[derive(Debug, Clone, PartialEq)]
enum SequenceState {
    /// Initial state or after a non-blocking action. Any action allowed.
    Normal,
    /// After a blocking action (script/read_file/read_msg).
    /// Only blocking actions allowed. Idle is silently ignored.
    AfterBlocking,
    /// After idle. Zero tolerance: any action is rejected (idle ignored).
    AfterIdle,
}

/// Result of sequence guard check.
#[derive(Debug, Clone, PartialEq)]
pub enum SequenceVerdict {
    /// Action is allowed, proceed with execution.
    Allow,
    /// Action should be silently ignored (idle after blocking/idle).
    Ignore,
    /// Action violates sequence rules, abort inference.
    Reject(String),
}

/// Sequence guard that tracks action execution state within a beat.
///
/// @TRACE: ACTION
pub struct SequenceGuard {
    state: SequenceState,
    instance_id: String,
}

impl SequenceGuard {
    /// Create a new sequence guard for a beat.
    pub fn new(instance_id: &str) -> Self {
        Self {
            state: SequenceState::Normal,
            instance_id: instance_id.to_string(),
        }
    }

    /// Check if an action is allowed given the current state.
    pub fn check(&mut self, action: &Action) -> SequenceVerdict {
        let is_blocking = action.is_blocking();
        let is_idle = matches!(action, Action::Idle { .. });

        match &self.state {
            SequenceState::Normal => {
                if is_idle {
                    self.state = SequenceState::AfterIdle;
                } else if is_blocking {
                    self.state = SequenceState::AfterBlocking;
                }
                SequenceVerdict::Allow
            }
            SequenceState::AfterBlocking => {
                if is_idle {
                    SequenceVerdict::Ignore
                } else if is_blocking {
                    SequenceVerdict::Allow
                } else if matches!(action, Action::SendMsg { .. }) {
                    SequenceVerdict::Allow
                } else {
                    SequenceVerdict::Reject(
                        crate::policy::messages::sequence_reject_after_blocking(
                            &self.instance_id,
                            &action.to_string(),
                        ),
                    )
                }
            }
            SequenceState::AfterIdle => {
                if is_idle {
                    SequenceVerdict::Ignore
                } else {
                    SequenceVerdict::Reject(crate::policy::messages::sequence_reject_after_idle(
                        &self.instance_id,
                        &action.to_string(),
                    ))
                }
            }
        }
    }
}

impl Action {
    /// Whether this action is "blocking" (requires result feedback).
    pub fn is_blocking(&self) -> bool {
        matches!(self, Action::Script { .. } | Action::ReadMsg)
    }
}

// ─── Directory and file constants ────────────────────────────────
/// @TRACE: MEMORY

/// Knowledge subdirectory under memory_dir

// ─── Configuration ───────────────────────────────────────────────

// ─── Action Record ───────────────────────────────────────────────

/// Record of a single action's execution within a transaction.
///
/// @TRACE: ACTION
#[derive(Debug, Clone)]
pub struct ActionRecord {
    /// Unique action ID: timestamp_hextoken format
    pub action_id: String,
    /// The action that was executed
    pub action: Action,
    /// Execution start time
    pub started_at: Instant,
    /// Execution duration (None if not yet completed)
    pub duration: Option<std::time::Duration>,
}

// ─── Transaction ─────────────────────────────────────────────────

/// Runtime context for a single beat (heartbeat cycle).
///
/// Created at the start of each beat, consumed at the end.
/// Tracks all action records and the action separator token.
///
/// @TRACE: ACTION, BEAT
pub struct Transaction {
    /// All action records in this beat
    pub action_records: Vec<ActionRecord>,
    /// Beat start time
    pub started_at: Instant,
    /// Instance ID (for logging)
    pub instance_id: String,
    /// Whether to cancel subsequent idle actions (set when send_msg fails)
    pub cancel_idle: bool,
}

impl Transaction {
    /// Create a new transaction for a beat.
    ///
    /// @TRACE: BEAT
    pub fn new(instance_id: &str) -> Self {
        info!("[BEAT-{}] Transaction created", instance_id);
        Self {
            action_records: Vec::new(),
            started_at: Instant::now(),
            instance_id: instance_id.to_string(),
            cancel_idle: false,
        }
    }

    /// Generate a unique action ID: YYYYMMDDHHmmss_6hexchars
    pub fn generate_action_id(&self) -> String {
        action_output::generate_action_id()
    }

    /// Record an action's "doing" phase (before execution).
    ///
    /// @TRACE: ACTION
    pub fn record_doing(&mut self, action: Action) -> String {
        let action_id = action_output::generate_action_id();
        info!(
            "[ACTION-{}] START {} ({})",
            self.instance_id, action_id, action
        );
        self.action_records.push(ActionRecord {
            action_id: action_id.clone(),
            action,
            started_at: Instant::now(),
            duration: None,
        });
        action_id
    }

    /// Record an action's "done" phase (after execution).
    ///
    /// @TRACE: ACTION
    pub fn record_done(&mut self, action_id: &str) {
        if let Some(record) = self
            .action_records
            .iter_mut()
            .find(|r| r.action_id == action_id)
        {
            record.duration = Some(record.started_at.elapsed());
            info!(
                "[ACTION-{}] END {} ({:.1}s)",
                self.instance_id,
                action_id,
                record.duration.unwrap_or_default().as_secs_f64()
            );
        } else {
            warn!(
                "[ACTION-{}] record_done called for unknown action_id: {}",
                self.instance_id, action_id
            );
        }
    }

}

// ─── Alice ───────────────────────────────────────────────────────

/// The agent instance. The soul container.
///
/// One Alice = one agent with its own workspace, memory, and identity.
/// The beat() method drives the heartbeat cycle.
///
/// ## Directory Layout
///
/// ```text
/// instance_dir/
///   ├── memory/
///   │   ├── sessions/    ← history.txt + session blocks (JSONL) + current.txt
///   │   └── knowledge/   ← topic files
///   └── workspace/       ← agent's working directoryectory
/// ```
///
/// @HUB - Central struct. All trace lines converge here.
/// @TRACE: BEAT, INSTANCE, MEMORY
pub struct Alice {
    /// Instance persistent state (settings, memory, chat, workspace).
    pub instance: instance::Instance,
    /// Log directory path
    pub log_dir: PathBuf,
    /// Environment configuration (shared, read-only after startup).
    pub env_config: Arc<crate::policy::EnvConfig>,
    /// Current inference log path (Some = inferring, None = idle)
    /// @TRACE: INFER
    pub current_infer_log_path: Option<PathBuf>,
    /// Per-instance LLM channel configurations (shared with SignalHub for API access).
    pub(crate) channel_configs: Arc<RwLock<Vec<LlmConfig>>>,
    /// Per-instance channel rotation counter (shared with SignalHub for API access).
    pub(crate) channel_index: Arc<AtomicU64>,
    /// Whether last beat resulted in idle
    pub last_was_idle: bool,
    /// Idle timeout in seconds (Some = timed idle, None = wait indefinitely)
    pub idle_timeout_secs: Option<u64>,
    /// When idle started (unix timestamp seconds), for countdown display
    pub idle_since: Option<u64>,
    /// Whether this instance runs with root privileges (no sandboxing)
    pub privileged: bool,
    /// System start time (for prompt)
    pub system_start_time: chrono::DateTime<Local>,
    /// Total beat count for this instance (used with max_beats limit)
    pub beat_count: Counter<u32>,
    /// Maximum beats allowed (None = unlimited). From settings.json "max_beats".
    pub max_beats: Option<u32>,
    /// Whether this instance has completed its first idle (born = ready for user interaction).
    pub born: bool,
    /// Public host address for URL generation (e.g. "example.com:8081").
    /// Set by AliceEngine from settings (inherited from env var via global settings).
    pub host: Option<String>,
    /// Shell environment description for prompt (e.g. "Linux系统").
    /// Set by AliceEngine from settings (inherited from env var via global settings).
    pub shell_env: Option<String>,
    /// Consecutive inference failure count (for exponential backoff).
    inference_failures: Counter<u32>,
    /// Backoff deadline: skip inference until this instant.
    inference_backoff_until: Option<Instant>,
    /// Extra LLM configurations for failover (manual switch via API).
    /// Display name from settings.json (e.g. "小白", "牧星").
    pub instance_name: Option<String>,
    /// Signal handles for interrupt and switch-model (None in test mode).
    pub signals: Option<signal::InstanceSignals>,

    /// Maximum number of session blocks before history rolling is triggered.
    pub session_blocks_limit: u32,
    /// Maximum size of a single session block file in KB.
    pub session_block_kb: u32,
    /// Maximum history file size in KB (target for history rolling output).
    pub history_kb: u32,
    /// Safety valve: max consecutive beats without idle before forced cooldown.
    pub safety_max_consecutive_beats: u32,
    /// Safety valve: cooldown duration in seconds after triggering.
    pub safety_cooldown_secs: u64,

    /// Global settings store for hot-reloading channels each beat.
    pub global_settings_store: Option<GlobalSettingsStore>,
    /// Extension handler for cross-instance communication (contacts, relay).
    pub extension: Option<Arc<dyn ExtensionHandler>>,
}

impl Alice {
    /// Create a new Alice instance from an instance directory.
    ///
    /// @TRACE: INSTANCE
    pub fn new(
        instance: instance::Instance,
        log_dir: PathBuf,
        channel_configs: Arc<RwLock<Vec<LlmConfig>>>,
        channel_index: Arc<AtomicU64>,
        env_config: Arc<crate::policy::EnvConfig>,
        global_settings_store: Option<GlobalSettingsStore>,
        extension: Option<Arc<dyn ExtensionHandler>>,
    ) -> Result<Self> {
        // Read settings overrides before instance is moved into struct
        let mem_cfg = &crate::policy::EngineConfig::get().memory;
        let settings = instance.settings.load().ok();
        let session_blocks_limit = settings
            .as_ref()
            .and_then(|s| s.session_blocks_limit)
            .unwrap_or(mem_cfg.session_blocks_limit);
        let session_block_kb = settings
            .as_ref()
            .and_then(|s| s.session_block_kb)
            .unwrap_or(mem_cfg.session_block_kb);
        let history_kb = settings
            .as_ref()
            .and_then(|s| s.history_kb)
            .unwrap_or(mem_cfg.history_kb);
        let safety_max_consecutive_beats = settings
            .as_ref()
            .and_then(|s| s.safety_max_consecutive_beats)
            .unwrap_or(mem_cfg.safety_max_consecutive_beats);
        let safety_cooldown_secs = settings
            .as_ref()
            .and_then(|s| s.safety_cooldown_secs)
            .unwrap_or(mem_cfg.safety_cooldown_secs);

        info!(
            "[INSTANCE-{}] Alice created at {}",
            instance.id,
            instance.instance_dir.display()
        );
        Ok(Self {
            instance,
            log_dir,
            current_infer_log_path: None,
            channel_configs,
            channel_index,
            last_was_idle: true,
            idle_timeout_secs: None,
            idle_since: None,
            privileged: false,
            system_start_time: Local::now(),
            beat_count: Counter::<u32>::new(),
            max_beats: None,
            born: false,
            host: None,
            shell_env: None,
            instance_name: None,
            signals: None,
            inference_failures: Counter::<u32>::new(),
            inference_backoff_until: None,
            session_blocks_limit,
            session_block_kb,
            history_kb,
            safety_max_consecutive_beats,
            safety_cooldown_secs,

            env_config,
            global_settings_store,
            extension,
        })
    }

    // ─── Channel Management (per-instance) ──────────────────────

    /// Build an OpenAiChannel from the current channel config.
    pub fn create_channel(&self) -> OpenAiChannel {
        let configs = self.channel_configs.read().unwrap();
        let idx = self.channel_index.load(std::sync::atomic::Ordering::Relaxed) as usize % configs.len();
        let config = &configs[idx];
        let llm_policy = &crate::policy::EngineConfig::get().llm;
        let (api_url, model_id) = llm_policy.resolve_model(&config.model);
        let mut channel = OpenAiChannel::new(&api_url, &model_id, &config.api_key);
        let max_tokens = config.max_tokens.unwrap_or(llm_policy.max_tokens);
        channel = channel.with_max_tokens(max_tokens);
        // TODO: temperature from config
        channel
    }

    /// Advance to the next channel (round-robin failover). Returns rotation info if multiple channels.
    pub fn advance_channel(&self) -> Option<(String, String)> {
        let configs = self.channel_configs.read().unwrap();
        let len = configs.len();
        if len <= 1 {
            return None;
        }
        drop(configs);
        let old_counter = self.channel_index.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let configs = self.channel_configs.read().unwrap();
        let len = configs.len();
        let old_idx = old_counter as usize % len;
        let new_idx = (old_counter as usize + 1) % len;
        let old_name = Self::channel_display_name(old_idx);
        let new_name = Self::channel_display_name(new_idx);
        info!(
            "[CHANNEL-{}] Rotated from {} to {}",
            self.instance.id, old_name, new_name
        );
        Some((old_name, new_name))
    }

    /// Return current channels status: (list of display names, counter, current index).
    pub fn channels_status(&self) -> (Vec<String>, u64, usize) {
        let configs = self.channel_configs.read().unwrap();
        let len = configs.len();
        let counter = self.channel_index.load(std::sync::atomic::Ordering::Relaxed);
        let current_idx = counter as usize % len;
        let names: Vec<String> = (0..len).map(Self::channel_display_name).collect();
        (names, counter, current_idx)
    }

    /// Select a specific channel by index.
    pub fn select_channel(&self, idx: usize) {
        let configs = self.channel_configs.read().unwrap();
        let len = configs.len();
        let clamped = if idx >= len { 0 } else { idx };
        drop(configs);
        self.channel_index.store(clamped as u64, std::sync::atomic::Ordering::Relaxed);
        info!(
            "[CHANNEL-{}] Selected channel {} ({})",
            self.instance.id,
            clamped,
            Self::channel_display_name(clamped)
        );
    }

    /// Hot-update channel configurations.
    pub fn update_channel_configs(&self, configs: Vec<LlmConfig>) {
        let mut guard = self.channel_configs.write().unwrap();
        *guard = configs;
    }

    /// Display name for a channel index: "primary" for 0, "extra{N}" for others.
    pub fn channel_display_name(idx: usize) -> String {
        signal::channel_display_name(idx)
    }

    // ─── Sessions access ────────────────────────────────────────

    // ─── History Rolling ────────────────────────────────────────

    pub fn prepare_roll(&mut self) -> anyhow::Result<Option<RollTask>> {
        let blocks = self.instance.memory.list_session_blocks_db()?;
        if (blocks.len() as u32) < self.session_blocks_limit {
            return Ok(None);
        }

        let oldest_block = &blocks[0];

        // Idempotency check
        if let Some(last_rolled) = self.instance.memory.get_last_rolled() {
            if last_rolled == oldest_block.as_str() {
                info!(
                    "[ROLL-{}] Idempotency: block {} was already compressed, deleting residual",
                    self.instance.id, oldest_block
                );
                self.instance.memory.delete_session_block_db(oldest_block).ok();
                self.instance.memory.clear_last_rolled();
                return Ok(None);
            }
            // Stale marker, clean up
            self.instance.memory.clear_last_rolled();
        }

        info!(
            "[ROLL-{}] History rolling triggered: {} blocks >= limit {}, preparing {}",
            self.instance.id,
            blocks.len(),
            self.session_blocks_limit,
            oldest_block
        );

        // Read and render the oldest block
        let block_entries = self.instance.memory.read_session_entries_db(oldest_block)?;
        if block_entries.is_empty() {
            self.instance.memory.delete_session_block_db(oldest_block).ok();
            return Ok(None);
        }

        let session_blocks = crate::prompt::extract_session_blocks_from_entries(&block_entries, self);
        let rendered_block = session_blocks
            .iter()
            .map(|b| b.to_markdown())
            .collect::<Vec<_>>()
            .join("\n\n");

        // Read current history from DB
        let current_history = self.instance.memory.read_history();

        // Build LLM prompt via CompressRequest
        let content = if current_history.is_empty() {
            rendered_block.clone()
        } else {
            format!("{}\n\n{}", current_history, rendered_block)
        };
        let request = crate::inference::compress::CompressRequest {
            requirement: format!("不超过{}KB", self.history_kb),
            content,
        };

        // Clone channel Arc refs for background thread
        Ok(Some(RollTask {
            memory: self.instance.memory.clone(),
            oldest_block: oldest_block.clone(),
            request,
            instance_id: self.instance.id.clone(),
            channel_configs: self.channel_configs.clone(),
            channel_index: self.channel_index.clone(),
            log_dir: self.log_dir.clone(),
            infer_log_enabled: self.env_config.infer_log_enabled,
        }))
    }

    /// Check if history rolling is needed and execute it.
    ///
    /// Triggered by engine main loop after beats. Checks if session block count
    /// exceeds `session_blocks_limit`. If so, takes the oldest block, compresses
    /// it into history via an independent LLM call, then deletes the block.
    ///
    /// @TRACE: MEMORY
    pub async fn check_and_roll_history(&mut self) -> Result<Option<String>> {
        let blocks = self.instance.memory.list_session_blocks_db()?;
        if (blocks.len() as u32) < self.session_blocks_limit {
            return Ok(None);
        }

        let oldest_block = &blocks[0];

        // Idempotency check: if this block was already compressed but not deleted
        // (e.g., process killed between history write and block deletion),
        // just delete it and skip re-compression.
        if let Some(last_rolled) = self.instance.memory.get_last_rolled() {
            if last_rolled == oldest_block.as_str() {
                info!(
                    "[ROLL-{}] Idempotency: block {} was already compressed, deleting residual",
                    self.instance.id, oldest_block
                );
                self.instance.memory.delete_session_block_db(oldest_block)?;
                self.instance.memory.clear_last_rolled();
                return Ok(Some(crate::policy::messages::roll_deleted_residual(
                    oldest_block,
                )));
            }
            // Stale marker, clean up
            self.instance.memory.clear_last_rolled();
        }

        info!(
            "[ROLL-{}] History rolling triggered: {} blocks >= limit {}, rolling {}",
            self.instance.id,
            blocks.len(),
            self.session_blocks_limit,
            oldest_block
        );

        // 1. Read and render the oldest block
        let entries = self.instance.memory.read_session_entries_db(oldest_block)?;
        if entries.is_empty() {
            // Empty block, just delete it
            self.instance.memory.delete_session_block_db(oldest_block)?;
            return Ok(Some(crate::policy::messages::roll_deleted_empty(
                oldest_block,
            )));
        }

        let session_blocks: Vec<crate::inference::beat::SessionBlock> = entries
            .iter()
            .map(|e| crate::inference::beat::SessionBlock {
                start_time: e.first_msg.clone(),
                end_time: e.last_msg.clone(),
                messages: vec![],
                summary: e.summary.clone(),
            })
            .collect();
        let rendered_block = session_blocks
            .iter()
            .map(|b| b.to_markdown())
            .collect::<Vec<_>>()
            .join("\n\n");

        // 2. Read current history from DB
        let current_history = self.instance.memory.read_history();

        // 3. Build LLM request via CompressRequest
        let content = if current_history.is_empty() {
            rendered_block.clone()
        } else {
            format!("{}\n\n{}", current_history, rendered_block)
        };
        let request = crate::inference::compress::CompressRequest {
            requirement: format!("不超过{}KB", self.history_kb),
            content,
        };

        // 4. Call LLM via infer() (synchronous, blocking) with on_text for compress log
        info!(
            "[ROLL-{}] Calling LLM for history compression",
            self.instance.id
        );
        let channel = self.create_channel();
        let roll_infer_log_dir = self.log_dir.join(crate::policy::log_formats::INFER_DIR).join(&self.instance.id);
        std::fs::create_dir_all(&roll_infer_log_dir).ok();
        let roll_ts = crate::policy::log_formats::format_infer_timestamp(&chrono::Local::now());
        let compress_log_path = roll_infer_log_dir.join(crate::policy::log_formats::infer_compress_out_filename(&roll_ts));
        let mut compress_log_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&compress_log_path).ok();
        let on_text: Option<Box<dyn FnMut(&str) + Send + '_>> = Some(Box::new(move |chunk: &str| {
            if let Some(ref mut f) = compress_log_file {
                use std::io::Write;
                let _ = f.write_all(chunk.as_bytes());
                let _ = f.flush();
            }
        }));
        let on_input: Option<Box<dyn FnOnce(&str) + Send>> = if self.env_config.infer_log_enabled {
            let in_log_path = roll_infer_log_dir.join(
                crate::policy::log_formats::infer_in_filename(&roll_ts)
            );
            Some(Box::new(move |prompt: &str| {
                crate::logging::write_infer_input_log(&in_log_path, prompt);
            }))
        } else {
            None
        };
        let results = infer_with_on_text::<_, crate::inference::compress::CompressOutput>(&channel, &request, on_text, on_input, None, None).await
            .map_err(|e| anyhow::anyhow!(e))?;
        let output = results.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("LLM returned no compress output"))?;

        let clean_history = output.summary.trim();
        if clean_history.is_empty() {
            warn!(
                "[ROLL-{}] LLM returned empty history, aborting roll",
                self.instance.id
            );
            return Ok(Some(crate::policy::messages::roll_llm_empty().to_string()));
        }

        // 5. Commit history via DB
        let old_kb = self.instance.memory.read_history().len() as u64 / 1024;
        self.instance
            .memory
            .commit_history(clean_history, oldest_block)?;
        let new_kb = self.instance.memory.read_history().len() as u64 / 1024;

        let result = crate::policy::messages::roll_result(old_kb, new_kb);
        info!("[ROLL-{}] {}", self.instance.id, result);

        Ok(Some(result))
    }

    // ─── Other ──────────────────────────────────────────────────

    /// Count unread user messages (delegates to ChatHistory).
    pub fn count_unread_messages(&self) -> i64 {
        self.instance
            .chat
            .lock()
            .unwrap()
            .count_unread_user_messages(&self.instance.id)
            .unwrap_or(0)
    }

    /// Check inference backoff status. Returns remaining duration if still backing off,
    /// or None if no backoff / backoff expired (auto-clears on expiry).
    pub fn backoff_remaining(&mut self) -> Option<std::time::Duration> {
        if let Some(deadline) = self.inference_backoff_until {
            if Instant::now() < deadline {
                let remaining = deadline.duration_since(Instant::now());
                info!(
                    "[BACKOFF-{}] Inference backoff active, {:.0}s remaining (failures={})",
                    self.instance.id,
                    remaining.as_secs_f64(),
                    self.inference_failures.value()
                );
                return Some(remaining);
            }
            self.inference_backoff_until = None;
            info!(
                "[BACKOFF-{}] Backoff expired, retrying inference (failures={})",
                self.instance.id,
                self.inference_failures.value()
            );
        }
        None
    }

    /// Set inference backoff after a failure.
    /// Returns (backoff_secs, rotation_info) where rotation_info is Some((from, to)) if channel rotated.
    fn set_inference_backoff(&mut self) -> (u64, Option<(String, String)>) {
        self.inference_failures.increment();
        let policy = &crate::policy::EngineConfig::get().engine;
        let backoff_secs = self.inference_failures.exponential_backoff(
            policy.inference_backoff_base_secs,
            policy.inference_backoff_max_exponent,
            policy.inference_backoff_cap_secs,
        );
        self.inference_backoff_until =
            Some(Instant::now() + std::time::Duration::from_secs(backoff_secs));
        let rotation = self.advance_channel();
        warn!(
            "[BACKOFF-{}] Inference failed ({} consecutive), backing off {}s",
            self.instance.id,
            self.inference_failures.value(),
            backoff_secs
        );
        (backoff_secs, rotation)
    }

    /// Unified anomaly notification: write to both agent memory and user-visible chat.
    /// This is the ONLY place that handles "right to know" for anomalies.
    /// All anomaly sources should either call this directly or bail!() to let the
    /// engine's unified error handler call it.
    pub fn notify_anomaly(&mut self, message: &str) {
        let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
        self.instance
            .chat
            .lock()
            .unwrap()
            .write_system_message(message, &timestamp)
            .ok();

        warn!("[ANOMALY-{}] {}", self.instance.id, message);
    }

    // ─── Beat ────────────────────────────────────────────────────

    /// One heartbeat cycle. The core cognitive loop.
    ///
    /// Flow: check messages → build prompt → LLM inference → stream actions → execute
    ///
    /// @TRACE: BEAT
    pub async fn beat(&mut self) -> Result<()> {
        let beat_start = Instant::now();
        info!("[BEAT-{}] Heartbeat start", self.instance.id);

        // 1. Check for unread messages
        let unread_count = self.count_unread_messages();
        info!(
            "[BEAT-{}] Unread messages: {}",
            self.instance.id, unread_count
        );

        // If no unread and last was idle, skip this beat
        if unread_count == 0 && self.last_was_idle {
            info!(
                "[BEAT-{}] No unread + last idle, skipping",
                self.instance.id
            );
            return Ok(());
        }

        // 1.5. Hard-control auto-read: unread → skip LLM, execute ReadMsg directly
        if unread_count > 0 {
            info!(
                "[BEAT-{}] Hard-control: {} unread, auto-reading",
                self.instance.id, unread_count
            );

            let mut tx = Transaction::new(&self.instance.id);

            let action_id = tx.record_doing(Action::ReadMsg);

            // DB: insert action_log (executing)
            let action_data_json = serde_json::to_string(&Action::ReadMsg).unwrap_or_default();
            self.instance.memory.insert_action_log(
                &action_id, Action::ReadMsg.type_name(), &action_data_json, &action_id[..14],
            ).ok();

            let result = execute_action(&Action::ReadMsg, self, &mut tx).await;
            match result {
                Ok(ref output) => {
                    tx.record_done(&action_id);
                    self.instance.memory.complete_action_log(&action_id, output).ok();
                }
                Err(ref e) => {
                    tx.record_done(&action_id);
                    let error_output = ActionOutput::Note { text: action_output::action_error(e) };
                    self.instance.memory.complete_action_log(&action_id, &error_output).ok();
                }
            }

            self.last_was_idle = false;
            info!(
                "[BEAT-{}] Hard-control auto-read complete ({:.1}s)",
                self.instance.id,
                beat_start.elapsed().as_secs_f64()
            );
            return Ok(());
        }

        // 1.7. Inference backoff (checked in instance_thread, not here)

        // 2. Create transaction
        let mut tx = Transaction::new(&self.instance.id);

        // 3. Build inference request
        // Fetch contacts from extension handler (silent degradation on failure)
        let (contacts_info, extra_skills) = match &self.extension {
            Some(ext) => {
                let contacts_list = ext.fetch_contacts(self.instance.id.clone())
                    .unwrap_or_default();
                let contacts = crate::persist::hooks::format_contacts_list(&contacts_list);
                (contacts, String::new())
            }
            None => (String::new(), String::new()),
        };

        let request = build_beat_request(self, self.host.as_deref(), contacts_info, extra_skills);

        // 5. Set up inference log
        let (log_path, log_timestamp) =
            crate::logging::create_infer_log_path(&self.log_dir, &self.instance.id);
        self.current_infer_log_path = Some(log_path.clone());

        // Mark born on first inference start (not just first idle)
        if !self.born {
            self.born = true;
            info!(
                "[BORN-{}] Instance born (first inference)",
                self.instance.id
            );
        }

        // Update engine status: inferring
        if let Some(ref signals) = self.signals {
            let log_path_str = log_path.display().to_string();
            let born = self.born;
            signals.update_status(|s| {
                s.inferring = true;
                s.born = born;
                s.log_path = Some(log_path_str.clone());
            });
        }

        // 6. LLM inference (via stream_infer with on_text callback for out.log)
        info!("[INFER-{}] Starting inference", self.instance.id);
        let channel = self.create_channel();
        let mut log_file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&log_path).ok();
        let on_text: Option<Box<dyn FnMut(&str) + Send + '_>> = Some(Box::new(move |chunk: &str| {
            if let Some(ref mut f) = log_file {
                use std::io::Write;
                let _ = f.write_all(chunk.as_bytes());
                let _ = f.flush();
            }
        }));
        let on_input: Option<Box<dyn FnOnce(&str) + Send>> = if self.env_config.infer_log_enabled {
            let in_log_path = log_path.parent().unwrap().join(
                crate::policy::log_formats::infer_in_filename(&log_timestamp)
            );
            Some(Box::new(move |prompt: &str| {
                crate::logging::write_infer_input_log(&in_log_path, prompt);
            }))
        } else {
            None
        };
        let preamble_holder: std::sync::Arc<std::sync::Mutex<Option<String>>> = std::sync::Arc::new(std::sync::Mutex::new(None));
        let preamble_clone = preamble_holder.clone();
        let on_preamble: Option<Box<dyn FnOnce(&str) + Send + '_>> = Some(Box::new(move |text: &str| {
            *preamble_clone.lock().unwrap() = Some(text.to_string());
        }));
        let mut stream_iter = stream_infer_with_on_text::<_, Action>(&channel, &request, on_text, on_input, on_preamble, None).await
            .map_err(|e| {
                let (backoff, rotation) = self.set_inference_backoff();
                anyhow::anyhow!(
                    "{}",
                    crate::policy::messages::inference_error(&e, backoff, rotation.as_ref())
                )
            })?;

        // 7. Stream actions: consume and execute (with sequence guard)
        self.last_was_idle = false;
        let mut guard = SequenceGuard::new(&self.instance.id);
        let mut inference_error: Option<String> = None;

        // Handle preamble: convert to thinking action if LLM output text before first action
        if let Some(preamble_text) = preamble_holder.lock().unwrap().take() {
            info!(
                "[PREAMBLE-{}] LLM output preamble before first action ({} chars), converting to thinking",
                self.instance.id, preamble_text.len()
            );
            let thinking_content = format!(
                "⚠️ 你的输出在首个action前包含了多余内容。如果需要记录思考，请使用thinking action。以下内容已自动纳入thinking：\n\n{}",
                preamble_text
            );
            let thinking_action = Action::Thinking { content: thinking_content.clone() };
            let preamble_action_id = action_output::generate_action_id();
            let preamble_json = serde_json::to_string(&thinking_action).unwrap_or_default();
            self.instance.memory.insert_action_log(
                &preamble_action_id, "thinking", &preamble_json, &preamble_action_id[..14],
            )?;
            self.instance.memory.complete_action_log(&preamble_action_id, &ActionOutput::Empty)?;
        }

        while let Some(result) = stream_iter.next().await {
            // Check for interrupt signal between actions
            if self.signals.as_ref().map_or(false, |s| s.check_interrupt()) {
                warn!(
                    "[INTERRUPT-{}] Interrupt signal detected, aborting inference",
                    self.instance.id
                );
                let interrupt_text = action_output::inference_interrupted().to_string();
                // DB: record interrupt as done note
                let note_id = action_output::generate_action_id();
                self.instance.memory.insert_done_note(&note_id, "interrupt", &interrupt_text).ok();
                break;
            }

            match result {
                Ok(action) => {
                    // Sequence guard check
                    match guard.check(&action) {
                        SequenceVerdict::Allow => {}
                        SequenceVerdict::Ignore => {
                            info!(
                                "[SEQUENCE-{}] Ignoring action: {}",
                                self.instance.id, action
                            );
                            continue;
                        }
                        SequenceVerdict::Reject(reason) => {
                            warn!("{}", reason);
                            let reject_text =
                                action_output::hallucination_defense_interrupted(&reason);
                            // DB: record reject as done note
                            let note_id = action_output::generate_action_id();
                            self.instance.memory.insert_done_note(&note_id, "reject", &reject_text).ok();
                            break;
                        }
                    }

                    let action_id = tx.record_doing(action.clone());

                    // DB: insert action_log (executing)
                    let action_data_json = serde_json::to_string(&action).unwrap_or_default();
                    self.instance.memory.insert_action_log(
                        &action_id, action.type_name(), &action_data_json, &action_id[..14],
                    ).ok();

                    // Cancel idle if a prior send_msg failed in this beat
                    if tx.cancel_idle && matches!(action, Action::Idle { .. }) {
                        info!(
                            "[BEAT-{}] Cancelling idle: send_msg failed earlier in this beat",
                            self.instance.id
                        );

                        // Record done
                        let cancelled_output = ActionOutput::Note { text: "idle cancelled: send_msg failed earlier in this beat".into() };
                        tx.record_done(&action_id);

                        // DB: complete action_log (done) for cancelled idle
                        self.instance.memory.complete_action_log(&action_id, &cancelled_output).ok();

                        continue;
                    }

                    // Execute action
                    let result = execute_action(&action, self, &mut tx).await;

                    // Check if this was an idle action
                    if let Action::Idle { timeout_secs } = &action {
                        self.last_was_idle = true;
                        self.idle_timeout_secs = *timeout_secs;
                        self.idle_since = Some(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        );
                        if !self.born {
                            self.born = true;
                            info!("[BORN-{}] Instance born (first idle)", self.instance.id);
                        }
                    }

                    // Record done + DB complete
                    match result {
                        Ok(ref output) => {
                            tx.record_done(&action_id);
                            self.instance.memory.complete_action_log(&action_id, output).ok();
                        }
                        Err(ref e) => {
                            tx.record_done(&action_id);
                            let error_output = ActionOutput::Note { text: action_output::action_error(e) };
                            self.instance.memory.complete_action_log(&action_id, &error_output).ok();
                        }
                    }



                    // Blocking action: end inference after execution
                    if action.is_blocking() {
                        info!(
                            "[BEAT-{}] Blocking action '{}' executed, ending inference",
                            self.instance.id, action
                        );
                        break;
                    }
                }
                Err(e) => {
                    inference_error = Some(e);
                    break;
                }
            }
        }

        // Handle inference result
        if let Some(e) = inference_error {
            let (backoff, rotation) = self.set_inference_backoff();
            anyhow::bail!(
                "{}",
                crate::policy::messages::inference_error(&e, backoff, rotation.as_ref())
            );
        } else {
            // Inference completed successfully (iterator exhausted or normal break)
            if self.inference_failures.value() > 0 {
                info!("[BACKOFF-{}] Inference succeeded, resetting backoff (was {} failures)",
                    self.instance.id, self.inference_failures.value());
            }
            self.inference_failures.reset();
            self.inference_backoff_until = None;
            info!("[INFER-{}] Inference complete", self.instance.id);
        }

        // 8. Cleanup
        self.current_infer_log_path = None;

        // Note: idle status is written by the main loop (engine/mod.rs) when it confirms
        // the instance is truly idle, not here at beat() end. This prevents the frontend
        // from seeing brief "idle" flickers between consecutive beats in a reasoning chain.

        // Capture: use only the last Summary action (multiple summaries can occur if agent
        // outputs more than one in a single inference; only the last one has meaningful content
        // since earlier ones clear current, making subsequent summaries see empty current).
        if let Some(last_summary) = tx.action_records.iter().rev().find_map(|r| {
            if let crate::inference::Action::Summary { content } = &r.action {
                Some(content.as_str())
            } else {
                None
            }
        }) {
            info!(
                "[CAPTURE-{}] Triggering capture from last summary ({} summaries total)",
                self.instance.id,
                tx.action_records.iter().filter(|r| matches!(&r.action, crate::inference::Action::Summary { .. })).count()
            );
            spawn_capture_task(self, last_summary, &self.log_dir);
        }

        info!(
            "[BEAT-{}] Heartbeat end ({:.1}s, {} actions)",
            self.instance.id,
            beat_start.elapsed().as_secs_f64(),
            tx.action_records.len()
        );

        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────

/// Data needed to execute history rolling in a background thread.
/// Generate a short random end-marker token for capture/compress truncation defense.
/// Format: `###END_{6-hex}###`, e.g. `###END_f22332###`
pub struct RollTask {
    pub memory: crate::persist::memory::Memory,
    pub oldest_block: String,
    pub request: crate::inference::compress::CompressRequest,
    pub instance_id: String,
    pub channel_configs: Arc<RwLock<Vec<LlmConfig>>>,
    pub channel_index: Arc<std::sync::atomic::AtomicU64>,
    pub log_dir: PathBuf,
    pub infer_log_enabled: bool,
}

pub struct CaptureTask {
    pub memory: crate::persist::memory::Memory,
    pub request: crate::inference::capture::CaptureRequest,
    pub instance_id: String,
    pub channel_configs: Arc<RwLock<Vec<LlmConfig>>>,
    pub channel_index: Arc<std::sync::atomic::AtomicU64>,
    pub log_dir: PathBuf,
    pub infer_log_enabled: bool,
}

/// Build an OpenAiChannel from configs and atomic index (shared logic for Alice and background tasks).
pub(crate) fn build_channel(
    configs: &RwLock<Vec<LlmConfig>>,
    index: &std::sync::atomic::AtomicU64,
) -> mad_hatter::llm::OpenAiChannel {
    let guard = configs.read().unwrap();
    let idx = index.load(std::sync::atomic::Ordering::Relaxed) as usize % guard.len().max(1);
    let config = &guard[idx];
    let llm_policy = &crate::policy::EngineConfig::get().llm;
    let (api_url, model_id) = llm_policy.resolve_model(&config.model);
    let mut channel = mad_hatter::llm::OpenAiChannel::new(&api_url, &model_id, &config.api_key);
    let max_tokens = config.max_tokens.unwrap_or(llm_policy.max_tokens);
    channel = channel.with_max_tokens(max_tokens);
    // TODO: temperature from config
    channel
}

pub async fn execute_capture_task(task: CaptureTask) -> anyhow::Result<String> {
    info!(
        "[CAPTURE-{}] Background: calling LLM for knowledge capture",
        task.instance_id
    );

    let old_kb = task.memory.read_knowledge().len() as u64 / 1024;

    let channel = build_channel(&task.channel_configs, &task.channel_index);
    let (capture_log_path, capture_ts) = {
        let infer_log_dir = task.log_dir.join(crate::policy::log_formats::INFER_DIR).join(&task.instance_id);
        std::fs::create_dir_all(&infer_log_dir).ok();
        let ts = crate::policy::log_formats::format_infer_timestamp(&chrono::Local::now());
        let path = infer_log_dir.join(crate::policy::log_formats::infer_capture_out_filename(&ts));
        (path, ts)
    };
    let mut capture_log_file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&capture_log_path).ok();
    let on_text: Option<Box<dyn FnMut(&str) + Send + '_>> = Some(Box::new(move |chunk: &str| {
        if let Some(ref mut f) = capture_log_file {
            use std::io::Write;
            let _ = f.write_all(chunk.as_bytes());
            let _ = f.flush();
        }
    }));
    let on_input: Option<Box<dyn FnOnce(&str) + Send>> = if task.infer_log_enabled {
        let in_log_path = capture_log_path.parent().unwrap().join(
            crate::policy::log_formats::infer_in_filename(&capture_ts)
        );
        Some(Box::new(move |prompt: &str| {
            crate::logging::write_infer_input_log(&in_log_path, prompt);
        }))
    } else {
        None
    };
    let results = infer_with_on_text::<_, crate::inference::capture::CaptureOutput>(&channel, &task.request, on_text, on_input, None, None).await
        .map_err(|e| anyhow::anyhow!(e))?;
    let output = results.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("LLM returned no capture output"))?;

    let clean_knowledge = output.knowledge.trim();
    if clean_knowledge.is_empty() {
        anyhow::bail!("LLM returned empty knowledge");
    }

    task.memory.write_knowledge(clean_knowledge)?;

    let new_kb = task.memory.read_knowledge().len() as u64 / 1024;

    let result = crate::policy::messages::capture_result(old_kb, new_kb);
    info!("[CAPTURE-{}] Background: {}", task.instance_id, result);
    Ok(result)
}

/// Prepare history rolling if needed (fast, non-blocking).
/// Returns Some(RollTask) if rolling is needed, None otherwise.

/// Execute history rolling task (designed for background thread).
/// Does LLM call + commit history via Memory (atomic write + delete block).
pub async fn execute_roll_task(task: RollTask) -> anyhow::Result<String> {
    info!(
        "[ROLL-{}] Background: calling LLM for history compression",
        task.instance_id
    );

    let channel = build_channel(&task.channel_configs, &task.channel_index);
    let (compress_log_path, roll_ts) = {
        let infer_log_dir = task.log_dir.join(crate::policy::log_formats::INFER_DIR).join(&task.instance_id);
        std::fs::create_dir_all(&infer_log_dir).ok();
        let ts = crate::policy::log_formats::format_infer_timestamp(&chrono::Local::now());
        let path = infer_log_dir.join(crate::policy::log_formats::infer_compress_out_filename(&ts));
        (path, ts)
    };
    let mut compress_log_file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&compress_log_path).ok();
    let on_text: Option<Box<dyn FnMut(&str) + Send + '_>> = Some(Box::new(move |chunk: &str| {
        if let Some(ref mut f) = compress_log_file {
            use std::io::Write;
            let _ = f.write_all(chunk.as_bytes());
            let _ = f.flush();
        }
    }));
    let on_input: Option<Box<dyn FnOnce(&str) + Send>> = if task.infer_log_enabled {
        let in_log_path = compress_log_path.parent().unwrap().join(
            crate::policy::log_formats::infer_in_filename(&roll_ts)
        );
        Some(Box::new(move |prompt: &str| {
            crate::logging::write_infer_input_log(&in_log_path, prompt);
        }))
    } else {
        None
    };
    let results = infer_with_on_text::<_, crate::inference::compress::CompressOutput>(&channel, &task.request, on_text, on_input, None, None).await
        .map_err(|e| anyhow::anyhow!(e))?;
    let output = results.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("LLM returned no compress output"))?;

    let clean_history = output.summary.trim();
    if clean_history.is_empty() {
        anyhow::bail!("LLM returned empty history");
    }

    // Commit via Memory (marker → write history → delete block → clear marker)
    let old_kb = task.memory.read_history().len() as u64 / 1024;
    task.memory
        .commit_history(clean_history, &task.oldest_block)?;
    let new_kb = task.memory.read_history().len() as u64 / 1024;

    let result = crate::policy::messages::roll_result(old_kb, new_kb);
    info!("[ROLL-{}] Background: {}", task.instance_id, result);

    Ok(result)
}

pub fn spawn_capture_task(alice: &Alice, summary_content: &str, log_dir: &std::path::Path) {
    let request = crate::prompt::build_capture_request(alice, summary_content);
    let task = CaptureTask {
        memory: alice.instance.memory.clone(),
        request,
        instance_id: alice.instance.id.clone(),
        channel_configs: alice.channel_configs.clone(),
        channel_index: alice.channel_index.clone(),
        log_dir: log_dir.to_path_buf(),
        infer_log_enabled: alice.env_config.infer_log_enabled,
    };

    let chat = alice.instance.chat.clone();
    tokio::spawn(async move {
        let notify_msg = match execute_capture_task(task).await {
            Ok(msg) => {
                info!("[CAPTURE] {}", msg);
                msg
            }
            Err(e) => {
                error!("[CAPTURE] Background capture failed: {}", e);
                crate::policy::messages::capture_failed(&e.to_string())
            }
        };
        let ts = crate::persist::chat::ChatHistory::now_timestamp();
        if let Ok(mut chat) = chat.lock() {
            let _ = chat.write_system_message(&notify_msg, &ts);
        }
    });
}



pub mod signal;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test Alice with proper directory structure.
    fn create_test_alice() -> (Alice, TempDir) {
        let tmp = TempDir::new().unwrap();

        // Create minimal settings.json for Instance::open
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1"}"#).unwrap();

        // Instance::open creates all subdirectories automatically
        let instance = crate::persist::instance::Instance::open(tmp.path()).unwrap();

        let log_dir = tmp.path().join("logs");
        let channel_configs = Arc::new(std::sync::RwLock::new(vec![Default::default()]));
        let channel_index = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let env_config = Arc::new(crate::policy::EnvConfig::from_env());
        let alice = Alice::new(instance, log_dir, channel_configs, channel_index, env_config, None, None).unwrap();
        (alice, tmp)
    }

    #[test]
    fn test_alice_creation() {
        let (alice, tmp) = create_test_alice();
        assert_eq!(
            alice.instance.id,
            tmp.path().file_name().unwrap().to_str().unwrap()
        );
        assert!(alice.current_infer_log_path.is_none());
        assert_eq!(
            alice.instance.memory.memory_dir(),
            tmp.path().join("memory")
        );
        assert_eq!(
            alice.instance.memory.sessions_dir(),
            tmp.path().join("memory").join("sessions")
        );
        assert_eq!(alice.instance.workspace, tmp.path().join("workspace"));
        // Verify directories were created
        assert!(alice.instance.memory.sessions_dir().exists());
    }



    #[test]
    fn test_transaction_creation() {
        let tx = Transaction::new("test");
        assert!(tx.action_records.is_empty());
    }

    #[test]
    fn test_transaction_action_recording() {
        let mut tx = Transaction::new("test");
        let action_id = tx.record_doing(
            Action::Idle { timeout_secs: None },
        );
        assert_eq!(tx.action_records.len(), 1);

        tx.record_done(&action_id);
        assert!(tx.action_records[0].duration.is_some());
    }

    #[test]
    fn test_generate_action_id_format() {
        let tx = Transaction::new("test");
        let id = tx.generate_action_id();
        assert!(id.len() >= 20);
        assert!(id.contains('_'));
    }



    #[test]
    fn test_count_unread_messages() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(alice.count_unread_messages(), 0);
    }

    // ─── Sequence Guard Tests ────────────────────────────────────

    #[test]
    fn test_sequence_guard_normal_allows_all() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(
            guard.check(&Action::Thinking {
                content: "hi".into()
            }),
            SequenceVerdict::Allow
        );
    }

    #[test]
    fn test_sequence_guard_normal_to_idle() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(
            guard.check(&Action::Idle { timeout_secs: None }),
            SequenceVerdict::Allow
        );
    }

    #[test]
    fn test_sequence_guard_idle_then_reject() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(
            guard.check(&Action::Idle { timeout_secs: None }),
            SequenceVerdict::Allow
        );
        match guard.check(&Action::ReadMsg) {
            SequenceVerdict::Reject(_) => {}
            other => panic!("Expected Reject, got {:?}", other),
        }
    }

    #[test]
    fn test_sequence_guard_idle_then_idle_ignored() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(
            guard.check(&Action::Idle { timeout_secs: None }),
            SequenceVerdict::Allow
        );
        assert_eq!(
            guard.check(&Action::Idle { timeout_secs: None }),
            SequenceVerdict::Ignore
        );
    }

    #[test]
    fn test_sequence_guard_blocking_then_blocking_allowed() {
        let mut guard = SequenceGuard::new("test");
        let script = Action::Script {
            content: "echo hi".into(),
        };
        assert_eq!(guard.check(&script), SequenceVerdict::Allow);
        assert_eq!(guard.check(&Action::ReadMsg), SequenceVerdict::Allow);
    }

    #[test]
    fn test_sequence_guard_blocking_then_idle_ignored() {
        let mut guard = SequenceGuard::new("test");
        let script = Action::Script {
            content: "echo hi".into(),
        };
        assert_eq!(guard.check(&script), SequenceVerdict::Allow);
        assert_eq!(
            guard.check(&Action::Idle { timeout_secs: None }),
            SequenceVerdict::Ignore
        );
    }

    #[test]
    fn test_sequence_guard_blocking_then_nonblocking_rejected() {
        let mut guard = SequenceGuard::new("test");
        let script = Action::Script {
            content: "echo hi".into(),
        };
        assert_eq!(guard.check(&script), SequenceVerdict::Allow);
        match guard.check(&Action::Thinking {
            content: "hmm".into(),
        }) {
            SequenceVerdict::Reject(_) => {}
            other => panic!("Expected Reject, got {:?}", other),
        }
    }

    #[test]
    fn test_sequence_guard_nonblocking_chain() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(
            guard.check(&Action::Thinking {
                content: "a".into()
            }),
            SequenceVerdict::Allow
        );
        assert_eq!(
            guard.check(&Action::SendMsg {
                recipient: "u".into(),
                content: "hi".into()
            }),
            SequenceVerdict::Allow
        );
        assert_eq!(
            guard.check(&Action::WriteFile {
                path: "f".into(),
                content: "c".into()
            }),
            SequenceVerdict::Allow
        );
    }

    #[test]
    fn test_sequence_guard_bab_pattern() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(
            guard.check(&Action::Thinking {
                content: "plan".into()
            }),
            SequenceVerdict::Allow
        );
        assert_eq!(
            guard.check(&Action::Script {
                content: "echo hi".into()
            }),
            SequenceVerdict::Allow
        );
        match guard.check(&Action::Thinking {
            content: "reflect".into(),
        }) {
            SequenceVerdict::Reject(_) => {}
            other => panic!("Expected Reject, got {:?}", other),
        }
    }

    #[test]
    fn test_action_is_blocking() {
        assert!(Action::Script { content: "".into() }.is_blocking());
        assert!(Action::ReadMsg.is_blocking());

        assert!(!Action::Idle { timeout_secs: None }.is_blocking());
        assert!(!Action::Thinking { content: "".into() }.is_blocking());
        assert!(!Action::SendMsg {
            recipient: "".into(),
            content: "".into()
        }
        .is_blocking());
        assert!(!Action::WriteFile {
            path: "".into(),
            content: "".into()
        }
        .is_blocking());
    }
}
