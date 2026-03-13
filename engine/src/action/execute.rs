//! # Action Execution
//!
//! Dispatches parsed actions to their concrete execution logic.
//! Each action variant maps to a specific operation on Alice's resources.
//!
//! @TRACE: ACTION

use anyhow::{bail, Context, Result};
use tracing::{info, warn};

use crate::core::{Alice, Transaction};
use crate::external::shell::{resolve_action_path, Shell};
use crate::inference::Action;
use crate::persist::hooks::ContactInfo;
use crate::policy::action_output as out;

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
        Action::ReplaceInFile { path, search, replace } => execute_replace_in_file(alice, tx, path, search, replace),
        Action::Summary { content } => execute_summary(alice, tx, content),

        Action::SetProfile { content } => execute_set_profile(alice, tx, &content),
        Action::CreateInstance { name, knowledge } => {
            execute_create_instance(alice, tx, name, knowledge)
        }
        Action::Distill {
            target_action_id,
            summary,
        } => execute_distill(alice, tx, target_action_id, summary),
    }
}

// ─── Individual action executors ─────────────────────────────────

fn execute_idle(
    _alice: &mut Alice,
    tx: &mut Transaction,
    timeout_secs: Option<u64>,
) -> Result<String> {
    match timeout_secs {
        Some(secs) => info!("[ACTION-{}] idle (timeout: {}s)", tx.instance_id, secs),
        None => info!("[ACTION-{}] idle", tx.instance_id),
    }
    Ok(String::new())
}

fn execute_read_msg(alice: &mut Alice, tx: &mut Transaction) -> Result<String> {
    info!("[ACTION-{}] read_msg", tx.instance_id);

    let messages = alice
        .instance
        .chat
        .lock()
        .unwrap()
        .read_unread_user_messages(&alice.instance.id)
        .context("Failed to read unread messages")?;

    if messages.is_empty() {
        return Ok(out::inbox_empty());
    }

    // Build known sender set: "user" (owner) + contacts IDs
    let contact_ids: Vec<String> = alice
        .hooks_caller
        .as_ref()
        .map(|hc| {
            hc.fetch_contacts(&alice.instance.id)
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.id)
                .collect()
        })
        .unwrap_or_default();

    let mut result = String::new();
    for msg in &messages {
        let is_known = msg.sender.is_empty() || msg.sender == "user"
            || contact_ids.iter().any(|id| id == &msg.sender);
        result.push_str(&out::read_msg_entry(
            &msg.role,
            &msg.sender,
            &alice.instance.id,
            &msg.timestamp,
            &msg.content,
            is_known,
        ));
    }

    Ok(result)
}

