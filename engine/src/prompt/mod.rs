//! # Prompt Module
//!
//! Data extraction layer: reads Alice's state (memory, chat, config)
//! and assembles inference request structs for the inference module.
//!
//! Protocol definitions (templates, rendering, parsing) live in `inference/`.

use crate::core::Alice;
use crate::inference::beat::BeatRequest;

// ---------------------------------------------------------------------------
// Session block rendering (depends on Alice for chat DB access)
// ---------------------------------------------------------------------------

/// Render a session block JSONL with chat messages from database.
///
/// Input JSONL lines like:
///   {"first_msg":"20260223155500","last_msg":"20260223160100","summary":"Alice read and replied"}
///
/// For each line, fetches actual chat messages from chat.db in the
/// [first_msg, last_msg] range, then appends the summary.
pub fn render_session_block(jsonl_content: &str, alice: &Alice) -> String {
    use crate::persist::SessionBlockEntry;

    let mut sections = Vec::new();
    for line in jsonl_content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<SessionBlockEntry>(line) {
            let mut parts = Vec::new();

            // Fetch chat messages from database (truncate long content)
            if !entry.first_msg.is_empty() && !entry.last_msg.is_empty() {
                if let Ok(messages) = alice.instance.chat.read_messages_in_range(&entry.first_msg, &entry.last_msg) {
                    for msg in &messages {
                        let content_display = if msg.content.len() > 200 {
                            format!("{}...(略)", crate::safe_truncate(&msg.content, 200))
                        } else {
                            msg.content.clone()
                        };
                        parts.push(format!("{} [{}]: {}", msg.sender, msg.timestamp, content_display));
                    }
                }
            }

            // Append summary
            if !entry.summary.is_empty() {
                parts.push(format!("[总结] {}", entry.summary));
            }

            if !parts.is_empty() {
                sections.push(parts.join("\n"));
            }
        }
    }
    sections.join("\n\n")
}

// ---------------------------------------------------------------------------
// Prompt builder — extracts data from Alice, delegates rendering to inference
// ---------------------------------------------------------------------------

/// Build a BeatRequest from Alice's current state.
///
/// Reads memory, chat, config from Alice and assembles a BeatRequest struct.
/// The caller passes this to LlmClient which handles rendering and inference internally.
pub fn build_beat_request(
    alice: &Alice,
    host: Option<&str>,
) -> BeatRequest {
    let knowledge_content = load_knowledge_file(alice);

    let history_content = {
        let h = alice.instance.memory.history.read().unwrap_or_default();
        if h.is_empty() { "(空)".to_string() } else { h.to_string() }
    };

    let daily_rendered = render_all_session_blocks(alice);

    let current_content = {
        let c = alice.instance.memory.current.read().unwrap_or_default();
        if c.is_empty() { "(空)".to_string() } else { c.to_string() }
    };

    let unread_count = alice.count_unread_messages();

    BeatRequest {
        action_token: String::new(), // filled by infer_beat internally
        instance_id: alice.instance.id.clone(),
        instance_name: alice.instance_name.clone(),
        shell_env: alice.env_config.shell_env.clone(),
        host: host.map(|s| s.to_string()),
        system_start_time: alice.system_start_time.format("%Y%m%d%H%M%S").to_string(),
        knowledge_content,
        history_content,
        daily_rendered,
        current_content,
        unread_count: unread_count.try_into().unwrap_or(0),
    }
}

/// Load knowledge from memory for injection into prompt.
/// Returns formatted knowledge section or empty string if empty.
fn load_knowledge_file(alice: &Alice) -> String {
    let content = alice.instance.memory.knowledge.read().unwrap_or_default();
    if content.trim().is_empty() {
        return String::new();
    }
    format!("### 要点与知识 ###\n{}\n", content.trim())
}

/// Load knowledge raw content from memory (for summary prompt).
/// Returns raw content or empty string.
pub fn load_knowledge_raw(alice: &Alice) -> String {
    alice.instance.memory.knowledge.read().unwrap_or_default()
}

