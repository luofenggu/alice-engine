//! # Prompt Module
//!
//! Data extraction layer: reads Alice's state (memory, chat, config)
//! and assembles inference request structs for the inference module.
//!
//! Protocol definitions (templates, rendering, parsing) live in `inference/`.

use crate::core::Alice;
use crate::inference::beat::{
    self, BeatRequest, PromptMessage, SessionBlockData, SessionEntryData,
};
use crate::inference::capture::CaptureRequest;
use crate::policy::messages;

// ---------------------------------------------------------------------------
// Session block data extraction (depends on Alice for chat DB access)
// ---------------------------------------------------------------------------

/// Extract session entry data from a session block JSONL.
///
/// Input JSONL lines like:
///   {"first_msg":"20260223155500","last_msg":"20260223160100","summary":"Alice read and replied"}
///
/// For each line, fetches actual chat messages from chat.db in the
/// [first_msg, last_msg] range, returning raw data structs.
pub fn extract_session_block_data(
    block_entries: &[crate::persist::SessionBlockEntry],
    alice: &Alice,
) -> Vec<SessionEntryData> {
    let mut entries = Vec::new();
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
                    messages.push(PromptMessage {
                        role: msg.role.clone(),
                        sender: msg.sender.clone(),
                        timestamp: msg.timestamp.clone(),
                        content: msg.content.clone(),
                    });
                }
            }
        }

        entries.push(SessionEntryData {
            messages,
            summary: entry.summary.clone(),
        });
    }
    entries
}

// ---------------------------------------------------------------------------
// Prompt builder — extracts data from Alice, delegates rendering to inference
// ---------------------------------------------------------------------------

/// Build a BeatRequest from Alice's current state.
///
/// Reads memory, chat, config from Alice and assembles a BeatRequest struct
/// with pre-rendered field values. The caller passes this to LlmClient which
/// handles system prompt rendering and inference internally.
pub fn build_beat_request(
    alice: &Alice,
    host: Option<&str>,
    contacts_info: String,
    extra_skills: String,
) -> BeatRequest {
    let knowledge_content = load_knowledge_raw(alice);
    let history_content = alice.instance.memory.history.read().unwrap_or_default();
    let session_blocks = extract_all_session_blocks(alice);
    let current_content = alice.instance.memory.current.read().unwrap_or_default();
    let skill_content = alice.instance.skill.read().unwrap_or_default();
    let unread_count: usize = alice.count_unread_messages().try_into().unwrap_or(0);

    // Build pre-rendered field values
    let skill = beat::build_skill_content(
        host,
        &alice.instance.id,
        alice.env_config.http_port,
        &skill_content,
        &extra_skills,
    );

    let knowledge = beat::build_knowledge_content(&knowledge_content);

    let history = if history_content.is_empty() {
        messages::empty_placeholder().to_string()
    } else {
        history_content.clone()
    };

    let sessions = beat::render_session_blocks(&session_blocks, &alice.instance.id);

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
        sessions.len(),
        current.len(),
        knowledge.len(),
        skill.len(),
    );

    BeatRequest {
        skill,
        knowledge,
        history,
        sessions,
        environment,
        current,
        status,
    }
}

/// Load knowledge raw content from memory.
/// Returns raw content or empty string.
pub fn load_knowledge_raw(alice: &Alice) -> String {
    alice.instance.memory.knowledge.read().unwrap_or_default()
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
            let mut parts = Vec::new();
            for block in session_blocks {
                let mut entry_texts = Vec::new();
                for entry in block.entries {
                    let messages = entry
                        .messages
                        .iter()
                        .map(|m| format!("{} [{}]: {}", m.sender, m.timestamp, m.content))
                        .collect::<Vec<_>>()
                        .join("\n");
                    entry_texts.push(format!(
                        "messages:\n{}\nsummary:\n{}",
                        messages, entry.summary
                    ));
                }
                parts.push(format!(
                    "[{}]\n{}",
                    block.block_name,
                    entry_texts.join("\n\n")
                ));
            }
            parts.join("\n\n")
        }
    };

    let current_content = alice.instance.memory.current.read().unwrap_or_default();

    CaptureRequest {
        knowledge_content,
        recent_content,
        current_content,
        summary_content: summary_content.to_string(),
    }
}

