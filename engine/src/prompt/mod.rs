//! # Prompt Module
//!
//! Data extraction layer: reads Alice's state (memory, chat, config)
//! and assembles inference request structs for the inference module.
//!
//! Protocol definitions (templates, rendering, parsing) live in `inference/`.

use crate::core::Alice;
use crate::inference::beat::{self, BeatRequest, SessionBlock, SessionMessage};
use crate::inference::capture::CaptureRequest;
use crate::policy::messages;
use mad_hatter::llm::ToMarkdown;

// ---------------------------------------------------------------------------
// Session block data extraction (depends on Alice for chat DB access)
// ---------------------------------------------------------------------------

/// Extract session blocks from a session block JSONL.
///
/// Input JSONL lines like:
///   {"first_msg":"20260223155500","last_msg":"20260223160100","summary":"Alice read and replied"}
///
/// For each line, fetches actual chat messages from chat.db in the
/// [first_msg, last_msg] range, returning SessionBlock structs.
pub fn extract_session_blocks_from_entries(
    block_entries: &[crate::persist::SessionBlockEntry],
    alice: &Alice,
) -> Vec<SessionBlock> {
    let mut blocks = Vec::new();
    for entry in block_entries {
        let mut messages = Vec::new();

        // Fetch chat messages from database (raw, no truncation here)
        if !entry.first_msg.is_empty() && !entry.last_msg.is_empty() {
            if let Ok(db_messages) = alice
                .instance
                .chat
                .lock()
                .unwrap()
                .read_messages_in_range(&entry.first_msg, &entry.last_msg)
            {
                for msg in &db_messages {
                    messages.push(SessionMessage {
                        sender_role: msg.role.clone(),
                        sender_id: if msg.sender.is_empty() {
                            None
                        } else {
                            Some(msg.sender.clone())
                        },
                        timestamp: msg.timestamp.clone(),
                        content: msg.content.clone(),
                    });
                }
            }
        }

        blocks.push(SessionBlock {
            start_time: entry.first_msg.clone(),
            end_time: entry.last_msg.clone(),
            messages,
            summary: entry.summary.clone(),
        });
    }
    blocks
}

// ---------------------------------------------------------------------------
// Prompt builder — extracts data from Alice, delegates rendering to inference
// ---------------------------------------------------------------------------

/// Build a BeatRequest from Alice's current state.
///
/// Reads memory, chat, config from Alice and assembles a BeatRequest struct
/// with pre-rendered field values. The caller passes this to the LLM channel which
/// handles system prompt rendering and inference internally.
pub fn build_beat_request(
    alice: &Alice,
    host: Option<&str>,
    contacts_info: String,
    extra_skills: String,
) -> BeatRequest {
    let knowledge_content = load_knowledge_raw(alice);
    let history_content = alice.instance.memory.read_history();
    let session_blocks = extract_all_session_blocks(alice);
    let current_content = alice.instance.memory.render_current_from_db().unwrap_or_default();
    let skill_content = alice.instance.skill.read().unwrap_or_default();
    let unread_count: usize = alice.count_unread_messages().try_into().unwrap_or(0);

    // Build pre-rendered field values
    let skill = beat::make_reserved_skill(
        host,
        &alice.instance.id,
        alice.env_config.http_port,
    );

    let extra_skill = {
        let mut parts: Vec<&str> = Vec::new();
        if !skill_content.trim().is_empty() {
            parts.push(&skill_content);
        }
        if !extra_skills.trim().is_empty() {
            parts.push(&extra_skills);
        }
        parts.join("\n\n")
    };

    let knowledge = beat::build_knowledge_content(&knowledge_content);

    let history = if history_content.is_empty() {
        messages::empty_placeholder().to_string()
    } else {
        history_content.clone()
    };

    let sessions_size = beat::estimate_sessions_size(&session_blocks);

    let environment = beat::build_environment(
        &alice.instance.id,
        alice.instance_name.as_deref(),
        &contacts_info,
        alice.shell_env.as_deref().unwrap_or_default(),
        host,
    );

    let current = if current_content.is_empty() {
        messages::empty_placeholder().to_string()
    } else {
        current_content.clone()
    };

    let status = beat::build_status(
        &alice.instance.id,
        alice.instance_name.as_deref(),
        alice.system_start_time,
        unread_count,
        history.len(),
        sessions_size,
        current.len(),
        knowledge.len(),
        skill.len() + extra_skill.len(),
    );

    BeatRequest {
        skill,
        extra_skill,
        knowledge,
        history,
        sessions: session_blocks,
        environment,
        current,
        status,
    }
}

/// Load knowledge raw content from memory.
/// Returns raw content or empty string.
pub fn load_knowledge_raw(alice: &Alice) -> String {
    alice.instance.memory.read_knowledge()
}

/// Build capture request from current memory state.
/// Input = knowledge + recent session blocks + current increment + this summary.
pub fn build_capture_request(alice: &Alice, summary_content: &str) -> CaptureRequest {
    let knowledge_content = load_knowledge_raw(alice);

    let recent_content = {
        let session_blocks = extract_all_session_blocks(alice);
        if session_blocks.is_empty() {
            String::new()
        } else {
            session_blocks
                .iter()
                .map(|b| b.to_markdown())
                .collect::<Vec<_>>()
                .join("\n\n")
        }
    };

    let current_content = alice.instance.memory.render_current_from_db().unwrap_or_default();

    CaptureRequest {
        knowledge_content,
        recent_content,
        current_content,
        summary_content: summary_content.to_string(),
    }
}

