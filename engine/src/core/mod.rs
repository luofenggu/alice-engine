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

use crate::action::Action;
use crate::action::execute::execute_action;
use crate::persist::instance;
use crate::llm::{LlmClient, LlmConfig, ChatMessage, InferenceStream, StreamItem, RecvResult};
use crate::prompt::build_prompts;

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
                    SequenceVerdict::Reject(format!(
                        "[SEQUENCE-{}] Non-blocking action '{}' after blocking action — aborting inference",
                        self.instance_id, action
                    ))
                }
            }
            SequenceState::AfterIdle => {
                if is_idle {
                    SequenceVerdict::Ignore
                } else {
                    SequenceVerdict::Reject(format!(
                        "[SEQUENCE-{}] Action '{}' after idle — zero tolerance, aborting inference",
                        self.instance_id, action
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

/// Agent instance configuration.
///
/// @TRACE: INSTANCE
#[derive(Debug, Clone)]
pub struct AliceConfig {
    /// LLM model identifier (e.g. "anthropic/claude-sonnet-4")
    pub model: String,
    /// API endpoint URL
    pub api_url: String,
    /// API key for authentication
    pub api_key: String,
    /// Maximum tokens for LLM response
    pub max_tokens: u32,
    /// Temperature for LLM sampling
    pub temperature: f64,
    /// Log directory path
    pub log_dir: PathBuf,
    /// Beat interval in seconds (sleep between beats when idle)
    pub beat_interval_secs: u64,
    /// Fixed action separator token. None = random per beat (default).
    pub action_separator: Option<String>,
}

impl Default for AliceConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            api_url: String::new(),
            api_key: String::new(),
            max_tokens: 16384,
            temperature: 0.5,
            log_dir: PathBuf::from("/root/alice-logs"),
            beat_interval_secs: 3,
            action_separator: None,
        }
    }
}

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
    /// Action separator token for this beat (6-char hex)
    pub separator_token: String,
    /// Full separator prefix: "###ACTION_{token}###-"
    pub separator_prefix: String,
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
    pub fn new(instance_id: &str, separator_token: &str) -> Self {
        let separator_prefix = format!("###ACTION_{}###-", separator_token);
        info!("[BEAT-{}] Transaction created, separator: {}", instance_id, separator_token);
        Self {
            separator_token: separator_token.to_string(),
            separator_prefix,
            action_records: Vec::new(),
            started_at: Instant::now(),
            instance_id: instance_id.to_string(),
        }
    }

    /// Generate a unique action ID: YYYYMMDDHHmmss_6hexchars
    pub fn generate_action_id(&self) -> String {
        let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();
        let hex: String = (0..6)
            .map(|_| format!("{:x}", rand::random::<u8>() % 16))
            .collect();
        format!("{}_{}", timestamp, hex)
    }

    /// Record an action's "doing" phase (before execution).
    ///
    /// @TRACE: ACTION
    pub fn record_doing(&mut self, action: Action, doing_text: String) -> String {
        let action_id = self.generate_action_id();
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
            text.push_str(&format!(
                "---------行为编号[{}]开始---------\n",
                record.action_id
            ));
            text.push_str(&record.doing_text);
            if let Some(done) = &record.done_text {
                text.push_str(done);
            }
            text.push_str(&format!(
                "\n---------行为编号[{}]结束---------\n",
                record.action_id
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
    /// Instance configuration
    pub config: AliceConfig,
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
    pub system_start_time: String,
    /// Mock streams for scripted testing. Each beat consumes one Vec<StreamItem>.
    /// None = production mode (use LLM). Some = test mode (use mock).
    mock_streams: Option<VecDeque<Vec<StreamItem>>>,
    /// Mock sync responses for scripted testing. Each infer_sync call consumes one String.
    /// None = production mode (use LLM). Some = test mode (use mock).
    mock_sync_responses: Option<VecDeque<String>>,
    /// Total beat count for this instance (used with max_beats limit)
    pub beat_count: u32,
    /// Maximum beats allowed (None = unlimited). From settings.json "max_beats".
    pub max_beats: Option<u32>,
    /// Fixed action separator token. None = random per beat.
    pub action_separator: Option<String>,
    /// Whether this instance has completed its first idle (born = ready for user interaction).
    pub born: bool,
    /// Public host address for URL generation (e.g. "example.com:8081").
    /// Set by AliceEngine from ALICE_HOST env var.
    pub host: Option<String>,
    /// Consecutive inference failure count (for exponential backoff).
    inference_failures: u32,
    /// Backoff deadline: skip inference until this instant.
    inference_backoff_until: Option<Instant>,
    /// Extra LLM configurations for failover (manual switch via API).
    pub extra_configs: Vec<crate::llm::LlmConfig>,
    /// Active config index: 0 = primary, 1+ = extra_configs[index-1].
    pub active_config_index: usize,
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
}

impl Alice {
    /// Create a new Alice instance from an instance directory.
    ///
    /// @TRACE: INSTANCE
    pub fn new(instance: instance::Instance, config: AliceConfig, env_config: Arc<crate::policy::EnvConfig>) -> Result<Self> {
        let user_id = instance.user_id().to_string();
        let action_separator = config.action_separator.clone();

        let llm_config = LlmConfig {
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
        };
        let llm_client = LlmClient::new(llm_config);

        info!("[INSTANCE-{}] Alice created for user {} at {}", instance.id, user_id, instance.instance_dir.display());
        Ok(Self {
            instance,
            user_id,
            config,
            current_infer_log_path: None,
            llm_client,
            last_was_idle: false,
            idle_timeout_secs: None,
            idle_since: None,
            privileged: false,
            system_start_time: Local::now().format("%Y%m%d%H%M%S").to_string(),
            mock_streams: None,
            mock_sync_responses: None,
            beat_count: 0,
            max_beats: None,
            action_separator,
            born: false,
            host: None,
            extra_configs: Vec::new(),
            active_config_index: 0,
            instance_name: None,
            signals: None,
            inference_failures: 0,
            inference_backoff_until: None,
            session_blocks_limit: 4,
            session_block_kb: 2,
            history_kb: 2,
            safety_max_consecutive_beats: 10,
            safety_cooldown_secs: 30,
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

    /// Switch to a different model configuration by index.
    /// 0 = primary, 1+ = extra_configs[index-1].
    pub fn switch_model(&mut self, index: usize) -> anyhow::Result<()> {
        if index == 0 {
            // Switch back to primary
            self.llm_client.config.api_url = self.config.api_url.clone();
            self.llm_client.config.api_key = self.config.api_key.clone();
            self.llm_client.config.model = self.config.model.clone();
            self.active_config_index = 0;
            info!("[MODEL-{}] Switched to primary: {}", self.instance.id, self.config.model);
        } else {
            let extra_index = index - 1;
            let extra = self.extra_configs.get(extra_index)
                .ok_or_else(|| anyhow::anyhow!("Invalid model index: {} (have {} extras)", index, self.extra_configs.len()))?;
            self.llm_client.config.api_url = extra.api_url.clone();
            self.llm_client.config.api_key = extra.api_key.clone();
            self.llm_client.config.model = extra.model.clone();
            self.active_config_index = index;
            info!("[MODEL-{}] Switched to extra[{}]: {}", self.instance.id, extra_index, extra.model);
        }
        Ok(())
    }

    /// Check if mock sync responses are available (for test mode detection).
    pub fn has_mock_sync_responses(&self) -> bool {
        self.mock_sync_responses.as_ref().map_or(false, |q| !q.is_empty())
    }

    /// Sync inference with mock support. Used by capture and compress.
    /// In test mode, consumes from mock_sync_responses. In production, calls LLM.
    pub fn infer_sync_or_mock(
        &mut self,
        messages: Vec<crate::llm::ChatMessage>,
        max_tokens: u32,
    ) -> anyhow::Result<(String, Option<crate::llm::UsageInfo>)> {
        if let Some(ref mut mocks) = self.mock_sync_responses {
            if let Some(response) = mocks.pop_front() {
                info!("[INFER-SYNC-{}] Using mock response ({} chars)", self.instance.id, response.len());
                return Ok((response, None));
            }
        }
        self.llm_client.infer_sync(messages, max_tokens, &self.instance.id)
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
        let last_rolled_path = self.instance.memory.last_rolled_path();
        if last_rolled_path.exists() {
            if let Ok(last_rolled) = std::fs::read_to_string(&last_rolled_path) {
                let last_rolled = last_rolled.trim();
                if last_rolled == oldest_block.as_str() {
                    info!("[ROLL-{}] Idempotency: block {} was already compressed, deleting residual",
                        self.instance.id, oldest_block);
                    self.instance.memory.delete_session_block(oldest_block)?;
                    let _ = std::fs::remove_file(&last_rolled_path);
                    return Ok(None);
                }
            }
            let _ = std::fs::remove_file(&last_rolled_path);
        }

        info!("[ROLL-{}] History rolling triggered: {} blocks >= limit {}, preparing {}",
            self.instance.id, blocks.len(), self.session_blocks_limit, oldest_block);

        // Read and render the oldest block
        let block_content = self.instance.memory.read_session_block(oldest_block)?;
        if block_content.trim().is_empty() {
            self.instance.memory.delete_session_block(oldest_block)?;
            return Ok(None);
        }

        let rendered_block = crate::prompt::render_session_block(&block_content, self);

        // Read current history
        let current_history = self.instance.memory.history.read()?;

        // Build LLM prompt
        let history_kb = self.history_kb;
        let system_msg = crate::safe_render(crate::prompt::HISTORY_COMPRESS_PROMPT, &[
            ("{{HISTORY_KB}}", &history_kb.to_string()),
        ]);

        let user_msg = if current_history.is_empty() {
            rendered_block.clone()
        } else {
            format!("{}\n\n{}", current_history, rendered_block)
        };

        let messages = vec![
            crate::llm::ChatMessage::system(&system_msg),
            crate::llm::ChatMessage::user(&user_msg),
        ];

        // Clone LLM config for background thread
        let llm_config = self.llm_client.config.clone();

        Ok(Some(RollTask {
            sessions_dir: self.instance.memory.sessions_dir().to_path_buf(),
            oldest_block: oldest_block.clone(),
            messages,
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
        let last_rolled_path = self.instance.memory.last_rolled_path();
        if last_rolled_path.exists() {
            if let Ok(last_rolled) = std::fs::read_to_string(&last_rolled_path) {
                let last_rolled = last_rolled.trim();
                if last_rolled == oldest_block {
                    info!("[ROLL-{}] Idempotency: block {} was already compressed, deleting residual",
                        self.instance.id, oldest_block);
                    self.instance.memory.delete_session_block(oldest_block)?;
                    let _ = std::fs::remove_file(&last_rolled_path);
                    return Ok(Some(format!("deleted residual block {} (already compressed)", oldest_block)));
                }
            }
            // Stale marker, clean up
            let _ = std::fs::remove_file(&last_rolled_path);
        }

        info!("[ROLL-{}] History rolling triggered: {} blocks >= limit {}, rolling {}",
            self.instance.id, blocks.len(), self.session_blocks_limit, oldest_block);

        // 1. Read and render the oldest block
        let block_content = self.instance.memory.read_session_block(oldest_block)?;
        if block_content.trim().is_empty() {
            // Empty block, just delete it
            self.instance.memory.delete_session_block(oldest_block)?;
            return Ok(Some(format!("deleted empty block {}", oldest_block)));
        }

        let rendered_block = crate::prompt::render_session_block(&block_content, self);

        // 2. Read current history (from memory handle)
        let current_history = self.instance.memory.history.read()?;

        // 3. Build LLM prompt for compression
        let history_kb = self.history_kb;
        let system_msg = crate::safe_render(crate::prompt::HISTORY_COMPRESS_PROMPT, &[
            ("{{HISTORY_KB}}", &history_kb.to_string()),
        ]);

        let user_msg = if current_history.is_empty() {
            rendered_block.clone()
        } else {
            format!("{}\n\n{}", current_history, rendered_block)
        };

        let messages = vec![
            crate::llm::ChatMessage::system(&system_msg),
            crate::llm::ChatMessage::user(&user_msg),
        ];

        // 4. Call LLM (synchronous, blocking)
        info!("[ROLL-{}] Calling LLM for history compression", self.instance.id);
        let (new_history, usage) = self.infer_sync_or_mock(
            messages,
            4096,
        )?;

        if new_history.trim().is_empty() {
            warn!("[ROLL-{}] LLM returned empty history, aborting roll", self.instance.id);
            return Ok(Some("LLM returned empty, roll aborted".to_string()));
        }

        // 5. Commit history: atomic write history + delete oldest block
        // Write idempotency marker before commit.
        let last_rolled_path = self.instance.memory.last_rolled_path();
        let _ = std::fs::write(&last_rolled_path, oldest_block.as_bytes());

        self.instance.memory.commit_history(new_history.trim(), oldest_block)?;

        // Clean up marker after successful commit
        let _ = std::fs::remove_file(&last_rolled_path);

        let usage_info = if let Some(u) = usage {
            format!(", tokens: {}+{}", u.input_tokens, u.output_tokens)
        } else {
            String::new()
        };

        let result = format!(
            "history rolled: block {} compressed into history.txt{}",
            oldest_block, usage_info
        );
        info!("[ROLL-{}] {}", self.instance.id, result);

        Ok(Some(result))
    }

    // ─── Other ──────────────────────────────────────────────────

    /// Count unread user messages (delegates to ChatHistory).
    pub fn count_unread_messages(&self) -> i64 {
        self.instance.chat.count_unread_user_messages().unwrap_or(0)
    }

    /// Set inference backoff after a failure.
    /// Exponential backoff: min(10 * 2^(n-1), 300) seconds.
    fn set_inference_backoff(&mut self) {
        self.inference_failures += 1;
        let backoff_secs = std::cmp::min(10u64 * (1u64 << (self.inference_failures - 1).min(5)), 300);
        self.inference_backoff_until = Some(Instant::now() + std::time::Duration::from_secs(backoff_secs));
        warn!("[BACKOFF-{}] Inference failed ({} consecutive), backing off {}s",
            self.instance.id, self.inference_failures, backoff_secs);
    }

    /// Unified anomaly notification: write to both agent memory and user-visible chat.
    /// This is the ONLY place that handles "right to know" for anomalies.
    /// All anomaly sources should either call this directly or bail!() to let the
    /// engine's unified error handler call it.
    pub fn notify_anomaly(&mut self, message: &str) {
        let marker = format!(
            "---------系统异常通知---------\n{}\n",
            message
        );
        self.instance.memory.append_current(&marker).ok();

        let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();
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

            let separator_token: String = self.action_separator.clone().unwrap_or_else(|| {
                (0..6).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect()
            });
            let mut tx = Transaction::new(&self.instance.id, &separator_token);

            let doing_text = format!(
                "{}\n---action executing, result pending---\n",
                build_doing_description(&Action::ReadMsg),
            );
            let action_id = tx.record_doing(Action::ReadMsg, doing_text);

            let result = execute_action(&Action::ReadMsg, self, &mut tx);
            let done_text = match result {
                Ok(ref output) if output.is_empty() => String::new(),
                Ok(output) => format!("\n{}", output),
                Err(e) => format!("\nERROR: {}\n", e),
            };
            tx.record_done(&action_id, done_text);

            if let Some(record) = tx.action_records.last() {
                let action_text = format!(
                    "---------行为编号[{}]开始---------\n{}{}\n---------行为编号[{}]结束---------\n",
                    record.action_id,
                    record.doing_text,
                    record.done_text.as_deref().unwrap_or(""),
                    record.action_id,
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
                    self.instance.id, remaining.as_secs_f64(), self.inference_failures);
                return Ok(());
            }
            self.inference_backoff_until = None;
            info!("[BACKOFF-{}] Backoff expired, retrying inference (failures={})",
                self.instance.id, self.inference_failures);
        }

        // 2. Get separator token
        let separator_token: String = self.action_separator.clone().unwrap_or_else(|| {
            (0..6)
                .map(|_| format!("{:x}", rand::random::<u8>() % 16))
                .collect()
        });

        // 3. Create transaction
        let mut tx = Transaction::new(&self.instance.id, &separator_token);

        // 4. Build prompts
        let (system_prompt, user_prompt, _snapshot) = build_prompts(
            self,
            &separator_token,
            self.host.as_deref(),
        );

        let messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
        ];

        // 5. Set up inference log
        let (log_path, log_timestamp) = crate::logging::create_infer_log_path(
            &self.config.log_dir, &self.instance.id,
        );
        self.current_infer_log_path = Some(log_path.clone());

        // Write input log (only if enabled via env config)
        if self.env_config.infer_log_enabled {
            crate::logging::write_infer_input_log(
                &self.config.log_dir, &self.instance.id, &log_timestamp,
                &self.config.model, &self.config.api_url,
                &system_prompt, &user_prompt,
            );
        }

        // Mark born on first inference start (not just first idle)
        if !self.born {
            self.born = true;
            info!("[BORN-{}] Instance born (first inference)", self.instance.id);
        }

        // Update engine status: inferring
        if let Some(ref signals) = self.signals {
            let log_path_str = log_path.display().to_string();
            let model_count = 1 + self.extra_configs.len();
            let active_model = self.active_config_index;
            let born = self.born;
            signals.update_status(|s| {
                s.inferring = true;
                s.born = born;
                s.log_path = Some(log_path_str.clone());
                s.active_model = active_model;
                s.model_count = model_count;
            });
        }

        // 6. LLM inference (or mock stream for testing)
        let stream: InferenceStream = if let Some(ref mut streams) = self.mock_streams {
            info!("[INFER-{}] Using mock stream", self.instance.id);
            InferenceStream::mock(streams.pop_front().unwrap_or_default())
        } else {
            info!("[INFER-{}] Starting inference", self.instance.id);
            self.llm_client.infer_async(
                messages,
                &separator_token,
                log_path.clone(),
                self.instance.id.clone(),
            )
        };

        // 7. Stream actions: consume and execute (with sequence guard)
        self.last_was_idle = false;
        let mut guard = SequenceGuard::new(&self.instance.id);

        loop {
            // Check for interrupt signal before consuming next stream item
            if self.signals.as_ref().map_or(false, |s| s.check_interrupt()) {
                warn!("[INTERRUPT-{}] Interrupt signal detected, aborting inference", self.instance.id);
                let interrupt_text = "---------推理被用户中断---------\n".to_string();
                self.instance.memory.append_current(&interrupt_text).ok();
                break;
            }

            match stream.next_or_timeout(std::time::Duration::from_millis(200)) {
                RecvResult::Timeout => continue,
                RecvResult::Disconnected => {
                    self.set_inference_backoff();
                    anyhow::bail!("推理连接异常断开，将在{}秒后重试。",
                        std::cmp::min(10u64 * (1u64 << (self.inference_failures - 1).min(5)), 300));
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
                                let reject_text = format!(
                                    "---------序列防御中断---------\n{}\n",
                                    reason
                                );
                                self.instance.memory.append_current(&reject_text).ok();
                                break;
                            }
                        }

                        // Build doing text
                        let doing_text = format!(
                            "{}\n---action executing, result pending---\n",
                            build_doing_description(&action),
                        );

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
                            Ok(output) => format!("\n{}", output),
                            Err(e) => format!("\nERROR: {}\n", e),
                        };
                        tx.record_done(&action_id, done_text);

                        // Append this action's record to current session immediately
                        if let Some(record) = tx.action_records.last() {
                            let action_text = format!(
                                "---------行为编号[{}]开始---------\n{}{}\n---------行为编号[{}]结束---------\n",
                                record.action_id,
                                record.doing_text,
                                record.done_text.as_deref().unwrap_or(""),
                                record.action_id,
                            );
                            self.instance.memory.append_current(&action_text).ok();
                        }

                        // Blocking action: end inference after execution
                        if action.is_blocking() {
                            info!("[BEAT-{}] Blocking action '{}' executed, ending inference", self.instance.id, action);
                            break;
                        }
                    }
                    StreamItem::Done(text, _usage) => {
                        // Reset inference backoff on success
                        if self.inference_failures > 0 {
                            info!("[BACKOFF-{}] Inference succeeded, resetting backoff (was {} failures)",
                                self.instance.id, self.inference_failures);
                        }
                        self.inference_failures = 0;
                        self.inference_backoff_until = None;

                        info!("[INFER-{}] Inference complete, {} chars output",
                            self.instance.id, text.len());


                        let _ = text;
                        break;
                    }
                    StreamItem::Error(e) => {
                        self.set_inference_backoff();
                        anyhow::bail!("推理过程出错: {}，将在{}秒后重试。", e,
                            std::cmp::min(10u64 * (1u64 << (self.inference_failures - 1).min(5)), 300));
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

/// Build a human-readable description of an action for the "doing" text.
pub fn build_doing_description(action: &Action) -> String {
    match action {
        Action::Idle { timeout_secs: None } => "idle".to_string(),
        Action::Idle { timeout_secs: Some(secs) } => format!("idle ({}s)", secs),
        Action::ReadMsg => "你打开了收件箱，开始阅读来信。".to_string(),
        Action::SendMsg { recipient, content } =>
            format!("you send a letter to [{}]: \n\n{}\n", recipient, content),
        Action::Thinking { content } =>
            format!("记录思考: {}", content),
        Action::Script { content } =>
            format!("execute script: \n{}", content),
        Action::WriteFile { path, content } => {
            #[cfg(feature = "remember")]
            {
                match crate::action::extract_remember_fragments(content) {
                    Some(fragments) => format!("write file [{}]\n[以下仅为REMEMBER标记的关键片段，非完整文件内容]\n{}", path, fragments),
                    None => format!("write file [{}]", path),
                }
            }
            #[cfg(not(feature = "remember"))]
            {
                let _ = content;
                format!("write file [{}]", path)
            }
        }
        Action::ReplaceInFile { path, .. } =>
            format!("replace in file [{}]", path),
        Action::Summary { .. } =>
            "summary (小结)".to_string(),

        Action::SetProfile { entries } => {
            let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
            format!("set_profile [{}]", keys.join(", "))
        }
        Action::CreateInstance { name, knowledge } =>
            format!("create_instance: {} ({} bytes knowledge)", name, knowledge.len()),
        Action::Forget { target_action_id, summary } =>
            format!("forget [{}]: {}", target_action_id, crate::safe_truncate(summary, 80)),
    }
}

// ─── Tests ───────────────────────────────────────────────────────

/// Data needed to execute history rolling in a background thread.
pub struct RollTask {
    pub sessions_dir: std::path::PathBuf,
    pub oldest_block: String,
    pub messages: Vec<crate::llm::ChatMessage>,
    pub instance_id: String,
    pub llm_config: crate::llm::LlmConfig,
}

/// Prepare history rolling if needed (fast, non-blocking).
/// Returns Some(RollTask) if rolling is needed, None otherwise.

/// Execute history rolling task (designed for background thread).
/// Does LLM call + atomic write history + delete block.
pub fn execute_roll_task(task: RollTask) -> anyhow::Result<String> {
    use anyhow::Context;

    // Create a temporary LLM client for this task
    let llm_client = crate::llm::LlmClient::new(task.llm_config);

    info!("[ROLL-{}] Background: calling LLM for history compression", task.instance_id);
    let (new_history, usage) = llm_client.infer_sync(
        task.messages,
        4096,
        &task.instance_id,
    )?;

    if new_history.trim().is_empty() {
        anyhow::bail!("LLM returned empty history");
    }

    // Atomic write: history.txt.tmp -> rename -> delete block
    let history_path = task.sessions_dir.join("history.txt");
    let tmp_path = task.sessions_dir.join("history.txt.tmp");

    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp_path)
            .with_context(|| "Failed to create history.txt.tmp")?;
        f.write_all(new_history.trim().as_bytes())
            .with_context(|| "Failed to write history.txt.tmp")?;
        f.sync_all()
            .with_context(|| "Failed to fsync history.txt.tmp")?;
    }
    std::fs::rename(&tmp_path, &history_path)
        .with_context(|| "Failed to rename history.txt.tmp")?;

    // Write idempotency marker
    let last_rolled_path = task.sessions_dir.join(".last_rolled");
    let _ = std::fs::write(&last_rolled_path, task.oldest_block.as_bytes());

    // Delete the block file
    let block_path = task.sessions_dir.join(&task.oldest_block);
    std::fs::remove_file(&block_path)
        .with_context(|| format!("Failed to delete block {}", task.oldest_block))?;

    // Clean up marker
    let _ = std::fs::remove_file(&last_rolled_path);

    let usage_info = if let Some(u) = usage {
        format!(", tokens: {}+{}", u.input_tokens, u.output_tokens)
    } else {
        String::new()
    };

    let result = format!(
        "history rolled: block {} compressed into history.txt{}",
        task.oldest_block, usage_info
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

        let config = AliceConfig {
            log_dir: tmp.path().join("logs"),
            ..Default::default()
        };
        let env_config = Arc::new(crate::policy::EnvConfig::from_env());
        let alice = Alice::new(instance, config, env_config).unwrap();
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
        let tx = Transaction::new("test", "abc123");
        assert_eq!(tx.separator_token, "abc123");
        assert_eq!(tx.separator_prefix, "###ACTION_abc123###-");
        assert!(tx.action_records.is_empty());
    }

    #[test]
    fn test_transaction_action_recording() {
        let mut tx = Transaction::new("test", "abc123");
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
        let mut tx = Transaction::new("test", "abc123");
        let id = tx.record_doing(Action::Idle { timeout_secs: None }, "doing idle\n".to_string());
        tx.record_done(&id, "done idle\n".to_string());

        let text = tx.build_session_text();
        assert!(text.contains("行为编号"));
        assert!(text.contains("doing idle"));
        assert!(text.contains("done idle"));
    }

    #[test]
    fn test_generate_action_id_format() {
        let tx = Transaction::new("test", "abc123");
        let id = tx.generate_action_id();
        assert!(id.len() >= 20);
        assert!(id.contains('_'));
    }

    #[test]
    fn test_build_doing_description() {
        assert_eq!(build_doing_description(&Action::Idle { timeout_secs: None }), "idle");
        assert_eq!(build_doing_description(&Action::Idle { timeout_secs: Some(30) }), "idle (30s)");
        assert!(build_doing_description(&Action::ReadMsg).contains("收件箱"));

        let send = Action::SendMsg {
            recipient: "user1".to_string(),
            content: "hello".to_string(),
        };
        let desc = build_doing_description(&send);
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