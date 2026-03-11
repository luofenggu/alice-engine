//! # Beat Inference Protocol
//!
//! Defines the request/response protocol for one React cognitive beat.
//! BeatRequest is a pure data struct with ToMarkdown derive.
//! stream_infer() uses to_markdown() + schema_markdown() to build the prompt.


use crate::policy::messages;

use mad_hatter::ToMarkdown;

const RESERVED_SKILL_TEMPLATE: &str = include_str!("../../templates/reserved_skill.txt");

pub const INITIAL_HISTORY: &str = include_str!("../../templates/initial_history.txt");

const SOFT_LIMIT: usize = 180_000;

use crate::policy::action_output as out;
use crate::policy::EngineConfig;

// ---------------------------------------------------------------------------
// Data structs — raw data extracted from Alice's state
// ---------------------------------------------------------------------------

/// A single chat message (raw data, not yet formatted).
pub struct PromptMessage {
    pub role: String,
    pub sender: String,
    pub timestamp: String,
    pub content: String,
}

/// One session entry: chat messages + summary (raw data).
pub struct SessionEntryData {
    pub messages: Vec<PromptMessage>,
    pub summary: String,
}

/// One session block: block name + entries (raw data).
pub struct SessionBlockData {
    pub block_name: String,
    pub entries: Vec<SessionEntryData>,
}

// ---------------------------------------------------------------------------
// BeatRequest — pure data struct for one beat's prompt rendering
// ---------------------------------------------------------------------------

/// 你醒了，你发现自己身处一个密闭房间，桌子上摆放着几样东西。
/// 收件箱：你可以在此收到来信
/// 寄件箱：你可以在此寄出信件
/// 工作目录：你可以在此读写文件、执行脚本以完成任务
#[derive(ToMarkdown)]
pub struct BeatRequest {
    /// skill
    pub skill: String,
    /// 知识
    pub knowledge: String,
    /// 经历
    pub history: String,
    /// 近况
    pub sessions: String,
    /// 环境信息
    pub environment: String,
    /// current
    pub current: String,
    /// 当前状态
    pub status: String,
}

