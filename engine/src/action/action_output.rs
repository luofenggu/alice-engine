//! # Action Output Formatting
//!
//! Centralized formatting for action execution results that enter agent memory (current.txt).
//!
//! These messages are special: they are not just human-readable feedback, but also
//! part of the agent's memory format. Some formats (e.g., [MSG:xxx] markers) are
//! parsed by other code (e.g., extract_msg_ids). Changing formats here may affect parsers.
//!
//! Guardian: file-level exempt (similar to messages.rs).

/// Context string before [MSG:xxx] in send results.
/// Used by extract_msg_ids to identify trusted send markers.
pub const MSG_SEND_CONTEXT: &str = "send success ";

/// Context string after [MSG:xxx] in read results.
/// Used by extract_msg_ids to identify trusted read markers.
pub const MSG_READ_CONTEXT: &str = "发来一条消息：";

// ─── Script execution ────────────────────────────────────────────

/// Format truncated output indicator.
pub fn truncated_output(truncated: &str, total_bytes: usize) -> String {
    format!("{}...\n[truncated, total {} bytes]", truncated, total_bytes)
}

/// Format script execution result for agent memory.
pub fn script_result(duration_secs: f64, output: &str, exit_code: Option<i32>) -> String {
    let duration_str = format!("{:.1}s", duration_secs);
    let exit_info = match exit_code {
        Some(0) => String::new(),
        Some(code) => format!("\n[exit code: {}]", code),
        None => "\n[exit code: unknown]".to_string(),
    };
    format!("---exec result ({})---\n{}{}\n", duration_str, output, exit_info)
}

// ─── File write ──────────────────────────────────────────────────

/// Format write success with full file content (for .md files).
pub fn write_success_full(path: &str, bytes: usize, lines: usize, content: &str) -> String {
    format!(
        "write success [{}] ({} bytes, {} lines)\n\n---file content---\n{}\n",
        path, bytes, lines, content
    )
}

/// Format write success with skeleton extraction.
pub fn write_success_skeleton(path: &str, bytes: usize, lines: usize, skeleton: &str) -> String {
    format!(
        "write success [{}] ({} bytes, {} lines)\n\n--- skeleton (auto-extracted, showing interface & comments only, not full content) ---\n{}\n",
        path, bytes, lines, skeleton
    )
}

/// Format write success with head/tail preview.
pub fn write_success_preview(path: &str, bytes: usize, lines: usize, preview: &str) -> String {
    format!(
        "write success [{}] ({} bytes, {} lines)\n\n--- preview (first 10 + last 5 lines, not full content) ---\n{}\n",
        path, bytes, lines, preview
    )
}

/// Format a single preview line with line number.
pub fn preview_line(line_number: usize, content: &str) -> String {
    format!("{:>4}: {}", line_number, content)
}

/// Preview ellipsis separator.
pub const PREVIEW_ELLIPSIS: &str = "     ...";

// ─── Replace in file ─────────────────────────────────────────────

/// Format replace match error (matched 0 or >1 times).
pub fn replace_match_error(search_preview: &str, count: usize) -> String {
    format!("  ERROR: '{}...' matched {} times (expected 1)", search_preview, count)
}

/// Format single replace block success.
pub fn replace_block_success(search_preview: &str) -> String {
    format!("  replaced '{}...'", search_preview)
}

/// Format replace operation summary with details.
pub fn replace_result(total_replaced: usize, detail_lines: &[String]) -> String {
    let summary = format!("replaced {} block(s) successfully", total_replaced);
    let detail = detail_lines.join("\n");
    format!("{}\n{}\n", summary, detail)
}

// ─── Message read/send ───────────────────────────────────────────

/// Format empty inbox message.
pub fn inbox_empty() -> String {
    "(收件箱为空)\n".to_string()
}

/// Format a single read message entry.
pub fn read_msg_entry(sender: &str, timestamp: &str, content: &str) -> String {
    format!("{} [MSG:{}]{}\n\n{}\n", sender, timestamp, MSG_READ_CONTEXT, content)
}

/// Format send failure for unknown recipient.
pub fn send_failed_unknown_recipient(recipient: &str) -> String {
    format!(
        "send failed: unknown recipient '{}'. You can only send messages to your user.\n",
        recipient
    )
}

/// Format send success confirmation.
pub fn send_success(timestamp: &str) -> String {
    format!("{}[MSG:{}]\n", MSG_SEND_CONTEXT, timestamp)
}

// ─── Summary ─────────────────────────────────────────────────────

/// Format empty current message for summary.
pub fn summary_empty() -> String {
    "current is empty, nothing to summarize\n".to_string()
}

/// Format knowledge rewrite confirmation.
pub fn knowledge_rewritten(chars: usize) -> String {
    format!("\nknowledge: rewritten {} chars", chars)
}

/// Format knowledge skip message.
pub fn knowledge_skipped() -> String {
    "\nknowledge: no knowledge marker found, skipped".to_string()
}

/// Format summary completion statistics.
pub fn summary_complete(
    msg_count: usize,
    first_msg: &str,
    last_msg: &str,
    block_name: &str,
    knowledge_info: &str,
) -> String {
    format!(
        "小结完成: {}个消息ID({}~{}) → sessions/{}.jsonl, current已清空{}\n",
        msg_count, first_msg, last_msg, block_name, knowledge_info
    )
}

// ─── Forget ──────────────────────────────────────────────────────

/// Format action block start marker.
pub fn action_block_start(action_id: &str) -> String {
    format!("---------行为编号[{}]开始---------", action_id)
}

/// Format action block end marker.
pub fn action_block_end(action_id: &str) -> String {
    format!("---------行为编号[{}]结束---------", action_id)
}

/// Format forgotten action block replacement.
pub fn forgotten_block(action_id: &str, summary: &str) -> String {
    format!(
        "---------行为编号[{}]开始---------\n[已提炼] {}\n---------行为编号[{}]结束---------\n",
        action_id, summary.trim(), action_id
    )
}

// ─── Set profile ─────────────────────────────────────────────────

/// Format unknown profile key error.
pub fn profile_unknown_key(key: &str) -> String {
    format!("set_profile failed: unknown key '{}'\n", key)
}

/// Format profile update success.
pub fn profile_updated(detail: &str) -> String {
    format!("profile updated: {}\n", detail)
}

// ─── Create instance ─────────────────────────────────────────────

/// Format instance creation success.
pub fn instance_created(id: &str, name: &str, knowledge_bytes: usize) -> String {
    format!(
        "instance created: {} (name: {}), knowledge: {} bytes written\nEngine hot-scan will start it automatically.\n",
        id, name, knowledge_bytes
    )
}

