//! # Logging Module
//!
//! Centralized logging configuration, rotation, and inference log management.
//! All log-related logic lives here; business code only calls these functions.
//!
//! @TRACE: LOG-CLEANUP

use chrono::Local;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::EnvFilter;

use crate::policy::log_formats as logfmt;

// ─── Tracing initialization ─────────────────────────────────────

/// Custom local time formatter for tracing (matches prompt timestamp format).
struct LocalTimer;

impl FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let now = Local::now();
        logfmt::write_log_timestamp(w, &now)
    }
}

/// Initialize the global tracing subscriber with local timestamps and env filter.
///
/// Call once at program start. Uses `RUST_LOG` env var for filtering,
/// defaults to `info` level.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_timer(LocalTimer)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(logfmt::DEFAULT_LOG_LEVEL)),
        )
        .init();
}

/// Set up a global panic hook that writes crash info to `{logs_dir}/crash.log`.
///
/// Panics are logged both via tracing and appended to the crash log file
/// for post-mortem analysis.
pub fn setup_crash_hook(logs_dir: &Path) {
    let crash_log_path = logs_dir.join(logfmt::CRASH_LOG);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = logfmt::panic_message(info);
        tracing::error!("{}", msg);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_log_path)
        {
            let timestamp = logfmt::format_log_timestamp(&Local::now());
            let _ = logfmt::write_crash_log_line(&mut f, &timestamp, &msg);
        }
        default_hook(info);
    }));
}

// ─── Log rotation and cleanup ───────────────────────────────────

/// Clean up old inference logs (older than retention_days).
///
/// Walks `{logs_dir}/infer/{instance_id}/` subdirectories and removes
/// files whose modification time exceeds the retention period.
///
/// @TRACE: LOG-CLEANUP
pub fn cleanup_old_infer_logs(logs_dir: &Path, retention_days: u64) {
    let infer_dir = logs_dir.join(logfmt::INFER_DIR);
    if !infer_dir.exists() {
        return;
    }

    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(retention_days * logfmt::SECS_PER_DAY))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let mut cleaned_files = crate::util::Counter::<u32>::new();
    let mut cleaned_bytes = crate::util::Counter::<u64>::new();

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
                                    cleaned_files.increment();
                                    cleaned_bytes.add(size);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if cleaned_files.value() > 0 {
        info!(
            "[LOG-CLEANUP] Removed {} inference log files ({:.1}MB), retention={}d",
            cleaned_files.value(),
            cleaned_bytes.value() as f64 / logfmt::BYTES_PER_MB as f64,
            retention_days
        );
    } else {
        info!(
            "[LOG-CLEANUP] No old inference logs to clean (retention={}d)",
            retention_days
        );
    }
}

/// Rotate engine.log if it exceeds max_size_mb.
///
/// Simple single-rotation: `engine.log` → `engine.log.1`.
/// Previous `.1` is overwritten.
///
/// @TRACE: LOG-CLEANUP
pub fn rotate_engine_log(logs_dir: &Path, max_size_mb: u64) {
    let log_file = logs_dir.join(logfmt::ENGINE_LOG);
    if !log_file.exists() {
        return;
    }

    if let Ok(metadata) = log_file.metadata() {
        let size_mb = metadata.len() / logfmt::BYTES_PER_MB;
        if size_mb >= max_size_mb {
            let rotated = logs_dir.join(logfmt::ENGINE_LOG_ROTATED);
            // Remove old rotated file if exists
            std::fs::remove_file(&rotated).ok();
            // Rename current to .1
            if std::fs::rename(&log_file, &rotated).is_ok() {
                info!(
                    "[LOG-CLEANUP] Rotated engine.log ({}MB) -> engine.log.1",
                    size_mb
                );
            }
        }
    }
}

// ─── Inference log management ───────────────────────────────────

/// Create the inference log directory and return the output log path.
///
/// Returns `(out_log_path, timestamp_string)` where timestamp can be reused
/// for the corresponding input log.
pub fn create_infer_log_path(logs_dir: &Path, instance_id: &str) -> (PathBuf, String) {
    let infer_log_dir = logs_dir.join(logfmt::INFER_DIR).join(instance_id);
    std::fs::create_dir_all(&infer_log_dir).ok();
    let log_timestamp = logfmt::format_infer_timestamp(&Local::now());
    let log_path = infer_log_dir.join(logfmt::infer_out_filename(&log_timestamp));
    (log_path, log_timestamp)
}

/// Write the inference input log (system + user prompts) for debugging.
///
/// Only writes if `ALICE_INFER_LOG_IN` env var is set to `true` or `1`.
/// Uses the same timestamp as the output log for correlation.
pub fn write_infer_input_log(
    in_log_path: &Path,
    prompt: &str,
) {
    let in_log_content = logfmt::infer_input_log_content(prompt);
    if let Err(e) = std::fs::write(in_log_path, &in_log_content) {
        warn!("Failed to write in-log {:?}: {}", in_log_path, e);
    }
}