/// Resolve a recipient string to an instance ID using the contacts list.
/// Supports: direct ID match, exact name match, or "名称(id)" format extraction.
fn resolve_recipient_id(recipient: &str, contacts: &[ContactInfo]) -> Option<String> {
    // 1. Direct ID match
    if contacts.iter().any(|c| c.id == recipient) {
        return Some(recipient.to_string());
    }
    // 2. Exact name match
    if let Some(c) = contacts.iter().find(|c| c.name.as_deref() == Some(recipient)) {
        return Some(c.id.clone());
    }
    // 3. Extract ID from "名称(id)" format (e.g. "进化三号（产品）(48f5fd)")
    if let Some(start) = recipient.rfind('(') {
        if let Some(id) = recipient[start + 1..].strip_suffix(')') {
            if !id.is_empty() && contacts.iter().any(|c| c.id == id) {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn execute_send_msg(
    alice: &mut Alice,
    tx: &mut Transaction,
    recipient: &str,
    content: &str,
) -> Result<String> {
    info!("[ACTION-{}] send_msg to {}", tx.instance_id, recipient);

    // Send to user directly (recipient is "user" or empty)
    if recipient == "user" || recipient.is_empty() {
        let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
        alice
            .instance
            .chat
            .lock()
            .unwrap()
            .write_agent_reply(&alice.instance.id, content, &timestamp, "")
            .context("Failed to write agent reply")?;
        return Ok(out::send_success(&timestamp));
    }

    // Non-user recipient: must go through contacts lookup
    let hooks_caller = match alice.hooks_caller.as_ref() {
        Some(hc) => hc,
        None => {
            warn!(
                "[ACTION-{}] send_msg to '{}' failed: no hooks_caller available",
                tx.instance_id, recipient
            );
            tx.cancel_idle = true;
            return Ok(out::send_failed_service_unavailable(recipient));
        }
    };

    // Fetch contacts list
    let contacts = match hooks_caller.fetch_contacts(&alice.instance.id) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "[ACTION-{}] send_msg to '{}' failed: contacts fetch error: {}",
                tx.instance_id, recipient, e
            );
            tx.cancel_idle = true;
            return Ok(out::send_failed_service_unavailable(recipient));
        }
    };

    // Resolve recipient ID from contacts
    let resolved = match resolve_recipient_id(recipient, &contacts) {
        Some(id) => id,
        None => {
            warn!(
                "[ACTION-{}] send_msg to '{}' failed: recipient not in contacts",
                tx.instance_id, recipient
            );
            tx.cancel_idle = true;
            return Ok(out::send_failed_recipient_not_found(recipient, &contacts));
        }
    };

    if resolved != recipient {
        info!(
            "[ACTION-{}] resolved recipient '{}' -> '{}'",
            tx.instance_id, recipient, resolved
        );
    }

    // Relay message
    match hooks_caller.relay_message(&alice.instance.id, &resolved, content) {
        Ok(response) if response.success => {
            info!(
                "[ACTION-{}] send_msg relayed to '{}' via hooks",
                tx.instance_id, resolved
            );
            let timestamp = crate::persist::chat::ChatHistory::now_timestamp();
            alice
                .instance
                .chat
                .lock()
                .unwrap()
                .write_agent_reply(&alice.instance.id, content, &timestamp, &resolved)
                .context("Failed to write relayed agent reply")?;
            Ok(out::send_success(&timestamp))
        }
        Ok(response) => {
            warn!(
                "[ACTION-{}] send_msg relay rejected for '{}': {}",
                tx.instance_id,
                resolved,
                response.message.unwrap_or_default()
            );
            tx.cancel_idle = true;
            Ok(out::send_failed_relay_error(recipient))
        }
        Err(e) => {
            warn!(
                "[ACTION-{}] send_msg relay failed for '{}': {}",
                tx.instance_id, resolved, e
            );
            tx.cancel_idle = true;
            Ok(out::send_failed_relay_error(recipient))
        }
    }
}

fn execute_thinking(_alice: &mut Alice, tx: &mut Transaction, content: &str) -> Result<String> {
    info!(
        "[ACTION-{}] thinking ({} chars)",
        tx.instance_id,
        content.len()
    );
    Ok(String::new())
}

fn execute_script(alice: &mut Alice, tx: &mut Transaction, content: &str) -> Result<String> {
    info!(
        "[ACTION-{}] script ({} chars)",
        tx.instance_id,
        content.len()
    );
    let shell = make_shell(alice);
    let result = shell.exec(content)?;

    let output = out::truncate_result(&result.output);
    Ok(out::script_result(
        result.duration.as_secs_f64(),
        &output,
        result.exit_code,
    ))
}

/// Extract a skeleton view of file content based on file extension.
/// Delegates to SkeletonConfig for language-aware extraction logic.
fn extract_skeleton(path: &str, content: &str) -> String {
    use crate::external::{ExtractionResult, SkeletonConfig};

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let total_bytes = content.len();

    match SkeletonConfig::get().extract_from_path(path, content) {
        ExtractionResult::Full => {
            let display = out::truncate_result(content);
            out::write_success_full(path, total_bytes, total_lines, &display)
        }
        ExtractionResult::Skeleton(skeleton) => {
            out::write_success_skeleton(path, total_bytes, total_lines, &skeleton)
        }
        ExtractionResult::NoRule => {
            let preview = out::format_preview(&lines);
            out::write_success_preview(path, total_bytes, total_lines, &preview)
        }
    }
}

fn execute_write_file(
    alice: &mut Alice,
    tx: &mut Transaction,
    path: &str,
    content: &str,
) -> Result<String> {
    info!("[ACTION-{}] write_file: {}", tx.instance_id, path);

    let abs_path = resolve_action_path(&alice.instance.workspace, path)?;

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
            bail!(
                "write_file failed (exit {}): {}",
                result.exit_code_display(),
                result.output.trim()
            );
        }
    }

    Ok(extract_skeleton(path, content))
}
fn execute_replace_in_file(
    alice: &mut Alice,
    tx: &mut Transaction,
    path: &str,
    search: &str,
    replace: &str,
) -> Result<String> {
    info!(
        "[ACTION-{}] replace_in_file: {}",
        tx.instance_id,
        path,
    );

    let abs_path = resolve_action_path(&alice.instance.workspace, path)?;
    let truncated = crate::util::safe_truncate(search, out::truncate_display_limit());

    if alice.privileged {
        // Direct filesystem access for privileged instances
        let content = std::fs::read_to_string(&abs_path)
            .with_context(|| format!("Failed to read file: {}", path))?;

        match crate::util::replace_once(&content, search, replace) {
            Ok(new_content) => {
                std::fs::write(&abs_path, &new_content)
                    .with_context(|| format!("Failed to write file: {}", path))?;
                Ok(out::replace_success(&truncated))
            }
            Err(count) => {
                Ok(out::replace_match_error(&truncated, count))
            }
        }
    } else {
        // Shell-based access for sandboxed instances
        let shell = make_shell(alice);
        let read_result = shell.read_file(&abs_path.to_string_lossy())?;
        if !read_result.success() {
            bail!(
                "replace_in_file: failed to read {} (exit {}): {}",
                path,
                read_result.exit_code_display(),
                read_result.output.trim()
            );
        }

        let content = read_result.output;

        match crate::util::replace_once(&content, search, replace) {
            Ok(new_content) => {
                let shell = make_shell(alice);
                let write_result = shell.write_file(&abs_path.to_string_lossy(), &new_content)?;
                if !write_result.success() {
                    bail!(
                        "replace_in_file: failed to write {} (exit {}): {}",
                        path,
                        write_result.exit_code_display(),
                        write_result.output.trim()
                    );
                }
                Ok(out::replace_success(&truncated))
            }
            Err(count) => {
                Ok(out::replace_match_error(&truncated, count))
            }
        }
    }
}

