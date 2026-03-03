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

use crate::util::Counter;
use crate::core::{Alice, AliceConfig};
use crate::persist::instance::{InstanceStore, InstanceSettingsExt};
use crate::core::signal::SignalHub;





// InstanceSettingsExt imported above with InstanceStore

// ─── Free function: sandbox user management ──────────────────────



// ─── Free function: shutdown signal check ────────────────────────

/// Check for graceful shutdown signal file.
/// Returns true if signal detected (caller should initiate shutdown).
///
/// @TRACE: SHUTDOWN
fn check_shutdown_signal(pid_file: &Path, signal_file: &Path) -> bool {
    let signal_path = signal_file;
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
    /// PID file path.
    pid_file: PathBuf,
    /// Instance store for managing instance lifecycle.
    instance_store: InstanceStore,
    /// Signal hub for inter-thread communication (interrupt, switch-model).
    signal_hub: SignalHub,
    /// Environment configuration.
    env_config: Arc<crate::policy::EnvConfig>,
    /// Temporary buffer for instances during restore (drained to threads in run()).
    instances: Vec<(String, Alice)>,
}

impl AliceEngine {
    /// Create a new engine.
    pub fn new(instances_base: PathBuf, logs_dir: PathBuf, signal_hub: SignalHub, env_config: Arc<crate::policy::EnvConfig>) -> Self {
        let pid_file = env_config.pid_file_path(&instances_base);
        let instance_store = InstanceStore::new(instances_base.clone());
        Self {
            instances_base,
            logs_dir,
            pid_file,
            instance_store,
            signal_hub,
            env_config,
            instances: Vec::new(),
        }
    }



