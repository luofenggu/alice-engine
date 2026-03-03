//! # Log Formats — System-internal messages and paths for logging infrastructure.
//!
//! Counterpart to `messages.rs` (user-facing messages).
//! These are messages the system leaves for itself: log file names,
//! timestamp formats, and log content templates.

// ─── Log file paths ─────────────────────────────────────────────

pub const CRASH_LOG: &str = "crash.log";
pub const ENGINE_LOG: &str = "engine.log";
pub const ENGINE_LOG_ROTATED: &str = "engine.log.1";
pub const INFER_DIR: &str = "infer";

// ─── Timestamp formats ─────────────────────────────────────────

/// Timestamp format for tracing output and crash log lines.
pub const LOG_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// Timestamp format for inference log filenames (millisecond precision).
pub const INFER_LOG_TIME_FORMAT: &str = "%Y%m%d%H%M%S%3f";

// ─── Default configuration ─────────────────────────────────────

pub const DEFAULT_LOG_LEVEL: &str = "info";

// ─── Numeric constants ─────────────────────────────────────────

pub const SECS_PER_DAY: u64 = 86400;
pub const BYTES_PER_MB: u64 = 1_048_576;

// ─── Log content formatters ────────────────────────────────────

/// Format a timestamp using the standard log time format.
pub fn format_log_timestamp(time: &chrono::DateTime<chrono::Local>) -> String {
    time.format(LOG_TIME_FORMAT).to_string()
}

/// Format a timestamp for inference log filenames.
pub fn format_infer_timestamp(time: &chrono::DateTime<chrono::Local>) -> String {
    time.format(INFER_LOG_TIME_FORMAT).to_string()
}

/// Format a panic message for logging.
pub fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    format!("[PANIC] {}", info)
}

/// Format a crash log line with timestamp.
pub fn crash_log_line(timestamp: &str, msg: &str) -> String {
    format!("[{}] {}", timestamp, msg)
}

/// Build the inference output log filename.
pub fn infer_out_filename(timestamp: &str) -> String {
    format!("{}.out.log", timestamp)
}

/// Build the inference input log filename.
pub fn infer_in_filename(timestamp: &str) -> String {
    format!("{}.in.log", timestamp)
}

/// Build the inference input log content.
pub fn infer_input_log_content(
    model: &str,
    api_url: &str,
    system_prompt: &str,
    user_prompt: &str,
) -> String {
    format!(
        "[model: {}]\n[endpoint: {}]\n\n=== SYSTEM PROMPT ({} chars) ===\n{}\n\n=== USER PROMPT ({} chars) ===\n{}\n",
        model, api_url,
        system_prompt.len(), system_prompt,
        user_prompt.len(), user_prompt,
    )
}

