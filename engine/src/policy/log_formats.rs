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

// ─── Thread naming ─────────────────────────────────────────────

/// Format for instance worker thread names (visible in logs and system tools).
pub fn thread_name(instance: &str) -> String {
    format!("thread-instance-{}", instance)
}

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

/// Write a formatted log timestamp directly to a fmt::Write sink.
pub fn write_log_timestamp(
    w: &mut impl std::fmt::Write,
    now: &chrono::DateTime<chrono::Local>,
) -> std::fmt::Result {
    write!(w, "{}", format_log_timestamp(now))
}

/// Write a crash log line directly to an io::Write sink (with newline).
pub fn write_crash_log_line(
    f: &mut impl std::io::Write,
    timestamp: &str,
    msg: &str,
) -> std::io::Result<()> {
    writeln!(f, "{}", crash_log_line(timestamp, msg))
}

/// Build the inference output log filename.
pub fn infer_out_filename(timestamp: &str) -> String {
    format!("{}.out.log", timestamp)
}

/// Build the capture output log filename.
pub fn infer_capture_out_filename(timestamp: &str) -> String {
    format!("{}.out.capture.log", timestamp)
}

/// Build the compress output log filename.
pub fn infer_compress_out_filename(timestamp: &str) -> String {
    format!("{}.out.compress.log", timestamp)
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
