//! # Prompt Module
//!
//! Constructs the system and user prompts for the React cognitive loop.
//! Templates are embedded at compile time via `include_str!()`.
//!
//! ## New Memory Architecture
//!
//! - history (long-range narrative) + daily (JSONL) + current (raw action records)
//! - knowledge topics (dynamically loaded)
//! - knowledge.md (always injected if exists)
//! - No more persistent/working/session split

use chrono::Local;
use crate::core::Alice;

// Compile-time embedded templates
const TEMPLATE_SYSTEM: &str = include_str!("../../templates/react_system.txt");
const TEMPLATE_USER: &str = include_str!("../../templates/react_user.txt");
#[cfg(feature = "welcome-letter")]
pub const WELCOME_LETTER: &str = include_str!("../../templates/welcome_letter.txt");
pub const INITIAL_HISTORY: &str = include_str!("../../templates/initial_history.txt");
pub const HISTORY_COMPRESS_PROMPT: &str = include_str!("../../templates/history_compress.txt");
const APP_GUIDE_TEMPLATE: &str = include_str!("../../templates/app_guide.txt");

/// Knowledge file name (single file, always injected into prompt).
pub const KNOWLEDGE_FILE: &str = "knowledge.md";

/// Soft limit for total prompt content size (characters).
/// When exceeded, a warning is shown to the agent.
const SOFT_LIMIT: usize = 180_000;

// ---------------------------------------------------------------------------
// Separator constructors
// ---------------------------------------------------------------------------

/// Build the action separator prefix: `###ACTION_{token}###-`
pub fn make_action_separator(token: &str) -> String {
    format!("###ACTION_{}###-", token)
}

// ---------------------------------------------------------------------------
// MemorySnapshot
// ---------------------------------------------------------------------------

/// Cached memory content loaded during prompt construction.
pub struct MemorySnapshot {
    pub history: String,
    pub current: String,
}

// ---------------------------------------------------------------------------
// Session block rendering
// ---------------------------------------------------------------------------

/// Render a session block JSONL with chat messages from database.
///
/// Input JSONL lines like:
///   {"first_msg":"20260223155500","last_msg":"20260223160100","summary":"Alice read and replied"}
///
/// For each line, fetches actual chat messages from chat.db in the
/// [first_msg, last_msg] range, then appends the summary.
///
/// Output:
///   24007 [20260223155500]: hello
///   alice [20260223155600]: hi back
///   [总结] Alice read and replied
pub fn render_session_block(jsonl_content: &str, alice: &Alice) -> String {
    let mut sections = Vec::new();
    for line in jsonl_content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            let first_msg = value.get("first_msg").and_then(|s| s.as_str()).unwrap_or("");
            let last_msg = value.get("last_msg").and_then(|s| s.as_str()).unwrap_or("");
            let summary = value.get("summary").and_then(|s| s.as_str()).unwrap_or("");

            let mut parts = Vec::new();

            // Fetch chat messages from database (truncate long content)
            if !first_msg.is_empty() && !last_msg.is_empty() {
                if let Ok(messages) = alice.instance.chat.read_messages_in_range(first_msg, last_msg) {
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
            if !summary.is_empty() {
                parts.push(format!("[总结] {}", summary));
            }

            if !parts.is_empty() {
                sections.push(parts.join("\n"));
            }
        }
    }
    sections.join("\n\n")
}

// ---------------------------------------------------------------------------
// ReactPrompt — prompt builder for one beat
// ---------------------------------------------------------------------------

