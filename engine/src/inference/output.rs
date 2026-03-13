//! # Action Output Types
//!
//! Structured output from action execution.
//! ActionOutput enum mirrors Action enum — one output variant per action type.
//! ActionRecord combines input + status + output for rendering.

use serde::{Deserialize, Serialize};

use crate::bindings::db::ActionLogRow;
use crate::inference::Action;

/// Truncation limit for stdout display.
const STDOUT_TRUNCATE_LIMIT: usize = 102_400;

/// Structured output from action execution.
///
/// Each variant corresponds to an Action input variant.
/// Serialized as JSON with `type` tag for storage in action_log.action_output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ActionOutput {
    /// No output (idle, thinking)
    Empty,

    /// Messages read from inbox
    ReadMsg {
        /// List of message entries
        entries: Vec<ReadMsgEntry>,
    },

    /// Message sent successfully
    SendMsg {
        /// Message ID (timestamp) assigned after sending
        msg_id: String,
    },

    /// Message send failed
    SendMsgFailed {
        /// Human-readable error description
        error: String,
    },

    /// Script execution result
    Script {
        /// Standard output (may be truncated)
        stdout: String,
        /// Process exit code
        exit_code: i32,
        /// Execution time in seconds
        elapsed_secs: f64,
        /// Whether stdout was truncated
        truncated: bool,
    },

    /// File written successfully
    WriteFile {
        /// Extracted skeleton (interfaces + comments)
        skeleton: String,
        /// File size in bytes
        bytes: usize,
        /// Number of lines
        lines: usize,
    },

    /// Replace in file succeeded
    ReplaceInFile {
        /// Number of matches (always 1 for success)
        match_count: usize,
        /// Context before replacement
        before: String,
        /// Context after replacement
        after: String,
    },

    /// Replace in file failed
    ReplaceInFileFailed {
        /// Number of matches found (0 or >1)
        match_count: usize,
        /// Preview of search text
        search_preview: String,
    },

    /// Summary completed successfully
    Summary {
        /// Session block name created
        block_name: String,
        /// Knowledge character count after capture
        knowledge_chars: usize,
        /// Number of message IDs captured
        msg_count: i64,
        /// Message ID range string
        msg_range: String,
    },

    /// Summary skipped (nothing to summarize)
    SummaryEmpty,

    /// Distill completed
    Distill {
        /// Original output size in bytes
        old_bytes: usize,
        /// Distilled text size in bytes
        new_bytes: usize,
    },

    /// Profile updated successfully
    SetProfile {
        /// Description of what was updated
        updated: String,
    },

    /// Profile update failed
    SetProfileFailed {
        /// The unknown key that was provided
        unknown_key: String,
    },

    /// New instance created
    CreateInstance {
        /// Instance ID of the created instance
        instance_id: String,
        /// Display name
        name: String,
        /// Knowledge size in bytes
        knowledge_bytes: usize,
    },

    /// Generic text note (for interrupts, rejects, errors, etc.)
    Note {
        /// Note text content
        text: String,
    },
}

/// A single message entry from read_msg.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadMsgEntry {
    /// Message role (user/agent/system)
    pub role: String,
    /// Sender identifier
    pub sender: String,
    /// Message timestamp
    pub timestamp: String,
    /// Message content
    pub content: String,
    /// Whether sender is known (in contacts)
    pub is_known: bool,
}

/// Status of an action in the action log.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionStatus {
    Executing,
    Done,
    Distilled,
}

/// Complete action record for rendering.
pub struct ActionRecord {
    pub action_id: String,
    pub action_type: String,
    pub input: Action,
    pub status: ActionStatus,
    pub output: Option<ActionOutput>,
    pub distill_text: Option<String>,
}

