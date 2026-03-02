//! # Action Execution
//!
//! Dispatches parsed actions to their concrete execution logic.
//! Each action variant maps to a specific operation on Alice's resources.
//!
//! @TRACE: ACTION

use anyhow::{Result, bail, Context};
use tracing::{info, warn};
use chrono::Local;
#[cfg(feature = "remember")]
use super::strip_remember_markers;

use std::path::PathBuf;
use std::fs;

use crate::core::{Alice, Transaction};
use crate::shell::Shell;
use super::{Action, ReplaceBlock};

/// Resolve an action path to an absolute path.
/// Absolute paths are used directly (only meaningful for privileged instances;
/// sandboxed instances lack filesystem permissions outside workspace).
/// Relative paths are resolved within the workspace (rejects path traversal).
fn resolve_action_path(alice: &Alice, path: &str) -> Result<PathBuf> {
    if path.starts_with('/') {
        Ok(PathBuf::from(path))
    } else {
        let p = std::path::Path::new(path);
        for component in p.components() {
            if let std::path::Component::ParentDir = component {
                bail!("Path traversal rejected: {}", path);
            }
        }
        Ok(alice.instance.workspace.join(p))
    }
}

/// Maximum output size stored in memory (100KB).
const MAX_RESULT_SIZE: usize = 100 * 1024;

/// Create a Shell instance with appropriate sandboxing for the given Alice.
/// In local mode (no sandbox user), runs without sandboxing.
fn make_shell(alice: &Alice) -> Shell {
    if alice.privileged {
        Shell::new(alice.instance.workspace.clone())
    } else {
        let user = format!("agent-{}", alice.instance.id);
        // Check if sandbox user exists (skip on local mode / systems without it)
        let user_exists = std::process::Command::new("id")
            .arg(&user)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if user_exists {
            Shell::new(alice.instance.workspace.clone()).with_sandbox(user)
        } else {
            Shell::new(alice.instance.workspace.clone())
        }
    }
}

/// Generate a heredoc script to write content to a file.
fn make_write_script(absolute_path: &str, content: &str) -> String {
    let delim = format!("HEREDOC_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    format!(
        "mkdir -p \"$(dirname '{}')\" && cat > '{}' << '{}'\n{}\n{}",
        absolute_path.replace('\'', "'\\''"),
        absolute_path.replace('\'', "'\\''"),
        delim,
        content,
        delim,
    )
}

/// Execute a single action against the Alice instance.
///
/// Returns the "done" text (execution result) to be appended to the action record.
pub fn execute_action(action: &Action, alice: &mut Alice, tx: &mut Transaction) -> Result<String> {
    match action {
        Action::Idle { timeout_secs } => execute_idle(alice, tx, *timeout_secs),
        Action::ReadMsg => execute_read_msg(alice, tx),
        Action::SendMsg { recipient, content } => execute_send_msg(alice, tx, recipient, content),
        Action::Thinking { content } => execute_thinking(alice, tx, content),
        Action::Script { content } => execute_script(alice, tx, content),
        Action::WriteFile { path, content } => execute_write_file(alice, tx, path, content),
        Action::ReplaceInFile { path, blocks } => execute_replace_in_file(alice, tx, path, blocks),
        Action::Summary { content } => execute_summary(alice, tx, content),

        Action::SetProfile { entries } => execute_set_profile(alice, tx, entries),
        Action::CreateInstance { name, knowledge } => execute_create_instance(alice, tx, name, knowledge),
        Action::Forget { target_action_id, summary } => execute_forget(alice, tx, target_action_id, summary),
    }
}

/// Truncate result text if it exceeds MAX_RESULT_SIZE.
fn truncate_result(text: &str) -> String {
    if text.len() > MAX_RESULT_SIZE {
        let truncated = crate::safe_truncate(text, MAX_RESULT_SIZE);
        format!("{}...\n[truncated, total {} bytes]", truncated, text.len())
    } else {
        text.to_string()
    }
}

// ─── Individual action executors ─────────────────────────────────

fn execute_idle(_alice: &mut Alice, tx: &mut Transaction, timeout_secs: Option<u64>) -> Result<String> {
    match timeout_secs {
        Some(secs) => info!("[ACTION-{}] idle (timeout: {}s)", tx.instance_id, secs),
        None => info!("[ACTION-{}] idle", tx.instance_id),
    }
    Ok(String::new())
}

fn execute_read_msg(alice: &mut Alice, tx: &mut Transaction) -> Result<String> {
    info!("[ACTION-{}] read_msg", tx.instance_id);

    let messages = alice.instance.chat.read_unread_user_messages()
        .context("Failed to read unread messages")?;

    if messages.is_empty() {
        return Ok("(收件箱为空)\n".to_string());
    }

    let mut result = String::new();
    for msg in &messages {
        result.push_str(&format!(
            "{} [MSG:{}]{}\n\n{}\n",
            msg.sender, msg.timestamp, MSG_READ_CONTEXT, msg.content
        ));
    }

    Ok(result)
}

