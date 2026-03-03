//! # Action Execution
//!
//! Dispatches parsed actions to their concrete execution logic.
//! Each action variant maps to a specific operation on Alice's resources.
//!
//! @TRACE: ACTION

use anyhow::{Result, bail, Context};
use tracing::{info, warn};
#[cfg(feature = "remember")]
use crate::inference::strip_remember_markers;

use std::path::PathBuf;

use crate::core::{Alice, Transaction};
use crate::external::shell::Shell;
use crate::inference::{Action, ReplaceBlock};
use crate::policy::action_output as out;

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


/// Create a Shell instance with appropriate sandboxing for the given Alice.
/// In local mode (no sandbox user), runs without sandboxing.
fn make_shell(alice: &Alice) -> Shell {
    let sandbox_user = if alice.privileged {
        None
    } else {
        Shell::detect_sandbox_user(&alice.instance.id)
    };
    Shell::new(alice.instance.workspace.clone(), sandbox_user)
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
        Action::Summary { content, knowledge } => execute_summary(alice, tx, content, knowledge.clone()),

        Action::SetProfile { update } => execute_set_profile(alice, tx, &update),
        Action::CreateInstance { name, knowledge } => execute_create_instance(alice, tx, name, knowledge),
        Action::Forget { target_action_id, summary } => execute_forget(alice, tx, target_action_id, summary),
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
        return Ok(out::inbox_empty());
    }

    let mut result = String::new();
    for msg in &messages {
        result.push_str(&out::read_msg_entry(&msg.sender, &msg.timestamp, &msg.content));
    }

    Ok(result)
}

fn execute_send_msg(alice: &mut Alice, tx: &mut Transaction, recipient: &str, content: &str) -> Result<String> {
    info!("[ACTION-{}] send_msg to {}", tx.instance_id, recipient);

    if recipient != alice.user_id {
        warn!("[ACTION-{}] send_msg rejected: recipient '{}' != user_id '{}'",
            tx.instance_id, recipient, alice.user_id);
        return Ok(out::send_failed_unknown_recipient(recipient));
    }

    let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
    alice.instance.chat.write_agent_reply(&alice.instance.id, content, &timestamp)
        .context("Failed to write agent reply")?;

    Ok(out::send_success(&timestamp))
}

fn execute_thinking(_alice: &mut Alice, tx: &mut Transaction, content: &str) -> Result<String> {
    info!("[ACTION-{}] thinking ({} chars)", tx.instance_id, content.len());
    Ok(String::new())
}

fn execute_script(alice: &mut Alice, tx: &mut Transaction, content: &str) -> Result<String> {
    info!("[ACTION-{}] script ({} chars)", tx.instance_id, content.len());
    let shell = make_shell(alice);
    let result = shell.exec(content)?;

    let output = out::truncate_result(&result.output);
    Ok(out::script_result(result.duration.as_secs_f64(), &output, result.exit_code))
}

