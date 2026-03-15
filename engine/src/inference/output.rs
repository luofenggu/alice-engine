//! # Action Output Types
//!
//! Structured output from action execution.
//! ActionOutput enum mirrors Action enum — one output variant per action type.
//! ActionView provides flattened rendering for current prompt.

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


/// Flattened view of an action for current prompt rendering.
///
/// Built from DB rows (action_input + action_output JSON).
/// Only derives ToMarkdown — never used for parsing.
/// Fields are detailed Option<T> — no big concatenated strings.
#[derive(Debug, ToMarkdown)]
pub struct ActionView {
        pub action_id: String,

    /// action_type
    pub action_type: String,

    /// started_at
    pub started_at: Option<String>,

    // -- input side --

    /// content
    #[markdown(weak)]
    pub content: Option<String>,

    /// recipient
    pub recipient: Option<String>,

    /// message
    #[markdown(weak)]
    pub message: Option<String>,

    /// file_path
    pub file_path: Option<String>,

    /// file_content
    #[markdown(weak)]
    pub file_content: Option<String>,

    /// search_text
    #[markdown(weak)]
    pub search_text: Option<String>,

    /// replace_text
    #[markdown(weak)]
    pub replace_text: Option<String>,

    /// timeout_secs
    pub timeout_secs: Option<u64>,

    /// target_action_id
    pub target_action_id: Option<String>,

    /// distill_summary
    #[markdown(weak)]
    pub distill_summary: Option<String>,

    /// instance_name
    pub instance_name: Option<String>,

    /// knowledge
    #[markdown(weak)]
    pub knowledge: Option<String>,

    /// profile_settings
    pub profile_settings: Option<String>,

    // -- output side --

    /// stdout
    #[markdown(weak)]
    pub stdout: Option<String>,

    /// exit_code
    pub exit_code: Option<i32>,

    /// elapsed_secs
    pub elapsed_secs: Option<f64>,

    /// skeleton
    #[markdown(weak)]
    pub skeleton: Option<String>,

    /// msg_id
    pub msg_id: Option<String>,

    /// messages
    #[markdown(weak)]
    pub messages: Option<String>,

    /// match_count
    pub match_count: Option<usize>,

    /// before_context
    #[markdown(weak)]
    pub before_context: Option<String>,

    /// after_context
    #[markdown(weak)]
    pub after_context: Option<String>,

    /// block_name
    pub block_name: Option<String>,

    /// created_instance_id
    pub created_instance_id: Option<String>,

    /// error
    #[markdown(weak)]
    pub error: Option<String>,

    // -- status --

    /// note
    #[markdown(weak)]
    pub note: Option<String>,

    /// distill_text
    #[markdown(weak)]
    pub distill_text: Option<String>,
}

impl ActionView {
    /// Create an empty ActionView with only action_id and action_type set.
    pub(crate) fn empty(action_id: String, action_type: String) -> Self {
        ActionView {
            action_id,
            action_type,
            started_at: None,
            content: None,
            recipient: None,
            message: None,
            file_path: None,
            file_content: None,
            search_text: None,
            replace_text: None,
            timeout_secs: None,
            target_action_id: None,
            distill_summary: None,
            instance_name: None,
            knowledge: None,
            profile_settings: None,
            stdout: None,
            exit_code: None,
            elapsed_secs: None,
            skeleton: None,
            msg_id: None,
            messages: None,
            match_count: None,
            before_context: None,
            after_context: None,
            block_name: None,
            created_instance_id: None,
            error: None,
            note: None,
            distill_text: None,
        }
    }

    /// Fill input fields from a deserialized Action.
    fn fill_input(&mut self, action: &Action) {
        match action {
            Action::Idle { timeout_secs } => {
                self.timeout_secs = *timeout_secs;
            }
            Action::ReadMsg => {}
            Action::SendMsg { recipient, content } => {
                self.recipient = Some(recipient.clone());
                self.message = Some(content.clone());
            }
            Action::Thinking { content } => {
                self.content = Some(content.clone());
            }
            Action::Script { content } => {
                self.content = Some(content.clone());
            }
            Action::WriteFile { path, content } => {
                self.file_path = Some(path.clone());
                self.file_content = Some(content.clone());
            }
            Action::ReplaceInFile { path, search, replace } => {
                self.file_path = Some(path.clone());
                self.search_text = Some(search.clone());
                self.replace_text = Some(replace.clone());
            }
            Action::Summary { content } => {
                self.content = Some(content.clone());
            }
            Action::Distill { target_action_id, summary } => {
                self.target_action_id = Some(target_action_id.clone());
                self.distill_summary = Some(summary.clone());
            }
            Action::SetProfile { content } => {
                self.profile_settings = Some(content.clone());
            }
            Action::CreateInstance { name, knowledge } => {
                self.instance_name = Some(name.clone());
                self.knowledge = Some(knowledge.clone());
            }
        }
    }