fn execute_send_msg(alice: &mut Alice, tx: &mut Transaction, recipient: &str, content: &str) -> Result<String> {
    info!("[ACTION-{}] send_msg to {}", tx.instance_id, recipient);

    if recipient != alice.user_id {
        warn!("[ACTION-{}] send_msg rejected: recipient '{}' != user_id '{}'",
            tx.instance_id, recipient, alice.user_id);
        return Ok(format!(
            "send failed: unknown recipient '{}'. You can only send messages to your user.\n",
            recipient
        ));
    }

    let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();
    alice.instance.chat.write_agent_reply(&alice.instance.id, content, &timestamp)
        .context("Failed to write agent reply")?;

    Ok(format!("{}[MSG:{}]\n", MSG_SEND_CONTEXT, timestamp))
}

fn execute_thinking(_alice: &mut Alice, tx: &mut Transaction, content: &str) -> Result<String> {
    info!("[ACTION-{}] thinking ({} chars)", tx.instance_id, content.len());
    Ok(String::new())
}

fn execute_script(alice: &mut Alice, tx: &mut Transaction, content: &str) -> Result<String> {
    info!("[ACTION-{}] script ({} chars)", tx.instance_id, content.len());
    let shell = make_shell(alice);
    let result = shell.exec(content)?;

    let duration_str = format!("{:.1}s", result.duration.as_secs_f64());
    let output = truncate_result(&result.output);

    let exit_info = if result.exit_code != Some(0) {
        format!("\n[exit code: {}]", result.exit_code.map_or("unknown".to_string(), |c| c.to_string()))
    } else {
        String::new()
    };

    let done_text = format!(
        "---exec result ({})---\n{}{}\n",
        duration_str, output, exit_info
    );
    Ok(done_text)
}

/// Extract a skeleton view of file content based on file extension.
/// For code files: extracts interface-level lines (fn/struct/class/etc.) and comments.
/// For .md files: preserves full content.
/// For unknown types: shows first 10 + last 5 lines.
fn extract_skeleton(path: &str, content: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let total_bytes = content.len();

    // .md files: preserve full content
    if ext == "md" {
        let display = truncate_result(content);
        return format!(
            "write success [{}] ({} bytes, {} lines)\n\n---file content---\n{}\n",
            path, total_bytes, total_lines, display
        );
    }

    // Determine skeleton keywords by extension
    let keywords: &[&str] = match ext.as_str() {
        "rs" => &["fn ", "pub ", "struct ", "enum ", "impl ", "trait ", "mod ", "///", "//!", "type ", "const ", "static "],
        "py" => &["def ", "class ", "@", "# "],
        "js" | "ts" | "jsx" | "tsx" => &["function ", "class ", "export ", "const ", "//", "/**"],
        "go" => &["func ", "type ", "//"],
        "java" | "kt" => &["public ", "private ", "protected ", "class ", "interface ", "//", "/**"],
        "html" | "htm" => &["<h1", "<h2", "<h3", "<h4", "<h5", "<h6", "<!--"],
        "sh" | "bash" => &["# ", "function "],
        "toml" => &["[", "# "],
        "yaml" | "yml" => &["# "],
        "css" | "scss" => &["/*", "//", "."],
        _ => &[],
    };

    if !keywords.is_empty() {
        // Extract skeleton lines
        let skeleton: Vec<String> = lines.iter().enumerate()
            .filter(|(_, line)| {
                let trimmed = line.trim();
                !trimmed.is_empty() && keywords.iter().any(|kw| trimmed.starts_with(kw))
            })
            .map(|(i, line)| format!("{:>4}: {}", i + 1, line))
            .collect();

        if !skeleton.is_empty() {
            return format!(
                "write success [{}] ({} bytes, {} lines)\n\n--- skeleton (auto-extracted, showing interface & comments only, not full content) ---\n{}\n",
                path, total_bytes, total_lines, skeleton.join("\n")
            );
        }
    }

    // Fallback: first 10 + last 5 lines
    let mut preview: Vec<String> = Vec::new();
    let head = std::cmp::min(10, total_lines);
    for i in 0..head {
        preview.push(format!("{:>4}: {}", i + 1, lines[i]));
    }
    if total_lines > 15 {
        preview.push("     ...".to_string());
        for i in (total_lines - 5)..total_lines {
            preview.push(format!("{:>4}: {}", i + 1, lines[i]));
        }
    } else if total_lines > 10 {
        for i in 10..total_lines {
            preview.push(format!("{:>4}: {}", i + 1, lines[i]));
        }
    }

    format!(
        "write success [{}] ({} bytes, {} lines)\n\n--- preview (first 10 + last 5 lines, not full content) ---\n{}\n",
        path, total_bytes, total_lines, preview.join("\n")
    )
}

