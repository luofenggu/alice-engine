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

use std::collections::VecDeque;
use std::sync::Arc;
use std::path::PathBuf;
use std::time::Instant;
use anyhow::Result;
use tracing::{info, warn};
use chrono::Local;

use crate::inference::Action;
use crate::util::Counter;
use crate::action::execute::execute_action;
use crate::persist::instance;
use crate::policy::action_output;
use crate::external::llm::{LlmClient, LlmConfig, InferenceStream, StreamItem, RecvResult};
use crate::prompt::build_beat_request;

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
                    SequenceVerdict::Reject(crate::policy::messages::sequence_reject_after_blocking(
                        &self.instance_id, &action.to_string()
                    ))
                }
            }
            SequenceState::AfterIdle => {
                if is_idle {
                    SequenceVerdict::Ignore
                } else {
                    SequenceVerdict::Reject(crate::policy::messages::sequence_reject_after_idle(
                        &self.instance_id, &action.to_string()
                    ))
                }
            }
        }
    }
}

impl Action {
    /// Whether this action is "blocking" (requires result feedback).
    pub fn is_blocking(&self) -> bool {
        matches!(self,
            Action::Script { .. } |
            Action::ReadMsg
        )
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
        info!("[ACTION-{}] START {} ({})", self.instance_id, action_id, action);
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
        if let Some(record) = self.action_records.iter_mut()
            .find(|r| r.action_id == action_id)
        {
            record.done_text = Some(done_text);
            record.duration = Some(record.started_at.elapsed());
            info!("[ACTION-{}] END {} ({:.1}s)",
                self.instance_id, action_id,
                record.duration.unwrap_or_default().as_secs_f64());
        } else {
            warn!("[ACTION-{}] record_done called for unknown action_id: {}",
                self.instance_id, action_id);
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
    /// User ID this instance belongs to (cached from settings).
    pub user_id: String,
    /// Log directory path
    pub log_dir: PathBuf,
    /// Environment configuration (shared, read-only after startup).
    pub env_config: Arc<crate::policy::EnvConfig>,
    /// Current inference log path (Some = inferring, None = idle)
    /// @TRACE: INFER
    pub current_infer_log_path: Option<PathBuf>,
    /// LLM client
    pub(crate) llm_client: LlmClient,
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
    /// Mock streams for scripted testing. Each beat consumes one Vec<StreamItem>.
    /// None = production mode (use LLM). Some = test mode (use mock).
    mock_streams: Option<VecDeque<Vec<StreamItem>>>,
    /// Mock sync responses for scripted testing. Each infer_sync call consumes one String.
    /// None = production mode (use LLM). Some = test mode (use mock).
    mock_sync_responses: Option<VecDeque<String>>,
    /// Total beat count for this instance (used with max_beats limit)
    pub beat_count: Counter<u32>,
    /// Maximum beats allowed (None = unlimited). From settings.json "max_beats".
    pub max_beats: Option<u32>,
    /// Whether this instance has completed its first idle (born = ready for user interaction).
    pub born: bool,
    /// Public host address for URL generation (e.g. "example.com:8081").
    /// Set by AliceEngine from ALICE_HOST env var.
    pub host: Option<String>,
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
    /// Stream poll interval in milliseconds.
    pub stream_poll_interval_ms: u64,
}

impl Alice {
    /// Create a new Alice instance from an instance directory.
    ///
    /// @TRACE: INSTANCE
    pub fn new(instance: instance::Instance, log_dir: PathBuf, llm_config: LlmConfig, env_config: Arc<crate::policy::EnvConfig>) -> Result<Self> {
        let user_id = instance.user_id().to_string();

        let llm_client = LlmClient::new(llm_config);

        info!("[INSTANCE-{}] Alice created for user {} at {}", instance.id, user_id, instance.instance_dir.display());
        Ok(Self {
            instance,
            user_id,
            log_dir,
            current_infer_log_path: None,
            llm_client,
            last_was_idle: false,
            idle_timeout_secs: None,
            idle_since: None,
            privileged: false,
            system_start_time: Local::now(),
            mock_streams: None,
            mock_sync_responses: None,
            beat_count: Counter::<u32>::new(),
            max_beats: None,
            born: false,
            host: None,
            instance_name: None,
            signals: None,
            inference_failures: Counter::<u32>::new(),
            inference_backoff_until: None,
            session_blocks_limit: crate::policy::EngineConfig::get().memory.session_blocks_limit,
            session_block_kb: crate::policy::EngineConfig::get().memory.session_block_kb,
            history_kb: crate::policy::EngineConfig::get().memory.history_kb,
            safety_max_consecutive_beats: crate::policy::EngineConfig::get().memory.safety_max_consecutive_beats,
            safety_cooldown_secs: crate::policy::EngineConfig::get().memory.safety_cooldown_secs,
            stream_poll_interval_ms: crate::policy::EngineConfig::get().streaming.poll_interval_ms,
            env_config,
        })
    }

    /// Set mock streams for scripted testing.
    pub fn set_mock_streams(&mut self, streams: Vec<Vec<StreamItem>>) {
        self.mock_streams = Some(VecDeque::from(streams));
    }

    /// Set mock sync responses for scripted testing (capture/compress).
    pub fn set_mock_sync_responses(&mut self, responses: Vec<String>) {
        self.mock_sync_responses = Some(VecDeque::from(responses));
    }



    /// Check if mock sync responses are available (for test mode detection).
    pub fn has_mock_sync_responses(&self) -> bool {
        self.mock_sync_responses.as_ref().map_or(false, |q| !q.is_empty())
    }

    /// Sync compress inference with mock support.
    /// In test mode, consumes from mock_sync_responses. In production, calls LLM.
    pub fn infer_compress_or_mock(
        &mut self,
        request: crate::inference::compress::CompressRequest,
    ) -> anyhow::Result<(String, Option<crate::external::llm::UsageInfo>)> {
        if let Some(ref mut mocks) = self.mock_sync_responses {
            if let Some(response) = mocks.pop_front() {
                info!("[INFER-SYNC-{}] Using mock response ({} chars)", self.instance.id, response.len());
                return Ok((response, None));
            }
        }
        self.llm_client.infer_compress(request, &self.instance.id)
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
                info!("[ROLL-{}] Idempotency: block {} was already compressed, deleting residual",
                    self.instance.id, oldest_block);
                self.instance.memory.delete_session_block(oldest_block)?;
                self.instance.memory.clear_last_rolled();
                return Ok(None);
            }
            // Stale marker, clean up
            self.instance.memory.clear_last_rolled();
        }

        info!("[ROLL-{}] History rolling triggered: {} blocks >= limit {}, preparing {}",
            self.instance.id, blocks.len(), self.session_blocks_limit, oldest_block);

        // Read and render the oldest block
        let block_entries = self.instance.memory.read_session_entries(oldest_block)?;
        if block_entries.is_empty() {
            self.instance.memory.delete_session_block(oldest_block)?;
            return Ok(None);
        }

        let entries = crate::prompt::extract_session_block_data(&block_entries, self);
        let rendered_block = crate::inference::beat::format_session_entries(&entries);

        // Read current history
        let current_history = self.instance.memory.history.read()?;

        // Build LLM prompt via CompressRequest
        let request = crate::inference::compress::CompressRequest {
            history_kb: self.history_kb as usize,
            session_content: rendered_block.clone(),
            current_history: current_history.clone(),
        };

        // Clone LLM config for background thread
        let llm_config = self.llm_client.config.clone();

        Ok(Some(RollTask {
            memory: self.instance.memory.clone(),
            oldest_block: oldest_block.clone(),
            request,
            instance_id: self.instance.id.clone(),
            llm_config,
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
                info!("[ROLL-{}] Idempotency: block {} was already compressed, deleting residual",
                    self.instance.id, oldest_block);
                self.instance.memory.delete_session_block(oldest_block)?;
                self.instance.memory.clear_last_rolled();
                return Ok(Some(crate::policy::messages::roll_deleted_residual(oldest_block)));
            }
            // Stale marker, clean up
            self.instance.memory.clear_last_rolled();
        }

        info!("[ROLL-{}] History rolling triggered: {} blocks >= limit {}, rolling {}",
            self.instance.id, blocks.len(), self.session_blocks_limit, oldest_block);

        // 1. Read and render the oldest block
        let entries = self.instance.memory.read_session_entries(oldest_block)?;
        if entries.is_empty() {
            // Empty block, just delete it
            self.instance.memory.delete_session_block(oldest_block)?;
            return Ok(Some(crate::policy::messages::roll_deleted_empty(oldest_block)));
        }

        let session_entries: Vec<crate::inference::beat::SessionEntryData> = entries.iter().map(Into::into).collect();
        let rendered_block = crate::inference::beat::format_session_entries(&session_entries);

        // 2. Read current history (from memory handle)
        let current_history = self.instance.memory.history.read()?;

        // 3. Build LLM request via CompressRequest
        let request = crate::inference::compress::CompressRequest {
            history_kb: self.history_kb as usize,
            session_content: rendered_block.clone(),
            current_history: current_history.clone(),
        };

        // 4. Call LLM (synchronous, blocking)
        info!("[ROLL-{}] Calling LLM for history compression", self.instance.id);
        let (new_history, usage) = self.infer_compress_or_mock(request)?;

        if new_history.trim().is_empty() {
            warn!("[ROLL-{}] LLM returned empty history, aborting roll", self.instance.id);
            return Ok(Some(crate::policy::messages::roll_llm_empty().to_string()));
        }

        // 5. Commit history (marker lifecycle managed inside commit_history)
        self.instance.memory.commit_history(new_history.trim(), oldest_block)?;

        let result = crate::policy::messages::roll_result(
            oldest_block,
            usage.as_ref().map(|u| (u.input_tokens, u.output_tokens)),
        );
        info!("[ROLL-{}] {}", self.instance.id, result);

        Ok(Some(result))
    }

    // ─── Other ──────────────────────────────────────────────────

    /// Count unread user messages (delegates to ChatHistory).
    pub fn count_unread_messages(&self) -> i64 {
        self.instance.chat.count_unread_user_messages().unwrap_or(0)
    }

    /// Set inference backoff after a failure. Returns the backoff duration in seconds.
    fn set_inference_backoff(&mut self) -> u64 {
        self.inference_failures.increment();
        let policy = &crate::policy::EngineConfig::get().engine;
        let backoff_secs = self.inference_failures.exponential_backoff(
            policy.inference_backoff_base_secs,
            policy.inference_backoff_max_exponent,
            policy.inference_backoff_cap_secs,
        );
        self.inference_backoff_until = Some(Instant::now() + std::time::Duration::from_secs(backoff_secs));
        warn!("[BACKOFF-{}] Inference failed ({} consecutive), backing off {}s",
            self.instance.id, self.inference_failures.value(), backoff_secs);
        backoff_secs
    }

    /// Unified anomaly notification: write to both agent memory and user-visible chat.
    /// This is the ONLY place that handles "right to know" for anomalies.
    /// All anomaly sources should either call this directly or bail!() to let the
    /// engine's unified error handler call it.
    pub fn notify_anomaly(&mut self, message: &str) {
        let marker = action_output::anomaly_notification(message);
        self.instance.memory.append_current(&marker).ok();

        let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
        self.instance.chat.write_agent_reply(
            &self.instance.id,
            message,
            &timestamp,
        ).ok();

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
        info!("[BEAT-{}] Unread messages: {}", self.instance.id, unread_count);

        // If no unread and last was idle, skip this beat
        if unread_count == 0 && self.last_was_idle {
            info!("[BEAT-{}] No unread + last idle, skipping", self.instance.id);
            return Ok(());
        }

        // 1.5. Hard-control auto-read: unread → skip LLM, execute ReadMsg directly
        if unread_count > 0 {
            info!("[BEAT-{}] Hard-control: {} unread, auto-reading", self.instance.id, unread_count);

            let mut tx = Transaction::new(&self.instance.id);

            let doing_text = action_output::build_doing_text(&Action::ReadMsg);
            let action_id = tx.record_doing(Action::ReadMsg, doing_text);

            let result = execute_action(&Action::ReadMsg, self, &mut tx);
            let done_text = match result {
                Ok(ref output) if output.is_empty() => String::new(),
                Ok(output) => action_output::build_done_text(&output),
                Err(e) => action_output::action_error(&e),
            };
            tx.record_done(&action_id, done_text);

            if let Some(record) = tx.action_records.last() {
                let action_text = action_output::action_block_full(
                    &record.action_id,
                    &record.doing_text,
                    record.done_text.as_deref(),
                );
                self.instance.memory.append_current(&action_text).ok();
            }

            self.last_was_idle = false;
            info!("[BEAT-{}] Hard-control auto-read complete ({:.1}s)",
                self.instance.id, beat_start.elapsed().as_secs_f64());
            return Ok(());
        }


        // 1.7. Inference backoff
        if let Some(deadline) = self.inference_backoff_until {
            if Instant::now() < deadline {
                let remaining = deadline.duration_since(Instant::now());
                info!("[BACKOFF-{}] Inference backoff active, {:.0}s remaining (failures={})",
                    self.instance.id, remaining.as_secs_f64(), self.inference_failures.value());
                return Ok(());
            }
            self.inference_backoff_until = None;
            info!("[BACKOFF-{}] Backoff expired, retrying inference (failures={})",
                self.instance.id, self.inference_failures.value());
        }

        // 2. Create transaction
        let mut tx = Transaction::new(&self.instance.id);

        // 3. Build inference request
        let request = build_beat_request(self, self.host.as_deref());

        // 5. Set up inference log
        let (log_path, log_timestamp) = crate::logging::create_infer_log_path(
            &self.log_dir, &self.instance.id,
        );
        self.current_infer_log_path = Some(log_path.clone());

        // Mark born on first inference start (not just first idle)
        if !self.born {
            self.born = true;
            info!("[BORN-{}] Instance born (first inference)", self.instance.id);
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

        // 6. LLM inference (or mock stream for testing)
        let stream: InferenceStream = if let Some(ref mut streams) = self.mock_streams {
            info!("[INFER-{}] Using mock stream", self.instance.id);
            InferenceStream::mock(streams.pop_front().unwrap_or_default())
        } else {
            info!("[INFER-{}] Starting inference", self.instance.id);
            self.llm_client.infer_beat(
                request,
                log_path.clone(),
                &self.log_dir,
                &log_timestamp,
                self.instance.id.clone(),
                self.env_config.infer_log_enabled,
            )
        };

        // 7. Stream actions: consume and execute (with sequence guard)
        self.last_was_idle = false;
        let mut guard = SequenceGuard::new(&self.instance.id);

        loop {
            // Check for interrupt signal before consuming next stream item
            if self.signals.as_ref().map_or(false, |s| s.check_interrupt()) {
                warn!("[INTERRUPT-{}] Interrupt signal detected, aborting inference", self.instance.id);
                let interrupt_text = action_output::inference_interrupted().to_string();
                self.instance.memory.append_current(&interrupt_text).ok();
                break;
            }

            match stream.next_or_timeout(std::time::Duration::from_millis(self.stream_poll_interval_ms)) {
                RecvResult::Timeout => continue,
                RecvResult::Disconnected => {
                    let backoff = self.set_inference_backoff();
                    anyhow::bail!("推理连接异常断开，将在{}秒后重试。", backoff);
                }
                RecvResult::Item(item) => match item {
                    StreamItem::Action(action) => {
                        // Sequence guard check
                        match guard.check(&action) {
                            SequenceVerdict::Allow => {}
                            SequenceVerdict::Ignore => {
                                info!("[SEQUENCE-{}] Ignoring action: {}", self.instance.id, action);
                                continue;
                            }
                            SequenceVerdict::Reject(reason) => {
                                warn!("{}", reason);
                                let reject_text = action_output::hallucination_defense_interrupted(&reason);
                                self.instance.memory.append_current(&reject_text).ok();
                                break;
                            }
                        }

                        // Build doing text
                        let doing_text = action_output::build_doing_text(&action);

                        let action_id = tx.record_doing(action.clone(), doing_text);

                        // Execute action
                        let result = execute_action(&action, self, &mut tx);

                        // Check if this was an idle action
                        if let Action::Idle { timeout_secs } = &action {
                            self.last_was_idle = true;
                            self.idle_timeout_secs = *timeout_secs;
                            self.idle_since = Some(std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs());
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

                        // Append this action's record to current session immediately
                        if let Some(record) = tx.action_records.last() {
                            let action_text = action_output::action_block_full(
                                &record.action_id,
                                &record.doing_text,
                                record.done_text.as_deref(),
                            );
                            self.instance.memory.append_current(&action_text).ok();
                        }

                        // Blocking action: end inference after execution
                        if action.is_blocking() {
                            info!("[BEAT-{}] Blocking action '{}' executed, ending inference", self.instance.id, action);
                            break;
                        }
                    }
                    StreamItem::Done(_actions, _usage) => {
                        // Reset inference backoff on success
                        if self.inference_failures.value() > 0 {
                            info!("[BACKOFF-{}] Inference succeeded, resetting backoff (was {} failures)",
                                self.instance.id, self.inference_failures.value());
                        }
                        self.inference_failures.reset();
                        self.inference_backoff_until = None;

                        info!("[INFER-{}] Inference complete", self.instance.id);
                        break;
                    }
                    StreamItem::Error(e) => {
                        let backoff = self.set_inference_backoff();
                        anyhow::bail!("推理过程出错: {}，将在{}秒后重试。", e, backoff);
                    }
                },

            }
        }

        // 8. Cleanup
        self.current_infer_log_path = None;

        // Note: idle status is written by the main loop (engine/mod.rs) when it confirms
        // the instance is truly idle, not here at beat() end. This prevents the frontend
        // from seeing brief "idle" flickers between consecutive beats in a reasoning chain.

        info!("[BEAT-{}] Heartbeat end ({:.1}s, {} actions)",
            self.instance.id,
            beat_start.elapsed().as_secs_f64(),
            tx.action_records.len());

        Ok(())
    }
}



// ─── Tests ───────────────────────────────────────────────────────

/// Data needed to execute history rolling in a background thread.
pub struct RollTask {
    pub memory: crate::persist::memory::Memory,
    pub oldest_block: String,
    pub request: crate::inference::compress::CompressRequest,
    pub instance_id: String,
    pub llm_config: crate::external::llm::LlmConfig,
}

/// Prepare history rolling if needed (fast, non-blocking).
/// Returns Some(RollTask) if rolling is needed, None otherwise.

/// Execute history rolling task (designed for background thread).
/// Does LLM call + commit history via Memory (atomic write + delete block).
pub fn execute_roll_task(task: RollTask) -> anyhow::Result<String> {
    // Create a temporary LLM client for this task
    let llm_client = crate::external::llm::LlmClient::new(task.llm_config);

    info!("[ROLL-{}] Background: calling LLM for history compression", task.instance_id);
    let (new_history, usage) = llm_client.infer_compress(
        task.request,
        &task.instance_id,
    )?;

    if new_history.trim().is_empty() {
        anyhow::bail!("LLM returned empty history");
    }

    // Commit via Memory (marker → write history → delete block → clear marker)
    task.memory.commit_history(new_history.trim(), &task.oldest_block)?;

    let result = crate::policy::messages::roll_result(
        &task.oldest_block,
        usage.as_ref().map(|u| (u.input_tokens, u.output_tokens)),
    );
    info!("[ROLL-{}] Background: {}", task.instance_id, result);

    Ok(result)
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
        let llm_config = LlmConfig { model: String::new(), api_key: String::new() };
        let env_config = Arc::new(crate::policy::EnvConfig::from_env());
        let alice = Alice::new(instance, log_dir, llm_config, env_config).unwrap();
        (alice, tmp)
    }

    #[test]
    fn test_alice_creation() {
        let (alice, tmp) = create_test_alice();
        assert_eq!(alice.instance.id, tmp.path().file_name().unwrap().to_str().unwrap());
        assert_eq!(alice.user_id, "user1");
        assert!(alice.current_infer_log_path.is_none());
        assert_eq!(alice.instance.memory.memory_dir(), tmp.path().join("memory"));
        assert_eq!(alice.instance.memory.sessions_dir(), tmp.path().join("memory").join("sessions"));
        assert_eq!(alice.instance.workspace, tmp.path().join("workspace"));
        // Verify directories were created
        assert!(alice.instance.memory.sessions_dir().exists());
    }

    #[test]
    fn test_history_read_write() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(alice.instance.memory.history.read().unwrap(), "");
        alice.instance.memory.history.write("hello history").unwrap();
        assert_eq!(alice.instance.memory.history.read().unwrap(), "hello history");
    }

    #[test]
    fn test_current_read_write_append() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(alice.instance.memory.current.read().unwrap(), "");
        alice.instance.memory.write_current("line1").unwrap();
        assert_eq!(alice.instance.memory.current.read().unwrap(), "line1");
        alice.instance.memory.append_current("line2").unwrap();
        assert_eq!(alice.instance.memory.current.read().unwrap(), "line1\nline2");
    }