    /// Discover and restore instances from the instances directory.
    ///
    /// @TRACE: INSTANCE
    fn restore_instances(&mut self) -> Result<()> {
        let ids = self.instance_store.list_ids()
            .with_context(|| format!("Failed to list instances in: {}",
                self.instances_base.display()))?;

        for id in ids {
            let instance_dir = self.instances_base.join(&id);

            match self.create_instance(&id, &instance_dir) {
                Ok(()) => {
                    info!("[INSTANCE] restored id={}", id);
                }
                Err(e) => {
                    error!("[INSTANCE] failed to restore {}: {}", id, e);
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
        let instance = crate::persist::instance::Instance::open(instance_dir)?;
        let mut settings = instance.settings.load()?;
        settings.apply_env_fallbacks(&self.env_config);
        settings.validate()?;

        let config = AliceConfig {
            model: settings.model.clone(),
            api_key: settings.api_key.clone(),
            log_dir: self.logs_dir.clone(),

        };

        let mut alice = Alice::new(instance, config, self.env_config.clone())?;

        alice.instance_name = settings.name;

        alice.privileged = settings.privileged;
        if let Some(v) = settings.safety_max_consecutive_beats { alice.safety_max_consecutive_beats = v; }
        if let Some(v) = settings.safety_cooldown_secs { alice.safety_cooldown_secs = v; }
        alice.host = self.env_config.host.clone();


        // Auto-create sandbox user (紧箍咒) for non-privileged instances
        if !settings.privileged {
            let engine_policy = &crate::policy::EngineConfig::get().engine;
            if let Err(e) = crate::external::shell::ensure_sandbox_user(&engine_policy.sandbox_user_prefix, name, &alice.instance.workspace) {
                warn!("[SANDBOX] Skipping sandbox setup for {}: {} (sandbox commands not available)", name, e);
            }
        }

        // Security isolation (紧箍咒) — permissions managed by persist layer
        alice.instance.apply_security_permissions();

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

        // test- prefixed instances default to limited beats if not explicitly set
        alice.max_beats = settings.max_beats.or_else(|| {
            let engine_policy = &crate::policy::EngineConfig::get().engine;
            if name.starts_with(&engine_policy.test_instance_prefix) {
                Some(engine_policy.test_instance_max_beats)
            } else {
                None
            }
        });

        // Write initial memory (imprint learning) on first creation
        if alice.instance.memory.history.read()?.is_empty() {
            alice.instance.memory.history.write(crate::inference::beat::INITIAL_HISTORY).ok();
            info!("[INSTANCE] Initial history written for {}", name);
        }

        // Register signals for inter-thread communication
        alice.signals = Some(self.signal_hub.register(name));

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

        // 1. Clean up old logs
        let retention_days = self.env_config.infer_log_retention_days;
        crate::logging::cleanup_old_infer_logs(&self.logs_dir, retention_days);
        crate::logging::rotate_engine_log(&self.logs_dir, crate::policy::EngineConfig::get().engine.log_rotate_max_mb);

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
                .name(format!("thread-instance-{}", name))
                .spawn(move || {
                    Self::instance_thread(alice, shutdown_clone);
                })
                .with_context(|| format!("Failed to spawn thread for instance {}", name))?;
            info!("[THREAD] Spawned thread for instance: {}", name);
            threads.insert(name, handle);
        }

        // 5. Main loop: hot-scan, cold-clean, shutdown signal
        loop {
            let engine_policy = &crate::policy::EngineConfig::get().engine;
            std::thread::sleep(Duration::from_secs(engine_policy.main_loop_interval_secs));

            // Check graceful shutdown signal
            if check_shutdown_signal(&self.pid_file, &self.env_config.shutdown_signal_file) {
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
            if let Ok(ids) = self.instance_store.list_ids() {
                let new_ids: Vec<_> = ids.into_iter()
                    .filter(|id| !threads.contains_key(id))
                    .collect();

                for name in new_ids {
                    let instance_dir = self.instances_base.join(&name);
                    match self.create_instance(&name, &instance_dir) {
                        Ok(()) => {
                            // Pop the instance we just pushed to self.instances
                            if let Some((inst_name, alice)) = self.instances.pop() {
                                let shutdown_clone = Arc::clone(&shutdown);
                                let handle = std::thread::Builder::new()
                                    .name(format!("thread-instance-{}", inst_name))
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



    /// Independent heartbeat loop for a single instance.
    /// Runs in its own thread. Exits when:
    /// - shutdown signal is set (engine restart)
    /// - settings.json is missing (instance deleted)
    /// - beat limit reached
    fn instance_thread(mut alice: Alice, shutdown: Arc<AtomicBool>) {
        let instance_id = alice.instance.id.clone();
        let rolling_in_progress = Arc::new(AtomicBool::new(false));
        let instance_dir = alice.instance.instance_dir.clone();
        info!("[THREAD-{}] Instance thread started", instance_id);

        let mut consecutive_beats = Counter::<u32>::new();
        let mut idle_elapsed = Counter::<u64>::new();
        let engine_policy = &crate::policy::EngineConfig::get().engine;
        let error_backoff_secs = engine_policy.error_backoff_secs;

        loop {
            // Check shutdown signal
            if shutdown.load(Ordering::Relaxed) {
                info!("[THREAD-{}] Shutdown signal received, exiting", instance_id);
                break;
            }

            // Hot-reload settings via Document (also detects instance deletion)
            match alice.instance.settings.load() {
                Ok(s) => {
                    if let Some(v) = s.safety_max_consecutive_beats { alice.safety_max_consecutive_beats = v; }
                    if let Some(v) = s.safety_cooldown_secs { alice.safety_cooldown_secs = v; }
                    if let Some(v) = s.session_blocks_limit { alice.session_blocks_limit = v; }
                    if let Some(v) = s.session_block_kb { alice.session_block_kb = v; }
                    if let Some(v) = s.history_kb { alice.history_kb = v; }

                    // Hot-reload instance name
                    if s.name != alice.instance_name {
                        alice.instance_name = s.name.clone();
                    }

                    // Hot-reload privileged
                    if s.privileged != alice.privileged {
                        info!("[HOT-RELOAD-{}] Privileged changed: {} -> {}", instance_id, alice.privileged, s.privileged);
                        alice.privileged = s.privileged;
                    }

                    // Hot-reload model and api_key
                    if !s.model.is_empty() && s.model != alice.config.model {
                        info!("[HOT-RELOAD-{}] Model changed: {} -> {}", instance_id, alice.config.model, s.model);
                        alice.config.model = s.model.clone();
                        alice.llm_client.config.model = s.model.clone();
                    }
                    if !s.api_key.is_empty() && s.api_key != alice.config.api_key {
                        info!("[HOT-RELOAD-{}] API key changed", instance_id);
                        alice.config.api_key = s.api_key.clone();
                        alice.llm_client.config.api_key = s.api_key.clone();
                    }

                }
                Err(_) => {
                    // reload failed = file missing or corrupted, instance likely deleted
                    info!("[THREAD-{}] Settings reload failed, instance likely deleted. Exiting.", instance_id);
                    break;
                }
            }

            // Idle polling: if last beat was idle and no unread, write idle status and sleep
            if alice.last_was_idle && alice.count_unread_messages() == 0 {
                // Write idle status here (not in beat()) so observe never sees
                // a false "idle" between consecutive beats in a reasoning chain.
                if let Some(ref signals) = alice.signals {
                    let born = alice.born;
                    let idle_timeout = alice.idle_timeout_secs;
                    let idle_since = alice.idle_since;
                    signals.update_status(|s| {
                        s.inferring = false;
                        s.born = born;
                        s.last_beat = std::time::Instant::now();
                        s.idle_timeout_secs = idle_timeout;
                        s.idle_since = idle_since;
                    });
                }

                consecutive_beats.reset();
                std::thread::sleep(Duration::from_secs(engine_policy.beat_interval_secs));

                // Re-check after sleep
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                // Check interrupt signal during idle (cancel timeout → infinite idle)
                if alice.signals.as_ref().map_or(false, |s| s.check_interrupt()) {
                    info!("[INTERRUPT-{}] Interrupt during idle, cancelling timeout", instance_id);
                    alice.idle_timeout_secs = None;
                    alice.idle_since = None;
                    idle_elapsed.reset();
                    // Update status to reflect cancelled timeout
                    if let Some(ref signals) = alice.signals {
                        signals.update_status(|s| {
                            s.inferring = false;
                            s.last_beat = std::time::Instant::now();
                            s.born = alice.born;
                            s.idle_timeout_secs = None;
                            s.idle_since = None;
                        });
                    }
                    continue;
                }

                // Check idle timeout (timed idle wakeup)
                if let Some(timeout) = alice.idle_timeout_secs {
                    idle_elapsed.add(engine_policy.beat_interval_secs);
                    if idle_elapsed.value() >= timeout {
                        info!("[IDLE-TIMEOUT-{}] Idle timeout {}s reached (elapsed {}s), waking up",
                            instance_id, timeout, idle_elapsed.value());
                        alice.idle_timeout_secs = None;
                        alice.idle_since = None;
                        alice.last_was_idle = false;  // So beat() won't skip
                        idle_elapsed.reset();
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
            if consecutive_beats.value() >= alice.safety_max_consecutive_beats {
                warn!("[SAFETY-{}] {} consecutive beats without idle — forcing cooldown ({}s)",
                    instance_id, consecutive_beats.value(), alice.safety_cooldown_secs);
                alice.notify_anomaly(&crate::policy::messages::safety_valve_triggered(
                    consecutive_beats.value(), alice.safety_cooldown_secs
                ));
                alice.last_was_idle = true;
                consecutive_beats.reset();
                std::thread::sleep(Duration::from_secs(alice.safety_cooldown_secs));
                continue;
            }

            // Max beats limit check
            if let Some(max) = alice.max_beats {
                if alice.beat_count.value() >= max {
                    if !alice.last_was_idle {
                        info!("[LIMIT-{}] Beat limit reached ({}/{}), forcing idle",
                            instance_id, alice.beat_count.value(), max);
                        alice.notify_anomaly(&crate::policy::messages::beat_limit_reached(alice.beat_count.value(), max));
                        alice.last_was_idle = true;
                    }
                    // Sleep and check shutdown, but don't beat
                    std::thread::sleep(Duration::from_secs(engine_policy.beat_interval_secs));
                    continue;
                }
            }

            // Reset idle elapsed counter when entering a beat
            idle_elapsed.reset();

            // Run beat
            let unread = alice.count_unread_messages();
            if unread > 0 || !alice.last_was_idle {
                info!("[BEAT-{}] wakeup unread={} idle={} consecutive={}",
                    instance_id, unread, alice.last_was_idle, consecutive_beats.value());
            }

            // Disk space check (periodic to avoid overhead)
            let engine_policy = &crate::policy::EngineConfig::get().engine;
            if consecutive_beats.value() % engine_policy.disk_check_interval_beats == 0 {
                if let Some(avail_mb) = crate::external::shell::available_mb(&instance_dir) {
                    if avail_mb < engine_policy.disk_min_available_mb {
                        alice.notify_anomaly(&crate::policy::messages::disk_space_low(avail_mb));
                    }
                }
            }

            match alice.beat() {
                Ok(()) => {
                    alice.beat_count.increment();
                    if alice.last_was_idle {
                        consecutive_beats.reset();
                    } else {
                        consecutive_beats.increment();
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
                        instance_id, e, error_backoff_secs);
                    alice.current_infer_log_path = None;
                    if let Some(ref signals) = alice.signals {
                        signals.update_status(|s| {
                            s.inferring = false;
                            s.born = alice.born;
                            s.last_beat = std::time::Instant::now();
                            s.log_path = None;
                            s.idle_timeout_secs = None;
                            s.idle_since = None;
                        });
                    }
                    alice.notify_anomaly(&crate::policy::messages::beat_error(&e));
                    alice.last_was_idle = true;
                    consecutive_beats.reset();
                    std::thread::sleep(Duration::from_secs(error_backoff_secs));
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

// ─── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::Document;
    use crate::persist::instance::InstanceSettings;
    use tempfile::TempDir;



    #[test]
    fn test_instance_settings_load() {
        let tmp = TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path,
            r#"{"api_key":"sk-test-key","model":"openrouter@anthropic/claude-sonnet-4"}"#
        ).unwrap();

        let doc: Document<InstanceSettings> = Document::open(&settings_path).unwrap();
        let settings = doc.load().unwrap();
        assert_eq!(settings.api_key, "sk-test-key");
        assert_eq!(settings.model, "openrouter@anthropic/claude-sonnet-4");
    }



    #[test]
    fn test_engine_creation() {
        let tmp = TempDir::new().unwrap();
        let engine = AliceEngine::new(
            tmp.path().to_path_buf(),
            tmp.path().join("logs"),
            SignalHub::new(),
            std::sync::Arc::new(crate::policy::EnvConfig::from_env()),
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
            SignalHub::new(),
            std::sync::Arc::new(crate::policy::EnvConfig::from_env()),
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
            SignalHub::new(),
            std::sync::Arc::new(crate::policy::EnvConfig::from_env()),
        );
        engine.restore_instances().unwrap();

        assert_eq!(engine.instances.len(), 1);
        assert_eq!(engine.instances[0].0, "valid");
    }

    #[test]
    fn test_check_restart_signal_no_file() {
        let pid_file = PathBuf::from("/tmp/test-alice-engine.pid");
        let signal_file = PathBuf::from("/tmp/test-alice-shutdown.signal");
        assert!(!check_shutdown_signal(&pid_file, &signal_file));
    }

    #[test]
    fn test_instance_settings_max_beats() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        // With max_beats
        std::fs::write(&path,
            r#"{"api_key":"sk-test","model":"test","max_beats":20}"#
        ).unwrap();
        let doc: Document<InstanceSettings> = Document::open(&path).unwrap();
        assert_eq!(doc.load().unwrap().max_beats, Some(20));

        // Without max_beats
        std::fs::write(&path,
            r#"{"api_key":"sk-test","model":"test"}"#
        ).unwrap();
        let doc: Document<InstanceSettings> = Document::open(&path).unwrap();
        assert_eq!(doc.load().unwrap().max_beats, None);
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
            SignalHub::new(),
            std::sync::Arc::new(crate::policy::EnvConfig::from_env()),
        );
        engine.restore_instances().unwrap();

        // Find instances by name
        let test_instance = engine.instances.iter().find(|(n, _)| n == "test-foo");
        let normal_instance = engine.instances.iter().find(|(n, _)| n == "normal");

        assert_eq!(test_instance.unwrap().1.max_beats, Some(10));
        assert_eq!(normal_instance.unwrap().1.max_beats, None);
    }
}