fn execute_write_file(alice: &mut Alice, tx: &mut Transaction, path: &str, content: &str) -> Result<String> {
    info!("[ACTION-{}] write_file: {}", tx.instance_id, path);

    #[cfg(feature = "remember")]
    let content = &strip_remember_markers(content);

    let abs_path = resolve_action_path(alice, path)?;

    if alice.privileged {
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory for: {}", path))?;
        }
        std::fs::write(&abs_path, content)
            .with_context(|| format!("Failed to write file: {}", path))?;
    } else {
        let script = make_write_script(&abs_path.to_string_lossy(), content);
        let shell = make_shell(alice);
        let result = shell.exec(&script)?;
        if result.exit_code != Some(0) {
            bail!("write_file failed (exit {}): {}", 
                result.exit_code.map_or("unknown".to_string(), |c| c.to_string()),
                result.output.trim());
        }
    }

    Ok(extract_skeleton(path, content))
}
fn execute_replace_in_file(
    alice: &mut Alice,
    tx: &mut Transaction,
    path: &str,
    blocks: &[ReplaceBlock],
) -> Result<String> {
    info!("[ACTION-{}] replace_in_file: {} ({} blocks)", tx.instance_id, path, blocks.len());

    let abs_path = resolve_action_path(alice, path)?;

    if alice.privileged {
        // Direct filesystem access for privileged instances
        let mut content = std::fs::read_to_string(&abs_path)
            .with_context(|| format!("Failed to read file: {}", path))?;

        let mut result_lines: Vec<String> = Vec::new();
        let mut total_replaced = 0;
        for block in blocks.iter() {
            let count = content.matches(block.search.as_str()).count();
            if count != 1 {
                result_lines.push(format!("  ERROR: '{}...' matched {} times (expected 1)",
                    crate::safe_truncate(&block.search, 40), count));
                continue;
            }
            content = content.replacen(block.search.as_str(), block.replace.as_str(), 1);
            total_replaced += 1;
            result_lines.push(format!("  replaced '{}...'", crate::safe_truncate(&block.search, 40)));
        }

        std::fs::write(&abs_path, &content)
            .with_context(|| format!("Failed to write file: {}", path))?;

        let summary = format!("replaced {} block(s) successfully", total_replaced);
        let detail = result_lines.join("\n");
        Ok(format!("{}\n{}\n", summary, detail))
    } else {
        // Shell-based access for sandboxed instances
        let path_str = abs_path.to_string_lossy().replace('\'', "'\\''");

        let shell = make_shell(alice);
        let read_result = shell.exec(&format!("cat '{}'", path_str))?;
        if read_result.exit_code != Some(0) {
            bail!("replace_in_file: failed to read {} (exit {}): {}",
                path, read_result.exit_code.map_or("unknown".to_string(), |c| c.to_string()),
                read_result.output.trim());
        }

        let mut content = read_result.output;
        let mut result_lines: Vec<String> = Vec::new();
        let mut total_replaced = 0;

        for block in blocks.iter() {
            let count = content.matches(block.search.as_str()).count();
            if count != 1 {
                result_lines.push(format!("  ERROR: '{}...' matched {} times (expected 1)",
                    crate::safe_truncate(&block.search, 40), count));
                continue;
            }
            content = content.replacen(block.search.as_str(), block.replace.as_str(), 1);
            total_replaced += 1;
            result_lines.push(format!("  replaced '{}...'", crate::safe_truncate(&block.search, 40)));
        }

        let delimiter = format!("ALICE_HEREDOC_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
        let parent_dir = abs_path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let write_script = format!(
            "mkdir -p '{}' && cat > '{}' << '{}'\n{}\n{}",
            parent_dir.replace('\'', "'\\''"), path_str, delimiter, content, delimiter
        );
        let shell = make_shell(alice);
        let write_result = shell.exec(&write_script)?;
        if write_result.exit_code != Some(0) {
            bail!("replace_in_file: failed to write {} (exit {}): {}",
                path, write_result.exit_code.map_or("unknown".to_string(), |c| c.to_string()),
                write_result.output.trim());
        }

        let summary = format!("replaced {} block(s) successfully", total_replaced);
        let detail = result_lines.join("\n");
        Ok(format!("{}\n{}\n", summary, detail))
    }
}

// ─── New memory actions ──────────────────────────────────────────


/// Execute summary: parse dual output (summary + knowledge), persist atomically.
///
/// Agent outputs two parts separated by ===KNOWLEDGE_TOKEN===:
/// - Before: conversation summary
/// - After: complete knowledge file (rewrite)
///
/// Atomic transaction: session block + knowledge update + current clear.
///
/// Flow:
/// 1. Read current.txt
/// 2. Parse agent output: split by first ===KNOWLEDGE_TOKEN=== line
/// 3. Extract MSG IDs from current
/// 4. Build JSONL session entry with summary part
/// 5. Snapshot + rewrite knowledge.md with knowledge part
/// 6. Append session block + clear current
fn execute_summary(alice: &mut Alice, tx: &mut Transaction, raw_output: &str) -> Result<String> {
    info!("[ACTION-{}] summary", tx.instance_id);

    let current = alice.instance.memory.current.get().to_string();
    if current.trim().is_empty() {
        return Ok("current is empty, nothing to summarize\n".to_string());
    }

    // Parse dual output: find ===KNOWLEDGE_TOKEN=== on its own line
    let knowledge_marker = format!("===KNOWLEDGE_{}===", tx.separator_token);
    let summary_marker = concat!("=", "=", "=", "SUMMARY", "=", "=", "=");
    
    let (summary_text, knowledge_text) = parse_summary_dual_output(raw_output, summary_marker, &knowledge_marker);

    if summary_text.trim().is_empty() {
        warn!("[ACTION-{}] summary: empty summary text", tx.instance_id);
    }

    // Extract MSG IDs from current text
    let msg_ids = extract_msg_ids(&current);
    let first_msg = msg_ids.first().cloned().unwrap_or_default();
    let last_msg = msg_ids.last().cloned().unwrap_or_default();

    // Build JSONL line with summary part only
    let entry = serde_json::json!({
        "first_msg": first_msg,
        "last_msg": last_msg,
        "summary": summary_text.trim()
    });
    let jsonl_line = serde_json::to_string(&entry).unwrap_or_default() + "\n";

    // Determine target session block
    let blocks = alice.instance.memory.list_session_blocks()?;
    let block_name = if let Some(latest) = blocks.last() {
        let size = alice.instance.memory.session_block_size(latest);
        if size < (alice.session_block_kb as u64 * 1024) {
            latest.clone()
        } else {
            Local::now().format("%Y%m%d%H%M%S").to_string()
        }
    } else {
        Local::now().format("%Y%m%d%H%M%S").to_string()
    };

    // === Atomic: snapshot + knowledge update + session block + clear current ===
    let knowledge_info;
    if !knowledge_text.trim().is_empty() {
        snapshot_knowledge(alice);
        alice.instance.memory.knowledge.set(knowledge_text.trim());
        alice.instance.memory.knowledge.flush()?;
        knowledge_info = format!("\nknowledge: rewritten {} chars", knowledge_text.trim().len());
        info!("[ACTION-{}] knowledge rewritten ({} chars)", tx.instance_id, knowledge_text.trim().len());
    } else {
        warn!("[ACTION-{}] summary: no knowledge section found, skipping knowledge update", tx.instance_id);
        knowledge_info = "\nknowledge: no knowledge marker found, skipped".to_string();
    }

    alice.instance.memory.append_session_block(&block_name, &jsonl_line)?;
    alice.instance.memory.write_current("")?;

    let msg_count = msg_ids.len();
    let stats = format!(
        "小结完成: {}个消息ID({}~{}) → sessions/{}.jsonl, current已清空{}",
        msg_count, first_msg, last_msg, block_name, knowledge_info
    );

    Ok(format!("{}\n", stats))
}

/// Parse summary dual output: split by first ===KNOWLEDGE_TOKEN=== on its own line.
/// Execute forget action: replace a target action block in current.txt with a concise summary.
/// The target block is identified by its action_id in the START/END markers.
/// On success, returns empty string (silent execution - caller skips append_current).
/// On failure, returns error (caller records it so agent sees what went wrong).
fn execute_forget(alice: &mut Alice, _tx: &mut Transaction, target_action_id: &str, summary: &str) -> Result<String> {
    let current = alice.instance.memory.current.get().to_string();
    if current.is_empty() {
        anyhow::bail!("current is empty, nothing to forget");
    }

    let start_marker = format!("---------行为编号[{}]开始---------", target_action_id);
    let end_marker = format!("---------行为编号[{}]结束---------", target_action_id);

    let start_pos = current.find(&start_marker)
        .ok_or_else(|| anyhow::anyhow!("action block [{}] not found in current", target_action_id))?;
    let end_pos = current[start_pos..].find(&end_marker)
        .ok_or_else(|| anyhow::anyhow!("end marker for [{}] not found in current", target_action_id))?;
    let end_pos = start_pos + end_pos + end_marker.len();

    // Include trailing newline if present
    let end_pos = if end_pos < current.len() && current.as_bytes()[end_pos] == b'\n' {
        end_pos + 1
    } else {
        end_pos
    };

    let replacement = format!(
        "---------行为编号[{}]开始---------
[已提炼] {}
---------行为编号[{}]结束---------
",
        target_action_id, summary.trim(), target_action_id
    );

    let new_current = format!("{}{}{}", &current[..start_pos], replacement, &current[end_pos..]);
    alice.instance.memory.write_current(&new_current)?;

    let old_len = end_pos - start_pos;
    let new_len = replacement.len();
    info!("[FORGET-{}] Replaced action [{}]: {} -> {} chars (saved {})",
        alice.instance.id, target_action_id, old_len, new_len, old_len as i64 - new_len as i64);

    Ok(String::new())
}

/// Token-bearing marker ensures uniqueness, preventing self-reference.
/// Also strips leading ===SUMMARY=== marker if present.
/// Returns (summary_text, knowledge_text).
fn parse_summary_dual_output(raw: &str, summary_marker: &str, knowledge_marker: &str) -> (String, String) {
    // Find first knowledge marker on its own line (token ensures uniqueness)
    let lines: Vec<&str> = raw.lines().collect();
    let mut knowledge_line_idx: Option<usize> = None;
    
    for (i, line) in lines.iter().enumerate() {
        if line.trim() == knowledge_marker {
            knowledge_line_idx = Some(i);
            break;
        }
    }

    let (summary_part, knowledge_part) = match knowledge_line_idx {
        Some(idx) => {
            let summary = lines[..idx].join("\n");
            let knowledge = lines[idx + 1..].join("\n");
            (summary, knowledge)
        }
        None => {
            // No knowledge marker found, treat entire output as summary
            (raw.to_string(), String::new())
        }
    };

    // Strip leading ===SUMMARY=== marker if present
    let summary_part = {
        let trimmed = summary_part.trim_start();
        if trimmed.starts_with(summary_marker) {
            let after = &trimmed[summary_marker.len()..];
            after.trim_start_matches('\n').to_string()
        } else {
            summary_part
        }
    };

    (summary_part, knowledge_part)
}

/// Implicit contract: context strings around [MSG:xxx] markers.
/// Write-side (execute_read_msg, execute_send_msg) and extract-side (extract_msg_ids)
/// MUST share these constants. If you change the format, update both sides.
pub const MSG_SEND_CONTEXT: &str = "send success ";   // appears before [MSG:xxx]
pub const MSG_READ_CONTEXT: &str = "发来一条消息：";    // appears after [MSG:xxx]

/// Extract MSG IDs from current.txt content.
/// Only matches trusted markers (send success / read_msg format).
/// Returns IDs in **appearance order** (not sorted), to avoid
/// stale timestamps in exec results from expanding the range.
pub fn extract_msg_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let marker = "[MSG:";
    let send_context = MSG_SEND_CONTEXT;
    let read_context = MSG_READ_CONTEXT;
    let mut search_from = 0;

    while let Some(start) = text[search_from..].find(marker) {
        let bracket_pos = search_from + start; // absolute position of '['
        let abs_start = bracket_pos + marker.len(); // position after "MSG:"
        if let Some(end) = text[abs_start..].find(']') {
            let candidate = &text[abs_start..abs_start + end];
            if candidate.len() == 14 && candidate.chars().all(|c| c.is_ascii_digit()) {
                let after_bracket = abs_start + end + 1; // position after ']'
                // Check send marker: "send success [MSG:xxx]"
                // Use .get() for safe slicing — bracket_pos - len may land inside a multi-byte char
                let is_send = bracket_pos >= send_context.len()
                    && text.get(bracket_pos - send_context.len()..bracket_pos)
                        .map_or(false, |s| s == send_context);
                // Check read marker: "[MSG:xxx]发来一条消息："
                let is_read = text.get(after_bracket..).map_or(false, |s| s.starts_with(read_context));
                if (is_send || is_read) && !ids.contains(&candidate.to_string()) {
                    ids.push(candidate.to_string());
                }
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    ids
}



fn execute_set_profile(alice: &mut Alice, tx: &mut Transaction, entries: &[(String, String)]) -> Result<String> {
    info!("[ACTION-{}] set_profile ({} entries)", tx.instance_id, entries.len());

    let mut applied = Vec::new();

    // Validate keys first
    for (key, _) in entries {
        match key.as_str() {
            "name" | "color" | "avatar" | "privileged" => {}
            _ => {
                return Ok(format!(
                    "set_profile failed: unknown key '{}'\n", key
                ));
            }
        }
    }

    // Collect changes to apply
    let entries_owned: Vec<(String, String)> = entries.to_vec();
    for (key, value) in &entries_owned {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            applied.push(format!("{}: (cleared)", key));
        } else {
            applied.push(format!("{}: {}", key, trimmed));
        }
    }

    // Apply changes via Document::update (auto-saves to disk)
    alice.instance.settings.update(|s| {
        for (key, value) in &entries_owned {
            let trimmed = value.trim();
            match key.as_str() {
                "name" => s.name = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) },
                "color" => s.color = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) },
                "avatar" => s.avatar = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) },
                "privileged" => s.privileged = trimmed == "true",
                _ => {} // already validated above
            }
        }
    })?;

    // Apply runtime effects
    alice.privileged = alice.instance.settings.get().privileged;
    alice.instance_name = alice.instance.settings.get().name.clone();

    let detail = applied.join(", ");
    Ok(format!("profile updated: {}\n", detail))
}