    #[test]
    fn test_session_block_append_and_read() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(alice.instance.memory.read_session_block("20260223172500").unwrap(), "");
        alice.instance.memory.append_session_block("20260223172500", "{\"first_msg\":\"a\",\"last_msg\":\"b\",\"summary\":\"test\"}\n").unwrap();
        let content = alice.instance.memory.read_session_block("20260223172500").unwrap();
        assert!(content.contains("summary"));
    }

    #[test]
    fn test_session_block_size() {
        let (alice, _tmp) = create_test_alice();
        assert_eq!(alice.instance.memory.session_block_size("20260223172500"), 0);
        alice.instance.memory.append_session_block("20260223172500", "some content\n").unwrap();
        assert!(alice.instance.memory.session_block_size("20260223172500") > 0);
    }

    #[test]
    fn test_list_session_blocks() {
        let (alice, _tmp) = create_test_alice();
        alice.instance.memory.append_session_block("20260223172500", "line\n").unwrap();
        alice.instance.memory.append_session_block("20260221150000", "line\n").unwrap();
        alice.instance.memory.append_session_block("20260222100000", "line\n").unwrap();
        let blocks = alice.instance.memory.list_session_blocks().unwrap();
        assert_eq!(blocks, vec!["20260221150000", "20260222100000", "20260223172500"]);
    }

    #[test]
    fn test_delete_session_block() {
        let (alice, _tmp) = create_test_alice();
        alice.instance.memory.append_session_block("20260223172500", "line\n").unwrap();
        assert!(!alice.instance.memory.read_session_block("20260223172500").unwrap().is_empty());
        alice.instance.memory.delete_session_block("20260223172500").unwrap();
        assert_eq!(alice.instance.memory.read_session_block("20260223172500").unwrap(), "");
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
        let id = tx.record_doing(Action::Idle { timeout_secs: None }, "doing idle\n".to_string());
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
        assert_eq!(action_output::build_doing_description(&Action::Idle { timeout_secs: None }), "idle");
        assert_eq!(action_output::build_doing_description(&Action::Idle { timeout_secs: Some(30) }), "idle (30s)");
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
        assert_eq!(guard.check(&Action::Thinking { content: "hi".into() }), SequenceVerdict::Allow);
    }

    #[test]
    fn test_sequence_guard_normal_to_idle() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(guard.check(&Action::Idle { timeout_secs: None }), SequenceVerdict::Allow);
    }

    #[test]
    fn test_sequence_guard_idle_then_reject() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(guard.check(&Action::Idle { timeout_secs: None }), SequenceVerdict::Allow);
        match guard.check(&Action::ReadMsg) {
            SequenceVerdict::Reject(_) => {}
            other => panic!("Expected Reject, got {:?}", other),
        }
    }

    #[test]
    fn test_sequence_guard_idle_then_idle_ignored() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(guard.check(&Action::Idle { timeout_secs: None }), SequenceVerdict::Allow);
        assert_eq!(guard.check(&Action::Idle { timeout_secs: None }), SequenceVerdict::Ignore);
    }

    #[test]
    fn test_sequence_guard_blocking_then_blocking_allowed() {
        let mut guard = SequenceGuard::new("test");
        let script = Action::Script { content: "echo hi".into() };
        assert_eq!(guard.check(&script), SequenceVerdict::Allow);
        assert_eq!(guard.check(&Action::ReadMsg), SequenceVerdict::Allow);
    }

    #[test]
    fn test_sequence_guard_blocking_then_idle_ignored() {
        let mut guard = SequenceGuard::new("test");
        let script = Action::Script { content: "echo hi".into() };
        assert_eq!(guard.check(&script), SequenceVerdict::Allow);
        assert_eq!(guard.check(&Action::Idle { timeout_secs: None }), SequenceVerdict::Ignore);
    }

    #[test]
    fn test_sequence_guard_blocking_then_nonblocking_rejected() {
        let mut guard = SequenceGuard::new("test");
        let script = Action::Script { content: "echo hi".into() };
        assert_eq!(guard.check(&script), SequenceVerdict::Allow);
        match guard.check(&Action::Thinking { content: "hmm".into() }) {
            SequenceVerdict::Reject(_) => {}
            other => panic!("Expected Reject, got {:?}", other),
        }
    }

    #[test]
    fn test_sequence_guard_nonblocking_chain() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(guard.check(&Action::Thinking { content: "a".into() }), SequenceVerdict::Allow);
        assert_eq!(guard.check(&Action::SendMsg { recipient: "u".into(), content: "hi".into() }), SequenceVerdict::Allow);
        assert_eq!(guard.check(&Action::WriteFile { path: "f".into(), content: "c".into() }), SequenceVerdict::Allow);
    }

    #[test]
    fn test_sequence_guard_bab_pattern() {
        let mut guard = SequenceGuard::new("test");
        assert_eq!(guard.check(&Action::Thinking { content: "plan".into() }), SequenceVerdict::Allow);
        assert_eq!(guard.check(&Action::Script { content: "echo hi".into() }), SequenceVerdict::Allow);
        match guard.check(&Action::Thinking { content: "reflect".into() }) {
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
        assert!(!Action::SendMsg { recipient: "".into(), content: "".into() }.is_blocking());
        assert!(!Action::WriteFile { path: "".into(), content: "".into() }.is_blocking());
    }
}
// ─── Scripted Tests (Episode 1: Hello World) ────────────────────