/// Extract a skeleton view of file content based on file extension.
/// Delegates to SkeletonConfig for language-aware extraction logic.
fn extract_skeleton(path: &str, content: &str) -> String {
    use crate::external::{SkeletonConfig, ExtractionResult};

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let total_bytes = content.len();

    match SkeletonConfig::get().extract_from_path(path, content) {
        ExtractionResult::Full => {
            let display = out::truncate_result(content);
            out::write_success_full(path, total_bytes, total_lines, &display)
        }
        ExtractionResult::Skeleton(skeleton) => {
            out::write_success_skeleton(path, total_bytes, total_lines, &skeleton.join("\n"))
        }
        ExtractionResult::NoRule => {
            let preview = out::format_preview(&lines);
            out::write_success_preview(path, total_bytes, total_lines, &preview)
        }
    }
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
        let shell = make_shell(alice);
        let result = shell.write_file(&abs_path.to_string_lossy(), content)?;
        if !result.success() {
            bail!("write_file failed (exit {}): {}", 
                result.exit_code_display(),
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
        for block in blocks.iter() {
            let truncated = crate::safe_truncate(&block.search, out::truncate_display_limit());
            match crate::util::replace_once(&content, block.search.as_str(), block.replace.as_str()) {
                Ok(new_content) => {
                    content = new_content;
                    result_lines.push(out::replace_block_success(&truncated));
                }
                Err(count) => {
                    result_lines.push(out::replace_match_error(&truncated, count));
                }
            }
        }

        std::fs::write(&abs_path, &content)
            .with_context(|| format!("Failed to write file: {}", path))?;

        Ok(out::replace_result(&result_lines))
    } else {
        // Shell-based access for sandboxed instances
        let shell = make_shell(alice);
        let read_result = shell.read_file(&abs_path.to_string_lossy())?;
        if !read_result.success() {
            bail!("replace_in_file: failed to read {} (exit {}): {}",
                path, read_result.exit_code_display(),
                read_result.output.trim());
        }

        let mut content = read_result.output;
        let mut result_lines: Vec<String> = Vec::new();

        for block in blocks.iter() {
            let truncated = crate::safe_truncate(&block.search, out::truncate_display_limit());
            match crate::util::replace_once(&content, block.search.as_str(), block.replace.as_str()) {
                Ok(new_content) => {
                    content = new_content;
                    result_lines.push(out::replace_block_success(&truncated));
                }
                Err(count) => {
                    result_lines.push(out::replace_match_error(&truncated, count));
                }
            }
        }

        let shell = make_shell(alice);
        let write_result = shell.write_file(&abs_path.to_string_lossy(), &content)?;
        if !write_result.success() {
            bail!("replace_in_file: failed to write {} (exit {}): {}",
                path, write_result.exit_code_display(),
                write_result.output.trim());
        }

        Ok(out::replace_result(&result_lines))
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
/// Execute summary: build session entry, commit to memory, update knowledge.
fn execute_summary(alice: &mut Alice, tx: &mut Transaction, raw_output: &str, knowledge: Option<String>) -> Result<String> {
    use crate::persist::SessionBlockEntry;

    info!("[ACTION-{}] summary", tx.instance_id);

    let current = alice.instance.memory.current.read().unwrap();
    if current.trim().is_empty() {
        return Ok(out::summary_empty());
    }

    let summary_text = raw_output;

    if summary_text.trim().is_empty() {
        warn!("[ACTION-{}] summary: empty summary text", tx.instance_id);
    }

    // Extract MSG IDs from current text
    let msg_ids = crate::inference::beat::extract_msg_ids(&current);
    let first_msg = msg_ids.first().cloned().unwrap_or_default();
    let last_msg = msg_ids.last().cloned().unwrap_or_default();

    // Build typed session entry
    let entry = SessionBlockEntry {
        first_msg: first_msg.clone(),
        last_msg: last_msg.clone(),
        summary: summary_text.trim().to_string(),
    };

    // Prepare knowledge text (Some non-empty → write, otherwise skip)
    let knowledge_text = knowledge.as_deref()
        .filter(|k| !k.trim().is_empty())
        .map(|k| k.trim());

    let knowledge_info = if knowledge_text.is_some() {
        let k = knowledge_text.unwrap();
        info!("[ACTION-{}] knowledge rewritten ({} chars)", tx.instance_id, k.len());
        out::knowledge_rewritten(k.len())
    } else {
        if knowledge.is_some() {
            warn!("[ACTION-{}] summary: empty knowledge section, skipping knowledge update", tx.instance_id);
        } else {
            warn!("[ACTION-{}] summary: no knowledge section found, skipping knowledge update", tx.instance_id);
        }
        out::knowledge_skipped()
    };

    // Commit: session block + optional knowledge + clear current
    let block_name = alice.instance.memory.commit_summary(
        &entry,
        alice.session_block_kb,
        knowledge_text,
    )?;

    let msg_count = msg_ids.len();
    Ok(out::summary_complete(msg_count, &first_msg, &last_msg, &block_name, &knowledge_info))
}

/// Parse summary dual output: split by first ===KNOWLEDGE_TOKEN=== on its own line.
/// Execute forget action: replace a target action block in current.txt with a concise summary.
/// The target block is identified by its action_id in the START/END markers.
/// On success, returns empty string (silent execution - caller skips append_current).
/// On failure, returns error (caller records it so agent sees what went wrong).
fn execute_forget(alice: &mut Alice, _tx: &mut Transaction, target_action_id: &str, summary: &str) -> Result<String> {
    let (old_len, new_len) = alice.instance.memory.replace_action_block(target_action_id, summary.trim())?;

    info!("[FORGET-{}] Replaced action [{}]: {} -> {} chars (saved {})",
        alice.instance.id, target_action_id, old_len, new_len, old_len as i64 - new_len as i64);

    Ok(String::new())
}

fn execute_set_profile(alice: &mut Alice, tx: &mut Transaction, update: &alice_rpc::SettingsUpdate) -> Result<String> {
    info!("[ACTION-{}] set_profile", tx.instance_id);

    let mut settings = alice.instance.settings.load()?;
    update.apply_to(&mut settings);
    alice.instance.settings.save(&settings)?;

    // Apply runtime effects
    alice.privileged = settings.privileged;
    alice.instance_name = settings.name.clone();

    Ok(out::profile_updated(&update))
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

    // Create instance atomically via InstanceStore
    let knowledge_opt = if knowledge.is_empty() { None } else { Some(knowledge) };
    let store = crate::persist::instance::InstanceStore::new(instances_dir.to_path_buf());
    let instance = store.create(
        &alice.user_id,
        Some(name),
        knowledge_opt,
    ).context("Failed to create instance")?;

    info!("[ACTION-{}] Created new instance: {} (name: {}, knowledge: {} bytes, awaiting hot-scan)",
        alice.instance.id, instance.id, name, knowledge.len());

    Ok(out::instance_created(&instance.id, name, knowledge.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn setup() -> (Alice, Transaction, TempDir) {
        let tmp = TempDir::new().unwrap();

        // Create minimal settings.json for Instance::open
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1","api_key":"test","model":"test@test"}"#).unwrap();

        // Instance::open creates all subdirectories automatically
        let instance = crate::persist::instance::Instance::open(tmp.path()).unwrap();

        let env_config = std::sync::Arc::new(crate::policy::EnvConfig::from_env());
        let mut alice = Alice::new(instance, tmp.path().join("logs"), crate::external::llm::LlmConfig { model: String::new(), api_key: String::new() }, env_config).unwrap();
        alice.privileged = true;
        let tx = Transaction::new("test");
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
        alice.instance.chat.write_user_message("24007", "hello agent", "20260220120000").unwrap();
        alice.instance.chat.write_user_message("24007", "how are you?", "20260220120001").unwrap();

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
        let action = Action::Summary { content: "some summary".to_string(), knowledge: None };
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

        let action = Action::Summary {
            content: "Alice read a greeting and replied".to_string(),
            knowledge: Some("# Test Knowledge\n- item 1".to_string()),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("小结完成"));
        assert!(result.contains("2个消息ID"));

        // current should be cleared
        let current = alice.instance.memory.current.read().unwrap();
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
            content: "test summary".to_string(),
            knowledge: Some("# Knowledge".to_string()),
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
        assert_eq!(out::truncate_result(short), "hello");

        let long = "x".repeat(102_400 + 100);
        let truncated = out::truncate_result(&long);
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
}


