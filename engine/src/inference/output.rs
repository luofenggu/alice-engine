//! # Action Output Types
//!
//! Structured output from action execution.
//! ActionOutput enum mirrors Action enum — one output variant per action type.
//! ActionRecord combines input + status + output for rendering.

use mad_hatter::ToMarkdown;
use serde::{Deserialize, Serialize};

use crate::bindings::db::ActionLogRow;
use crate::inference::Action;

/// Truncation limit for stdout display.
const STDOUT_TRUNCATE_LIMIT: usize = 102_400;

/// Structured output from action execution.
///
/// Each variant corresponds to an Action input variant.
/// Serialized as JSON with `type` tag for storage in action_log.action_output.
#[derive(Debug, Clone, Serialize, Deserialize, ToMarkdown)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ToMarkdown)]
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

#[derive(ToMarkdown)]
pub struct ActionRecord {
    /// action_id
    pub action_id: String,
    #[markdown(skip)]
    pub action_type: String,
    #[markdown(skip)]
    pub status: ActionStatus,
    #[markdown(flatten)]
    pub input: Option<Action>,
    #[markdown(flatten)]
    pub output: Option<ActionOutput>,
    /// distill_text
    pub distill_text: Option<String>,
}

impl ActionRecord {
    /// Build from a DB row. Falls back to Note on deserialization failure.
    pub fn from_db_row(row: &ActionLogRow) -> Self {
        use crate::bindings::db::{ACTION_STATUS_DISTILLED, ACTION_STATUS_DONE};

        let input: Option<Action> = if row.action_input.is_empty() {
            None
        } else {
            Some(serde_json::from_str(&row.action_input)
                .unwrap_or_else(|_| Action::Thinking {
                    content: format!("[deserialization failed] {}", truncate_for_display(&row.action_input, 100)),
                }))
        };

        let status = match row.status.as_str() {
            s if s == ACTION_STATUS_DONE => ActionStatus::Done,
            s if s == ACTION_STATUS_DISTILLED => ActionStatus::Distilled,
            _ => ActionStatus::Executing,
        };

        // For distilled records, don't load input or output (only distill_text)
        let (final_input, output) = if status == ActionStatus::Distilled {
            (None, None)
        } else if input.is_none() {
            // Empty action_input (e.g. interrupt/reject/inference_error notes)
            let output = row.action_output.as_deref().and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    serde_json::from_str::<ActionOutput>(s)
                        .ok()
                        .or_else(|| Some(ActionOutput::Note { text: s.to_string() }))
                        .and_then(|o| if matches!(o, ActionOutput::Empty) { None } else { Some(o) })
                }
            });
            (None, output)
        } else {
            let output = row.action_output.as_deref().and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    serde_json::from_str::<ActionOutput>(s)
                        .ok()
                        .or_else(|| Some(ActionOutput::Note { text: s.to_string() }))
                        .and_then(|o| if matches!(o, ActionOutput::Empty) { None } else { Some(o) })
                }
            });
            (input, output)
        };

        ActionRecord {
            action_id: row.action_id.clone(),
            action_type: row.action_type.clone(),
            input: final_input,
            status,
            output,
            distill_text: row.distill_text.clone(),
        }
    }
}

impl ActionOutput {
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
    let head_end = text.floor_char_boundary(keep);
    let tail_start = text.ceil_char_boundary(text.len() - keep);
    let head = &text[..head_end];
    let tail = &text[tail_start..];
    (format!(
        "{}...\n\n[truncated: {} bytes total, showing first and last ~{} bytes]\n\n...{}",
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
        let end = text.floor_char_boundary(max_len);
        format!("{}...", &text[..end])
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