/// Build the system prompt and user prompt for one React beat.
///
/// Reads memory from Alice's sessions/ and knowledge/ directories.
/// Returns `(system_prompt, user_prompt, memory_snapshot)`.
pub fn build_prompts(
    alice: &Alice,
    action_token: &str,
    host: Option<&str>,
) -> (String, String, MemorySnapshot) {
    // Load knowledge from memory
    let knowledge_content = load_knowledge_file(alice);

    // Load history from memory
    let history_content = {
        let h = alice.instance.memory.history.get();
        if h.is_empty() { "(空)".to_string() } else { h.to_string() }
    };

    // Load and render session blocks
    let daily_rendered = render_all_session_blocks(alice);

    // Load current from memory
    let current_content = {
        let c = alice.instance.memory.current.get();
        if c.is_empty() { "(空)".to_string() } else { c.to_string() }
    };

    let unread_count = alice.count_unread_messages();
    let current_time = Local::now().format("%Y%m%d%H%M%S").to_string();

    // Build system prompt
    let system_prompt = crate::safe_render(TEMPLATE_SYSTEM, &[
        ("{{TOKEN}}", action_token),
        ("{{REMEMBER_START}}", crate::action::REMEMBER_START_MARKER),
        ("{{REMEMBER_END}}", crate::action::REMEMBER_END_MARKER),
    ]);

    // Build memory status
    let memory_status = make_memory_status(
        &alice.instance.id,
        alice.instance_name.as_deref(),
        history_content.len(),
        daily_rendered.len(),
        current_content.len(),
        knowledge_content.len(),
    );

    let shell_env = std::env::var("ALICE_SHELL_ENV")
        .unwrap_or_else(|_| "Linux系统，请生成bash脚本".to_string());
    let host_line = make_host_line(host);

    // Build knowledge section: knowledge file + forced app guide
    let app_guide = make_app_guide_knowledge(host, &alice.instance.id);
    let full_knowledge = if app_guide.is_empty() {
        knowledge_content.clone()
    } else if knowledge_content.is_empty() {
        app_guide
    } else {
        format!("{}\n\n{}", knowledge_content, app_guide)
    };

    // Build user prompt — controlled small variables first, then uncontrolled large content.
    // Rationale: if memory/knowledge contains template markers like "{{TOKEN}}",
    // replacing them after vars are already consumed prevents injection.
    let user_prompt = crate::safe_render(TEMPLATE_USER, &[
        ("{{CURRENT_TIME}}", &current_time),
        ("{{SYSTEM_START_TIME}}", &alice.system_start_time),
        ("{{UNREAD_COUNT}}", &unread_count.to_string()),
        ("{{MEMORY_STATUS}}", &memory_status),
        ("{{INSTANCE_ID}}", &alice.instance.id),
        ("{{SHELL_ENV}}", &shell_env),
        ("{{HOST_INFO}}", &host_line),
        ("{{KNOWLEDGE}}", &full_knowledge),
        ("{{HISTORY_MEMORY}}", &history_content),
        ("{{DAILY_MEMORY}}", &daily_rendered),
        ("{{CURRENT_MEMORY}}", &current_content),
    ]);

    let snapshot = MemorySnapshot {
        history: history_content,
        current: current_content,
    };

    (system_prompt, user_prompt, snapshot)
}

/// Load knowledge from memory for injection into prompt.
/// Returns formatted knowledge section or empty string if empty.
fn load_knowledge_file(alice: &Alice) -> String {
    let content = alice.instance.memory.knowledge.get();
    if content.trim().is_empty() {
        return String::new();
    }
    format!("### 要点与知识 ###\n{}\n", content.trim())
}