impl From<&crate::persist::SessionBlockEntry> for SessionEntryData {
    fn from(entry: &crate::persist::SessionBlockEntry) -> Self {
        SessionEntryData {
            messages: vec![],
            summary: entry.summary.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Public formatting — used by both prompt building and compress
// ---------------------------------------------------------------------------

/// Format session entries into display text.
/// Used by prompt builder for beat prompts and by compress for history rolling.
pub fn format_session_entries(entries: &[SessionEntryData], self_id: &str) -> String {
    let truncate_len = EngineConfig::get().memory.message_truncate_length;

    let mut sections = Vec::new();
    for entry in entries {
        let mut parts = Vec::new();

        for msg in &entry.messages {
            let content_display = if msg.content.len() > truncate_len {
                messages::truncated_content(&crate::util::safe_truncate(&msg.content, truncate_len))
            } else {
                msg.content.clone()
            };
            parts.push(messages::chat_message(
                &msg.role,
                &msg.sender,
                self_id,
                &msg.timestamp,
                &content_display,
            ));
        }

        if !entry.summary.is_empty() {
            parts.push(messages::session_summary(&entry.summary));
        }

        if !parts.is_empty() {
            sections.push(parts.join("\n"));
        }
    }
    sections.join("\n\n")
}

/// Render session blocks into display text.
pub fn render_session_blocks(blocks: &[SessionBlockData], self_id: &str) -> String {
    if blocks.is_empty() {
        return String::new();
    }

    let mut sections = Vec::new();
    for block in blocks {
        let rendered = format_session_entries(&block.entries, self_id);
        if !rendered.is_empty() {
            sections.push(format!("[{}]\n{}", block.block_name, rendered));
        }
    }
    sections.join("\n\n")
}

// ---------------------------------------------------------------------------
// Response parsing — second-layer parsing of specific action content
// ---------------------------------------------------------------------------

/// Parse dual-output summary: split into summary text and knowledge text.
/// The knowledge_marker contains the current beat's random token, ensuring
/// it cannot match any historical content (self-reference defense).
pub fn parse_summary_dual_output(
    raw: &str,
    summary_marker: &str,
    knowledge_marker: &str,
) -> (String, String) {
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

/// Extract MSG IDs from current.txt content.
/// Only matches trusted markers (send success / read_msg format).
/// Returns IDs in **appearance order** (not sorted), to avoid
/// stale timestamps in exec results from expanding the range.
pub fn extract_msg_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let marker = "[MSG:";
    let send_context = out::MSG_SEND_CONTEXT;
    let read_context = out::MSG_READ_CONTEXT;
    let mut search_from = 0;

    while let Some(start) = text[search_from..].find(marker) {
        let bracket_pos = search_from + start;
        let abs_start = bracket_pos + marker.len();
        if let Some(end) = text[abs_start..].find(']') {
            let candidate = &text[abs_start..abs_start + end];
            if candidate.len() == 14 && candidate.chars().all(|c| c.is_ascii_digit()) {
                let after_bracket = abs_start + end + 1;
                let is_send = bracket_pos >= send_context.len()
                    && text
                        .get(bracket_pos - send_context.len()..bracket_pos)
                        .map_or(false, |s| s == send_context);
                let is_read = text
                    .get(after_bracket..)
                    .map_or(false, |s| s.starts_with(read_context));
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

// ---------------------------------------------------------------------------
// Helper functions (pure, no Alice dependency)
// ---------------------------------------------------------------------------

/// Build skill content: default skill (app guide) + instance custom skill + extra skills.
/// Returns combined content WITHOUT section header (caller/ToMarkdown adds it).
pub fn build_skill_content(
    host: Option<&str>,
    instance_id: &str,
    http_port: u16,
    skill_content: &str,
    extra_skills: &str,
) -> String {
    let default_skill = make_reserved_skill(host, instance_id, http_port);

    let mut parts: Vec<&str> = Vec::new();
    if !default_skill.is_empty() {
        parts.push(&default_skill);
    }
    if !skill_content.trim().is_empty() {
        parts.push(skill_content);
    }
    if !extra_skills.trim().is_empty() {
        parts.push(extra_skills);
    }

    parts.join("\n\n")
}

/// Build knowledge content with sub-section header.
/// Returns content WITH "### 要点与知识 ###" sub-header, or empty string.
pub fn build_knowledge_content(knowledge_content: &str) -> String {
    if knowledge_content.trim().is_empty() {
        String::new()
    } else {
        messages::knowledge_section(knowledge_content)
    }
}

/// Build environment info string.
pub fn build_environment(
    instance_id: &str,
    instance_name: Option<&str>,
    contacts_info: &str,
    shell_env: &str,
    host: Option<&str>,
) -> String {
    let name_part = match instance_name {
        Some(name) if !name.is_empty() => format!("{}（{}）", name, instance_id),
        _ => instance_id.to_string(),
    };
    let mut lines = vec![format!("你是{}", name_part)];
    if !contacts_info.is_empty() {
        lines.push(contacts_info.to_string());
    }
    lines.push(format!("脚本环境：{}", shell_env));
    let host_line = make_host_line(host);
    if !host_line.is_empty() {
        lines.push(host_line);
    }
    lines.join("\n")
}

/// Build status string for the "当前状态" section.
pub fn build_status(
    instance_id: &str,
    instance_name: Option<&str>,
    system_start_time: chrono::DateTime<chrono::Local>,
    unread_count: usize,
    history_size: usize,
    sessions_size: usize,
    current_size: usize,
    knowledge_size: usize,
    skill_size: usize,
) -> String {
    let current_time = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let start_time = system_start_time.format("%Y%m%d%H%M%S").to_string();

    let memory_status = make_memory_status(
        instance_id,
        instance_name,
        history_size,
        sessions_size,
        current_size,
        knowledge_size + skill_size,
    );

    format!(
        "现在时刻：[{}]\n系统启动时刻：[{}]\n收件箱未读来信：[{}] 条\n{}",
        current_time, start_time, unread_count, memory_status
    )
}

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
    let knowledge_indicator = if knowledge_size < 51200 {
        messages::knowledge_capacity_ok(knowledge_size)
    } else if knowledge_size < 61440 {
        messages::knowledge_capacity_warning(knowledge_size)
    } else {
        messages::knowledge_capacity_critical(knowledge_size)
    };

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
        status.push_str(&messages::memory_over_limit(kb));
    }

    status
}

/// Build host info line (just the public address, if available).
fn make_host_line(host: Option<&str>) -> String {
    match host {
        Some(h) if !h.is_empty() => messages::host_info(h),
        _ => String::new(),
    }
}

/// Build reserved skill section (app guide + vision API + uploads).
/// Returns empty string if no host is configured.
fn make_reserved_skill(host: Option<&str>, instance_id: &str, port: u16) -> String {
    let h = match host {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => format!("localhost:{}", port),
    };
    RESERVED_SKILL_TEMPLATE
        .replace("{host}", &h)
        .replace("{instance}", instance_id)
        .replace("{port}", &port.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_summary_dual_output_with_knowledge() {
        let raw = "This is summary\n===KNOWLEDGE_abc123===\nknowledge content here";
        let (summary, knowledge) =
            parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_abc123===");
        assert_eq!(summary, "This is summary");
        assert_eq!(knowledge, "knowledge content here");
    }

    #[test]
    fn test_parse_summary_dual_output_no_knowledge() {
        let raw = "Just summary, no knowledge marker";
        let (summary, knowledge) =
            parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_xyz===");
        assert_eq!(summary, "Just summary, no knowledge marker");
        assert_eq!(knowledge, "");
    }

    #[test]
    fn test_parse_summary_dual_output_strip_summary_marker() {
        let raw = "===SUMMARY===\nActual summary\n===KNOWLEDGE_t1===\nknowledge";
        let (summary, knowledge) =
            parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_t1===");
        assert_eq!(summary, "Actual summary");
        assert_eq!(knowledge, "knowledge");
    }

    #[test]
    fn test_extract_msg_ids_send() {
        let text = "send success [MSG:20260303120000]\nsome other text";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260303120000"]);
    }

    #[test]
    fn test_extract_msg_ids_read() {
        let text = "[MSG:20260303130000]发来一条消息：\nhello";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260303130000"]);
    }

    #[test]
    fn test_extract_msg_ids_mixed() {
        let text = "send success [MSG:20260303120000]\n[MSG:20260303130000]发来一条消息：\nhello\nsend success [MSG:20260303120000]";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260303120000", "20260303130000"]);
    }

    #[test]
    fn test_extract_msg_ids_ignores_untrusted() {
        let text = "random [MSG:20260303120000] text without context";
        let ids = extract_msg_ids(text);
        assert!(ids.is_empty());
    }

    #[test]
    fn test_extract_msg_ids_system_message() {
        let text = "[系统通知] [MSG:20260307103449]发来一条消息：\n\n[验证] system消息测试\n";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260307103449"]);
    }

    #[test]
    fn test_make_memory_status_basic() {
        let status = make_memory_status("test-id", Some("TestBot"), 1000, 2000, 3000, 4000);
        assert!(status.contains("TestBot"));
        assert!(status.contains("test-id"));
        assert!(status.contains("3000"));
    }

    #[test]
    fn test_make_memory_status_over_limit() {
        let status = make_memory_status("id", None, 50000, 50000, 50000, 50000);
        assert!(status.contains("⚠️"));
    }

    #[test]
    fn test_make_host_line() {
        assert_eq!(
            make_host_line(Some("1.2.3.4:8080")),
            messages::host_info("1.2.3.4:8080")
        );
        assert_eq!(make_host_line(None), "");
        assert_eq!(make_host_line(Some("")), "");
    }

    #[test]
    fn test_make_reserved_skill_no_host() {
        // When host is None or empty, falls back to localhost:{port}
        let result = make_reserved_skill(None, "test", 8081);
        assert!(result.contains("localhost:8081"), "should fallback to localhost:port");
        assert!(result.contains("test"), "should contain instance_id");

        let result2 = make_reserved_skill(Some(""), "test", 8081);
        assert!(result2.contains("localhost:8081"), "empty host should also fallback");
    }

    #[test]
    fn test_format_session_entries_basic() {
        let entries = vec![SessionEntryData {
            messages: vec![PromptMessage {
                role: "user".into(),
                sender: "user".into(),
                timestamp: "20260303120000".into(),
                content: "hello".into(),
            }],
            summary: "User said hello".into(),
        }];
        let rendered = format_session_entries(&entries, "test_self");
        assert!(rendered.contains("user"));
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("User said hello"));
    }

    #[test]
    fn test_format_session_entries_truncates() {
        let long_content = "x".repeat(300);
        let entries = vec![SessionEntryData {
            messages: vec![PromptMessage {
                role: "agent".into(),
                sender: "test_self".into(),
                timestamp: "20260303120000".into(),
                content: long_content,
            }],
            summary: String::new(),
        }];
        let rendered = format_session_entries(&entries, "test_self");
        assert!(!rendered.contains(&"x".repeat(300)));
    }

    #[test]
    fn test_format_session_entries_empty() {
        let entries: Vec<SessionEntryData> = vec![];
        assert_eq!(format_session_entries(&entries, "test_self"), "");
    }

    #[test]
    fn test_build_skill_content() {
        let result = build_skill_content(Some("1.2.3.4:8080"), "test", 8081, "custom skill", "extra");
        assert!(result.contains("custom skill"));
        assert!(result.contains("extra"));
    }

    #[test]
    fn test_build_skill_content_empty() {
        let result = build_skill_content(None, "test", 8081, "", "");
        // Should still have reserved skill (app guide)
        assert!(result.contains("localhost:8081"));
    }

    #[test]
    fn test_build_knowledge_content() {
        let result = build_knowledge_content("# 泛准则\n- 谨慎加信任");
        assert!(result.contains("### 要点与知识 ###"));
        assert!(result.contains("谨慎加信任"));
    }

    #[test]
    fn test_build_knowledge_content_empty() {
        assert_eq!(build_knowledge_content(""), "");
        assert_eq!(build_knowledge_content("  "), "");
    }

    #[test]
    fn test_build_environment() {
        let env = build_environment("abc123", Some("TestBot"), "联系人列表", "Linux x86_64", Some("1.2.3.4:8080"));
        assert!(env.contains("TestBot（abc123）"));
        assert!(env.contains("联系人列表"));
        assert!(env.contains("脚本环境：Linux x86_64"));
        assert!(env.contains("1.2.3.4:8080"));
    }

    #[test]
    fn test_build_environment_no_contacts() {
        let env = build_environment("abc123", None, "", "Linux x86_64", None);
        assert!(env.contains("你是abc123"));
        assert!(!env.contains("联系人"));
    }

    #[test]
    fn test_to_markdown_output() {
        use mad_hatter::llm::ToMarkdown;
        let request = BeatRequest {
            skill: String::new(),
            knowledge: String::new(),
            history: "(空)".to_string(),
            sessions: String::new(),
            environment: "你是test".to_string(),
            current: "(空)".to_string(),
            status: "现在时刻：[20260311120000]".to_string(),
        };
        let output = request.to_markdown();
        // Empty skill and knowledge should be skipped by ToMarkdown
        assert!(!output.contains("### skill ###"));
        assert!(!output.contains("### 知识 ###"));
        // Non-empty fields should have headers
        assert!(output.contains("### 经历 ###"));
        assert!(output.contains("### 环境信息 ###"));
        assert!(output.contains("### current ###"));
        assert!(output.contains("### 当前状态 ###"));
        // Scene description (struct-level doc comment) should appear at the top
        assert!(output.contains("你醒了"));
    }
}

