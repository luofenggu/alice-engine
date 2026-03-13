//! # Action Output Formatting
//!
//! Residual formatting functions still used by core/mod.rs beat loop.
//! Most output formatting has moved to ActionOutput::render() in inference/output.rs.

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

// ─── Message context constants ──────────────────────────────────

/// Context string before [MSG:xxx] in send results.
/// Used by extract_msg_ids to identify trusted send markers.
pub const MSG_SEND_CONTEXT: &str = "send success ";

/// Context string after [MSG:xxx] in read results.
/// Used by extract_msg_ids to identify trusted read markers.
pub const MSG_READ_CONTEXT: &str = "发来一条消息：";

// ─── Action block formatting ─────────────────────────────────────

/// Format action block start marker.
pub fn action_block_start(action_id: &str) -> String {
    format!("---------行为编号[{}]开始---------", action_id)
}

/// Format action block end marker.
pub fn action_block_end(action_id: &str) -> String {
    format!("---------行为编号[{}]结束---------", action_id)
}

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

// ─── Doing/error text builders ───────────────────────────────────

/// Build the "doing" text for an action (description + executing marker).
pub fn build_doing_text(action: &Action) -> String {
    format!(
        "{}\n{}",
        build_doing_description(action),
        action_executing()
    )
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