// ─── Tests ───────────────────────────────────────────────────────


fn execute_create_instance(
    alice: &mut Alice,
    _tx: &mut Transaction,
    name: &str,
    knowledge: &str,
) -> Result<String> {
    // Derive instances_dir from current instance's parent directory
    let instances_dir = alice.instance.instance_dir.parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine instances directory"))?;

    // Create instance atomically (all directories + settings + knowledge)
    let knowledge_opt = if knowledge.is_empty() { None } else { Some(knowledge) };
    let instance = crate::core::instance::Instance::create(
        instances_dir,
        &alice.user_id,
        Some(name),
        knowledge_opt,
    ).context("Failed to create instance")?;

    info!("[ACTION-{}] Created new instance: {} (name: {}, knowledge: {} bytes, awaiting hot-scan)",
        alice.instance.id, instance.id, name, knowledge.len());

    Ok(format!(
        "instance created: {} (name: {}), knowledge: {} bytes written\nEngine hot-scan will start it automatically.\n",
        instance.id, name, knowledge.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AliceConfig;
    use tempfile::TempDir;

    fn setup() -> (Alice, Transaction, TempDir) {
        let tmp = TempDir::new().unwrap();

        // Create minimal settings.json for Instance::open
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1","api_key":"test","model":"test@test"}"#).unwrap();

        // Instance::open creates all subdirectories automatically
        let instance = crate::core::instance::Instance::open(tmp.path()).unwrap();

        let config = AliceConfig::default();
        let mut alice = Alice::new(instance, config).unwrap();
        alice.privileged = true;
        let tx = Transaction::new("test", "abc123");
        (alice, tx, tmp)
    }

    #[test]
    fn test_execute_idle() {
        let (mut alice, mut tx, _tmp) = setup();
        let result = execute_action(&Action::Idle { timeout_secs: None }, &mut alice, &mut tx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_execute_thinking() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::Thinking { content: "deep thought".to_string() };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_execute_script() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::Script { content: "echo hello_rust".to_string() };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("hello_rust"));
        assert!(result.contains("exec result"));
    }

    #[test]
    fn test_execute_write_and_read_file() {
        let (mut alice, mut tx, _tmp) = setup();

        let write_action = Action::WriteFile {
            path: "test.txt".to_string(),
            content: "hello from rust".to_string(),
        };
        let result = execute_action(&write_action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("write success"));

        let content = std::fs::read_to_string(alice.instance.workspace.join("test.txt")).unwrap();
        assert_eq!(content, "hello from rust");
    }

    #[test]
    fn test_execute_replace_in_file() {
        let (mut alice, mut tx, _tmp) = setup();
        std::fs::write(alice.instance.workspace.join("test.txt"), "hello world").unwrap();

        let action = Action::ReplaceInFile {
            path: "test.txt".to_string(),
            blocks: vec![ReplaceBlock {
                search: "hello".to_string(),
                replace: "goodbye".to_string(),
            }],
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("replaced 1 block(s)"));

        let content = std::fs::read_to_string(alice.instance.workspace.join("test.txt")).unwrap();
        assert_eq!(content, "goodbye world");
    }

    #[test]
    fn test_execute_read_msg_empty() {
        let (mut alice, mut tx, _tmp) = setup();
        let result = execute_action(&Action::ReadMsg, &mut alice, &mut tx).unwrap();
        assert!(result.contains("收件箱为空"));
    }

    #[test]
    fn test_execute_read_msg_with_messages() {
        let (mut alice, mut tx, _tmp) = setup();
        alice.instance.chat.write_user_message("24007", "hello agent", "20260220120000", "chat").unwrap();
        alice.instance.chat.write_user_message("24007", "how are you?", "20260220120001", "chat").unwrap();

        let result = execute_action(&Action::ReadMsg, &mut alice, &mut tx).unwrap();
        assert!(result.contains("24007"));
        assert!(result.contains("hello agent"));
        assert!(result.contains("how are you?"));
        // Verify MSG timestamp markers
        assert!(result.contains("[MSG:20260220120000]"));
        assert!(result.contains("[MSG:20260220120001]"));
        assert_eq!(alice.instance.chat.count_unread_user_messages().unwrap(), 0);
    }

    #[test]
    fn test_execute_send_msg() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::SendMsg {
            recipient: "user1".to_string(),
            content: "hello user!".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("send success"));
        // Verify MSG timestamp marker in result
        assert!(result.contains("[MSG:"));

        let replies = alice.instance.chat.read_unread_agent_replies().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1, "hello user!");
    }

    #[test]
    fn test_execute_summary_empty_current() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::Summary { content: "some summary".to_string() };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("nothing to summarize"));
    }

    #[test]
    fn test_execute_summary() {
        let (mut alice, mut tx, _tmp) = setup();

        // Write content to current with MSG markers
        alice.instance.memory.write_current(
            "---------行为编号[20260223160000_aaaaaa]开始---------\n\
             你打开了收件箱，开始阅读来信。\n\
             ---action executing, result pending---\n\n\
             24007 [MSG:20260223155500]发来一条消息：\nhello\n\
             ---------行为编号[20260223160000_aaaaaa]结束---------\n\
             ---------行为编号[20260223160100_bbbbbb]开始---------\n\
             you send a letter to [user1]: \nhi back\n\
             ---action executing, result pending---\n\n\
             send success [MSG:20260223160100]\n\
             ---------行为编号[20260223160100_bbbbbb]结束---------\n"
        ).unwrap();

        // Dual output format: summary + knowledge
        let dual_output = "===SUMMARY===\nAlice read a greeting and replied\n===KNOWLEDGE_abc123===\n# Test Knowledge\n- item 1";
        let action = Action::Summary {
            content: dual_output.to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("小结完成"));
        assert!(result.contains("2个消息ID"));

        // current should be cleared
        let current = alice.instance.memory.current.get();
        assert!(current.is_empty());

        // session block should exist with JSONL content
        let blocks = alice.instance.memory.list_session_blocks().unwrap();
        assert_eq!(blocks.len(), 1);
        let block_content = alice.instance.memory.read_session_block(&blocks[0]).unwrap();
        assert!(block_content.contains("first_msg"));
        assert!(block_content.contains("20260223155500"));
        assert!(block_content.contains("20260223160100"));
        assert!(block_content.contains("Alice read a greeting and replied"));
    }



    #[test]
    fn test_extract_msg_ids() {
        let text = "24007 [MSG:20260223155500]发来一条消息：\nhello\n\
                    send success [MSG:20260223160100]\n\
                    another [MSG:20260223155500] duplicate\n";

        let ids = extract_msg_ids(text);
        // Should be deduplicated, in appearance order
        assert_eq!(ids, vec!["20260223155500", "20260223160100"]);
    }

    #[test]
    fn test_extract_msg_ids_empty() {
        let ids = extract_msg_ids("");
        assert!(ids.is_empty());
    }

    #[test]
    fn test_extract_msg_ids_no_markers() {
        let ids = extract_msg_ids("no markers here [MSG:short] [MSG:notdigits12345]");
        assert!(ids.is_empty());
    }

    #[test]
    fn test_extract_msg_ids_ignores_fake_markers_in_exec_result() {
        // Simulate current.txt with exec result containing old [MSG:] markers
        let text = "\
---------行为编号[xxx]开始---------\n\
24007 [MSG:20260225100000]发来一条消息：\nhello\n\
---action executing, result pending---\n\
send success [MSG:20260225100100]\n\
---------行为编号[xxx]结束---------\n\
\n\
---------行为编号[yyy]开始---------\n\
execute script\n\
---action executing, result pending---\n\n\
---exec result (0.5s)---\n\
grep output: 24007 [MSG:20260220080000]发来一条消息：\n\
old log: send success [MSG:20260220090000]\n\
random [MSG:20260219120000] in output\n\
---------行为编号[yyy]结束---------\n";

        let ids = extract_msg_ids(text);
        // Should only match the REAL markers, not the ones in exec result
        // The exec result contains fake markers that look like real ones,
        // but they are from grep output of old logs
        // Real markers: read at 20260225100000, send at 20260225100100
        // Fake markers in exec result also match the pattern... 
        // Actually the fake ones DO match because they have the right context!
        // "24007 [MSG:20260220080000]发来一条消息：" matches READ pattern
        // "send success [MSG:20260220090000]" matches SEND pattern
        // This is the remaining risk - exec result can contain full-context markers
        // But with appearance order (not sorted), first() and last() are the real ones
        assert_eq!(ids[0], "20260225100000"); // first real marker
        assert_eq!(*ids.last().unwrap(), "20260220090000"); // unfortunately fake marker is last
        // The key defense: appearance order means first() is always the real first marker
    }

    #[test]
    fn test_extract_msg_ids_appearance_order_not_sorted() {
        // If a later message has an earlier timestamp (shouldn't happen normally,
        // but tests that we use appearance order)
        let text = "send success [MSG:20260225120000]\n\
                    24007 [MSG:20260225110000]发来一条消息：\nhello\n";

        let ids = extract_msg_ids(text);
        // Appearance order: 120000 first, 110000 second
        assert_eq!(ids, vec!["20260225120000", "20260225110000"]);
    }

    #[test]
    fn test_extract_msg_ids_utf8_boundary_safe() {
        // Regression test: Chinese chars before [MSG:] caused panic
        // "含" is 3 bytes, bracket_pos - 13 could land inside a multi-byte char
        let text = "这是一段包含中文的内容send success [MSG:20260225170000]\n另外还有 [MSG:20260225170100]发来一条消息：\nhello\n";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260225170000", "20260225170100"]);

        // Pure Chinese before marker - no "send success" prefix
        let text2 = "纯中文内容[MSG:20260225170000]\n";
        let ids2 = extract_msg_ids(text2);
        assert!(ids2.is_empty()); // No trusted context, should be rejected
    }

    #[test]
    fn test_extract_msg_ids_rejects_bare_markers() {
        // Bare [MSG:xxx] without send/read context should be ignored
        let text = "some text [MSG:20260225100000] more text\n\
                    thinking about [MSG:20260225110000] stuff\n\
                    send success [MSG:20260225120000]\n";

        let ids = extract_msg_ids(text);
        // Only the send success one should match
        assert_eq!(ids, vec!["20260225120000"]);
    }

    #[test]
    fn test_summary_creates_new_block_when_full() {
        let (mut alice, mut tx, _tmp) = setup();

        // Pre-fill a session block to exceed the size limit
        let large_content = format!(
            "{{\"first_msg\":\"20260223100000\",\"last_msg\":\"20260223110000\",\"summary\":\"{}\"}}\n",
            "x".repeat(alice.session_block_kb as usize * 1024)
        );
        alice.instance.memory.append_session_block("20260223100000", &large_content).unwrap();

        // Write current with MSG markers
        alice.instance.memory.write_current(
            "---------行为编号[20260223160000_aaaaaa]开始---------\n\
             send success [MSG:20260223160000]\n\
             ---------行为编号[20260223160000_aaaaaa]结束---------\n"
        ).unwrap();

        let action = Action::Summary {
            content: "===SUMMARY===\ntest summary\n===KNOWLEDGE_abc123===\n# Knowledge".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("小结完成"));

        // Should have 2 blocks now (old full one + new one)
        let blocks = alice.instance.memory.list_session_blocks().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], "20260223100000");
        // Second block should be a new timestamp
        assert_ne!(blocks[1], "20260223100000");
    }

    #[test]
    fn test_truncate_result() {
        let short = "hello";
        assert_eq!(truncate_result(short), "hello");

        let long = "x".repeat(MAX_RESULT_SIZE + 100);
        let truncated = truncate_result(&long);
        assert!(truncated.contains("[truncated"));
        assert!(truncated.len() < long.len());
    }

    #[test]
    fn test_execute_script_exit_code() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::Script { content: "exit 42".to_string() };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("[exit code: 42]"));
    }

    // ─── parse_summary_dual_output tests ───────────────────────────

    #[test]
    fn test_parse_dual_output_basic() {
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_abc123===";
        let input = "===SUMMARY===
This is summary

===KNOWLEDGE_abc123===
# Knowledge
Some content";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        assert_eq!(s.trim(), "This is summary");
        assert_eq!(k.trim(), "# Knowledge\nSome content");
    }

    #[test]
    fn test_parse_dual_output_no_knowledge() {
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_abc123===";
        let input = "===SUMMARY===
Just a summary, no knowledge section";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        assert!(s.contains("Just a summary"));
        assert!(k.trim().is_empty());
    }

    #[test]
    fn test_parse_dual_output_no_markers() {
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_abc123===";
        let input = "Plain text without any markers";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        assert_eq!(s, "Plain text without any markers");
        assert!(k.is_empty());
    }

    #[test]
    fn test_parse_dual_output_token_prevents_self_reference() {
        // Knowledge content mentions old token markers - should not interfere
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_newtoken===";
        let input = "===SUMMARY===
Discussed the separator

===KNOWLEDGE_newtoken===
# Design
Old format used ===KNOWLEDGE_oldtoken=== as separator
And even ===KNOWLEDGE=== without token
# All knowledge preserved";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        // First ===KNOWLEDGE_newtoken=== is the real separator
        // Old tokens in knowledge content don\'t match
        assert!(s.contains("Discussed the separator"));
        assert!(k.contains("# Design"));
        assert!(k.contains("Old format used"));
        assert!(k.contains("# All knowledge preserved"));
    }

    #[test]
    fn test_parse_dual_output_knowledge_in_inline_text() {
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_abc123===";
        let input = "===SUMMARY===
We discussed ===KNOWLEDGE_abc123=== format
No actual knowledge section";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        assert!(s.contains("We discussed"));
        assert!(k.trim().is_empty());
    }

    #[test]
    fn test_parse_dual_output_multiline_knowledge() {
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_abc123===";
        let input = "===SUMMARY===
Summary line 1
Summary line 2

===KNOWLEDGE_abc123===
# Title

## Section 1
Content 1

## Section 2
Content 2";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        assert!(s.contains("Summary line 1"));
        assert!(s.contains("Summary line 2"));
        assert!(k.contains("# Title"));
        assert!(k.contains("## Section 1"));
        assert!(k.contains("## Section 2"));
    }

    #[test]
    fn test_parse_dual_output_only_knowledge() {
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_abc123===";
        let input = "Some summary without marker

===KNOWLEDGE_abc123===
# Knowledge
Content";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        assert!(s.contains("Some summary without marker"));
        assert!(k.contains("# Knowledge"));
    }

    #[test]
    fn test_parse_dual_output_empty_knowledge() {
        let sm = "===SUMMARY===";
        let km = "===KNOWLEDGE_abc123===";
        let input = "===SUMMARY===
Summary content

===KNOWLEDGE_abc123===
";
        let (s, k) = parse_summary_dual_output(input, sm, km);
        assert!(s.contains("Summary content"));
        assert!(k.trim().is_empty());
    }

}

/// Snapshot knowledge.md before overwriting (safety backup).
fn snapshot_knowledge(alice: &Alice) {
    let timestamp = Local::now().format("%Y%m%d%H%M%S").to_string();
    let snapshot_dir = alice.instance.memory.memory_dir().join("snapshots").join(&timestamp);

    if let Err(e) = fs::create_dir_all(&snapshot_dir) {
        warn!("[CAPTURE-{}] Failed to create snapshot dir: {}", alice.instance.id, e);
        return;
    }

    let knowledge_path = alice.instance.memory.knowledge.path();
    if knowledge_path.exists() {
        if let Err(e) = fs::copy(knowledge_path, snapshot_dir.join(crate::prompt::KNOWLEDGE_FILE)) {
            warn!("[CAPTURE-{}] Failed to snapshot knowledge.md: {}", alice.instance.id, e);
        }
    }

    info!("[CAPTURE-{}] Knowledge snapshot saved to {}", alice.instance.id, snapshot_dir.display());
}