impl ActionRecord {
    /// Build from a DB row. Falls back to Note on deserialization failure.
    pub fn from_db_row(row: &ActionLogRow) -> Self {
        use crate::bindings::db::{ACTION_STATUS_DISTILLED, ACTION_STATUS_DONE};

        let input: Action = serde_json::from_str(&row.action_input)
            .unwrap_or_else(|_| Action::Thinking {
                content: format!("[deserialization failed] {}", truncate_for_display(&row.action_input, 100)),
            });

        let status = match row.status.as_str() {
            s if s == ACTION_STATUS_DONE => ActionStatus::Done,
            s if s == ACTION_STATUS_DISTILLED => ActionStatus::Distilled,
            _ => ActionStatus::Executing,
        };

        // For distilled records, don't load output (save space in prompt)
        let output = if status == ActionStatus::Distilled {
            None
        } else {
            row.action_output.as_deref().and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    serde_json::from_str::<ActionOutput>(s)
                        .ok()
                        .or_else(|| Some(ActionOutput::Note { text: s.to_string() }))
                }
            })
        };

        ActionRecord {
            action_id: row.action_id.clone(),
            action_type: row.action_type.clone(),
            input,
            status,
            output,
            distill_text: row.distill_text.clone(),
        }
    }

    /// Render this record for inclusion in the prompt current section.
    pub fn render(&self) -> String {
        let mut s = String::new();
        let block_start = format!("---------行为编号[{}]开始---------\n", self.action_id);
        let block_end = format!("---------行为编号[{}]结束---------\n", self.action_id);

        match &self.status {
            ActionStatus::Distilled => {
                s.push_str(&block_start);
                s.push_str("[已提炼] ");
                if let Some(ref dt) = self.distill_text {
                    s.push_str(dt);
                }
                s.push('\n');
                s.push_str(&block_end);
            }
            ActionStatus::Executing => {
                s.push_str(&block_start);
                s.push_str(&describe_action_input(&self.input));
                s.push_str("\n---action executing, result pending---\n");
                s.push_str(&block_end);
            }
            ActionStatus::Done => {
                s.push_str(&block_start);
                s.push_str(&describe_action_input(&self.input));
                s.push_str("\n---action executing, result pending---\n");
                if let Some(ref output) = self.output {
                    let rendered = output.render();
                    if !rendered.is_empty() {
                        s.push('\n');
                        s.push_str(&rendered);
                        s.push('\n');
                    }
                }
                s.push_str(&block_end);
            }
        }
        s
    }
}

impl ActionOutput {
    /// Render this output as human-readable text for the prompt.
    pub fn render(&self) -> String {
        match self {
            ActionOutput::Empty => String::new(),

            ActionOutput::ReadMsg { entries } => {
                if entries.is_empty() {
                    "收件箱为空，没有未读消息。\n".to_string()
                } else {
                    let mut s = String::new();
                    for entry in entries {
                        s.push_str(&render_msg_entry(entry));
                        s.push('\n');
                    }
                    s
                }
            }

            ActionOutput::SendMsg { msg_id } => {
                format!("send success [MSG:{}]\n", msg_id)
            }

            ActionOutput::SendMsgFailed { error } => {
                format!("{}\n", error)
            }

            ActionOutput::Script { stdout, exit_code, elapsed_secs, truncated: _ } => {
                let mut s = format!("\n---exec result ({:.1}s)---\n", elapsed_secs);
                s.push_str(stdout);
                if !stdout.ends_with('\n') {
                    s.push('\n');
                }
                if *exit_code != 0 {
                    s.push_str(&format!("\n[exit code: {}]\n", exit_code));
                }
                s
            }

            ActionOutput::WriteFile { skeleton, bytes, lines } => {
                let mut s = format!("write success ({} bytes, {} lines)\n", bytes, lines);
                if !skeleton.is_empty() {
                    s.push_str("skeleton:\n");
                    s.push_str(skeleton);
                    if !skeleton.ends_with('\n') {
                        s.push('\n');
                    }
                }
                s
            }

            ActionOutput::ReplaceInFile { match_count: _, before, after: _ } => {
                format!("replaced 1 block(s) successfully\n  replaced '{}'\n", before)
            }

            ActionOutput::ReplaceInFileFailed { match_count, search_preview } => {
                if *match_count == 0 {
                    format!("replace failed: search text not found in file\n  search: '{}'\n", search_preview)
                } else {
                    format!("replace failed: search text matched {} times (must be unique)\n  search: '{}'\n", match_count, search_preview)
                }
            }

            ActionOutput::Summary { block_name, knowledge_chars, msg_count, msg_range } => {
                let mut s = format!("小结完成，已归档到session block [{}]\n", block_name);
                s.push_str(&format!("知识库: {} 字符\n", knowledge_chars));
                if *msg_count > 0 {
                    s.push_str(&format!("{}个消息ID ({})\n", msg_count, msg_range));
                } else {
                    s.push_str("0个消息ID\n");
                }
                s
            }

            ActionOutput::SummaryEmpty => {
                "nothing to summarize (current is empty)\n".to_string()
            }

            ActionOutput::Distill { old_bytes, new_bytes } => {
                format!("distilled: {} bytes → {} bytes\n", old_bytes, new_bytes)
            }

            ActionOutput::SetProfile { updated } => {
                format!("profile updated: {}\n", updated)
            }

            ActionOutput::SetProfileFailed { unknown_key } => {
                format!("profile update failed: unknown key '{}'\n", unknown_key)
            }

            ActionOutput::CreateInstance { instance_id, name, knowledge_bytes } => {
                format!("instance created: {} (id: {}, knowledge: {} bytes)\n", name, instance_id, knowledge_bytes)
            }

            ActionOutput::Note { text } => {
                if text.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", text)
                }
            }
        }
    }

    /// Extract msg_id from SendMsg variant.
    pub fn msg_id(&self) -> Option<&str> {
        match self {
            ActionOutput::SendMsg { msg_id } => Some(msg_id),
            _ => None,
        }
    }

    /// Extract all message timestamps from ReadMsg entries.
    pub fn msg_ids(&self) -> Vec<&str> {
        match self {
            ActionOutput::ReadMsg { entries } => {
                entries.iter().map(|e| e.timestamp.as_str()).collect()
            }
            _ => vec![],
        }
    }
}