/// Render all session block JSONL files in chronological order.
pub fn render_all_session_blocks(alice: &Alice) -> String {
    let blocks = alice.instance.memory.list_session_blocks().unwrap_or_default();
    if blocks.is_empty() {
        return String::new();
    }

    let mut sections = Vec::new();
    for block_name in &blocks {
        if let Ok(content) = alice.instance.memory.read_session_block(block_name) {
            let rendered = render_session_block(&content, alice);
            if !rendered.is_empty() {
                sections.push(format!("[{}]\n{}", block_name, rendered));
            }
        }
    }

    sections.join("\n\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn setup_alice() -> (Alice, TempDir) {
        let tmp = TempDir::new().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1"}"#).unwrap();
        let instance = crate::persist::instance::Instance::open(tmp.path()).unwrap();
        let env_config = std::sync::Arc::new(crate::policy::EnvConfig::from_env());
        let alice = Alice::new(instance, tmp.path().join("logs"), Default::default(), env_config).unwrap();
        (alice, tmp)
    }

    #[test]
    fn test_render_session_block() {
        let (mut alice, _tmp) = setup_alice();
        alice.instance.chat.write_user_message("24007", "hello world", "20260223155500").unwrap();
        alice.instance.chat.write_agent_reply("alice", "hi back", "20260223155600").unwrap();

        let jsonl = r#"{"first_msg":"20260223155500","last_msg":"20260223155600","summary":"Alice read and replied"}"#;
        let rendered = render_session_block(jsonl, &alice);
        assert!(rendered.contains("24007 [20260223155500]: hello world"));
        assert!(rendered.contains("alice [20260223155600]: hi back"));
        assert!(rendered.contains("[总结] Alice read and replied"));
    }

    #[test]
    fn test_render_session_block_empty() {
        let (alice, _tmp) = setup_alice();
        assert_eq!(render_session_block("", &alice), "");
        assert_eq!(render_session_block("  \n  \n", &alice), "");
    }

    #[test]
    fn test_render_session_block_truncates_long_content() {
        let (mut alice, _tmp) = setup_alice();
        let long_content = "x".repeat(300);
        alice.instance.chat.write_user_message("24007", &long_content, "20260223155500").unwrap();

        let jsonl = r#"{"first_msg":"20260223155500","last_msg":"20260223155500","summary":"test"}"#;
        let rendered = render_session_block(jsonl, &alice);
        assert!(rendered.contains("...(略)"));
        assert!(!rendered.contains(&"x".repeat(300)));
        assert!(rendered.contains("[总结] test"));
    }

    #[test]
    fn test_render_session_block_no_chat() {
        let (alice, _tmp) = setup_alice();
        let jsonl = r#"{"first_msg":"20260223155500","last_msg":"20260223155600","summary":"Some work happened"}"#;
        let rendered = render_session_block(jsonl, &alice);
        assert!(rendered.contains("[总结] Some work happened"));
        assert!(!rendered.contains("24007"));
    }

    #[test]
    fn test_build_beat_request_empty_memory() {
        let (alice, _tmp) = setup_alice();
        let mut request = build_beat_request(&alice, None);
        request.action_token = "abc".to_string();
        let (system, user, _) = request.render();
        assert!(system.contains("###ACTION_abc###-"));
        assert!(user.contains("(空)"));
        assert_eq!(request.history_content, "(空)");
        assert_eq!(request.current_content, "(空)");
    }

    #[test]
    fn test_build_beat_request_with_session_block() {
        let (mut alice, _tmp) = setup_alice();
        alice.instance.memory.history.write("some history").unwrap();
        alice.instance.chat.write_user_message("24007", "hi there", "20260223120000").unwrap();
        let jsonl = r#"{"first_msg":"20260223120000","last_msg":"20260223120000","summary":"User said hi"}"#;
        alice.instance.memory.append_session_block("20260223120000", jsonl).unwrap();

        let request = build_beat_request(&alice, None);
        let (_, user, _) = request.render();
        assert!(user.contains("24007 [20260223120000]: hi there"));
        assert!(user.contains("[总结] User said hi"));
    }

    #[test]
    fn test_load_knowledge_file() {
        let (alice, _tmp) = setup_alice();
        let knowledge = load_knowledge_file(&alice);
        assert!(knowledge.is_empty());

        alice.instance.memory.knowledge.write("# 泛准则\n- 收到消息先回复").unwrap();
        let knowledge = load_knowledge_file(&alice);
        assert!(knowledge.contains("### 要点与知识 ###"));
        assert!(knowledge.contains("泛准则"));
    }

    #[test]
    fn test_load_knowledge_file_empty() {
        let (alice, _tmp) = setup_alice();
        alice.instance.memory.knowledge.write("  \n  ").unwrap();
        let knowledge = load_knowledge_file(&alice);
        assert!(knowledge.is_empty());
    }

    #[test]
    fn test_load_knowledge_raw() {
        let (alice, _tmp) = setup_alice();
        alice.instance.memory.knowledge.write("raw knowledge content").unwrap();
        let raw = load_knowledge_raw(&alice);
        assert_eq!(raw, "raw knowledge content");
    }

    #[test]
    fn test_build_beat_request_with_knowledge() {
        let (alice, _tmp) = setup_alice();
        alice.instance.memory.knowledge.write("# 泛准则\n- 谨慎加信任").unwrap();
        let request = build_beat_request(&alice, None);
        let (_, user, _) = request.render();
        assert!(user.contains("### 要点与知识 ###"));
        assert!(user.contains("谨慎加信任"));
    }
}
