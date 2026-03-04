//! # Beat Inference Protocol
//!
//! Defines the request/response protocol for one React cognitive beat.
//! BeatRequest is a pure data struct; render() is a pure function.
//! Callers (prompt module) are responsible for extracting raw data from Alice.

use crate::inference::safe_render;
use crate::policy::messages;

// Compile-time embedded templates
const TEMPLATE_SYSTEM: &str = include_str!("../../templates/react_system.txt");
const TEMPLATE_USER: &str = include_str!("../../templates/react_user.txt");
const APP_GUIDE_TEMPLATE: &str = include_str!("../../templates/app_guide.txt");

pub const INITIAL_HISTORY: &str = include_str!("../../templates/initial_history.txt");

pub const KNOWLEDGE_FILE: &str = "knowledge.md";
const SOFT_LIMIT: usize = 180_000;


use crate::policy::action_output as out;
use crate::policy::EngineConfig;

// ---------------------------------------------------------------------------
// Data structs — raw data extracted from Alice's state
// ---------------------------------------------------------------------------

/// A single chat message (raw data, not yet formatted).
pub struct PromptMessage {
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

/// All data needed to render one beat's prompts.
/// Callers fill this struct from Alice's state; render() is a pure function.
pub struct BeatRequest {
    pub action_token: String,
    pub instance_id: String,
    pub instance_name: Option<String>,
    pub shell_env: String,
    pub host: Option<String>,
    pub system_start_time: chrono::DateTime<chrono::Local>,
    pub knowledge_content: String,
    pub history_content: String,
    pub session_blocks: Vec<SessionBlockData>,
    pub current_content: String,
    pub skill_content: String,
    pub unread_count: usize,
}

/// Snapshot of memory state at the time of prompt rendering.
pub struct MemorySnapshot {
    pub history: String,
    pub current: String,
}

impl BeatRequest {
    /// Render system and user prompts from this request's data.
    /// Returns `(system_prompt, user_prompt, memory_snapshot)`.
    pub fn render(&self) -> (String, String, MemorySnapshot) {
        let current_time = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        let system_start_time = self.system_start_time.format("%Y%m%d%H%M%S").to_string();

        // System prompt
        let system_prompt = safe_render(TEMPLATE_SYSTEM, &[
            ("{{TOKEN}}", &self.action_token),

        ]);

        // Format raw data into display strings
        let history_display = if self.history_content.is_empty() {
            messages::empty_placeholder().to_string()
        } else {
            self.history_content.clone()
        };

        let current_display = if self.current_content.is_empty() {
            messages::empty_placeholder().to_string()
        } else {
            self.current_content.clone()
        };

        let daily_rendered = self.render_session_blocks();

        // Knowledge section (without app guide)
        let knowledge_section = self.build_knowledge_section();

        // Skill section: default skill (app guide) + instance custom skill
        let skill_section = self.build_skill_section();

        // Memory status
        let memory_status = make_memory_status(
            &self.instance_id,
            self.instance_name.as_deref(),
            history_display.len(),
            daily_rendered.len(),
            current_display.len(),
            knowledge_section.len() + skill_section.len(),
        );

        let host_line = make_host_line(self.host.as_deref());

        // User prompt
        let user_prompt = safe_render(TEMPLATE_USER, &[
            ("{{CURRENT_TIME}}", &current_time),
            ("{{SYSTEM_START_TIME}}", &system_start_time),
            ("{{UNREAD_COUNT}}", &self.unread_count.to_string()),
            ("{{MEMORY_STATUS}}", &memory_status),
            ("{{INSTANCE_ID}}", &self.instance_id),
            ("{{SHELL_ENV}}", &self.shell_env),
            ("{{HOST_INFO}}", &host_line),
            ("{{SKILL}}", &skill_section),
            ("{{KNOWLEDGE}}", &knowledge_section),
            ("{{HISTORY_MEMORY}}", &history_display),
            ("{{DAILY_MEMORY}}", &daily_rendered),
            ("{{CURRENT_MEMORY}}", &current_display),
        ]);

        let snapshot = MemorySnapshot {
            history: self.history_content.clone(),
            current: self.current_content.clone(),
        };

        (system_prompt, user_prompt, snapshot)
    }

    /// Render all session blocks into display text.
    fn render_session_blocks(&self) -> String {
        if self.session_blocks.is_empty() {
            return String::new();
        }

        let mut sections = Vec::new();
        for block in &self.session_blocks {
            let rendered = format_session_entries(&block.entries);
            if !rendered.is_empty() {
                sections.push(format!("[{}]\n{}", block.block_name, rendered));
            }
        }
        sections.join("\n\n")
    }

