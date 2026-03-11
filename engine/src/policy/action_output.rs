//! # Action Output Formatting
//!
//! Centralized formatting for action execution results that enter agent memory (current.txt).
//!
//! These messages are special: they are not just human-readable feedback, but also
//! part of the agent's memory format. Some formats (e.g., [MSG:xxx] markers) are
//! parsed by other code (e.g., extract_msg_ids). Changing formats here may affect parsers.
//!
//! Guardian: file-level exempt (similar to messages.rs).

use crate::inference::Action;
use chrono::Local;

// ─── ID Generation ───────────────────────────────────────────────

/// Generate a unique action ID: YYYYMMDDHHmmss_6hexchars
pub fn generate_action_id() -> String {
    let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();
    let hex: String = (0..6)
        .map(|_| format!("{:x}", rand::random::<u8>() % 16))
        .collect();
    format!("{}_{}", timestamp, hex)
}

// ─── Output strategy constants ───────────────────────────────────

/// Max bytes for script/command output before truncation.
const MAX_RESULT_BYTES: usize = 102_400;

/// Max bytes to show in search preview for replace_in_file.
const TRUNCATE_DISPLAY: usize = 40;

/// Max bytes to show in detail messages.

/// Number of head lines in preview.
const PREVIEW_HEAD_LINES: usize = 10;

/// Number of tail lines in preview.
const PREVIEW_TAIL_LINES: usize = 5;

/// Line count threshold to switch from head-only to head+tail preview.
const PREVIEW_THRESHOLD: usize = 15;

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
    format!(
        "---exec result ({})---\n{}{}\n",
        duration_str, output, exit_info
    )
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
fn preview_line(line_number: usize, content: &str) -> String {
    format!("{:>4}: {}", line_number, content)
}

/// Preview ellipsis separator.
const PREVIEW_ELLIPSIS: &str = "     ...";

/// Format head+tail preview from lines with line numbers.
/// Handles three cases: all lines fit in head, head+tail with ellipsis, head+remaining.
pub fn format_preview(lines: &[&str]) -> String {
    let total = lines.len();
    let actual_head = std::cmp::min(PREVIEW_HEAD_LINES, total);
    let mut preview: Vec<String> = Vec::new();
    for (line, num) in lines[..actual_head].iter().zip(1..) {
        preview.push(preview_line(num, line));
    }
    if total > PREVIEW_THRESHOLD {
        preview.push(PREVIEW_ELLIPSIS.to_string());
        for (line, num) in lines[total - PREVIEW_TAIL_LINES..]
            .iter()
            .zip(total - PREVIEW_TAIL_LINES + 1..)
        {
            preview.push(preview_line(num, line));
        }
    } else if total > PREVIEW_HEAD_LINES {
        for (line, num) in lines[PREVIEW_HEAD_LINES..]
            .iter()
            .zip(PREVIEW_HEAD_LINES + 1..)
        {
            preview.push(preview_line(num, line));
        }
    }
    preview.join("\n")
}

// ─── Replace in file ─────────────────────────────────────────────

/// Format replace match error (matched 0 or >1 times).
pub fn replace_match_error(search_preview: &str, count: usize) -> String {
    format!(
        "ERROR: '{}...' matched {} times (expected 1)",
        search_preview, count
    )
}

/// Format replace success.
pub fn replace_success(search_preview: &str) -> String {
    format!("replaced '{}...'", search_preview)
}

// ─── Message read/send ───────────────────────────────────────────

/// Format empty inbox message.
pub fn inbox_empty() -> String {
    "(收件箱为空)\n".to_string()
}