/// Generate a human-readable description of the action input (the "doing" line).
pub fn describe_action_input(action: &Action) -> String {
    match action {
        Action::Idle { timeout_secs } => {
            match timeout_secs {
                Some(secs) => format!("idle ({}s)", secs),
                None => "idle".to_string(),
            }
        }
        Action::Thinking { content } => {
            format!("记录思考: {}", content)
        }
        Action::ReadMsg => {
            "你打开了收件箱，开始阅读来信。".to_string()
        }
        Action::SendMsg { recipient, content: _ } => {
            format!("寄出信件给 {}", recipient)
        }
        Action::Script { content } => {
            let preview = truncate_for_display(content, 80);
            format!("execute script: \n{}", preview)
        }
        Action::WriteFile { path, content: _ } => {
            format!("write file: {}", path)
        }
        Action::ReplaceInFile { path, search: _, replace: _ } => {
            format!("replace in file [{}]", path)
        }
        Action::Summary { content } => {
            let preview = truncate_for_display(content, 120);
            format!("小结: {}", preview)
        }
        Action::Distill { target_action_id, summary } => {
            let preview = truncate_for_display(summary, 80);
            format!("提炼 [{}]: {}", target_action_id, preview)
        }
        Action::SetProfile { content } => {
            format!("设置个人资料: {}", content)
        }
        Action::CreateInstance { name, knowledge } => {
            format!("创建实例: {} (knowledge: {} bytes)", name, knowledge.len())
        }
    }
}

/// Render a single message entry for display in the prompt.
pub fn render_msg_entry(entry: &ReadMsgEntry) -> String {
    let sender_display = if entry.role == "agent" && !entry.is_known {
        format!("⚠️ 此消息来自未知发送者：{}\n{}", entry.sender, entry.sender)
    } else if entry.role == "user" {
        "user".to_string()
    } else {
        entry.sender.clone()
    };

    format!(
        "{} [MSG:{}]发来一条消息：\n\n{}",
        sender_display, entry.timestamp, entry.content
    )
}

/// Truncate stdout for display, preserving head and tail.
/// Returns (truncated_text, was_truncated).
pub fn truncate_stdout(text: &str) -> (String, bool) {
    if text.len() <= STDOUT_TRUNCATE_LIMIT {
        return (text.to_string(), false);
    }
    let keep = STDOUT_TRUNCATE_LIMIT / 2;
    let head = &text[..keep];
    let tail = &text[text.len() - keep..];
    (format!(
        "{}...\n\n[truncated: {} bytes total, showing first and last {} bytes]\n\n...{}",
        head,
        text.len(),
        keep,
        tail
    ), true)
}