/// Extract all session blocks as structured data in chronological order.
fn extract_all_session_blocks(alice: &Alice) -> Vec<SessionBlockData> {
    let blocks = alice
        .instance
        .memory
        .list_session_blocks()
        .unwrap_or_default();
    if blocks.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    for block_name in &blocks {
        if let Ok(block_entries) = alice.instance.memory.read_session_entries(block_name) {
            let entries = extract_session_block_data(&block_entries, alice);
            if !entries.is_empty() {
                result.push(SessionBlockData {
                    block_name: block_name.clone(),
                    entries,
                });
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mad_hatter::llm::ToMarkdown;
    use tempfile::TempDir;

    fn setup_alice() -> (Alice, TempDir) {
        let tmp = TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1"}"#).unwrap();
        let instance = crate::persist::instance::Instance::open(tmp.path()).unwrap();
        let env_config = std::sync::Arc::new(crate::policy::EnvConfig::from_env());
        let llm_client = std::sync::Arc::new(crate::external::llm::LlmClient::new(vec![Default::default()]));
        let alice = Alice::new(
            instance,
            tmp.path().join("logs"),
            llm_client,
            env_config,
            None,
            None,
        )
        .unwrap();
        (alice, tmp)
    }

    #[test]
    fn test_extract_session_block_data() {
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
        let entries = extract_session_block_data(&block_entries, &alice);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].messages.len(), 2);
        assert_eq!(entries[0].messages[0].sender, "user");
        assert_eq!(entries[0].messages[0].content, "hello world");
        assert_eq!(entries[0].messages[1].sender, "alice");
        assert_eq!(entries[0].summary, "Alice read and replied");
    }

    #[test]
    fn test_extract_session_block_data_empty() {
        let (alice, _tmp) = setup_alice();
        assert!(extract_session_block_data(&[], &alice).is_empty());
    }

    #[test]
    fn test_extract_session_block_data_no_chat() {
        let (alice, _tmp) = setup_alice();
        let block_entries = vec![crate::persist::SessionBlockEntry {
            first_msg: "20260223155500".to_string(),
            last_msg: "20260223155600".to_string(),
            summary: "Some work happened".to_string(),
        }];
        let entries = extract_session_block_data(&block_entries, &alice);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].messages.is_empty());
        assert_eq!(entries[0].summary, "Some work happened");
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
        alice.instance.memory.history.write("some history").unwrap();
        alice
            .instance
            .chat
            .lock()
            .unwrap()
            .write_user_message("hi there", "20260223120000")
            .unwrap();
        let jsonl = r#"{"first_msg":"20260223120000","last_msg":"20260223120000","summary":"User said hi"}"#;
        alice
            .instance
            .memory
            .append_session_block("20260223120000", jsonl)
            .unwrap();

        let request = build_beat_request(&alice, None, String::new(), String::new());
        let output = request.to_markdown();
        assert!(output.contains("user [20260223120000]: hi there"));
        assert!(output.contains("[总结] User said hi"));
    }

    #[test]
    fn test_load_knowledge_raw() {
        let (alice, _tmp) = setup_alice();
        alice
            .instance
            .memory
            .knowledge
            .write("raw knowledge content")
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
            .knowledge
            .write("# 泛准则\n- 谨慎加信任")
            .unwrap();
        let request = build_beat_request(&alice, None, String::new(), String::new());
        let output = request.to_markdown();
        assert!(output.contains("### 知识 ###"));
        assert!(output.contains("谨慎加信任"));
    }


}

#[cfg(test)]
mod prompt_dump_test;