/// Format a single read message entry.
/// Unified format for all message types (user, system, agent relay).
/// If `is_known_sender` is false, prepend a warning line.
pub fn read_msg_entry(
    role: &str,
    sender: &str,
    self_id: &str,
    timestamp: &str,
    content: &str,
    is_known_sender: bool,
) -> String {
    let prefix = match role {
        "user" => "user".to_string(),
        "system" => "system".to_string(),
        _ => {
            if sender == self_id {
                format!("you[{}]", self_id)
            } else {
                format!("agent[{}]", sender)
            }
        }
    };
    let warning = if is_known_sender {
        String::new()
    } else {
        format!("⚠️ 此消息来自未知发送者：{}\n", sender)
    };
    format!(
        "{}{} [MSG:{}]{}\n\n{}\n",
        warning, prefix, timestamp, MSG_READ_CONTEXT, content
    )
}

/// Format send failure: recipient not found in contacts list.
pub fn send_failed_recipient_not_found(recipient: &str, contacts: &[crate::persist::hooks::ContactInfo]) -> String {
    if contacts.is_empty() {
        format!(
            "发送失败：收件人 \"{}\" 不在你的联系人列表中。当前没有可用联系人\n",
            recipient
        )
    } else {
        let names: Vec<String> = contacts.iter().map(|c| {
            match &c.name {
                Some(name) if !name.is_empty() => format!("{}({})", name, c.id),
                _ => c.id.clone(),
            }
        }).collect();
        format!(
            "发送失败：收件人 \"{}\" 不在你的联系人列表中。当前可用联系人：{}\n",
            recipient, names.join(", ")
        )
    }
}

/// Format send failure: communication service unavailable.
pub fn send_failed_service_unavailable(recipient: &str) -> String {
    format!(
        "发送失败：通讯服务暂时不可用，无法发送消息给 \"{}\"。请稍后重试\n",
        recipient
    )
}

/// Format send failure: relay error.
pub fn send_failed_relay_error(recipient: &str) -> String {
    format!(
        "发送失败：消息转发给 \"{}\" 时出错，通讯服务可能异常。请稍后重试\n",
        recipient
    )
}

/// Format send success confirmation.
pub fn send_success(timestamp: &str) -> String {
    format!("{}[MSG:{}]\n", MSG_SEND_CONTEXT, timestamp)
}

/// Format idle cancellation message when a prior send_msg failed in the same beat.
pub fn idle_cancelled_after_send_failure() -> String {
    "idle已跳过：本轮有消息发送失败，请检查上方的错误信息\n".to_string()
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

/// Format distilled action block replacement.
pub fn distilled_block(action_id: &str, summary: &str) -> String {
    format!(
        "---------行为编号[{}]开始---------\n[已提炼] {}\n---------行为编号[{}]结束---------\n",
        action_id,
        summary.trim(),
        action_id
    )
}

// ─── Set profile ─────────────────────────────────────────────────

/// Format unknown profile key error.
pub fn profile_unknown_key(key: &str) -> String {
    format!("set_profile failed: unknown key '{}'\n", key)
}

/// Format profile update success.
pub fn profile_updated(update: &crate::persist::Settings) -> String {
    let mut fields = Vec::new();
    if let Some(ref v) = update.name {
        fields.push(format!("name: {}", v));
    }
    if let Some(ref v) = update.color {
        fields.push(format!("color: {}", v));
    }
    if let Some(ref v) = update.avatar {
        fields.push(format!("avatar: {}", v));
    }
    if fields.is_empty() {
        "profile updated (no changes)\n".to_string()
    } else {
        format!("profile updated: {}\n", fields.join(", "))
    }
}

// ─── Create instance ─────────────────────────────────────────────

/// Format instance creation success.
pub fn instance_created(id: &str, name: &str, knowledge_bytes: usize) -> String {
    format!(
        "instance created: {} (name: {}), knowledge: {} bytes written\nEngine hot-scan will start it automatically.\n",
        id, name, knowledge_bytes
    )
}

// ─── Action block formatting ─────────────────────────────────────

/// Format a complete action block (start + doing + done + end).
pub fn action_block_full(action_id: &str, doing_text: &str, done_text: Option<&str>) -> String {
    format!(
        "{}\n{}{}\n{}\n",
        action_block_start(action_id),
        doing_text,
        done_text.unwrap_or(""),
        action_block_end(action_id),
    )
}

/// Build the "doing" block: start marker + doing text (no end marker).
/// Used for write-ahead: written to current before action execution.
pub fn action_block_doing(action_id: &str, doing_text: &str) -> String {
    format!("{}\n{}", action_block_start(action_id), doing_text.trim_end_matches('\n'))
}

/// Build the "done" block: done text + end marker.
/// Used for write-ahead: appended to current after action execution.
pub fn action_block_done(action_id: &str, done_text: Option<&str>) -> String {
    format!("{}\n{}\n", done_text.unwrap_or(""), action_block_end(action_id))
}

/// Build the "doing" text for an action (description + executing marker).
pub fn build_doing_text(action: &Action) -> String {
    format!(
        "{}\n{}",
        build_doing_description(action),
        action_executing()
    )
}

/// Build the "done" text for an action result.
/// Returns empty string for empty output, or newline-prefixed output.
pub fn build_done_text(output: &str) -> String {
    if output.is_empty() {
        String::new()
    } else {
        format!("\n{}", output)
    }
}

/// Format the "action executing" pending marker.
pub fn action_executing() -> &'static str {
    "---action executing, result pending---\n"
}

