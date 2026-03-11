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
use crate::external::llm::LlmClient;
use mad_hatter::llm::stream_infer;
use crate::inference::Action;
use crate::persist::instance;
use crate::persist::hooks::HooksCaller;
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
    /// "doing" text written before execution
    pub doing_text: String,
    /// "done" text appended after execution (None if not yet executed)
    pub done_text: Option<String>,
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
    pub fn record_doing(&mut self, action: Action, doing_text: String) -> String {
        let action_id = action_output::generate_action_id();
        info!(
            "[ACTION-{}] START {} ({})",
            self.instance_id, action_id, action
        );
        self.action_records.push(ActionRecord {
            action_id: action_id.clone(),
            action,
            doing_text,
            done_text: None,
            started_at: Instant::now(),
            duration: None,
        });
        action_id
    }

    /// Record an action's "done" phase (after execution).
    ///
    /// @TRACE: ACTION
    pub fn record_done(&mut self, action_id: &str, done_text: String) {
        if let Some(record) = self
            .action_records
            .iter_mut()
            .find(|r| r.action_id == action_id)
        {
            record.done_text = Some(done_text);
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

    /// Build the full session text from all action records.
    /// This is appended to current.txt after each beat.
    pub fn build_session_text(&self) -> String {
        let mut text = String::new();
        for record in &self.action_records {
            text.push_str(&action_output::action_block_full(
                &record.action_id,
                &record.doing_text,
                record.done_text.as_deref(),
            ));
        }
        text
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
    /// LLM client
    pub(crate) llm_client: Arc<LlmClient>,
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
    /// Hooks caller for external extension points (contacts, relay, skills).
    pub hooks_caller: Option<Arc<HooksCaller>>,
}

impl Alice {
    /// Create a new Alice instance from an instance directory.
    ///
    /// @TRACE: INSTANCE
    pub fn new(
        instance: instance::Instance,
        log_dir: PathBuf,
        llm_client: Arc<LlmClient>,
        env_config: Arc<crate::policy::EnvConfig>,
        global_settings_store: Option<GlobalSettingsStore>,
        hooks_caller: Option<Arc<HooksCaller>>,
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
            llm_client,
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
            hooks_caller,
        })
    }

    // ─── Sessions access ────────────────────────────────────────

    // ─── History Rolling ────────────────────────────────────────

    pub fn prepare_roll(&mut self) -> anyhow::Result<Option<RollTask>> {
        let blocks = self.instance.memory.list_session_blocks()?;
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
                self.instance.memory.delete_session_block(oldest_block)?;
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
        let block_entries = self.instance.memory.read_session_entries(oldest_block)?;
        if block_entries.is_empty() {
            self.instance.memory.delete_session_block(oldest_block)?;
            return Ok(None);
        }

        let entries = crate::prompt::extract_session_block_data(&block_entries, self);
        let rendered_block = crate::inference::beat::format_session_entries(&entries, &self.instance.id);

        // Read current history
        let current_history = self.instance.memory.history.read()?;

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

        // Clone LLM configs for background thread
        Ok(Some(RollTask {
            memory: self.instance.memory.clone(),
            oldest_block: oldest_block.clone(),
            request,
            instance_id: self.instance.id.clone(),
            llm_client: self.llm_client.clone(),
        }))
    }

    /// Check if history rolling is needed and execute it.
    ///
    /// Triggered by engine main loop after beats. Checks if session block count
    /// exceeds `session_blocks_limit`. If so, takes the oldest block, compresses
    /// it into history via an independent LLM call, then deletes the block.
    ///
    /// @TRACE: MEMORY
    pub fn check_and_roll_history(&mut self) -> Result<Option<String>> {
        let blocks = self.instance.memory.list_session_blocks()?;
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
                self.instance.memory.delete_session_block(oldest_block)?;
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
        let entries = self.instance.memory.read_session_entries(oldest_block)?;
        if entries.is_empty() {
            // Empty block, just delete it
            self.instance.memory.delete_session_block(oldest_block)?;
            return Ok(Some(crate::policy::messages::roll_deleted_empty(
                oldest_block,
            )));
        }

        let session_entries: Vec<crate::inference::beat::SessionEntryData> =
            entries.iter().map(Into::into).collect();
        let rendered_block = crate::inference::beat::format_session_entries(&session_entries, &self.instance.id);

        // 2. Read current history (from memory handle)
        let current_history = self.instance.memory.history.read()?;

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

        // 4. Call LLM via infer() (synchronous, blocking)
        info!(
            "[ROLL-{}] Calling LLM for history compression",
            self.instance.id
        );
        let channel = self.llm_client.create_channel();
        let results = mad_hatter::llm::infer::<_, crate::inference::compress::CompressOutput>(&channel, &request)
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

        // 5. Commit history (marker lifecycle managed inside commit_history)
        let old_kb = std::fs::metadata(self.instance.memory.history.path())
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        self.instance
            .memory
            .commit_history(clean_history, oldest_block)?;
        let new_kb = std::fs::metadata(self.instance.memory.history.path())
            .map(|m| m.len() / 1024)
            .unwrap_or(0);

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
        let rotation = self.llm_client.advance_channel();
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
    pub fn beat(&mut self) -> Result<()> {
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

            let doing_text = action_output::build_doing_text(&Action::ReadMsg);
            let action_id = tx.record_doing(Action::ReadMsg, doing_text);

            // Phase 1 (Write-Ahead): write doing block before execution
            if let Some(record) = tx.action_records.last() {
                let doing_block = action_output::action_block_doing(
                    &record.action_id,
                    &record.doing_text,
                );
                self.instance.memory.append_current(&doing_block).ok();
            }

            let result = execute_action(&Action::ReadMsg, self, &mut tx);
            let done_text = match result {
                Ok(ref output) if output.is_empty() => String::new(),
                Ok(output) => action_output::build_done_text(&output),
                Err(e) => action_output::action_error(&e),
            };
            tx.record_done(&action_id, done_text);

            // Phase 2: append done block after execution
            if let Some(record) = tx.action_records.last() {
                let done_block = action_output::action_block_done(
                    &record.action_id,
                    record.done_text.as_deref(),
                );
                self.instance.memory.append_current(&done_block).ok();
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
        // Fetch contacts and extra skills from hooks (silent degradation on failure)
        let (contacts_info, extra_skills) = match &self.hooks_caller {
            Some(caller) => {
                let contacts = caller.format_contacts_for_prompt(&self.instance.id);
                let skills = caller.fetch_skills(&self.instance.id);
                (contacts, skills)
            }
            None => (String::new(), String::new()),
        };

        let request = build_beat_request(self, self.host.as_deref(), contacts_info, extra_skills);

        // 5. Set up inference log
        let (log_path, _log_timestamp) =
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

        // 6. LLM inference (via stream_infer)
        info!("[INFER-{}] Starting inference", self.instance.id);
        // TODO: inference logging — OpenAiChannel doesn't support log capture yet
        let channel = self.llm_client.create_channel();
        let stream_iter = stream_infer::<_, Action>(&channel, &request)
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

        for result in stream_iter {
            // Check for interrupt signal between actions
            if self.signals.as_ref().map_or(false, |s| s.check_interrupt()) {
                warn!(
                    "[INTERRUPT-{}] Interrupt signal detected, aborting inference",
                    self.instance.id
                );
                let interrupt_text = action_output::inference_interrupted().to_string();
                self.instance.memory.append_current(&interrupt_text).ok();
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
                            self.instance.memory.append_current(&reject_text).ok();
                            break;
                        }
                    }

                    // Build doing text
                    let doing_text = action_output::build_doing_text(&action);

                    let action_id = tx.record_doing(action.clone(), doing_text);

                    // Phase 1 (Write-Ahead): write doing block before execution
                    if let Some(record) = tx.action_records.last() {
                        let doing_block = action_output::action_block_doing(
                            &record.action_id,
                            &record.doing_text,
                        );
                        self.instance.memory.append_current(&doing_block).ok();
                    }

                    // Cancel idle if a prior send_msg failed in this beat
                    if tx.cancel_idle && matches!(action, Action::Idle { .. }) {
                        info!(
                            "[BEAT-{}] Cancelling idle: send_msg failed earlier in this beat",
                            self.instance.id
                        );

                        // Record done
                        let done_text = action_output::build_done_text(&action_output::idle_cancelled_after_send_failure());
                        tx.record_done(&action_id, done_text);

                        // Phase 2: append done block
                        if let Some(record) = tx.action_records.last() {
                            let done_block = action_output::action_block_done(
                                &record.action_id,
                                record.done_text.as_deref(),
                            );
                            self.instance.memory.append_current(&done_block).ok();
                        }
                        continue;
                    }

                    // Execute action
                    let result = execute_action(&action, self, &mut tx);

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

                    // Record done
                    let done_text = match result {
                        Ok(ref output) if output.is_empty() => String::new(),
                        Ok(output) => action_output::build_done_text(&output),
                        Err(e) => action_output::action_error(&e),
                    };
                    tx.record_done(&action_id, done_text);

                    // Phase 2: append done block after execution
                    // Summary clears current during execution, so skip done block
                    if !matches!(action, Action::Summary { .. }) {
                        if let Some(record) = tx.action_records.last() {
                            let done_block = action_output::action_block_done(
                                &record.action_id,
                                record.done_text.as_deref(),
                            );
                            self.instance.memory.append_current(&done_block).ok();
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
    pub llm_client: Arc<LlmClient>,
}

pub struct CaptureTask {
    pub memory: crate::persist::memory::Memory,
    pub request: crate::inference::capture::CaptureRequest,
    pub instance_id: String,
    pub llm_client: Arc<LlmClient>,
}

pub fn execute_capture_task(task: CaptureTask) -> anyhow::Result<String> {
    info!(
        "[CAPTURE-{}] Background: calling LLM for knowledge capture",
        task.instance_id
    );

    let old_kb = std::fs::metadata(task.memory.knowledge.path())
        .map(|m| m.len() / 1024)
        .unwrap_or(0);

    let channel = task.llm_client.create_channel();
    let results = mad_hatter::llm::infer::<_, crate::inference::capture::CaptureOutput>(&channel, &task.request)
        .map_err(|e| anyhow::anyhow!(e))?;
    let output = results.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("LLM returned no capture output"))?;

    let clean_knowledge = output.knowledge.trim();
    if clean_knowledge.is_empty() {
        anyhow::bail!("LLM returned empty knowledge");
    }

    task.memory.knowledge.write(clean_knowledge)?;

    let new_kb = std::fs::metadata(task.memory.knowledge.path())
        .map(|m| m.len() / 1024)
        .unwrap_or(0);

    let result = crate::policy::messages::capture_result(old_kb, new_kb);
    info!("[CAPTURE-{}] Background: {}", task.instance_id, result);
    Ok(result)
}

/// Prepare history rolling if needed (fast, non-blocking).
/// Returns Some(RollTask) if rolling is needed, None otherwise.

/// Execute history rolling task (designed for background thread).
/// Does LLM call + commit history via Memory (atomic write + delete block).
pub fn execute_roll_task(task: RollTask) -> anyhow::Result<String> {
    info!(
        "[ROLL-{}] Background: calling LLM for history compression",
        task.instance_id
    );

    let channel = task.llm_client.create_channel();
    let results = mad_hatter::llm::infer::<_, crate::inference::compress::CompressOutput>(&channel, &task.request)
        .map_err(|e| anyhow::anyhow!(e))?;
    let output = results.into_iter().next()
        .ok_or_else(|| anyhow::anyhow!("LLM returned no compress output"))?;

    let clean_history = output.summary.trim();
    if clean_history.is_empty() {
        anyhow::bail!("LLM returned empty history");
    }

    // Commit via Memory (marker → write history → delete block → clear marker)
    let old_kb = std::fs::metadata(task.memory.history.path())
        .map(|m| m.len() / 1024)
        .unwrap_or(0);
    task.memory
        .commit_history(clean_history, &task.oldest_block)?;
    let new_kb = std::fs::metadata(task.memory.history.path())
        .map(|m| m.len() / 1024)
        .unwrap_or(0);

    let result = crate::policy::messages::roll_result(old_kb, new_kb);
    info!("[ROLL-{}] Background: {}", task.instance_id, result);

    Ok(result)
}

pub fn spawn_capture_task(alice: &Alice, summary_content: &str, _log_dir: &std::path::Path) {
    let request = crate::prompt::build_capture_request(alice, summary_content);
    let task = CaptureTask {
        memory: alice.instance.memory.clone(),
        request,
        instance_id: alice.instance.id.clone(),
        llm_client: alice.llm_client.clone(),
    };

    let chat = alice.instance.chat.clone();
    std::thread::spawn(move || {
        let notify_msg = match execute_capture_task(task) {
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
        let llm_client = Arc::new(LlmClient::new(vec![Default::default()]));
        let env_config = Arc::new(crate::policy::EnvConfig::from_env());
        let alice = Alice::new(instance, log_dir, llm_client, env_config, None, None).unwrap();
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
    fn test_history_read_write() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(alice.instance.memory.history.read().unwrap(), "");
        alice
            .instance
            .memory
            .history
            .write("hello history")
            .unwrap();
        assert_eq!(
            alice.instance.memory.history.read().unwrap(),
            "hello history"
        );
    }

    #[test]
    fn test_current_read_write_append() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(alice.instance.memory.current.read().unwrap(), "");
        alice.instance.memory.write_current("line1").unwrap();
        assert_eq!(alice.instance.memory.current.read().unwrap(), "line1");
        alice.instance.memory.append_current("line2").unwrap();
        assert_eq!(
            alice.instance.memory.current.read().unwrap(),
            "line1\nline2"
        );
    }

    #[test]
    fn test_session_block_append_and_read() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(
            alice
                .instance
                .memory
                .read_session_block("20260223172500")
                .unwrap(),
            ""
        );
        alice
            .instance
            .memory
            .append_session_block(
                "20260223172500",
                "{\"first_msg\":\"a\",\"last_msg\":\"b\",\"summary\":\"test\"}\n",
            )
            .unwrap();
        let content = alice
            .instance
            .memory
            .read_session_block("20260223172500")
            .unwrap();
        assert!(content.contains("summary"));
    }

    #[test]
    fn test_session_block_size() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(
            alice.instance.memory.session_block_size("20260223172500"),
            0
        );
        alice
            .instance
            .memory
            .append_session_block("20260223172500", "some content\n")
            .unwrap();
        assert!(alice.instance.memory.session_block_size("20260223172500") > 0);
    }

    #[test]
    fn test_list_session_blocks() {
        let (alice, _tmp) = create_test_alice();
        alice
            .instance
            .memory
            .append_session_block("20260223172500", "line\n")
            .unwrap();
        alice
            .instance
            .memory
            .append_session_block("20260221150000", "line\n")
            .unwrap();
        alice
            .instance
            .memory
            .append_session_block("20260222100000", "line\n")
            .unwrap();
        let blocks = alice.instance.memory.list_session_blocks().unwrap();
        assert_eq!(
            blocks,
            vec!["20260221150000", "20260222100000", "20260223172500"]
        );
    }

    #[test]
    fn test_delete_session_block() {
        let (alice, _tmp) = create_test_alice();
        alice
            .instance
            .memory
            .append_session_block("20260223172500", "line\n")
            .unwrap();
        assert!(!alice
            .instance
            .memory
            .read_session_block("20260223172500")
            .unwrap()
            .is_empty());
        alice
            .instance
            .memory
            .delete_session_block("20260223172500")
            .unwrap();
        assert_eq!(
            alice
                .instance
                .memory
                .read_session_block("20260223172500")
                .unwrap(),
            ""
        );
    }

    #[test]
    fn test_session_files_in_sessions_dir() {
        let (alice, _tmp) = create_test_alice();
        alice.instance.memory.write_current("test content").unwrap();
        let current_file = alice.instance.memory.sessions_dir().join("current.txt");
        assert!(current_file.exists());
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
            "idle action doing\n".to_string(),
        );
        assert_eq!(tx.action_records.len(), 1);
        assert!(tx.action_records[0].done_text.is_none());

        tx.record_done(&action_id, "idle done\n".to_string());
        assert!(tx.action_records[0].done_text.is_some());
        assert!(tx.action_records[0].duration.is_some());
    }

    #[test]
    fn test_build_session_text() {
        let mut tx = Transaction::new("test");
        let id = tx.record_doing(
            Action::Idle { timeout_secs: None },
            "doing idle\n".to_string(),
        );
        tx.record_done(&id, "done idle\n".to_string());

        let text = tx.build_session_text();
        assert!(text.contains("行为编号"));
        assert!(text.contains("doing idle"));
        assert!(text.contains("done idle"));
    }

    #[test]
    fn test_generate_action_id_format() {
        let tx = Transaction::new("test");
        let id = tx.generate_action_id();
        assert!(id.len() >= 20);
        assert!(id.contains('_'));
    }

    #[test]
    fn test_build_doing_description() {
        use crate::policy::action_output;
        assert_eq!(
            action_output::build_doing_description(&Action::Idle { timeout_secs: None }),
            "idle"
        );
        assert_eq!(
            action_output::build_doing_description(&Action::Idle {
                timeout_secs: Some(30)
            }),
            "idle (30s)"
        );
        assert!(action_output::build_doing_description(&Action::ReadMsg).contains("收件箱"));

        let send = Action::SendMsg {
            recipient: "user1".to_string(),
            content: "hello".to_string(),
        };
        let desc = action_output::build_doing_description(&send);
        assert!(desc.contains("user1"));
        assert!(desc.contains("hello"));
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