// ─── New memory actions ──────────────────────────────────────────

/// Execute summary: persist conversation summary into session blocks and clear current.
///
/// Flow:
/// 1. Read current.txt
/// 2. Extract MSG IDs from current
/// 3. Build JSONL session entry with summary text
/// 4. Commit summary (session block + clear current)
///
/// Knowledge is maintained separately by async capture.
fn execute_summary(alice: &mut Alice, tx: &mut Transaction, raw_output: &str) -> Result<String> {
    use crate::persist::SessionBlockEntry;

    info!("[ACTION-{}] summary", tx.instance_id);

    let current = alice.instance.memory.render_current_from_db().unwrap_or_default();
    if current.trim().is_empty() {
        return Ok(out::summary_empty());
    }

    let summary_text = raw_output;

    if summary_text.trim().is_empty() {
        warn!("[ACTION-{}] summary: empty summary text", tx.instance_id);
    }

    // Query MSG IDs from DB (replaces extract_msg_ids text search)
    let (first_msg_opt, last_msg_opt) = alice.instance.memory.query_msg_range()
        .unwrap_or((None, None));
    let first_msg = first_msg_opt.unwrap_or_default();
    let last_msg = last_msg_opt.unwrap_or_default();

    // Build typed session entry
    let entry = SessionBlockEntry {
        first_msg: first_msg.clone(),
        last_msg: last_msg.clone(),
        summary: summary_text.trim().to_string(),
    };

    // Query msg count before commit (commit advances cursor, which would zero the count)
    let msg_count = alice.instance.memory.query_msg_count().unwrap_or(0) as usize;

    // Commit: session block + clear current
    let block_name = alice
        .instance
        .memory
        .commit_summary(&entry, alice.session_block_kb)?;
    Ok(out::summary_complete(
        msg_count,
        &first_msg,
        &last_msg,
        &block_name,
        &out::knowledge_skipped(),
    ))
}

/// Parse summary dual output: split by first ===KNOWLEDGE_TOKEN=== on its own line.
/// Execute forget action: replace a target action block in current.txt with a concise summary.
/// The target block is identified by its action_id in the START/END markers.
/// On success, returns empty string (silent execution - caller skips append_current).
/// On failure, returns error (caller records it so agent sees what went wrong).
fn execute_distill(
    alice: &mut Alice,
    _tx: &mut Transaction,
    target_action_id: &str,
    summary: &str,
) -> Result<String> {
    let (old_len, new_len) = alice
        .instance
        .memory
        .distill_action_log(target_action_id, summary.trim())?;

    info!(
        "[DISTILL-{}] Distilled action [{}]: {} -> {} chars (saved {})",
        alice.instance.id,
        target_action_id,
        old_len,
        new_len,
        old_len as i64 - new_len as i64
    );

    Ok(String::new())
}

