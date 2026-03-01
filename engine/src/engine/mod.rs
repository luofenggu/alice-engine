//! # Engine Module
//!
//! Multi-instance Alice management. Handles instance lifecycle,
//! heartbeat scheduling, and graceful restart.
//!
//! @TRACE: INSTANCE, RESTART, BEAT
//!
//! ## Architecture
//!
//! AliceEngine scans the instances directory, creates Alice instances,
//! and runs heartbeat loops for each. Supports graceful restart via
//! signal file detection.
//!
//! ## Directory Layout
//!
//! ```text
//! {instances_base}/
//!   ├── alice/              ← instance "alice"
//!   │   ├── settings.json   ← instance settings (root level)
//!   │   ├── memory/
//!   │   │   ├── sessions/   ← history.txt + daily JSONL + current.txt
//!   │   │   ├── knowledge/  ← legacy topic files (kept for cleanup compat)
//!   │   │   └── knowledge.md ← unified knowledge file (single file, always in prompt)
//!   │   └── workspace/      ← working directory + chat.db
//!   └── ...
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::thread::JoinHandle;
use anyhow::{Result, Context};
use tracing::{info, warn, error};
use chrono::Local;

use crate::core::{Alice, AliceConfig};

/// Graceful shutdown signal file path.
/// Written by engine.sh stop / self-deploy.sh to request graceful shutdown.
/// Engine checks this every 3s in main loop; instance threads check after each beat.
const SHUTDOWN_SIGNAL_FILE: &str = "/var/run/alice-engine-shutdown.signal";


/// Memory paths for backup (relative to memory_dir).
const MEMORY_BACKUP_PATHS: &[(&str, &str)] = &[
    ("sessions/history.txt", "history"),
    ("sessions/current.txt", "current"),
];

/// Settings file name in instance root directory.
const SETTINGS_FILE: &str = "settings.json";

// ─── Settings ────────────────────────────────────────────────────

/// Per-instance settings loaded from instance root settings.json.
#[derive(Debug)]
struct InstanceSettings {
    api_key: String,
    model: String,
    user_id: String,
    privileged: bool,
    max_beats: Option<u32>,
    action_separator: Option<String>,
    session_blocks_limit: Option<u32>,
    session_block_kb: Option<u32>,
    history_kb: Option<u32>,
    safety_max_consecutive_beats: Option<u32>,
    safety_cooldown_secs: Option<u64>,
    extra_models: Vec<(String, String)>,
    name: Option<String>,
}

impl InstanceSettings {
    /// Load settings from a JSON file.
    /// Default model when not specified in settings.json.
    const DEFAULT_MODEL: &str = "openrouter@anthropic/claude-opus-4.6";

    fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read settings: {}", path.display()))?;

        // api_key: settings.json > env ALICE_DEFAULT_API_KEY > error
        let api_key = extract_json_string(&content, "api_key")
            .or_else(|| std::env::var("ALICE_DEFAULT_API_KEY").ok())
            .ok_or_else(|| anyhow::anyhow!(
                "Missing api_key: set in settings.json or ALICE_DEFAULT_API_KEY env var"
            ))?;
        // model: settings.json > env ALICE_DEFAULT_MODEL > default
        let model = extract_json_string(&content, "model")
            .or_else(|| std::env::var("ALICE_DEFAULT_MODEL").ok())
            .unwrap_or_else(|| Self::DEFAULT_MODEL.to_string());
        // user_id: settings.json > env ALICE_USER_ID > "default"
        let user_id = extract_json_string(&content, "user_id")
            .or_else(|| std::env::var("ALICE_USER_ID").ok())
            .unwrap_or_else(|| "default".to_string());
        let privileged = extract_json_bool(&content, "privileged")
            .unwrap_or(false);
        let max_beats = extract_json_u32(&content, "max_beats");
        let action_separator = extract_json_string(&content, "action_separator");
        let session_blocks_limit = extract_json_u32(&content, "session_blocks_limit");
        let session_block_kb = extract_json_u32(&content, "session_block_kb");
        let history_kb = extract_json_u32(&content, "history_kb");
        let safety_max_consecutive_beats = extract_json_u32(&content, "safety_max_consecutive_beats");
        let safety_cooldown_secs = extract_json_u32(&content, "safety_cooldown_secs").map(|v| v as u64);