    /// Fill output fields from a deserialized ActionOutput.
    fn fill_output(&mut self, output: &ActionOutput) {
        match output {
            ActionOutput::Empty => {}
            ActionOutput::ReadMsg { entries } => {
                let rendered: Vec<String> = entries.iter().map(|e| render_msg_entry(e)).collect();
                self.messages = Some(rendered.join("\n"));
            }
            ActionOutput::SendMsg { msg_id } => {
                self.msg_id = Some(msg_id.clone());
            }
            ActionOutput::SendMsgFailed { error } => {
                self.error = Some(error.clone());
            }
            ActionOutput::Script { stdout, exit_code, elapsed_secs, truncated: _ } => {
                let (display_stdout, _) = truncate_stdout(stdout);
                self.stdout = Some(display_stdout);
                self.exit_code = Some(*exit_code);
                self.elapsed_secs = Some(*elapsed_secs);
            }
            ActionOutput::WriteFile { skeleton, bytes: _, lines: _ } => {
                self.skeleton = Some(skeleton.clone());
            }
            ActionOutput::ReplaceInFile { match_count, before, after } => {
                self.match_count = Some(*match_count);
                self.before_context = Some(before.clone());
                self.after_context = Some(after.clone());
            }
            ActionOutput::ReplaceInFileFailed { match_count, search_preview } => {
                self.match_count = Some(*match_count);
                self.error = Some(format!("match_count={}, search: {}", match_count, search_preview));
            }
            ActionOutput::Summary { block_name, knowledge_chars: _, msg_count: _, msg_range: _ } => {
                self.block_name = Some(block_name.clone());
            }
            ActionOutput::SummaryEmpty => {}
            ActionOutput::Distill { old_bytes: _, new_bytes: _ } => {}
            ActionOutput::SetProfile { updated } => {
                self.note = Some(updated.clone());
            }
            ActionOutput::SetProfileFailed { unknown_key } => {
                self.error = Some(format!("unknown key: {}", unknown_key));
            }
            ActionOutput::CreateInstance { instance_id, name, knowledge_bytes: _ } => {
                self.created_instance_id = Some(instance_id.clone());
                self.note = Some(format!("created: {} ({})", name, instance_id));
            }
            ActionOutput::Note { text } => {
                self.note = Some(text.clone());
            }
        }
    }

    /// Build from a DB row. Falls back gracefully on deserialization failure.
    pub fn from_db_row(row: &ActionLogRow) -> Self {
        use crate::bindings::db::{ACTION_STATUS_DISTILLED, ACTION_STATUS_DONE};

        let status = match row.status.as_str() {
            s if s == ACTION_STATUS_DONE => ActionStatus::Done,
            s if s == ACTION_STATUS_DISTILLED => ActionStatus::Distilled,
            _ => ActionStatus::Executing,
        };

        // Parse action for fill_input
        let action: Option<Action> = if row.action_input.is_empty() {
            None
        } else {
            serde_json::from_str(&row.action_input).ok()
        };

        let mut view = ActionView::empty(row.action_id.clone(), row.action_type.clone());
        view.started_at = Some(row.created_at.clone());

        // Executing status marker in note field
        if status == ActionStatus::Executing {
            view.note = Some("---action executing, result pending---".to_string());
        }

        // Distilled: only show distill_text
        if status == ActionStatus::Distilled {
            view.distill_text = row.distill_text.clone();
            return view;
        }

        // Fill input fields
        if let Some(ref action) = action {
            view.fill_input(action);
        }

        // Fill output fields (only when done)
        if status == ActionStatus::Done {
            if let Some(ref output_str) = row.action_output {
                if !output_str.is_empty() {
                    if let Ok(output) = serde_json::from_str::<ActionOutput>(output_str) {
                        if !matches!(output, ActionOutput::Empty) {
                            view.fill_output(&output);
                        }
                    } else {
                        // Deserialization failed — show raw as note
                        view.note = Some(truncate_for_display(output_str, 200));
                    }
                }
            }
        }

        // For non-enum action types (inference_error, etc.) with no input
        if row.action_input.is_empty() && view.note.is_none() {
            if let Some(ref output_str) = row.action_output {
                if !output_str.is_empty() {
                    // Try to parse as ActionOutput::Note
                    if let Ok(ActionOutput::Note { text }) = serde_json::from_str::<ActionOutput>(output_str) {
                        view.note = Some(text);
                    } else {
                        view.note = Some(truncate_for_display(output_str, 200));
                    }
                }
            }
        }

        view
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