/// Format action execution error.
pub fn action_error(e: &anyhow::Error) -> String {
    format!("\nERROR: {}\n", e)
}

// ─── Inference interruption ──────────────────────────────────────

/// Format user interrupt marker.
/// Tells the agent that inference was interrupted and they should idle to await instructions.
pub fn inference_interrupted() -> &'static str {
    "---------推理被用户中断，请idle等待用户指示---------\n"
}

/// Format hallucination defense interruption marker.
/// Uses "幻觉防御" terminology consistent with prompt (BeatRequest / Action doc comments).
pub fn hallucination_defense_interrupted(reason: &str) -> String {
    format!("---------幻觉防御中断---------\n{}\n", reason)
}

// ─── Action description (doing text) ─────────────────────────────

/// Build a human-readable description of an action for agent memory.
/// This is the "doing" part that appears before execution results.
pub fn build_doing_description(action: &Action) -> String {
    match action {
        Action::Idle { timeout_secs: None } => "idle".to_string(),
        Action::Idle {
            timeout_secs: Some(secs),
        } => format!("idle ({}s)", secs),
        Action::ReadMsg => "你打开了收件箱，开始阅读来信。".to_string(),
        Action::SendMsg { recipient, content } => {
            format!("you send a letter to [{}]: \n\n{}\n", recipient, content)
        }
        Action::Thinking { content } => format!("记录思考: {}", content),
        Action::Script { content } => format!("execute script: \n{}", content),
        Action::WriteFile { path, content } => {
            let _ = content;
            format!("write file [{}]", path)
        }
        Action::ReplaceInFile { path, .. } => format!("replace in file [{}]", path),
        Action::Summary { .. } => "summary (小结)".to_string(),
        Action::SetProfile { content } => {
            let preview = crate::util::safe_truncate(content, 60);
            format!("set_profile [{}]", preview)
        }
        Action::CreateInstance { name, knowledge } => format!(
            "create_instance: {} ({} bytes knowledge)",
            name,
            knowledge.len()
        ),
        Action::Distill {
            target_action_id,
            summary,
        } => format!(
            "distill [{}]: {}",
            target_action_id,
            crate::util::safe_truncate(summary, 80)
        ),
    }
}

// ─── Truncation ──────────────────────────────────────────────────

/// Truncate result text if it exceeds MAX_RESULT_BYTES.
pub fn truncate_result(text: &str) -> String {
    if text.len() > MAX_RESULT_BYTES {
        let truncated = crate::util::safe_truncate(text, MAX_RESULT_BYTES);
        truncated_output(truncated, text.len())
    } else {
        text.to_string()
    }
}

/// Get the truncate_display limit for replace_in_file search previews.
pub fn truncate_display_limit() -> usize {
    TRUNCATE_DISPLAY
}