    /// Build knowledge section (pure knowledge, no app guide).
    fn build_knowledge_section(&self) -> String {
        if self.knowledge_content.trim().is_empty() {
            String::new()
        } else {
            messages::knowledge_section(&self.knowledge_content)
        }
    }

    /// Build skill section: default skill (app guide) + instance custom skill.
    fn build_skill_section(&self) -> String {
        let default_skill = make_app_guide_knowledge(self.host.as_deref(), &self.instance_id);

        let combined = match (default_skill.is_empty(), self.skill_content.trim().is_empty()) {
            (true, true) => return String::new(),
            (false, true) => default_skill,
            (true, false) => self.skill_content.clone(),
            (false, false) => format!("{}\n\n{}", default_skill, self.skill_content),
        };

        format!("### skill ###\n{}\n", combined)
    }
}

// ---------------------------------------------------------------------------
// Public formatting — used by both render() and compress
// ---------------------------------------------------------------------------

impl From<&crate::persist::SessionBlockEntry> for SessionEntryData {
    fn from(entry: &crate::persist::SessionBlockEntry) -> Self {
        SessionEntryData {
            messages: vec![],
            summary: entry.summary.clone(),
        }
    }
}

/// Format session entries into display text.
/// Used by render() for beat prompts and by compress for history rolling.
pub fn format_session_entries(entries: &[SessionEntryData]) -> String {
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
            parts.push(messages::chat_message(&msg.sender, &msg.timestamp, &content_display));
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

// ---------------------------------------------------------------------------
// Response parsing — second-layer parsing of specific action content
// ---------------------------------------------------------------------------

/// Parse dual-output summary: split into summary text and knowledge text.
/// The knowledge_marker contains the current beat's random token, ensuring
/// it cannot match any historical content (self-reference defense).
pub fn parse_summary_dual_output(raw: &str, summary_marker: &str, knowledge_marker: &str) -> (String, String) {
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
                    && text.get(bracket_pos - send_context.len()..bracket_pos)
                        .map_or(false, |s| s == send_context);
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

// ---------------------------------------------------------------------------
// Helper functions (pure, no Alice dependency)
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
        Some(h) if !h.is_empty() => {
            let host_display = h.split(':').next().unwrap_or(h);
            messages::host_info(host_display)
        }
        _ => String::new(),
    }
}

/// Build app development guide as a forced-loaded knowledge section.
/// Returns empty string if no host is configured.
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

    #[test]
    fn test_parse_summary_dual_output_with_knowledge() {
        let raw = "This is summary\n===KNOWLEDGE_abc123===\nknowledge content here";
        let (summary, knowledge) = parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_abc123===");
        assert_eq!(summary, "This is summary");
        assert_eq!(knowledge, "knowledge content here");
    }

    #[test]
    fn test_parse_summary_dual_output_no_knowledge() {
        let raw = "Just summary, no knowledge marker";
        let (summary, knowledge) = parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_xyz===");
        assert_eq!(summary, "Just summary, no knowledge marker");
        assert_eq!(knowledge, "");
    }

    #[test]
    fn test_parse_summary_dual_output_strip_summary_marker() {
        let raw = "===SUMMARY===\nActual summary\n===KNOWLEDGE_t1===\nknowledge";
        let (summary, knowledge) = parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_t1===");
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
        assert_eq!(make_host_line(Some("1.2.3.4:8080")), messages::host_info("1.2.3.4"));
        assert_eq!(make_host_line(None), "");
        assert_eq!(make_host_line(Some("")), "");
    }

    #[test]
    fn test_make_app_guide_no_host() {
        assert_eq!(make_app_guide_knowledge(None, "test"), "");
        assert_eq!(make_app_guide_knowledge(Some(""), "test"), "");
    }

    #[test]
    fn test_format_session_entries_basic() {
        let entries = vec![
            SessionEntryData {
                messages: vec![
                    PromptMessage { sender: "user1".into(), timestamp: "20260303120000".into(), content: "hello".into() },
                ],
                summary: "User said hello".into(),
            },
        ];
        let rendered = format_session_entries(&entries);
        assert!(rendered.contains("user1"));
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("User said hello"));
    }

    #[test]
    fn test_format_session_entries_truncates() {
        let long_content = "x".repeat(300);
        let entries = vec![
            SessionEntryData {
                messages: vec![
                    PromptMessage { sender: "user1".into(), timestamp: "20260303120000".into(), content: long_content },
                ],
                summary: String::new(),
            },
        ];
        let rendered = format_session_entries(&entries);
        assert!(!rendered.contains(&"x".repeat(300)));
    }

    #[test]
    fn test_format_session_entries_empty() {
        let entries: Vec<SessionEntryData> = vec![];
        assert_eq!(format_session_entries(&entries), "");
    }
}