/// Truncate text for display purposes.
fn truncate_for_display(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        format!("{}...", &text[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_output_serialize_empty() {
        let output = ActionOutput::Empty;
        let json = serde_json::to_string(&output).unwrap();
        assert_eq!(json, r#"{"type":"Empty"}"#);
    }

    #[test]
    fn test_action_output_serialize_send_msg() {
        let output = ActionOutput::SendMsg { msg_id: "20260313120000".into() };
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("SendMsg"));
        assert!(json.contains("20260313120000"));
        let deserialized: ActionOutput = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ActionOutput::SendMsg { msg_id } if msg_id == "20260313120000"));
    }

    #[test]
    fn test_action_output_serialize_read_msg() {
        let output = ActionOutput::ReadMsg {
            entries: vec![ReadMsgEntry {
                role: "user".into(),
                sender: "user".into(),
                timestamp: "20260313120000".into(),
                content: "hello".into(),
                is_known: true,
            }],
        };
        let json = serde_json::to_string(&output).unwrap();
        let deserialized: ActionOutput = serde_json::from_str(&json).unwrap();
        match deserialized {
            ActionOutput::ReadMsg { entries } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].content, "hello");
            }
            _ => panic!("Expected ReadMsg"),
        }
    }

    #[test]
    fn test_action_output_render_empty() {
        let output = ActionOutput::Empty;
        assert_eq!(output.render(), "");
    }

    #[test]
    fn test_action_output_render_script() {
        let output = ActionOutput::Script {
            stdout: "hello world\n".into(),
            exit_code: 0,
            elapsed_secs: 1.5,
            truncated: false,
        };
        let rendered = output.render();
        assert!(rendered.contains("hello world"));
        assert!(rendered.contains("1.5s"));
    }

    #[test]
    fn test_action_output_render_script_nonzero_exit() {
        let output = ActionOutput::Script {
            stdout: "error\n".into(),
            exit_code: 1,
            elapsed_secs: 0.1,
            truncated: false,
        };
        let rendered = output.render();
        assert!(rendered.contains("[exit code: 1]"));
    }

    #[test]
    fn test_truncate_stdout_short() {
        let (text, truncated) = truncate_stdout("hello");
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_truncate_stdout_long() {
        let long = "x".repeat(STDOUT_TRUNCATE_LIMIT + 100);
        let (result, truncated) = truncate_stdout(&long);
        assert!(result.contains("[truncated"));
        assert!(result.len() < long.len());
        assert!(truncated);
    }

    #[test]
    fn test_describe_action_input_idle() {
        let desc = describe_action_input(&Action::Idle { timeout_secs: None });
        assert_eq!(desc, "idle");
        let desc = describe_action_input(&Action::Idle { timeout_secs: Some(120) });
        assert!(desc.contains("120"));
    }

    #[test]
    fn test_describe_action_input_read_msg() {
        let desc = describe_action_input(&Action::ReadMsg);
        assert!(desc.contains("收件箱"));
    }

    #[test]
    fn test_msg_id_extraction() {
        let send = ActionOutput::SendMsg { msg_id: "ts123".into() };
        assert_eq!(send.msg_id(), Some("ts123"));

        let empty = ActionOutput::Empty;
        assert_eq!(empty.msg_id(), None);
    }

    #[test]
    fn test_msg_ids_extraction() {
        let read = ActionOutput::ReadMsg {
            entries: vec![
                ReadMsgEntry {
                    role: "user".into(),
                    sender: "user".into(),
                    timestamp: "ts1".into(),
                    content: "a".into(),
                    is_known: true,
                },
                ReadMsgEntry {
                    role: "user".into(),
                    sender: "user".into(),
                    timestamp: "ts2".into(),
                    content: "b".into(),
                    is_known: true,
                },
            ],
        };
        let ids = read.msg_ids();
        assert_eq!(ids, vec!["ts1", "ts2"]);
    }
}