/// Load knowledge raw content from memory (for summary prompt).
/// Returns raw content or empty string.
pub fn load_knowledge_raw(alice: &Alice) -> String {
    alice.instance.memory.knowledge.get().to_string()
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
// Helper functions
// ---------------------------------------------------------------------------

/// Build memory status report with character counts and optional warning.
fn make_memory_status(
    instance_id: &str,
    instance_name: Option<&str>,
    history_size: usize,
    daily_size: usize,
    current_size: usize,
    knowledge_size: usize,
) -> String {
    let total = history_size + daily_size + current_size + knowledge_size;
    // Knowledge capacity indicator
    let knowledge_indicator = if knowledge_size < 51200 {
        format!("知识: {}/51200字符 🟢", knowledge_size)
    } else if knowledge_size < 61440 {
        format!("知识: {}/51200字符 ⚠️ 知识接近上限，summary时请精简", knowledge_size)
    } else {
        format!("知识: {}/51200字符 🔴 知识超出推荐容量，建议与用户商量裂变", knowledge_size)
    };

    // Instance identity line
    let instance_line = match instance_name {
        Some(name) if !name.is_empty() => format!("实例名：{}（{}）", name, instance_id),
        _ => format!("实例名：{}", instance_id),
    };

    let mut status = format!(
        "{}\ncurrent: {}字符 | 经历: {}字符 | 近况: {}字符 | {} | 合计: {}字符",
        instance_line, current_size, history_size, daily_size, knowledge_indicator, total
    );

    if total > SOFT_LIMIT {
        let kb = total / 1000;
        status.push_str(&format!(
            "\n⚠️ prompt总量已达{}KB（上限200K）！建议执行 summary 整理记忆。",
            kb
        ));
    }

    status
}

/// Build host info line (just the public address, if available).
fn make_host_line(host: Option<&str>) -> String {
    match host {
        Some(h) if !h.is_empty() => {
            let host_display = h.split(':').next().unwrap_or(h);
            format!("公网地址：{}", host_display)
        }
        _ => String::new(),
    }
}

/// Build app development guide as a forced-loaded knowledge section.
/// Returns empty string if no host is configured.
/// Template is compiled into the binary via include_str!().
fn make_app_guide_knowledge(host: Option<&str>, instance_id: &str) -> String {
    match host {
        Some(h) if !h.is_empty() => {
            let host_display = h.split(':').next().unwrap_or(h);
            APP_GUIDE_TEMPLATE
                .replace("{host}", host_display)
                .replace("{instance}", instance_id)
        }
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AliceConfig;
    use tempfile::TempDir;

    fn setup_alice() -> (Alice, TempDir) {
        let tmp = TempDir::new().unwrap();

        // Create minimal settings.json for Instance::open
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(&settings_path, r#"{"user_id":"user1"}"#).unwrap();

        // Instance::open creates all subdirectories automatically
        let instance = crate::core::instance::Instance::open(tmp.path()).unwrap();

        let config = AliceConfig {
            log_dir: tmp.path().join("logs"),
            ..Default::default()
        };
        let alice = Alice::new(instance, config).unwrap();
        (alice, tmp)
    }

    #[test]
    fn test_separators() {
        assert_eq!(make_action_separator("abc123"), "###ACTION_abc123###-");
    }

    #[test]
    fn test_render_session_block() {
        let (mut alice, _tmp) = setup_alice();

        // Write chat messages to database
        alice.instance.chat.write_user_message("24007", "hello world", "20260223155500", "chat").unwrap();
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

        // Write a chat message with content > 200 chars
        let long_content = "x".repeat(300);
        alice.instance.chat.write_user_message("24007", &long_content, "20260223155500", "chat").unwrap();

        let jsonl = r#"{"first_msg":"20260223155500","last_msg":"20260223155500","summary":"test"}"#;
        let rendered = render_session_block(jsonl, &alice);
        // Should be truncated
        assert!(rendered.contains("...(略)"));
        assert!(!rendered.contains(&"x".repeat(300)));
        // Summary should still be present
        assert!(rendered.contains("[总结] test"));
    }

    #[test]
    fn test_render_session_block_no_chat() {
        let (alice, _tmp) = setup_alice();
        // No messages in DB, should still show summary
        let jsonl = r#"{"first_msg":"20260223155500","last_msg":"20260223155600","summary":"Some work happened"}"#;
        let rendered = render_session_block(jsonl, &alice);
        assert!(rendered.contains("[总结] Some work happened"));
        assert!(!rendered.contains("24007"));
    }

    #[test]
    fn test_memory_status_normal() {
        let status = make_memory_status("test-001", None, 1000, 2000, 500, 3000);
        assert!(status.contains("合计: 6500字符"));
        assert!(!status.contains("⚠️"));
    }

    #[test]
    fn test_memory_status_warning() {
        let status = make_memory_status("test-001", Some("测试实例"), 1000, 2000, 100_000, 80000);
        assert!(status.contains("⚠️"));
        assert!(status.contains("整理记忆"));
    }

    #[test]
    fn test_host_line_no_host() {
        let line = make_host_line(None);
        assert!(line.is_empty());
    }

    #[test]
    fn test_host_line_with_host() {
        let line = make_host_line(Some("example.com"));
        assert_eq!(line, "公网地址：example.com");
    }

    #[test]
    fn test_host_line_strips_port() {
        let line = make_host_line(Some("example.com:8081"));
        assert_eq!(line, "公网地址：example.com");
    }

    #[test]
    fn test_app_guide_no_host() {
        let guide = make_app_guide_knowledge(None, "alice");
        assert!(guide.is_empty());
    }

    #[test]
    fn test_app_guide_with_host() {
        let guide = make_app_guide_knowledge(Some("example.com"), "alice");
        // Template is compiled in, should contain substituted values
        assert!(guide.contains("example.com"));
        assert!(guide.contains("alice"));
    }


    fn test_build_prompts_basic() {
        let (mut alice, _tmp) = setup_alice();

        alice.instance.memory.history.set("history data");
        alice.instance.memory.current.set("current data");

        let (system, user, snapshot) = build_prompts(&alice, "test123", None);

        assert!(system.contains("###ACTION_test123###-"));
        assert!(user.contains("history data"));
        assert!(user.contains("current data"));
        assert_eq!(snapshot.history, "history data");
        assert_eq!(snapshot.current, "current data");
    }

    #[test]
    fn test_build_prompts_empty_memory() {
        let (alice, _tmp) = setup_alice();

        let (system, user, snapshot) = build_prompts(&alice, "abc", None);

        assert!(system.contains("###ACTION_abc###-"));
        assert!(user.contains("(空)"));
        assert_eq!(snapshot.history, "(空)");
        assert_eq!(snapshot.current, "(空)");
    }

    #[test]
    fn test_build_prompts_with_session_block() {
        let (mut alice, _tmp) = setup_alice();

        alice.instance.memory.history.set("some history");
        // Write a chat message to DB and a session block referencing it
        alice.instance.chat.write_user_message("24007", "hi there", "20260223120000", "chat").unwrap();
        let jsonl = r#"{"first_msg":"20260223120000","last_msg":"20260223120000","summary":"User said hi"}"#;
        alice.instance.memory.append_session_block("20260223120000", jsonl).unwrap();

        let (_, user, _) = build_prompts(&alice, "tok", None);
        assert!(user.contains("24007 [20260223120000]: hi there"));
        assert!(user.contains("[总结] User said hi"));
    }

    #[test]
    fn test_load_knowledge_file() {
        let (mut alice, _tmp) = setup_alice();

        // No knowledge file yet
        let knowledge = load_knowledge_file(&alice);
        assert!(knowledge.is_empty(), "no knowledge file should return empty string");

        // Set knowledge content
        alice.instance.memory.knowledge.set("# 泛准则\n- 收到消息先回复\n\n# 引擎架构\n模块结构...");

        let knowledge = load_knowledge_file(&alice);
        assert!(knowledge.contains("### 要点与知识 ###"));
        assert!(knowledge.contains("泛准则"));
        assert!(knowledge.contains("引擎架构"));
    }

    #[test]
    fn test_load_knowledge_file_empty() {
        let (mut alice, _tmp) = setup_alice();

        alice.instance.memory.knowledge.set("  \n  ");

        let knowledge = load_knowledge_file(&alice);
        assert!(knowledge.is_empty(), "empty knowledge file should return empty string");
    }

    #[test]
    fn test_load_knowledge_file_raw() {
        let (mut alice, _tmp) = setup_alice();

        alice.instance.memory.knowledge.set("raw knowledge content");

        let raw = load_knowledge_raw(&alice);
        assert_eq!(raw, "raw knowledge content");
    }

    #[test]
    fn test_build_prompts_with_knowledge() {
        let (mut alice, _tmp) = setup_alice();

        alice.instance.memory.knowledge.set("# 泛准则\n- 谨慎加信任");

        let (_, user, _) = build_prompts(&alice, "tok", None);
        assert!(user.contains("### 要点与知识 ###"));
        assert!(user.contains("谨慎加信任"));
    }
}