fn execute_set_profile(
    alice: &mut Alice,
    tx: &mut Transaction,
    content: &str,
) -> Result<String> {
    info!("[ACTION-{}] set_profile", tx.instance_id);

    // Parse key:value lines into Settings
    let mut update = crate::persist::Settings::default();
    let _known_keys = ["name", "color", "avatar"];

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_lowercase();
            let value = line[colon_pos + 1..].trim().to_string();
            let value_opt = if value.is_empty() { None } else { Some(value) };
            match key.as_str() {
                "name" => update.name = value_opt,
                "color" => update.color = value_opt,
                "avatar" => update.avatar = value_opt,
                _ => {
                    return Ok(out::profile_unknown_key(&key));
                }
            }
        }
    }

    let settings = alice.instance.settings.load()?;
    let mut merged = update.clone();
    merged.merge_fallback(&settings);
    alice.instance.settings.save(&merged)?;

    // Apply runtime effects
    alice.privileged = merged.privileged_or_default();
    alice.instance_name = merged.name.clone();

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
    let instances_dir = alice
        .instance
        .instance_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine instances directory"))?;

    // Create instance atomically via InstanceStore
    let knowledge_opt = if knowledge.is_empty() {
        None
    } else {
        Some(knowledge)
    };
    let store = crate::persist::instance::InstanceStore::new(instances_dir.to_path_buf());
    let instance = store
        .create(Some(name), knowledge_opt, None)
        .context("Failed to create instance")?;

    info!(
        "[ACTION-{}] Created new instance: {} (name: {}, knowledge: {} bytes, awaiting hot-scan)",
        alice.instance.id,
        instance.id,
        name,
        knowledge.len()
    );

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
        std::fs::write(
            &settings_path,
            r#"{"user_id":"user1","api_key":"test","model":"test@test"}"#,
        )
        .unwrap();

        // Instance::open creates all subdirectories automatically
        let instance = crate::persist::instance::Instance::open(tmp.path()).unwrap();

        let env_config = std::sync::Arc::new(crate::policy::EnvConfig::from_env());
        let channel_configs = std::sync::Arc::new(std::sync::RwLock::new(vec![crate::external::llm::LlmConfig::default()]));
        let channel_index = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut alice = Alice::new(
            instance,
            tmp.path().join("logs"),
            channel_configs,
            channel_index,
            env_config,
            None,
            None,
        )
        .unwrap();
        alice.privileged = true;
        let tx = Transaction::new("test");
        (alice, tx, tmp)
    }

    #[test]
    fn test_execute_idle() {
        let (mut alice, mut tx, _tmp) = setup();
        let result =
            execute_action(&Action::Idle { timeout_secs: None }, &mut alice, &mut tx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_execute_thinking() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::Thinking {
            content: "deep thought".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_execute_script() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::Script {
            content: "echo hello_rust".to_string(),
        };
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
            search: "hello".to_string(),
            replace: "goodbye".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("replaced"));

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
        alice
            .instance
            .chat
            .lock()
            .unwrap()
            .write_user_message("hello agent", "20260220120000")
            .unwrap();
        alice
            .instance
            .chat
            .lock()
            .unwrap()
            .write_user_message("how are you?", "20260220120001")
            .unwrap();

        let result = execute_action(&Action::ReadMsg, &mut alice, &mut tx).unwrap();
        assert!(result.contains("user"));
        assert!(result.contains("hello agent"));
        assert!(result.contains("how are you?"));
        // Verify MSG timestamp markers
        assert!(result.contains("[MSG:20260220120000]"));
        assert!(result.contains("[MSG:20260220120001]"));
        assert_eq!(
            alice
                .instance
                .chat
                .lock()
                .unwrap()
                .count_unread_user_messages("test_instance")
                .unwrap(),
            0
        );
    }

    #[test]
    fn test_execute_send_msg_to_user() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::SendMsg {
            recipient: "user".to_string(),
            content: "hello user!".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("send success"));
        // Verify MSG timestamp marker in result
        assert!(result.contains("[MSG:"));

        let replies = alice
            .instance
            .chat
            .lock()
            .unwrap()
            .read_unread_agent_replies()
            .unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1, "hello user!");
    }

    #[test]
    fn test_execute_send_msg_no_hooks_caller_fails() {
        let (mut alice, mut tx, _tmp) = setup();
        // alice.hooks_caller is None by default in test setup
        let action = Action::SendMsg {
            recipient: "some_agent".to_string(),
            content: "hello agent!".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("发送失败"));
        assert!(result.contains("通讯服务"));
        assert!(!result.contains("send success"));
    }

    #[test]
    fn test_send_msg_failure_sets_cancel_idle() {
        let (mut alice, mut tx, _tmp) = setup();
        assert!(!tx.cancel_idle);
        // alice.hooks_caller is None by default → send to non-user recipient will fail
        let action = Action::SendMsg {
            recipient: "nonexistent_agent".to_string(),
            content: "hello".to_string(),
        };
        let _result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(tx.cancel_idle, "cancel_idle should be true after send_msg failure");
    }

    #[test]
    fn test_send_msg_to_user_does_not_set_cancel_idle() {
        let (mut alice, mut tx, _tmp) = setup();
        assert!(!tx.cancel_idle);
        let action = Action::SendMsg {
            recipient: "user".to_string(),
            content: "hello user".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("send success"));
        assert!(!tx.cancel_idle, "cancel_idle should remain false after successful send_msg");
    }

    #[test]
    fn test_execute_summary_empty_current() {
        let (mut alice, mut tx, _tmp) = setup();
        let action = Action::Summary {
            content: "some summary".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("nothing to summarize"));
    }

    #[test]
    fn test_execute_summary() {
        let (mut alice, mut tx, _tmp) = setup();

        // Insert action_log records so render_current_from_db() has content
        alice.instance.memory.insert_action_log(
            "20260223160000_aaaaaa", "read_msg",
            &serde_json::to_string(&Action::ReadMsg).unwrap(),
            "20260223160000",
        ).unwrap();
        alice.instance.memory.complete_action_log(
            "20260223160000_aaaaaa", "read result with messages",
            Some("20260223155500"), Some("20260223155500"),
        ).unwrap();
        alice.instance.memory.insert_action_log(
            "20260223160100_bbbbbb", "send_msg",
            &serde_json::to_string(&Action::SendMsg { recipient: "user1".into(), content: "hi back".into() }).unwrap(),
            "20260223160100",
        ).unwrap();
        alice.instance.memory.complete_action_log(
            "20260223160100_bbbbbb", "send success",
            Some("20260223160100"), Some("20260223160100"),
        ).unwrap();

        let action = Action::Summary {
            content: "Alice read a greeting and replied".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("小结完成"));
        assert!(result.contains("2个消息ID"));

        // current should be cleared (DB view: render_current_from_db returns empty after advance_cursor)
        let current = alice.instance.memory.render_current_from_db().unwrap_or_default();
        assert!(current.is_empty(), "render_current_from_db should be empty after summary, got: {}", current);

        // session block should exist in DB
        let blocks = alice.instance.memory.list_session_blocks_db().unwrap_or_default();
        assert_eq!(blocks.len(), 1);
        let entries = alice
            .instance
            .memory
            .read_session_entries_db(&blocks[0])
            .unwrap_or_default();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].first_msg, "20260223155500");
        assert_eq!(entries[0].last_msg, "20260223160100");
        assert!(entries[0].summary.contains("Alice read a greeting and replied"));
    }

    #[test]
    fn test_summary_creates_new_block_when_full() {
        let (mut alice, mut tx, _tmp) = setup();

        // Pre-fill a session block to exceed the size limit
        let large_entry = crate::persist::SessionBlockEntry {
            first_msg: "20260223100000".to_string(),
            last_msg: "20260223110000".to_string(),
            summary: "x".repeat(alice.session_block_kb as usize * 1024),
        };
        alice
            .instance
            .memory
            .insert_session_block_entry("20260223100000", &large_entry)
            .unwrap();

        // Insert action_log record so render_current_from_db() has content
        alice.instance.memory.insert_action_log(
            "20260223160000_aaaaaa", "send_msg",
            &serde_json::to_string(&Action::SendMsg { recipient: "user".into(), content: "test".into() }).unwrap(),
            "20260223160000",
        ).unwrap();
        alice.instance.memory.complete_action_log(
            "20260223160000_aaaaaa", "send success",
            Some("20260223160000"), Some("20260223160000"),
        ).unwrap();

        let action = Action::Summary {
            content: "test summary".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("小结完成"));

        // Should have 2 blocks now (old full one + new one)
        let blocks = alice.instance.memory.list_session_blocks_db().unwrap_or_default();
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
        let action = Action::Script {
            content: "exit 42".to_string(),
        };
        let result = execute_action(&action, &mut alice, &mut tx).unwrap();
        assert!(result.contains("[exit code: 42]"));
    }
}