/// Extract all session blocks as structured data in chronological order.
/// Each SessionBlockEntry becomes a SessionBlock.
fn extract_all_session_blocks(alice: &Alice) -> Vec<SessionBlock> {
    let block_names = alice
        .instance
        .memory
        .list_session_blocks_db()
        .unwrap_or_default();
    if block_names.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    for block_name in &block_names {
        let block_entries = alice
            .instance
            .memory
            .read_session_entries_db(block_name)
            .unwrap_or_default();
        let blocks = extract_session_blocks_from_entries(&block_entries, alice);
        result.extend(blocks);
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::SessionBlockEntry;
    use mad_hatter::llm::ToMarkdown;
    use tempfile::TempDir;

    fn setup_alice() -> (Alice, TempDir) {
        let tmp = TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1"}"#).unwrap();
        let instance = crate::persist::instance::Instance::open(tmp.path()).unwrap();
        let env_config = std::sync::Arc::new(crate::policy::EnvConfig::from_env());
        let channel_configs = std::sync::Arc::new(std::sync::RwLock::new(vec![crate::external::llm::LlmConfig::default()]));
        let channel_index = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let alice = Alice::new(
            instance,
            tmp.path().join("logs"),
            channel_configs,
            channel_index,
            env_config,
            None,
            None,
        )
        .unwrap();
        (alice, tmp)
    }

    #[test]
    fn test_extract_session_blocks_from_entries() {
        let (mut alice, _tmp) = setup_alice();
        alice
            .instance
            .chat
            .lock()
            .unwrap()
            .write_user_message("hello world", "20260223155500")
            .unwrap();
        alice
            .instance
            .chat
            .lock()
            .unwrap()
            .write_agent_reply("alice", "hi back", "20260223155600", "")
            .unwrap();

        let block_entries = vec![crate::persist::SessionBlockEntry {
            first_msg: "20260223155500".to_string(),
            last_msg: "20260223155600".to_string(),
            summary: "Alice read and replied".to_string(),
        }];
        let blocks = extract_session_blocks_from_entries(&block_entries, &alice);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].messages.len(), 2);
        assert_eq!(blocks[0].messages[0].sender_id, Some("user".to_string()));
        assert_eq!(blocks[0].messages[0].content, "hello world");
        assert_eq!(blocks[0].messages[1].sender_id, Some("alice".to_string()));
        assert_eq!(blocks[0].summary, "Alice read and replied");
        assert_eq!(blocks[0].start_time, "20260223155500");
        assert_eq!(blocks[0].end_time, "20260223155600");
    }

    #[test]
    fn test_extract_session_blocks_from_entries_empty() {
        let (alice, _tmp) = setup_alice();
        assert!(extract_session_blocks_from_entries(&[], &alice).is_empty());
    }

    #[test]
    fn test_extract_session_blocks_from_entries_no_chat() {
        let (alice, _tmp) = setup_alice();
        let block_entries = vec![crate::persist::SessionBlockEntry {
            first_msg: "20260223155500".to_string(),
            last_msg: "20260223155600".to_string(),
            summary: "Some work happened".to_string(),
        }];
        let blocks = extract_session_blocks_from_entries(&block_entries, &alice);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].messages.is_empty());
        assert_eq!(blocks[0].summary, "Some work happened");
    }

    #[test]
    fn test_build_beat_request_empty_memory() {
        let (alice, _tmp) = setup_alice();
        let request = build_beat_request(&alice, None, String::new(), String::new());
        let output = request.to_markdown();
        assert!(output.contains("(空)"));
    }

    #[test]
    fn test_build_beat_request_with_session_block() {
        let (mut alice, _tmp) = setup_alice();
        alice.instance.memory.write_history("some history").unwrap();
        alice
            .instance
            .chat
            .lock()
            .unwrap()
            .write_user_message("hi there", "20260223120000")
            .unwrap();
        alice
            .instance
            .memory
            .insert_session_block_entry(
                "20260223120000",
                &crate::persist::SessionBlockEntry {
                    first_msg: "20260223120000".to_string(),
                    last_msg: "20260223120000".to_string(),
                    summary: "User said hi".to_string(),
                },
            )
            .unwrap();

        let request = build_beat_request(&alice, None, String::new(), String::new());
        let output = request.to_markdown();
        assert!(output.contains("sender_role: user"), "should contain sender_role");
        assert!(output.contains("hi there"), "should contain message content");
        assert!(output.contains("summary: User said hi"), "should contain summary");
    }

    #[test]
    fn test_load_knowledge_raw() {
        let (alice, _tmp) = setup_alice();
        alice
            .instance
            .memory
            .write_knowledge("raw knowledge content")
            .unwrap();
        let raw = load_knowledge_raw(&alice);
        assert_eq!(raw, "raw knowledge content");
    }

    #[test]
    fn test_build_beat_request_with_knowledge() {
        let (alice, _tmp) = setup_alice();
        alice
            .instance
            .memory
            .write_knowledge("# 泛准则\n- 谨慎加信任")
            .unwrap();
        let request = build_beat_request(&alice, None, String::new(), String::new());
        let output = request.to_markdown();
        assert!(output.contains("### 知识 ###"));
        assert!(output.contains("谨慎加信任"));
    }


}

#[cfg(test)]
mod prompt_dump_test;