#[cfg(test)]
mod scripted_tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test Alice with privilege mode for scripted testing.
    fn create_scripted_alice() -> (Alice, TempDir) {
        let tmp = TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1"}"#).unwrap();
        let instance = crate::persist::instance::Instance::open(tmp.path()).unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let llm_config = LlmConfig { model: String::new(), api_key: String::new() };
        let env_config = Arc::new(crate::policy::EnvConfig::from_env());
        let mut alice = Alice::new(instance, log_dir, llm_config, env_config).unwrap();
        alice.privileged = true;
        (alice, tmp)
    }

    #[test]
    fn test_episode_1_hello_world() {
        // === Setup ===
        let (mut alice, _tmp) = create_scripted_alice();

        // Verify initial state
        assert_eq!(alice.count_unread_messages(), 0);
        assert!(!alice.last_was_idle);

        // === Act 1: User sends a message ===
        alice.instance.chat.write_user_message("user1", "你好，小白！", "20260301120000").unwrap();
        assert_eq!(alice.count_unread_messages(), 1);

        // === Act 2: First beat — auto-read (hard-control) ===
        alice.beat().unwrap();

        // After auto-read: message consumed, current updated
        assert_eq!(alice.count_unread_messages(), 0);
        let current = std::fs::read_to_string(alice.instance.memory.sessions_dir().join("current.txt")).unwrap();
        assert!(current.contains("你好，小白！"), "current should contain the user message after auto-read");
        assert!(!alice.last_was_idle, "auto-read should not set last_was_idle");

        // === Act 3: Second beat — LLM inference (mock) ===
        alice.set_mock_streams(vec![
            vec![
                StreamItem::Action(Action::Thinking { content: "用户在打招呼，我来回复".into() }),
                StreamItem::Action(Action::SendMsg { recipient: "user1".into(), content: "你好！很高兴见到你！".into() }),
                StreamItem::Action(Action::Idle { timeout_secs: None }),
                StreamItem::Done(vec![
                    Action::Thinking { content: "用户在打招呼，我来回复".into() },
                    Action::SendMsg { recipient: "user1".into(), content: "你好！很高兴见到你！".into() },
                    Action::Idle { timeout_secs: None },
                ], None),
            ],
        ]);

        alice.beat().unwrap();

        // After inference: current has all action records
        let current = std::fs::read_to_string(alice.instance.memory.sessions_dir().join("current.txt")).unwrap();
        assert!(current.contains("用户在打招呼"), "current should contain thinking content");
        assert!(current.contains("你好！很高兴见到你！"), "current should contain send_msg content");
        assert!(current.contains("idle"), "current should contain idle record");

        // Agent reply should be in chat
        let replies = alice.instance.chat.read_unread_agent_replies().unwrap();
        assert_eq!(replies.len(), 1, "should have exactly one agent reply");
        assert!(replies[0].1.contains("你好！很高兴见到你！"), "reply content should match");

        // State: last_was_idle should be true
        assert!(alice.last_was_idle, "should be idle after Idle action");

        // === Act 4: Third beat — should skip (no unread + idle) ===
        alice.beat().unwrap();
        // Current should not have grown (beat was skipped)
        let current_after_skip = std::fs::read_to_string(alice.instance.memory.sessions_dir().join("current.txt")).unwrap();
        assert_eq!(current.len(), current_after_skip.len(), "current should not change on skipped beat");
    }

    #[test]
    fn test_episode_2_file_ops_and_script() {
        // === Setup ===
        let (mut alice, _tmp) = create_scripted_alice();

        // === Act 1: User sends a message ===
        alice.instance.chat.write_user_message("user1", "帮我写个文件", "20260301130000").unwrap();
        alice.beat().unwrap(); // auto-read

        // === Act 2: Agent writes file then runs script (script is blocking) ===
        alice.set_mock_streams(vec![
            vec![
                StreamItem::Action(Action::Thinking { content: "用户要写文件，我来操作".into() }),
                StreamItem::Action(Action::WriteFile {
                    path: "hello.txt".into(),
                    content: "Hello World\n第二行".into(),
                }),
                StreamItem::Action(Action::Script { content: "cat hello.txt && echo done".into() }),
                // Script is blocking — beat ends here, Done won't be consumed
                StreamItem::Done(vec![], None),
            ],
        ]);

        alice.beat().unwrap();

        // Verify: file created in workspace
        let file_path = alice.instance.workspace.join("hello.txt");
        assert!(file_path.exists(), "hello.txt should exist in workspace");
        let file_content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(file_content, "Hello World\n第二行", "file content should match");

        // Verify: current contains script output
        let current_path = alice.instance.memory.sessions_dir().join("current.txt");
        let current = std::fs::read_to_string(&current_path).unwrap();
        assert!(current.contains("Hello World"), "current should contain script stdout");
        assert!(current.contains("done"), "current should contain 'done' from echo");
        assert!(current.contains("write file"), "current should record write_file action");

        // Script is blocking, so last_was_idle should be false
        assert!(!alice.last_was_idle, "blocking script should not set idle");

        // === Act 3: Next beat — agent reports back ===
        alice.set_mock_streams(vec![
            vec![
                StreamItem::Action(Action::SendMsg {
                    recipient: "user1".into(),
                    content: "文件写好了，内容已确认".into(),
                }),
                StreamItem::Action(Action::Idle { timeout_secs: Some(60) }),
                StreamItem::Done(vec![], None),
            ],
        ]);

        alice.beat().unwrap();

        // Verify: agent reply in chat
        let replies = alice.instance.chat.read_unread_agent_replies().unwrap();
        assert!(replies.iter().any(|r| r.1.contains("文件写好了")), "agent should have replied about file");

        // Verify: idle with timeout
        assert!(alice.last_was_idle);
        assert_eq!(alice.idle_timeout_secs, Some(60));
    }

    #[test]
    fn test_episode_3_memory_lifecycle() {
        // === Setup ===
        let (mut alice, tmp) = create_scripted_alice();

        // Lower thresholds: 1KB per block, 2 blocks trigger roll
        alice.session_block_kb = 1;
        alice.session_blocks_limit = 2;

        let sessions_dir = alice.instance.memory.sessions_dir().to_path_buf();
        let knowledge_path = tmp.path().join("memory").join("knowledge.md");
        let history_path = sessions_dir.join("history.txt");

        // Long summary text (~1100 chars) to ensure JSONL line > 1KB
        let long_summary = |phase: &str| -> String {
            format!(
                "Phase {p}: 用户要求开发Python计算器应用。agent分析了需求，创建了calc.py文件，\
                包含基本的四则运算功能。使用sys.argv接收命令行参数，支持加减乘除操作。\
                agent编写了完整的错误处理逻辑，包括参数数量检查、数值格式验证、除零保护。\
                测试验证了所有运算符的正确性。代码结构清晰，使用字典映射运算符到lambda函数。\
                用户对结果表示满意，确认功能符合预期。这个阶段的工作为后续功能扩展奠定了基础。\
                agent还检查了文件权限和编码格式，确保跨平台兼容性。整个开发过程顺利，\
                没有遇到重大技术障碍。agent在开发过程中保持了良好的代码规范，包括适当的注释、\
                清晰的变量命名和合理的函数划分。用户提出的所有需求都已完整实现并通过测试验证。\
                这次协作展示了agent高效的编码能力和对用户需求的准确理解。\
                后续计划包括添加更多数学运算支持和改进用户界面交互体验。\
                Phase {p} 的所有任务均已完成，等待用户的下一步指示。额外补充一些技术细节：\
                Python版本兼容性已验证（3.6+），f-string格式化输出清晰易读。",
                p = phase
            )
        };

        // ================================================================
        // Phase 1: "帮我写个Python计算器"
        // ================================================================

        // Beat 1: User message → auto-read
        alice.instance.chat.write_user_message("user1", "帮我写个Python计算器，支持加减乘", "20260301140000").unwrap();
        alice.beat().unwrap();
        assert_eq!(alice.count_unread_messages(), 0);

        // Beat 2: Thinking + WriteFile + Script (blocking)
        let calc_py = "import sys\nops = {'+': lambda a,b: a+b, '-': lambda a,b: a-b, '*': lambda a,b: a*b}\na, op, b = float(sys.argv[1]), sys.argv[2], float(sys.argv[3])\nprint(f\"{a} {op} {b} = {ops[op](a, b)}\")\n";
        alice.set_mock_streams(vec![vec![
            StreamItem::Action(Action::Thinking { content: "用户要写计算器，我来创建calc.py".into() }),
            StreamItem::Action(Action::WriteFile { path: "calc.py".into(), content: calc_py.into() }),
            StreamItem::Action(Action::Script { content: "python3 calc.py 2 + 3".into() }),
            StreamItem::Done(vec![], None),
        ]]);
        alice.beat().unwrap();

        // Verify: calc.py exists and script ran
        let calc_path = alice.instance.workspace.join("calc.py");
        assert!(calc_path.exists(), "calc.py should exist");
        let current = std::fs::read_to_string(sessions_dir.join("current.txt")).unwrap();
        assert!(current.contains("2.0 + 3.0 = 5.0"), "script should output 2+3=5");

        // Beat 3: SendMsg + Summary (with knowledge) + Idle
        // Summary executes mid-stream: reads current (which has all prior records), then clears it.
        // Idle executes after: its record goes into the freshly cleared current.
        let knowledge_v1 = "# Agent Knowledge\n\n## 项目\n- 正在开发Python计算器应用 calc.py\n- 支持加减乘运算\n";
        alice.set_mock_streams(vec![vec![
            StreamItem::Action(Action::SendMsg { recipient: "user1".into(), content: "计算器写好了！2+3=5 验证通过".into() }),
            StreamItem::Action(Action::Summary { content: long_summary("1"), knowledge: Some(knowledge_v1.into()) }),
            StreamItem::Action(Action::Idle { timeout_secs: None }),
            StreamItem::Done(vec![], None),
        ]]);
        alice.beat().unwrap();

        // Verify Phase 1: block created, knowledge written, current mostly cleared
        let blocks = alice.instance.memory.list_session_blocks().unwrap();
        if let Some(b) = blocks.last() {
            let size = alice.instance.memory.session_block_size(b);
        }
        assert_eq!(blocks.len(), 1, "Phase 1 should create 1 session block");
        let knowledge_content = std::fs::read_to_string(&knowledge_path).unwrap();
        assert!(knowledge_content.contains("Python计算器"), "knowledge should be written");
        let current = std::fs::read_to_string(sessions_dir.join("current.txt")).unwrap();
        assert!(!current.contains("帮我写个Python计算器"), "old messages should be cleared from current");
        assert!(alice.last_was_idle);

        // ================================================================
        // Phase 2: "加个除法和取模功能"
        // ================================================================

        // Beat 4: User message → auto-read (breaks idle-skip)
        alice.instance.chat.write_user_message("user1", "加个除法和取模功能", "20260301150000").unwrap();
        alice.beat().unwrap();

        // Beat 5: Thinking + ReplaceInFile + Script (blocking)
        alice.set_mock_streams(vec![vec![
            StreamItem::Action(Action::Thinking { content: "用户要加除法和取模，我来修改calc.py".into() }),
            StreamItem::Action(Action::ReplaceInFile {
                path: "calc.py".into(),
                blocks: vec![crate::inference::ReplaceBlock {
                    search: "'*': lambda a,b: a*b}".into(),
                    replace: "'*': lambda a,b: a*b, '/': lambda a,b: a/b, '%': lambda a,b: a%b}".into(),
                }],
            }),
            StreamItem::Action(Action::Script { content: "python3 calc.py 10 / 3".into() }),
            StreamItem::Done(vec![], None),
        ]]);
        alice.beat().unwrap();

        // Verify: calc.py updated and division works
        let calc_content = std::fs::read_to_string(&calc_path).unwrap();
        assert!(calc_content.contains("'/': lambda"), "calc.py should have division");
        let current = std::fs::read_to_string(sessions_dir.join("current.txt")).unwrap();
        assert!(current.contains("3.333"), "division result should appear in current");

        // Beat 6: SendMsg + Summary + Idle — block 1 full (>1KB), creates block 2
        alice.set_mock_streams(vec![vec![
            StreamItem::Action(Action::SendMsg { recipient: "user1".into(), content: "除法和取模已添加，10/3=3.333验证通过".into() }),
            StreamItem::Action(Action::Summary { content: long_summary("2"), knowledge: None }),
            StreamItem::Action(Action::Idle { timeout_secs: None }),
            StreamItem::Done(vec![], None),
        ]]);
        alice.beat().unwrap();

        let blocks = alice.instance.memory.list_session_blocks().unwrap();
        for b in &blocks {
            let size = alice.instance.memory.session_block_size(b);
        }
        assert_eq!(blocks.len(), 2, "Phase 2 should have 2 session blocks (block 1 full, block 2 created)");

        // ================================================================
        // Phase 3: Forget test + third summary
        // ================================================================

        // Beat 7: User message → auto-read
        alice.instance.chat.write_user_message("user1", "你刚才查了个大日志，可以forget一下释放空间，留下必要信息就行", "20260301160000").unwrap();
        alice.beat().unwrap();

        // Beat 8: Thinking + Script (large output, blocking)
        alice.set_mock_streams(vec![vec![
            StreamItem::Action(Action::Thinking { content: "让我先查看一下系统日志确认服务状态".into() }),
            StreamItem::Action(Action::Script {
                content: "for i in $(seq 1 50); do echo \"[LOG] $(date '+%Y-%m-%d %H:%M:%S') Service heartbeat check #$i: status=healthy, cpu=12%, mem=45%, connections=128\"; done".into()
            }),
            StreamItem::Done(vec![], None),
        ]]);
        alice.beat().unwrap();

        // Extract the Script action's action_id from current
        let current = std::fs::read_to_string(sessions_dir.join("current.txt")).unwrap();
        let current_before_forget_len = current.len();

        // Find action_id of the script action (the one whose block contains "[LOG]")
        let script_action_id = {
            let mut found = None;
            for (pos, _) in current.match_indices("行为编号[") {
                let rest = &current[pos + "行为编号[".len()..];
                if let Some(end) = rest.find(']') {
                    let id = &rest[..end];
                    let marker = format!("行为编号[{}]开始", id);
                    if let Some(start) = current.find(&marker) {
                        let block_end_marker = format!("行为编号[{}]结束", id);
                        let block_text = if let Some(end_pos) = current.find(&block_end_marker) {
                            &current[start..end_pos]
                        } else {
                            &current[start..]
                        };
                        if block_text.contains("[LOG]") {
                            found = Some(id.to_string());
                            break;
                        }
                    }
                }
            }
            found.expect("should find script action_id containing log output")
        };

        // Beat 9: Thinking + Forget + SendMsg + Idle
        alice.set_mock_streams(vec![vec![
            StreamItem::Action(Action::Thinking { content: "好的，我来forget那个大日志输出，只保留关键信息".into() }),
            StreamItem::Action(Action::Forget {
                target_action_id: script_action_id.clone(),
                summary: "查看了系统日志50条心跳记录，所有服务状态正常（healthy），CPU 12%，内存45%，连接数128".into(),
            }),
            StreamItem::Action(Action::SendMsg { recipient: "user1".into(), content: "已经forget了，日志显示服务一切正常".into() }),
            StreamItem::Action(Action::Idle { timeout_secs: None }),
            StreamItem::Done(vec![], None),
        ]]);
        alice.beat().unwrap();

        // Verify forget: current should be smaller, and contain [已提炼] marker
        let current = std::fs::read_to_string(sessions_dir.join("current.txt")).unwrap();
        assert!(current.len() < current_before_forget_len, "current should shrink after forget");
        assert!(current.contains("已提炼"), "current should contain forgotten marker");
        assert!(current.contains("心跳记录"), "forgotten summary should be present");

        // Beat 10: Summary + Idle — block 2 full, creates block 3
        // Need a user message first to break idle-skip
        alice.instance.chat.write_user_message("user1", "好的，继续吧", "20260301160100").unwrap();
        alice.beat().unwrap(); // auto-read

        alice.set_mock_streams(vec![vec![
            StreamItem::Action(Action::Summary { content: long_summary("3"), knowledge: None }),
            StreamItem::Action(Action::Idle { timeout_secs: None }),
            StreamItem::Done(vec![], None),
        ]]);
        alice.beat().unwrap();

        let blocks = alice.instance.memory.list_session_blocks().unwrap();
        assert_eq!(blocks.len(), 3, "Phase 3 should have 3 session blocks");

        // ================================================================
        // Phase 4: Roll — compress oldest block into history
        // ================================================================

        let oldest_block = blocks[0].clone();
        let mock_compressed_history = format!(
            "用户要求开发Python计算器应用。agent创建了calc.py，实现了加减乘除和取模运算。\
            所有功能经过测试验证正常工作。（压缩自session block {}）", oldest_block
        );
        alice.set_mock_sync_responses(vec![mock_compressed_history.clone()]);

        let roll_result = alice.check_and_roll_history().unwrap();
        assert!(roll_result.is_some(), "roll should have been triggered");

        // Verify roll results (privileged judge)
        let blocks_after = alice.instance.memory.list_session_blocks().unwrap();
        assert_eq!(blocks_after.len(), 2, "oldest block should be deleted after roll");
        assert!(!blocks_after.contains(&oldest_block), "oldest block should no longer exist");

        let history = std::fs::read_to_string(&history_path).unwrap();
        assert!(history.contains("Python计算器"), "history should contain compressed content");

        // .last_rolled marker should be cleaned up
        let last_rolled_path = sessions_dir.join(".last_rolled");
        assert!(!last_rolled_path.exists(), ".last_rolled marker should be cleared after successful roll");

        // Knowledge should still exist from Phase 1
        let knowledge = std::fs::read_to_string(&knowledge_path).unwrap();
        assert!(knowledge.contains("Python计算器"), "knowledge should persist through phases");

        // calc.py should still exist in workspace
        assert!(calc_path.exists(), "calc.py should still exist after roll");
    }
}
