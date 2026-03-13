//! # Action Output Formatting
//!
//! Residual formatting functions still used by core/mod.rs beat loop.
//! Most output formatting has moved to ActionOutput::render() in inference/output.rs.

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

// ─── Error/interruption text builders ───────────────────────────

/// Format action execution error.
pub fn action_error(e: &anyhow::Error) -> String {
    format!("\nERROR: {}\n", e)
}

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