        // extra_models: array of {api_key, model} for failover
        let extra_models = serde_json::from_str::<serde_json::Value>(&content)
            .ok()
            .and_then(|v| v.get("extra_models")?.as_array().cloned())
            .map(|arr| {
                arr.iter().filter_map(|item| {
                    let api_key = item.get("api_key")?.as_str()?.to_string();
                    let model = item.get("model")?.as_str()?.to_string();
                    Some((api_key, model))
                }).collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let name = extract_json_string(&content, "name");

        Ok(Self { api_key, model, user_id, privileged, max_beats, action_separator, session_blocks_limit, session_block_kb, history_kb, safety_max_consecutive_beats, safety_cooldown_secs, extra_models, name })
    }

    /// Parse model string "provider@model_id" into (api_url, model_id).
    fn parse_model(&self) -> (String, String) {
        Self::parse_model_str(&self.model)
    }

    /// Parse a model string "provider@model_id" into (api_url, model_id).
    fn parse_model_str(model: &str) -> (String, String) {
        if let Some(pos) = model.find('@') {
            let provider = &model[..pos];
            let model_id = &model[pos + 1..];
            let api_url = match provider {
                "openrouter" => "https://openrouter.ai/api/v1/chat/completions".to_string(),
                "openai" => "https://api.openai.com/v1/chat/completions".to_string(),
                "zenmux" => "https://zenmux.ai/api/v1/chat/completions".to_string(),
                other => {
                    warn!("Unknown provider '{}', using as direct URL", other);
                    other.to_string()
                }
            };
            (api_url, model_id.to_string())
        } else {
            (
                "https://openrouter.ai/api/v1/chat/completions".to_string(),
                model.to_string(),
            )
        }
    }
}

/// Extract a string value from simple JSON (no nested objects).
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let key_pos = json.find(&pattern)?;
    let after_key = &json[key_pos + pattern.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();
    let after_quote = after_colon.strip_prefix('"')?;
    let mut end = 0;
    let bytes = after_quote.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'"' && (end == 0 || bytes[end - 1] != b'\\') {
            break;
        }
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    Some(after_quote[..end].to_string()) // safe: end from ASCII byte scan
}

/// Extract an unsigned integer value from simple JSON (e.g. `"max_beats": 10`).
fn extract_json_u32(json: &str, key: &str) -> Option<u32> {
    let pattern = format!("\"{}\"", key);
    let key_pos = json.find(&pattern)?;
    let after_key = &json[key_pos + pattern.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let value = after_colon.trim_start();
    // Parse digits until non-digit
    let end = value.find(|c: char| !c.is_ascii_digit()).unwrap_or(value.len());
    if end == 0 { return None; }
    value[..end].parse().ok()
}

/// Extract a boolean value from simple JSON.
fn extract_json_bool(json: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{}\"", key);
    let key_pos = json.find(&pattern)?;
    let after_key = &json[key_pos + pattern.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let value = after_colon.trim_start();
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

// ─── Free function: sandbox user management ──────────────────────

/// Ensure a Linux sandbox user exists for 紧箍咒 (privilege demotion).
///
/// - Checks if user exists via `id {user}`
/// - Creates user if missing via `useradd -r -s /bin/bash --home-dir {workspace} {user}`
/// - Sets workspace directory ownership via `chown -R {user}:{user} {workspace}`
///
/// @TRACE: SHELL
fn ensure_sandbox_user(user: &str, workspace: &Path) -> Result<()> {
    use std::process::Command;

    let workspace_str = workspace.to_string_lossy();

    // Check if user already exists
    let check = Command::new("id").arg(user).output()
        .context("Failed to run 'id' command")?;

    if !check.status.success() {
        // Create user with home set to workspace
        info!("[SANDBOX] Creating sandbox user: {} (home={})", user, workspace_str);
        let create = Command::new("useradd")
            .args(["-r", "-s", "/bin/bash", "--home-dir", &workspace_str, user])
            .output()
            .context("Failed to run 'useradd' command")?;

        if !create.status.success() {
            let stderr = String::from_utf8_lossy(&create.stderr);
            anyhow::bail!("Failed to create sandbox user '{}': {}", user, stderr.trim());
        }
        info!("[SANDBOX] Created sandbox user: {}", user);
    }

    // Ensure workspace ownership (user:user so group matches)
    let owner = format!("{}:{}", user, user);
    let chown = Command::new("chown")
        .args(["-R", &owner, &workspace_str])
        .output()
        .context("Failed to run 'chown' command")?;

    if !chown.status.success() {
        let stderr = String::from_utf8_lossy(&chown.stderr);
        warn!("[SANDBOX] chown failed for {}: {}", user, stderr.trim());
    }

    Ok(())
}

// ─── Free function: shutdown signal check ────────────────────────

/// Check for graceful shutdown signal file.
/// Returns true if signal detected (caller should initiate shutdown).
///
/// @TRACE: SHUTDOWN
fn check_shutdown_signal(pid_file: &Path) -> bool {
    let signal_path = Path::new(SHUTDOWN_SIGNAL_FILE);
    if !signal_path.exists() {
        return false;
    }

    info!("[SHUTDOWN] signal-detected, initiating graceful shutdown");

    // Remove signal file
    std::fs::remove_file(signal_path).ok();

    // Clean up PID file
    std::fs::remove_file(pid_file).ok();

    true
}

// ─── Free function: log cleanup ──────────────────────────────────

/// Clean up old inference logs (older than retention_days).
///
/// @TRACE: LOG-CLEANUP
fn cleanup_old_infer_logs(logs_dir: &Path, retention_days: u64) {
    let infer_dir = logs_dir.join("infer");
    if !infer_dir.exists() {
        return;
    }

    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(retention_days * 86400))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let mut cleaned_files = 0u64;
    let mut cleaned_bytes = 0u64;

    // Walk instance subdirectories
    if let Ok(instances) = std::fs::read_dir(&infer_dir) {
        for entry in instances.filter_map(|e| e.ok()) {
            if !entry.path().is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(entry.path()) {
                for file in files.filter_map(|f| f.ok()) {
                    let path = file.path();
                    if let Ok(metadata) = path.metadata() {
                        if let Ok(modified) = metadata.modified() {
                            if modified < cutoff {
                                let size = metadata.len();
                                if std::fs::remove_file(&path).is_ok() {
                                    cleaned_files += 1;
                                    cleaned_bytes += size;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if cleaned_files > 0 {
        info!(
            "[LOG-CLEANUP] Removed {} inference log files ({:.1}MB), retention={}d",
            cleaned_files,
            cleaned_bytes as f64 / 1_048_576.0,
            retention_days
        );
    } else {
        info!("[LOG-CLEANUP] No old inference logs to clean (retention={}d)", retention_days);
    }
}

/// Rotate engine.log if it exceeds max_size_mb.
///
/// @TRACE: LOG-CLEANUP
fn rotate_engine_log(logs_dir: &Path, max_size_mb: u64) {
    let log_file = logs_dir.join("engine.log");
    if !log_file.exists() {
        return;
    }

    if let Ok(metadata) = log_file.metadata() {
        let size_mb = metadata.len() / 1_048_576;
        if size_mb >= max_size_mb {
            let rotated = logs_dir.join("engine.log.1");
            // Remove old rotated file if exists
            std::fs::remove_file(&rotated).ok();
            // Rename current to .1
            if std::fs::rename(&log_file, &rotated).is_ok() {
                info!("[LOG-CLEANUP] Rotated engine.log ({}MB) -> engine.log.1", size_mb);
            }
        }
    }
}

// ─── AliceEngine ─────────────────────────────────────────────────

/// Multi-instance Alice engine.
///
/// Manages instance lifecycle with per-instance threads.
/// Each instance runs its own heartbeat loop independently.
///
/// @HUB for engine management.
/// @TRACE: INSTANCE, RESTART, BEAT
pub struct AliceEngine {
    /// Base directory containing all instances.
    instances_base: PathBuf,
    /// Log directory.
    logs_dir: PathBuf,
    /// PID file path (local mode: base_dir/alice-engine.pid, cloud: /var/run/alice-engine.pid).
    pid_file: PathBuf,
    /// Temporary buffer for instances during restore (drained to threads in run()).
    instances: Vec<(String, Alice)>,
}

impl AliceEngine {
    /// Create a new engine.
    pub fn new(instances_base: PathBuf, logs_dir: PathBuf) -> Self {
        let pid_file = std::env::var("ALICE_PID_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| instances_base.parent().unwrap_or(&instances_base).join("alice-engine.pid"));
        Self {
            instances_base,
            logs_dir,
            pid_file,
            instances: Vec::new(),
        }
    }

    /// Backup all instances' memory files before starting.
    ///
    /// @TRACE: INSTANCE
    fn backup_all_memory(&self) {
        let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();

        for entry in std::fs::read_dir(&self.instances_base).into_iter().flatten() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.path().is_dir() {
                continue;
            }

            let memory_dir = entry.path().join("memory");
            if !memory_dir.exists() {
                continue;
            }

            let snapshots_dir = memory_dir.join("snapshots");
            std::fs::create_dir_all(&snapshots_dir).ok();

            for (rel_path, label) in MEMORY_BACKUP_PATHS {
                let src = memory_dir.join(rel_path);
                if !src.exists() || std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0) == 0 {
                    continue;
                }

                let snapshot_name = format!("{}_boot_{}.snapshot", label, timestamp);
                let dest = snapshots_dir.join(snapshot_name);
                std::fs::copy(&src, &dest).ok();
            }

            // Also backup daily files
            let sessions_dir = memory_dir.join("sessions");
            if sessions_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let fname = entry.file_name().to_string_lossy().to_string();
                        if fname.ends_with(".jsonl") {
                            let snapshot_name = format!("{}_boot_{}.snapshot", fname.trim_end_matches(".jsonl"), timestamp);
                            let dest = snapshots_dir.join(snapshot_name);
                            std::fs::copy(&entry.path(), &dest).ok();
                        }
                    }
                }
            }

            info!("[INSTANCE] boot-backup name={} ts={}",
                entry.file_name().to_string_lossy(), timestamp);
        }
    }

    /// Clean up .tmp files left by atomic_write after crash.
    /// These are harmless but should be cleaned to avoid confusion.
    fn cleanup_tmp_files(&self) {
        let mut cleaned = 0;
        for entry in std::fs::read_dir(&self.instances_base).into_iter().flatten() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.path().is_dir() {
                continue;
            }

            // Clean memory/ directory
            let memory_dir = entry.path().join("memory");
            if memory_dir.exists() {
                cleaned += Self::cleanup_tmp_in_dir(&memory_dir);
                // Also check sessions/ subdirectory
                let sessions_dir = memory_dir.join("sessions");
                if sessions_dir.exists() {
                    cleaned += Self::cleanup_tmp_in_dir(&sessions_dir);
                }
                // Also check knowledge/ subdirectory
                let knowledge_dir = memory_dir.join("knowledge");
                if knowledge_dir.exists() {
                    cleaned += Self::cleanup_tmp_in_dir(&knowledge_dir);
                }
            }
        }
        if cleaned > 0 {
            info!("[STARTUP] Cleaned {} .tmp residual files from previous crash", cleaned);
        }
    }

    fn cleanup_tmp_in_dir(dir: &Path) -> usize {
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file() && path.extension().map_or(false, |ext| ext == "tmp") {
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!("[STARTUP] Failed to clean tmp file {:?}: {}", path, e);
                    } else {
                        info!("[STARTUP] Cleaned tmp file: {:?}", path);
                        count += 1;
                    }
                }
            }
        }
        count
    }

    /// Discover and restore instances from the instances directory.
    ///
    /// @TRACE: INSTANCE
    fn restore_instances(&mut self) -> Result<()> {
        let entries: Vec<_> = std::fs::read_dir(&self.instances_base)
            .with_context(|| format!("Failed to read instances dir: {}",
                self.instances_base.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .collect();

        for entry in entries {
            let instance_dir = entry.path();
            let settings_path = instance_dir.join(SETTINGS_FILE);

            if !settings_path.exists() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();

            match self.create_instance(&name, &instance_dir) {
                Ok(()) => {
                    info!("[INSTANCE] restored name={}", name);
                }
                Err(e) => {
                    error!("[INSTANCE] failed to restore {}: {}", name, e);
                }
            }
        }

        Ok(())
    }

    /// Create a single instance from its directory.
    ///
    /// For non-privileged instances, automatically creates a Linux sandbox user
    /// (`agent-{name}`) and sets workspace ownership (紧箍咒).
    fn create_instance(&mut self, name: &str, instance_dir: &Path) -> Result<()> {
        let settings_path = instance_dir.join(SETTINGS_FILE);
        let settings = InstanceSettings::load(&settings_path)?;
        let (api_url, model) = settings.parse_model();

        let user_id = &settings.user_id;

        let config = AliceConfig {
            model,
            api_url,
            api_key: settings.api_key,
            max_tokens: 16384,
            temperature: 0.5,
            log_dir: self.logs_dir.clone(),
            beat_interval_secs: 3,
            action_separator: settings.action_separator,
        };

        let mut alice = Alice::new(name, user_id, instance_dir.to_path_buf(), config)?;

        // Build extra model configs for failover
        let extra_configs: Vec<crate::llm::LlmConfig> = settings.extra_models.iter().map(|(key, model_str)| {
            let (url, model_id) = InstanceSettings::parse_model_str(model_str);
            crate::llm::LlmConfig {
                api_url: url,
                api_key: key.clone(),
                model: model_id,
                max_tokens: 16384,
                temperature: 0.5,
            }
        }).collect();
        alice.extra_configs = extra_configs;
        alice.instance_name = settings.name;

        alice.privileged = settings.privileged;
        if let Some(v) = settings.safety_max_consecutive_beats { alice.safety_max_consecutive_beats = v; }
        if let Some(v) = settings.safety_cooldown_secs { alice.safety_cooldown_secs = v; }
        alice.host = std::env::var("ALICE_HOST").ok().filter(|s| !s.is_empty());


        // Auto-create sandbox user (紧箍咒) for non-privileged instances
        if !settings.privileged {
            let sandbox_user = format!("agent-{}", name);
            if let Err(e) = ensure_sandbox_user(&sandbox_user, &alice.workspace) {
                warn!("[SANDBOX] Skipping sandbox setup for {}: {} (sandbox commands not available)", name, e);
            }
        }

        // Security isolation: set directory permissions (安全隔离策略)
        // instance_dir=711, memory=700, data=700, workspace=750, settings=600
        {
            use std::os::unix::fs::PermissionsExt;
            let set_perm = |path: &std::path::Path, mode: u32| {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).ok();
            };
            set_perm(instance_dir, 0o711);
            set_perm(&alice.memory_dir, 0o700);
            set_perm(&instance_dir.join("data"), 0o700);
            set_perm(&alice.workspace, 0o750);
            set_perm(&instance_dir.join(SETTINGS_FILE), 0o600);
        }

        // Session blocks limit and history KB from settings
        if let Some(limit) = settings.session_blocks_limit {
            alice.session_blocks_limit = limit;
        }
        if let Some(kb) = settings.session_block_kb {
            alice.session_block_kb = kb;
        }
        if let Some(kb) = settings.history_kb {
            alice.history_kb = kb;
        }

        // test- prefixed instances default to max 10 beats if not explicitly set
        alice.max_beats = settings.max_beats.or_else(|| {
            if name.starts_with("test-") { Some(10) } else { None }
        });

        // Insert welcome letter on first creation (empty chat.db)
        #[cfg(feature = "welcome-letter")]
        if alice.chat_history.get_last_message_time().unwrap_or(0) == 0 {
            let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
            alice.chat_history.write_user_message(
                "system",
                crate::prompt::WELCOME_LETTER,
                &timestamp,
                "chat",
            ).ok();
            info!("[INSTANCE] Welcome letter inserted for {}", name);
        }

        // Write initial memory (imprint learning) on first creation
        if alice.read_history().unwrap_or_default().is_empty() {
            alice.write_history(crate::prompt::INITIAL_HISTORY).ok();
            info!("[INSTANCE] Initial history written for {}", name);
        }

        self.instances.push((name.to_string(), alice));
        Ok(())
    }
    /// Run the engine main loop. Blocks until shutdown.
    ///
    /// Spawns an independent thread for each instance. Main thread handles:
    /// - Hot-scan: discover new instances and spawn threads
    /// - Cold-clean: detect removed instances (thread self-exits)
    /// - Restart signal: set shutdown flag, wait for threads to finish
    ///
    /// Each instance thread runs its own heartbeat loop with:
    /// - Settings file check (exit if missing = instance deleted)
    /// - Hot-reload of mutable settings (safety valve params, session params)
    /// - Safety valve, beat limit, idle polling
    ///
    /// @TRACE: BEAT, RESTART
    pub fn run(&mut self) -> Result<()> {
        info!("Alice Engine (Rust) starting...");
        info!("Instances dir: {}", self.instances_base.display());
        info!("Logs dir: {}", self.logs_dir.display());

        // 1. Backup all memory before start
        self.backup_all_memory();

        // 1.5. Clean up .tmp residual files from previous crash
        self.cleanup_tmp_files();

        // 1.6. Clean up old logs
        let retention_days = std::env::var("ALICE_INFER_LOG_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(7);
        cleanup_old_infer_logs(&self.logs_dir, retention_days);
        rotate_engine_log(&self.logs_dir, 50);       // rotate at 50MB

        // 2. Restore instances
        self.restore_instances()?;
        info!("Alice Engine started. {} instance(s) restored.", self.instances.len());

        if self.instances.is_empty() {
            warn!("No instances found. Engine will wait for hot-scan.");
        }

        // 3. Write PID file
        self.write_pid_file();

        // 4. Spawn independent thread for each instance
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut threads: HashMap<String, JoinHandle<()>> = HashMap::new();

        // Drain instances from self and spawn threads
        let instances: Vec<(String, Alice)> = self.instances.drain(..).collect();
        for (name, alice) in instances {
            let shutdown_clone = Arc::clone(&shutdown);
            let handle = std::thread::Builder::new()
                .name(format!("instance-{}", name))
                .spawn(move || {
                    Self::instance_thread(alice, shutdown_clone);
                })
                .with_context(|| format!("Failed to spawn thread for instance {}", name))?;
            info!("[THREAD] Spawned thread for instance: {}", name);
            threads.insert(name, handle);
        }

        // 5. Main loop: hot-scan, cold-clean, shutdown signal
        loop {
            std::thread::sleep(Duration::from_secs(3));

            // Check graceful shutdown signal
            if check_shutdown_signal(&self.pid_file) {
                info!("[SHUTDOWN] Signaling all instance threads to shut down...");
                shutdown.store(true, Ordering::Relaxed);
                for (name, handle) in threads.drain() {
                    info!("[SHUTDOWN] Waiting for instance thread: {}", name);
                    handle.join().ok();
                }
                return Ok(());
            }

            // Clean up finished threads (instance self-exited, e.g. settings deleted)
            threads.retain(|name, handle| {
                if handle.is_finished() {
                    info!("[THREAD] Instance thread exited: {}", name);
                    false
                } else {
                    true
                }
            });

            // Hot-scan: discover new instances
            if let Ok(entries) = std::fs::read_dir(&self.instances_base) {
                let new_dirs: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        !threads.contains_key(&name)
                            && e.path().join(SETTINGS_FILE).exists()
                    })
                    .collect();

                for entry in new_dirs {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let instance_dir = entry.path();
                    match self.create_instance(&name, &instance_dir) {
                        Ok(()) => {
                            // Pop the instance we just pushed to self.instances
                            if let Some((inst_name, alice)) = self.instances.pop() {
                                let shutdown_clone = Arc::clone(&shutdown);
                                let handle = std::thread::Builder::new()
                                    .name(format!("instance-{}", inst_name))
                                    .spawn(move || {
                                        Self::instance_thread(alice, shutdown_clone);
                                    });
                                match handle {
                                    Ok(h) => {
                                        info!("[HOT-SCAN] New instance discovered and thread spawned: {}", inst_name);
                                        threads.insert(inst_name, h);
                                    }
                                    Err(e) => {
                                        error!("[HOT-SCAN] Failed to spawn thread for {}: {}", inst_name, e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("[HOT-SCAN] Failed to create instance {}: {}", name, e);
                        }
                    }
                }
            }
        }
    }

    /// Check available disk space. Returns available MB.
    /// Returns None if check fails.
    fn check_disk_space_mb(path: &Path) -> Option<u64> {
        let output = std::process::Command::new("df")
            .arg("-BM")  // block size = 1M
            .arg("--output=avail")
            .arg(path)
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parse: skip header line, get first number
        stdout.lines()
            .nth(1)?
            .trim()
            .trim_end_matches('M')
            .parse::<u64>()
            .ok()
    }

    /// Independent heartbeat loop for a single instance.
    /// Runs in its own thread. Exits when:
    /// - shutdown signal is set (engine restart)
    /// - settings.json is missing (instance deleted)
    /// - beat limit reached
    fn instance_thread(mut alice: Alice, shutdown: Arc<AtomicBool>) {
        let instance_id = alice.instance_id.clone();
        let rolling_in_progress = Arc::new(AtomicBool::new(false));
        let instance_dir = alice.instance_dir.clone();
        let settings_path = instance_dir.join(SETTINGS_FILE);
        info!("[THREAD-{}] Instance thread started", instance_id);

        let mut consecutive_beats: u32 = 0;
        let mut idle_elapsed: u64 = 0;
        const ERROR_BACKOFF_SECS: u64 = 10;

        loop {
            // Check shutdown signal
            if shutdown.load(Ordering::Relaxed) {
                info!("[THREAD-{}] Shutdown signal received, exiting", instance_id);
                break;
            }

            // Check settings.json exists (instance deleted = file moved to .trash)
            if !settings_path.exists() {
                info!("[THREAD-{}] Settings file missing, instance likely deleted. Exiting.", instance_id);
                break;
            }

            // Hot-reload mutable settings from settings.json
            if let Ok(content) = std::fs::read_to_string(&settings_path) {
                if let Some(v) = extract_json_u32(&content, "safety_max_consecutive_beats") {
                    alice.safety_max_consecutive_beats = v;
                }
                if let Some(v) = extract_json_u32(&content, "safety_cooldown_secs") {
                    alice.safety_cooldown_secs = v as u64;
                }
                if let Some(v) = extract_json_u32(&content, "session_blocks_limit") {
                    alice.session_blocks_limit = v;
                }
                if let Some(v) = extract_json_u32(&content, "session_block_kb") {
                    alice.session_block_kb = v;
                }
                if let Some(v) = extract_json_u32(&content, "history_kb") {
                    alice.history_kb = v;
                }

                // Hot-reload instance name
                let new_name = extract_json_string(&content, "name");
                if new_name != alice.instance_name {
                    alice.instance_name = new_name;
                }

                // Hot-reload privileged
                if let Some(v) = extract_json_bool(&content, "privileged") {
                    if v != alice.privileged {
                        info!("[HOT-RELOAD-{}] Privileged changed: {} -> {}", instance_id, alice.privileged, v);
                        alice.privileged = v;
                    }
                }

                // Hot-reload model and api_key
                if let Some(new_model_raw) = extract_json_string(&content, "model") {
                    let (new_api_url, new_model_id) = InstanceSettings::parse_model_str(&new_model_raw);
                    if new_model_id != alice.config.model || new_api_url != alice.config.api_url {
                        info!("[HOT-RELOAD-{}] Model changed: {} -> {}", instance_id, alice.config.model, new_model_id);
                        alice.config.model = new_model_id;
                        alice.config.api_url = new_api_url.clone();
                        alice.llm_client.config.model = alice.config.model.clone();
                        alice.llm_client.config.api_url = new_api_url;
                    }
                }
                if let Some(new_api_key) = extract_json_string(&content, "api_key") {
                    if new_api_key != alice.config.api_key {
                        info!("[HOT-RELOAD-{}] API key changed", instance_id);
                        alice.config.api_key = new_api_key.clone();
                        alice.llm_client.config.api_key = new_api_key;
                    }
                }

                // Hot-reload extra_models
                let new_extra_configs: Vec<crate::llm::LlmConfig> = serde_json::from_str::<serde_json::Value>(&content)
                    .ok()
                    .and_then(|v| v.get("extra_models")?.as_array().cloned())
                    .map(|arr| {
                        arr.iter().filter_map(|item| {
                            let api_key = item.get("api_key")?.as_str()?.to_string();
                            let model_str = item.get("model")?.as_str()?.to_string();
                            let (api_url, model_id) = InstanceSettings::parse_model_str(&model_str);
                            Some(crate::llm::LlmConfig {
                                api_url,
                                api_key,
                                model: model_id,
                                max_tokens: 16384,
                                temperature: 0.5,
                            })
                        }).collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if new_extra_configs.len() != alice.extra_configs.len() {
                    info!("[HOT-RELOAD-{}] Extra models changed: {} -> {} entries",
                        instance_id, alice.extra_configs.len(), new_extra_configs.len());
                    // Reset to primary if active extra was removed
                    if alice.active_config_index > 0 && alice.active_config_index > new_extra_configs.len() {
                        let _ = alice.switch_model(0);
                    }
                    alice.extra_configs = new_extra_configs;
                } else {
                    // Check if any entry changed
                    let changed = new_extra_configs.iter().zip(alice.extra_configs.iter())
                        .any(|(a, b)| a.api_url != b.api_url || a.model != b.model || a.api_key != b.api_key);
                    if changed {
                        info!("[HOT-RELOAD-{}] Extra models content changed", instance_id);
                        if alice.active_config_index > 0 {
                            let _ = alice.switch_model(0);
                        }
                        alice.extra_configs = new_extra_configs;
                    }
                }
            }

            // Idle polling: if last beat was idle and no unread, write idle status and sleep
            if alice.last_was_idle && alice.count_unread_messages() == 0 {
                // Write idle status here (not in beat()) so observe never sees
                // a false "idle" between consecutive beats in a reasoning chain.
                let idle_timeout_str = match alice.idle_timeout_secs {
                    Some(t) => t.to_string(),
                    None => "null".to_string(),
                };
                let idle_since_str = match alice.idle_since {
                    Some(s) => s.to_string(),
                    None => "null".to_string(),
                };
                let model_count = 1 + alice.extra_configs.len();
                let status_json = format!(
                    r#"{{"status":"idle","instance":"{}","lastBeat":"{}","duration":0.0,"born":{},"idleTimeoutSecs":{},"idleSince":{},"activeModel":{},"modelCount":{}}}"#,
                    instance_id,
                    chrono::Local::now().format("%Y%m%d%H%M%S"),
                    alice.born,
                    idle_timeout_str,
                    idle_since_str,
                    alice.active_config_index,
                    model_count,
                );
                alice.chat_history.update_status(&status_json).ok();

                consecutive_beats = 0;
                std::thread::sleep(Duration::from_secs(alice.config.beat_interval_secs));

                // Re-check after sleep
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                // Check interrupt signal during idle (cancel timeout → infinite idle)
                let interrupt_file = alice.instance_dir.join("interrupt.signal");
                if interrupt_file.exists() {
                    std::fs::remove_file(&interrupt_file).ok();
                    info!("[INTERRUPT-{}] Interrupt during idle, cancelling timeout", instance_id);
                    alice.idle_timeout_secs = None;
                    alice.idle_since = None;
                    idle_elapsed = 0;
                    // Update status to reflect cancelled timeout
                    let status_json = format!(
                        r#"{{"status":"idle","instance":"{}","lastBeat":"{}","duration":0.0,"born":{}}}"#,
                        instance_id,
                        chrono::Local::now().format("%Y%m%d%H%M%S"),
                        alice.born,
                    );
                    alice.chat_history.update_status(&status_json).ok();
                    continue;
                }

                // Check switch-model signal (manual model switching from frontend)
                let switch_file = alice.instance_dir.join("switch-model.signal");
                if switch_file.exists() {
                    if let Ok(content) = std::fs::read_to_string(&switch_file) {
                        std::fs::remove_file(&switch_file).ok();
                        if let Ok(index) = content.trim().parse::<usize>() {
                            let _ = alice.switch_model(index);
                            info!("[HOT-RELOAD-{}] Model switched to index {} via signal", instance_id, index);
                        }
                    } else {
                        std::fs::remove_file(&switch_file).ok();
                    }
                }

                // Check idle timeout (timed idle wakeup)
                if let Some(timeout) = alice.idle_timeout_secs {
                    idle_elapsed += alice.config.beat_interval_secs;
                    if idle_elapsed >= timeout {
                        info!("[IDLE-TIMEOUT-{}] Idle timeout {}s reached (elapsed {}s), waking up",
                            instance_id, timeout, idle_elapsed);
                        alice.idle_timeout_secs = None;
                        alice.idle_since = None;
                        alice.last_was_idle = false;  // So beat() won't skip
                        idle_elapsed = 0;
                        // Fall through to beat (don't continue)
                    } else if alice.count_unread_messages() == 0 {
                        continue;
                    }
                    // else: has unread messages, fall through
                } else if alice.count_unread_messages() == 0 {
                    continue;
                }
            }

            // Safety valve check
            if consecutive_beats >= alice.safety_max_consecutive_beats {
                warn!("[SAFETY-{}] {} consecutive beats without idle — forcing cooldown ({}s)",
                    instance_id, consecutive_beats, alice.safety_cooldown_secs);
                alice.notify_anomaly(&format!(
                    "安全阀触发：连续{}次推理未进入idle状态，强制冷却{}秒。这可能意味着推理陷入了循环。",
                    consecutive_beats, alice.safety_cooldown_secs
                ));
                alice.last_was_idle = true;
                consecutive_beats = 0;
                std::thread::sleep(Duration::from_secs(alice.safety_cooldown_secs));
                continue;
            }

            // Max beats limit check
            if let Some(max) = alice.max_beats {
                if alice.beat_count >= max {
                    if !alice.last_was_idle {
                        info!("[LIMIT-{}] Beat limit reached ({}/{}), forcing idle",
                            instance_id, alice.beat_count, max);
                        alice.notify_anomaly(&format!(
                            "推理次数已达上限（{}/{}），实例已停止推理。",
                            alice.beat_count, max
                        ));
                        alice.last_was_idle = true;
                    }
                    // Sleep and check shutdown, but don't beat
                    std::thread::sleep(Duration::from_secs(alice.config.beat_interval_secs));
                    continue;
                }
            }

            // Reset idle elapsed counter when entering a beat
            idle_elapsed = 0;

            // Run beat
            let unread = alice.count_unread_messages();
            if unread > 0 || !alice.last_was_idle {
                info!("[BEAT-{}] wakeup unread={} idle={} consecutive={}",
                    instance_id, unread, alice.last_was_idle, consecutive_beats);
            }

            // Disk space check (every 10 beats to avoid overhead)
            if consecutive_beats % 10 == 0 {
                if let Some(avail_mb) = Self::check_disk_space_mb(&instance_dir) {
                    if avail_mb < 100 {
                        alice.notify_anomaly(&format!(
                            "磁盘空间不足：仅剩 {}MB 可用。请清理磁盘空间，否则可能导致数据损坏。",
                            avail_mb
                        ));
                    }
                }
            }

            match alice.beat() {
                Ok(()) => {
                    alice.beat_count += 1;
                    if alice.last_was_idle {
                        consecutive_beats = 0;
                    } else {
                        consecutive_beats += 1;
                    }

                    // Check shutdown after beat (respond quickly to graceful shutdown)
                    if shutdown.load(Ordering::Relaxed) {
                        info!("[THREAD-{}] Shutdown signal after beat, exiting", instance_id);
                        break;
                    }

                    // History rolling (async — non-blocking)
                    if !rolling_in_progress.load(Ordering::Relaxed) {
                        match alice.prepare_roll() {
                            Ok(Some(task)) => {
                                let rolling = rolling_in_progress.clone();
                                rolling.store(true, Ordering::Relaxed);
                                let iid = instance_id.clone();
                                std::thread::spawn(move || {
                                    match crate::core::execute_roll_task(task) {
                                        Ok(result) => info!("[HISTORY-ROLL-{}] Background: {}", iid, result),
                                        Err(e) => error!("[HISTORY-ROLL-{}] Background failed: {}", iid, e),
                                    }
                                    rolling.store(false, Ordering::Relaxed);
                                });
                            }
                            Ok(None) => {}
                            Err(e) => {
                                error!("[HISTORY-ROLL-{}] Prepare failed: {}", instance_id, e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("[BEAT-{}] Error: {} — backing off {}s",
                        instance_id, e, ERROR_BACKOFF_SECS);
                    alice.current_infer_log_path = None;
                    let status_json = format!(
                        r#"{{"status":"idle","instance":"{}","lastBeat":"{}","duration":0.0,"born":{}}}"#,
                        instance_id,
                        chrono::Local::now().format("%Y%m%d%H%M%S"),
                        alice.born,
                    );
                    alice.chat_history.update_status(&status_json).ok();
                    alice.notify_anomaly(&format!("{}", e));
                    alice.last_was_idle = true;
                    consecutive_beats = 0;
                    std::thread::sleep(Duration::from_secs(ERROR_BACKOFF_SECS));
                }
            }
        }

        info!("[THREAD-{}] Instance thread exited", instance_id);
    }


    /// Write PID file.
    fn write_pid_file(&self) {
        let pid = std::process::id();
        if let Err(e) = std::fs::write(&self.pid_file, pid.to_string()) {
            warn!("Failed to write PID file: {}", e);
        } else {
            info!("PID file written: {} (PID {})", self.pid_file.display(), pid);
        }
    }
}


// ─── Shared Instance Creation ────────────────────────────────────

/// Create a new instance directory with minimal settings.json.
/// Returns (instance_id, instance_dir_path).
/// Engine hot-scan will discover and initialize the instance.
///
/// Shared by web API and agent action.
pub fn create_instance_dir(
    instances_dir: &Path,
    user_id: &str,
    display_name: Option<&str>,
) -> Result<(String, PathBuf), String> {
    // Generate 6-char random hex name
    let id: String = (0..6).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect();
    let instance_dir = instances_dir.join(&id);

    if instance_dir.exists() {
        return Err("Instance name collision, please retry".to_string());
    }

    std::fs::create_dir_all(&instance_dir)
        .map_err(|e| format!("Failed to create dir: {}", e))?;

    // Random color from 10 presets
    const PRESET_COLORS: &[&str] = &[
        "#6c5ce7", "#00b894", "#e17055", "#0984e3", "#fdcb6e",
        "#e84393", "#00cec9", "#a29bfe", "#ff7675", "#55efc4",
    ];
    let color = PRESET_COLORS[rand::random::<usize>() % PRESET_COLORS.len()];

    // Build settings JSON
    let mut settings_map = serde_json::Map::new();
    settings_map.insert("user_id".to_string(), serde_json::Value::String(user_id.to_string()));
    settings_map.insert("color".to_string(), serde_json::Value::String(color.to_string()));
    if let Some(name) = display_name {
        settings_map.insert("name".to_string(), serde_json::Value::String(name.to_string()));
    }
    let settings = serde_json::to_string(&serde_json::Value::Object(settings_map))
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    let settings_path = instance_dir.join("settings.json");
    std::fs::write(&settings_path, &settings)
        .map_err(|e| format!("Failed to write settings: {}", e))?;

    Ok((id, instance_dir))
}

// ─── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_extract_json_string() {
        let json = r#"{"api_key":"sk-test-123","model":"openrouter@anthropic/claude-sonnet-4"}"#;
        assert_eq!(extract_json_string(json, "api_key").unwrap(), "sk-test-123");
        assert_eq!(
            extract_json_string(json, "model").unwrap(),
            "openrouter@anthropic/claude-sonnet-4"
        );
        assert!(extract_json_string(json, "nonexistent").is_none());
    }

    #[test]
    fn test_extract_json_string_with_spaces() {
        let json = r#"{ "api_key" : "sk-123" , "model" : "test" }"#;
        assert_eq!(extract_json_string(json, "api_key").unwrap(), "sk-123");
        assert_eq!(extract_json_string(json, "model").unwrap(), "test");
    }

    #[test]
    fn test_instance_settings_parse_model_openrouter() {
        let settings = InstanceSettings {
            api_key: "sk-test".to_string(),
            model: "openrouter@anthropic/claude-opus-4.6".to_string(),
            user_id: "test-user".to_string(),
            privileged: false,
            max_beats: None,
            action_separator: None,
            session_blocks_limit: None,
            session_block_kb: None,
            history_kb: None,
            safety_max_consecutive_beats: None,
            safety_cooldown_secs: None,
            extra_models: vec![],
            name: None,
        };
        let (url, model) = settings.parse_model();
        assert_eq!(url, "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(model, "anthropic/claude-opus-4.6");
    }

    #[test]
    fn test_instance_settings_parse_model_openai() {
        let settings = InstanceSettings {
            api_key: "sk-test".to_string(),
            model: "openai@gpt-4".to_string(),
            user_id: "test-user".to_string(),
            privileged: false,
            max_beats: None,
            action_separator: None,
            session_blocks_limit: None,
            session_block_kb: None,
            history_kb: None,
            safety_max_consecutive_beats: None,
            safety_cooldown_secs: None,
            extra_models: vec![],
            name: None,
        };
        let (url, model) = settings.parse_model();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(model, "gpt-4");
    }

    #[test]
    fn test_instance_settings_parse_model_no_provider() {
        let settings = InstanceSettings {
            api_key: "sk-test".to_string(),
            model: "claude-sonnet-4".to_string(),
            user_id: "test-user".to_string(),
            privileged: false,
            max_beats: None,
            action_separator: None,
            session_blocks_limit: None,
            session_block_kb: None,
            history_kb: None,
            safety_max_consecutive_beats: None,
            safety_cooldown_secs: None,
            extra_models: vec![],
            name: None,
        };
        let (url, model) = settings.parse_model();
        assert_eq!(url, "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(model, "claude-sonnet-4");
    }

    #[test]
    fn test_instance_settings_load() {
        let tmp = TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path,
            r#"{"api_key":"sk-test-key","model":"openrouter@anthropic/claude-sonnet-4"}"#
        ).unwrap();

        let settings = InstanceSettings::load(&settings_path).unwrap();
        assert_eq!(settings.api_key, "sk-test-key");
        assert_eq!(settings.model, "openrouter@anthropic/claude-sonnet-4");
        assert!(settings.action_separator.is_none());
    }

    #[test]
    fn test_instance_settings_load_with_action_separator() {
        let tmp = TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path,
            r#"{"api_key":"sk-test","model":"openrouter@google/gemini-flash","action_separator":"fixed123"}"#
        ).unwrap();

        let settings = InstanceSettings::load(&settings_path).unwrap();
        assert_eq!(settings.action_separator, Some("fixed123".to_string()));
    }

    #[test]
    fn test_engine_creation() {
        let tmp = TempDir::new().unwrap();
        let engine = AliceEngine::new(
            tmp.path().to_path_buf(),
            tmp.path().join("logs"),
        );
        assert!(engine.instances.is_empty());
    }

    #[test]
    fn test_engine_restore_instances() {
        let tmp = TempDir::new().unwrap();

        let instance_dir = tmp.path().join("test-instance");
        let memory_dir = instance_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::create_dir_all(instance_dir.join("workspace")).unwrap();

        std::fs::write(
            instance_dir.join("settings.json"),
            r#"{"api_key":"sk-test","model":"openrouter@test-model"}"#,
        ).unwrap();

        let mut engine = AliceEngine::new(
            tmp.path().to_path_buf(),
            tmp.path().join("logs"),
        );
        engine.restore_instances().unwrap();

        assert_eq!(engine.instances.len(), 1);
        assert_eq!(engine.instances[0].0, "test-instance");
    }

    #[test]
    fn test_engine_restore_skips_invalid() {
        let tmp = TempDir::new().unwrap();

        // Directory without session/settings.json
        std::fs::create_dir_all(tmp.path().join("invalid")).unwrap();

        // Valid instance
        let valid_dir = tmp.path().join("valid");
        let memory_dir = valid_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::create_dir_all(valid_dir.join("workspace")).unwrap();
        std::fs::write(
            valid_dir.join("settings.json"),
            r#"{"api_key":"sk-test","model":"test"}"#,
        ).unwrap();

        let mut engine = AliceEngine::new(
            tmp.path().to_path_buf(),
            tmp.path().join("logs"),
        );
        engine.restore_instances().unwrap();

        assert_eq!(engine.instances.len(), 1);
        assert_eq!(engine.instances[0].0, "valid");
    }

    #[test]
    fn test_engine_backup_memory() {
        let tmp = TempDir::new().unwrap();

        let instance_dir = tmp.path().join("test");
        let memory_dir = instance_dir.join("memory");
        let sessions_dir = memory_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(sessions_dir.join("history.txt"), "history data").unwrap();
        std::fs::write(sessions_dir.join("current.txt"), "current data").unwrap();

        let engine = AliceEngine::new(
            tmp.path().to_path_buf(),
            tmp.path().join("logs"),
        );
        engine.backup_all_memory();

        let snapshots_dir = memory_dir.join("snapshots");
        assert!(snapshots_dir.exists());
        let snapshots: Vec<_> = std::fs::read_dir(&snapshots_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(snapshots.len(), 2);
    }

    #[test]
    fn test_check_restart_signal_no_file() {
        let pid_file = PathBuf::from("/tmp/test-alice-engine.pid");
        assert!(!check_shutdown_signal(&pid_file));
    }

    #[test]
    fn test_extract_json_u32() {
        let json = r#"{"max_beats": 10, "other": 42}"#;
        assert_eq!(extract_json_u32(json, "max_beats"), Some(10));
        assert_eq!(extract_json_u32(json, "other"), Some(42));
        assert_eq!(extract_json_u32(json, "missing"), None);
    }

    #[test]
    fn test_instance_settings_max_beats() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        // With max_beats
        std::fs::write(&path,
            r#"{"api_key":"sk-test","model":"test","max_beats":20}"#
        ).unwrap();
        let settings = InstanceSettings::load(&path).unwrap();
        assert_eq!(settings.max_beats, Some(20));

        // Without max_beats
        std::fs::write(&path,
            r#"{"api_key":"sk-test","model":"test"}"#
        ).unwrap();
        let settings = InstanceSettings::load(&path).unwrap();
        assert_eq!(settings.max_beats, None);
    }

    #[test]
    fn test_test_prefix_default_max_beats() {
        let tmp = TempDir::new().unwrap();

        // Create test-instance
        let instance_dir = tmp.path().join("test-foo");
        let memory_dir = instance_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::create_dir_all(instance_dir.join("workspace")).unwrap();
        std::fs::write(instance_dir.join("settings.json"),
            r#"{"api_key":"sk-test","model":"test"}"#
        ).unwrap();

        // Create normal instance
        let instance_dir2 = tmp.path().join("normal");
        let memory_dir2 = instance_dir2.join("memory");
        std::fs::create_dir_all(&memory_dir2).unwrap();
        std::fs::create_dir_all(instance_dir2.join("workspace")).unwrap();
        std::fs::write(instance_dir2.join("settings.json"),
            r#"{"api_key":"sk-test","model":"test"}"#
        ).unwrap();

        let mut engine = AliceEngine::new(
            tmp.path().to_path_buf(),
            tmp.path().join("logs"),
        );
        engine.restore_instances().unwrap();

        // Find instances by name
        let test_instance = engine.instances.iter().find(|(n, _)| n == "test-foo");
        let normal_instance = engine.instances.iter().find(|(n, _)| n == "normal");

        assert_eq!(test_instance.unwrap().1.max_beats, Some(10));
        assert_eq!(normal_instance.unwrap().1.max_beats, None);
    }
}