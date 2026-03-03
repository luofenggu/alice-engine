//! # Action Module
//!
//! The action type system for Alice's cognitive loop.
//! Uses Rust enum for compile-time exhaustiveness — the "免死金牌".
//! Adding a new action variant forces handling in ALL match sites.
//!
//! @TRACE: ACTION

pub mod execute;

// ---------------------------------------------------------------------------
// Marker constants — indirect construction to avoid self-referential conflicts
// ---------------------------------------------------------------------------
// When editing this file with replace_in_file, literal "<<<" or ">>>END" in source
// would be misinterpreted as block delimiters. concat!() builds them at compile
// time without the literal appearing in source.

/// `<<<SEARCH` marker for replace_in_file blocks.
/// Self-referential defense: concat!() avoids literal markers in source.
pub const SEARCH_MARKER: &str = "<<<SEARCH";
/// `===REPLACE` marker for replace_in_file blocks  
pub const REPLACE_MARKER: &str = "===REPLACE";
/// `>>>END` end marker for replace_in_file blocks
pub const BLOCK_END_MARKER: &str = ">>>END";
/// `<<<REMEMBER` start marker
pub const REMEMBER_START_MARKER: &str = "<<<REMEMBER";
/// `>>>REMEMBER` end marker
pub const REMEMBER_END_MARKER: &str = ">>>REMEMBER";

use std::fmt;
use anyhow::{Result, bail};

// ---------------------------------------------------------------------------
// Action enum — the heart of the type system
// ---------------------------------------------------------------------------

/// All possible actions an agent can take in one cognitive step.
///
/// @HUB for action types. Each variant maps to a capability.
/// @TRACE: ACTION
#[derive(Debug, Clone)]
pub enum Action {
    /// Do nothing, wait for next beat. Optional timeout in seconds.
    Idle { timeout_secs: Option<u64> },

    /// Read unread messages from inbox.
    ReadMsg,

    /// Send a message to a recipient.
    SendMsg {
        recipient: String,
        content: String,
    },

    /// Record thinking/planning (visible in memory).
    Thinking {
        content: String,
    },

    /// Execute a local shell script.
    Script {
        content: String,
    },

    /// Write a file in workspace.
    WriteFile {
        path: String,
        content: String,
    },

    /// Search-and-replace blocks in a file.
    ReplaceInFile {
        path: String,
        blocks: Vec<ReplaceBlock>,
    },


    /// Minor summary — compress gap actions in current session.
    /// Agent outputs the gap summary, engine assembles JSONL and writes daily.
    Summary {
        content: String,
    },


    /// Set agent profile fields (name, etc.).
    /// Should only be used when the user explicitly requests it.
    SetProfile {
        entries: Vec<(String, String)>,
    },

    /// Create a new agent instance (裂变).
    /// First line = display name, rest = knowledge.md content.
    CreateInstance {
        name: String,
        knowledge: String,
    },

    /// Forget (compress) a specific action block in current session.
    /// Replaces the target action's content with a concise summary.
    /// Silent execution: successful forget leaves no trace of itself in current.
    Forget {
        target_action_id: String,
        summary: String,
    },
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Idle { timeout_secs: None } => write!(f, "idle"),
            Action::Idle { timeout_secs: Some(secs) } => write!(f, "idle {}", secs),
            Action::ReadMsg => write!(f, "read_msg"),
            Action::SendMsg { recipient, .. } => write!(f, "send_msg → {}", recipient),
            Action::Thinking { .. } => write!(f, "thinking"),
            Action::Script { .. } => write!(f, "script"),
            Action::WriteFile { path, .. } => write!(f, "write_file → {}", path),
            Action::ReplaceInFile { path, blocks } => {
                write!(f, "replace_in_file → {} ({} blocks)", path, blocks.len())
            }
            Action::Summary { .. } => write!(f, "summary"),

            Action::SetProfile { entries } => {
                let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
                write!(f, "set_profile → {}", keys.join(", "))
            }
            Action::CreateInstance { name, .. } => write!(f, "create_instance → {}", name),
            Action::Forget { target_action_id, .. } => write!(f, "forget → {}", target_action_id),
        }
    }
}

// ---------------------------------------------------------------------------
// ReplaceBlock — search/replace pair
// ---------------------------------------------------------------------------

/// A single search-and-replace block for ReplaceInFile action.
#[derive(Debug, Clone)]
pub struct ReplaceBlock {
    pub search: String,
    pub replace: String,
}

// ---------------------------------------------------------------------------
// ActionRecord — execution record with doing/done separation
// ---------------------------------------------------------------------------

/// Record of an action's execution, written to session memory.
/// Implements doing/done separation: START+doing is written before execution,
/// done+END is appended after execution completes.
///
/// @TRACE: ACTION
#[derive(Debug, Clone)]
pub struct ActionRecord {
    /// Unique ID: timestamp_hextoken format
    pub action_id: String,
    /// The action that was/is being executed
    pub action: Action,
    /// "doing" text written before execution
    pub doing_text: String,
    /// "done" text appended after execution (None if still executing)
    pub done_text: Option<String>,
}

// ---------------------------------------------------------------------------
// Action name constants (used in separator parsing)
// ---------------------------------------------------------------------------

const ACTION_NAMES: &[(&str, ActionKind)] = &[
    ("idle", ActionKind::Idle),
    ("read_msg", ActionKind::ReadMsg),
    ("send_msg", ActionKind::SendMsg),
    ("thinking", ActionKind::Thinking),
    ("script", ActionKind::Script),
    ("write_file", ActionKind::WriteFile),
    ("replace_in_file", ActionKind::ReplaceInFile),
    ("summary", ActionKind::Summary),
    ("set_profile", ActionKind::SetProfile),
    ("create_instance", ActionKind::CreateInstance),
    ("forget", ActionKind::Forget),
];

/// Internal enum for parsing stage (before content is attached).
#[derive(Debug, Clone, Copy, PartialEq)]
enum ActionKind {
    Idle,
    ReadMsg,
    SendMsg,
    Thinking,
    Script,
    WriteFile,
    ReplaceInFile,
    Summary,
    SetProfile,
    CreateInstance,
    Forget,
}

// ---------------------------------------------------------------------------
// Parser — converts raw LLM output text into Vec<Action>
// ---------------------------------------------------------------------------

/// Parse raw LLM output into a sequence of actions.
///
/// The separator format is: `###ACTION_<token>###-<action_name>`
/// where `<token>` is a session-specific token for disambiguation.
///
/// @TRACE: ACTION - parsing LLM output
pub fn parse_actions(raw: &str, action_separator: &str, separator_token: &str) -> Result<Vec<Action>> {
    let mut actions = Vec::new();
    let prefix = format!("{}###-", action_separator);

    // Split by separator, skip the preamble before first separator
    let parts: Vec<&str> = raw.split(&prefix).collect();

    // First part is preamble (before any action), skip it
    for part in parts.iter().skip(1) {
        match parse_single_action(part, separator_token) {
            Ok(action) => actions.push(action),
            Err(e) => {
                // Convert parse error into a Thinking action so the agent sees the error
                let error_msg = format!(
                    "⚠️ action解析失败: {}\n原始输出: {}",
                    e,
                    crate::safe_truncate(part, 200)
                );
                tracing::warn!("[PARSE] {}", error_msg);
                actions.push(Action::Thinking { content: error_msg });
            }
        }
    }

    Ok(actions)
}

/// Parse a single action from text after the separator prefix.
fn parse_single_action(text: &str, separator_token: &str) -> Result<Action> {
    let (first_line, rest) = match text.find('\n') {
        Some(pos) => (text[..pos].trim(), &text[pos + 1..]),
        None => (text.trim(), ""),
    };

    let kind = ACTION_NAMES.iter()
        .find(|(name, _)| *name == first_line)
        .map(|(_, kind)| *kind)
        .ok_or_else(|| anyhow::anyhow!("Unknown action type: '{}'", first_line))?;

    match kind {
        ActionKind::Idle => {
            // Parse optional timeout from next line (e.g. "120" means wake up after 120s)
            let timeout_secs = if rest.trim().is_empty() {
                None
            } else {
                let first_rest_line = rest.lines().next().unwrap_or("").trim();
                if first_rest_line.is_empty() {
                    None
                } else {
                    Some(first_rest_line.parse::<u64>().map_err(|_| {
                        anyhow::anyhow!("Invalid idle timeout: '{}' (expected number of seconds)", first_rest_line)
                    })?)
                }
            };
            Ok(Action::Idle { timeout_secs })
        }
        ActionKind::ReadMsg => Ok(Action::ReadMsg),

        ActionKind::SendMsg => {
            let (recipient, content) = split_first_line(rest, "send_msg")?;
            Ok(Action::SendMsg {
                recipient: recipient.to_string(),
                content: content.to_string(),
            })
        }

        ActionKind::Thinking => {
            Ok(Action::Thinking {
                content: rest.to_string(),
            })
        }

        ActionKind::Script => {
            Ok(Action::Script {
                content: strip_markdown_code_block(rest),
            })
        }

        ActionKind::WriteFile => {
            let (path, content) = split_first_line(rest, "write_file")?;
            Ok(Action::WriteFile {
                path: path.to_string(),
                content: strip_markdown_code_block(content),
            })
        }

        ActionKind::ReplaceInFile => {
            let (path, blocks_text) = split_first_line(rest, "replace_in_file")?;
            let blocks = parse_replace_blocks(blocks_text, separator_token)?;
            Ok(Action::ReplaceInFile {
                path: path.to_string(),
                blocks,
            })
        }


        ActionKind::Summary => {
            Ok(Action::Summary {
                content: rest.to_string(),
            })
        }

        ActionKind::SetProfile => {
            parse_set_profile(rest)
        }

        ActionKind::CreateInstance => {
            let (name, knowledge) = split_first_line(rest, "create_instance")?;
            Ok(Action::CreateInstance {
                name: name.to_string(),
                knowledge: knowledge.to_string(),
            })
        }

        ActionKind::Forget => {
            let (target_action_id, summary) = split_first_line(rest, "forget")?;
            Ok(Action::Forget {
                target_action_id: target_action_id.to_string(),
                summary: summary.to_string(),
            })
        }
    }
}

/// Split text into first line and remainder. Errors if first line is empty.
fn split_first_line<'a>(text: &'a str, action_name: &str) -> Result<(&'a str, &'a str)> {
    let text = text.trim_start_matches('\n');
    match text.find('\n') {
        Some(pos) => {
            let first = text[..pos].trim();
            if first.is_empty() {
                bail!("{}: expected first line parameter", action_name);
            }
            Ok((first, &text[pos + 1..]))
        }
        None => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                bail!("{}: expected first line parameter", action_name);
            }
            Ok((trimmed, ""))
        }
    }
}

/// Strip markdown code block markers from content.
fn strip_markdown_code_block(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return text.to_string();
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() < 2 || lines.last().map(|l| l.trim()) != Some("```") {
        return text.to_string();
    }
    lines[1..lines.len() - 1].join("\n")
}

/// Parse replace blocks from text.
/// Find a marker that occupies its own line in `text`.
/// Returns the byte offset of the marker (not the preceding newline).
/// "Own line" means: preceded by `\n` or at text start, AND followed by `\n` or at text end.
fn find_own_line_marker(text: &str, marker: &str) -> Option<usize> {
    // Check at text start
    if text.starts_with(marker) {
        let after = marker.len();
        if after >= text.len() || text.as_bytes()[after] == b'\n' {
            return Some(0);
        }
    }
    // Search for \n{marker} where marker is followed by \n or end-of-text
    let nl_marker = format!("\n{}", marker);
    let mut from = 0;
    while let Some(p) = text[from..].find(&nl_marker) {
        let abs = from + p;
        let after = abs + nl_marker.len();
        if after >= text.len() || text.as_bytes()[after] == b'\n' {
            return Some(abs + 1); // +1 to skip the \n prefix, point to marker itself
        }
        from = abs + 1;
    }
    None
}

fn parse_replace_blocks(text: &str, separator_token: &str) -> Result<Vec<ReplaceBlock>> {
    let search_marker = format!("{}_{}", SEARCH_MARKER, separator_token);
    let replace_marker = format!("{}_{}", REPLACE_MARKER, separator_token);
    let end_marker = format!("{}_{}", BLOCK_END_MARKER, separator_token);

    let mut blocks = Vec::new();
    let mut remaining = text;

    while let Some(search_pos) = find_own_line_marker(remaining, &search_marker) {
        // <<<SEARCH_{token} must be followed by \n (content starts on next line)
        let after_search_end = search_pos + search_marker.len();
        if remaining.as_bytes().get(after_search_end) != Some(&b'\n') {
            bail!("<<<SEARCH must be followed by newline");
        }
        let after_search = &remaining[after_search_end + 1..];

        // Find ===REPLACE_{token} on its own line
        let replace_pos = find_own_line_marker(after_search, &replace_marker)
            .ok_or_else(|| anyhow::anyhow!("Missing ===REPLACE marker"))?;
        let after_replace_end = replace_pos + replace_marker.len();
        if after_search.as_bytes().get(after_replace_end) != Some(&b'\n') {
            bail!("===REPLACE must be followed by newline");
        }
        let after_replace = &after_search[after_replace_end + 1..];

        // Find >>>END_{token} on its own line (allows end-of-text after it)
        let end_pos = find_own_line_marker(after_replace, &end_marker)
            .ok_or_else(|| anyhow::anyhow!("Missing block end marker (must be on its own line)"))?;

        let search = &after_search[..replace_pos];
        let replace = &after_replace[..end_pos];

        let search = search.strip_suffix('\n').unwrap_or(search);
        let replace = replace.strip_suffix('\n').unwrap_or(replace);

        blocks.push(ReplaceBlock {
            search: search.to_string(),
            replace: replace.to_string(),
        });

        remaining = &after_replace[end_pos + end_marker.len()..];
    }

    if blocks.is_empty() {
        bail!("No replace blocks found");
    }

    Ok(blocks)
}

// ---------------------------------------------------------------------------
// REMEMBER marker utilities
// ---------------------------------------------------------------------------

#[cfg(feature = "remember")]
const REMEMBER_START: &str = REMEMBER_START_MARKER;
#[cfg(feature = "remember")]
const REMEMBER_END: &str = REMEMBER_END_MARKER;

#[cfg(feature = "remember")]
pub fn extract_remember_fragments(content: &str) -> Option<String> {
    let mut fragments = Vec::new();
    let mut remaining = content;

    while let Some(start_pos) = remaining.find(REMEMBER_START) {
        let after_marker = &remaining[start_pos + REMEMBER_START.len()..];
        let content_start = match after_marker.find('\n') {
            Some(pos) => pos + 1,
            None => break,
        };
        let fragment_text = &after_marker[content_start..];

        if let Some(end_pos) = fragment_text.find(REMEMBER_END) {
            let fragment = fragment_text[..end_pos].trim_end_matches('\n');
            if !fragment.is_empty() {
                fragments.push(fragment.to_string());
            }
            remaining = &fragment_text[end_pos + REMEMBER_END.len()..];
        } else {
            break;
        }
    }

    if fragments.is_empty() {
        None
    } else {
        Some(fragments.join("\n---\n"))
    }
}

#[cfg(feature = "remember")]
pub fn strip_remember_markers(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == REMEMBER_START || trimmed == REMEMBER_END {
            continue;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
    }
    if content.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Parse set_profile action content.
fn parse_set_profile(text: &str) -> Result<Action> {
    let mut entries = Vec::new();
    let known_keys = ["name", "color", "avatar"];

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_lowercase();
            let value = line[colon_pos + 1..].trim().to_string();
            if known_keys.contains(&key.as_str()) {
                entries.push((key, value));
            } else {
                bail!("set_profile: unknown key '{}' (known: {})", key, known_keys.join(", "));
            }
        } else {
            bail!("set_profile: invalid line '{}' (expected key: value)", line);
        }
    }

    if entries.is_empty() {
        bail!("set_profile: no entries found");
    }

    Ok(Action::SetProfile { entries })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SEP: &str = "###ACTION_test123";
    const TEST_TOKEN: &str = "test123";
    // Helper: markers with token suffix for replace_in_file tests
    macro_rules! sm { () => { "<<<SEARCH_test123" } }
    macro_rules! rm { () => { "===REPLACE_test123" } }
    macro_rules! em { () => { ">>>END_test123" } }

    #[test]
    fn test_parse_idle() {
        let raw = format!("some preamble\n{}###-idle", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Idle { timeout_secs: None }));
    }

    #[test]
    fn test_parse_idle_with_timeout() {
        let raw = format!("{}###-idle\n120", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Idle { timeout_secs: Some(120) } => {}
            other => panic!("Expected Idle with 120s timeout, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_idle_with_invalid_timeout() {
        let raw = format!("{}###-idle\nabc", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        // Invalid timeout should be parsed as Thinking (error recovery)
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Thinking { .. }));
    }

    #[test]
    fn test_idle_display_with_timeout() {
        assert_eq!(
            format!("{}", Action::Idle { timeout_secs: Some(60) }),
            "idle 60"
        );
        assert_eq!(
            format!("{}", Action::Idle { timeout_secs: None }),
            "idle"
        );
    }

    #[test]
    fn test_parse_read_msg() {
        let raw = format!("{}###-read_msg", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::ReadMsg));
    }

    #[test]
    fn test_parse_send_msg() {
        let raw = format!("{}###-send_msg\n24007\nHello there!\nSecond line.", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::SendMsg { recipient, content } => {
                assert_eq!(recipient, "24007");
                assert_eq!(content, "Hello there!\nSecond line.");
            }
            _ => panic!("Expected SendMsg"),
        }
    }

    #[test]
    fn test_parse_thinking() {
        let raw = format!("{}###-thinking\nI need to plan this carefully.\nStep 1...", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Thinking { content } => {
                assert!(content.contains("plan this carefully"));
            }
            _ => panic!("Expected Thinking"),
        }
    }

    #[test]
    fn test_parse_script() {
        let raw = format!("{}###-script\necho hello\nls -la", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Script { content } => {
                assert_eq!(content, "echo hello\nls -la");
            }
            _ => panic!("Expected Script"),
        }
    }

    #[test]
    fn test_parse_write_file() {
        let raw = format!("{}###-write_file\ntest.txt\nfile content here\nline 2", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::WriteFile { path, content } => {
                assert_eq!(path, "test.txt");
                assert_eq!(content, "file content here\nline 2");
            }
            _ => panic!("Expected WriteFile"),
        }
    }
    #[test]
    fn test_parse_replace_in_file() {
        let raw = format!(
            "{}###-replace_in_file\nconfig.toml\n{}\nold text\n{}\nnew text\n{}",
            SEP, sm!(), rm!(), em!()
        );
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::ReplaceInFile { path, blocks } => {
                assert_eq!(path, "config.toml");
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].search, "old text");
                assert_eq!(blocks[0].replace, "new text");
            }
            _ => panic!("Expected ReplaceInFile"),
        }
    }

    #[test]
    fn test_parse_replace_multiple_blocks() {
        let raw = format!(
            "{}###-replace_in_file\nfile.rs\n{}\nfoo\n{}\nbar\n{}\n{}\nbaz\n{}\nqux\n{}",
            SEP, sm!(), rm!(), em!(), sm!(), rm!(), em!()
        );
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        match &actions[0] {
            Action::ReplaceInFile { blocks, .. } => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].search, "foo");
                assert_eq!(blocks[0].replace, "bar");
                assert_eq!(blocks[1].search, "baz");
                assert_eq!(blocks[1].replace, "qux");
            }
            _ => panic!("Expected ReplaceInFile"),
        }
    }

    #[test]
    fn test_parse_summary() {
        let raw = format!("{}###-summary\nAlice读了代码，修改了配置文件。", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Summary { content } => {
                assert!(content.contains("读了代码"));
            }
            _ => panic!("Expected Summary"),
        }
    }



    #[test]
    fn test_parse_multiple_actions() {
        let raw = format!(
            "preamble\n{}###-thinking\nplanning...\n{}###-script\necho test\n{}###-idle",
            SEP, SEP, SEP
        );
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], Action::Thinking { .. }));
        assert!(matches!(actions[1], Action::Script { .. }));
        assert!(matches!(actions[2], Action::Idle { timeout_secs: None }));
    }

    #[test]
    fn test_strip_markdown_code_block_bash() {
        let input = "```bash\nwhoami\npwd\nls\n```";
        assert_eq!(strip_markdown_code_block(input), "whoami\npwd\nls");
    }

    #[test]
    fn test_strip_markdown_code_block_no_markers() {
        let input = "whoami\npwd\nls";
        assert_eq!(strip_markdown_code_block(input), input);
    }

    #[test]
    fn test_strip_markdown_code_block_only_opening() {
        let input = "```bash\nwhoami\npwd";
        assert_eq!(strip_markdown_code_block(input), input);
    }

    #[test]
    fn test_strip_markdown_code_block_generic() {
        let input = "```\nsome content\nmore content\n```";
        assert_eq!(strip_markdown_code_block(input), "some content\nmore content");
    }

    #[test]
    fn test_parse_unknown_action_becomes_thinking() {
        let raw = format!("{}###-unknown_action", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Thinking { content } => {
                assert!(content.contains("action解析失败"));
                assert!(content.contains("unknown_action"));
            }
            _ => panic!("Expected Thinking with parse error"),
        }
    }

    #[test]
    fn test_action_display() {
        assert_eq!(format!("{}", Action::Idle { timeout_secs: None }), "idle");
        assert_eq!(
            format!("{}", Action::SendMsg {
                recipient: "24007".to_string(),
                content: "hi".to_string(),
            }),
            "send_msg → 24007"
        );
        assert_eq!(
            format!("{}", Action::ReplaceInFile {
                path: "f.rs".to_string(),
                blocks: vec![
                    ReplaceBlock { search: "a".to_string(), replace: "b".to_string() },
                    ReplaceBlock { search: "c".to_string(), replace: "d".to_string() },
                ],
            }),
            "replace_in_file → f.rs (2 blocks)"
        );
        assert_eq!(
            format!("{}", Action::Summary { content: "test".to_string() }),
            "summary"
        );

    }

    // -----------------------------------------------------------------------
    // Error path tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_send_msg_empty_becomes_thinking() {
        let raw = format!("{}###-send_msg\n", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Thinking { content } => {
                assert!(content.contains("action解析失败"));
            }
            _ => panic!("Expected Thinking with parse error"),
        }
    }

    #[test]
    fn test_parse_write_file_empty_becomes_thinking() {
        let raw = format!("{}###-write_file\n", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Thinking { content } => {
                assert!(content.contains("action解析失败"));
            }
            _ => panic!("Expected Thinking with parse error"),
        }
    }

        #[test]
    fn test_parse_replace_rust_generics_not_false_match() {
        // Regression test: Rust generic closing brackets (e.g. HashMap<...<ChatHistory>>>>>,)
        // must NOT be matched as block end marker. Only line-start markers should match.
        let rust_code_search = "    connections: RwLock<HashMap<String, Arc<Mutex<Chat>>>>,
}";
        let rust_code_replace = "    connections: RwLock<HashMap<String, Arc<Mutex<Chat>>>>,
    extra_field: bool,
}";
        let raw = format!(
            "{}###-replace_in_file
mod.rs
{}
{}
{}
{}
{}",
            SEP, sm!(), rust_code_search, rm!(), rust_code_replace, em!()
        );
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::ReplaceInFile { path, blocks } => {
                assert_eq!(path, "mod.rs");
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].search, rust_code_search);
                assert_eq!(blocks[0].replace, rust_code_replace);
            }
            _ => panic!("Expected ReplaceInFile"),
        }
    }

    #[test]
    fn test_parse_replace_end_marker_own_line_variants() {
        let sm = format!("{}_{}", SEARCH_MARKER, TEST_TOKEN);
        let rm = format!("{}_{}", REPLACE_MARKER, TEST_TOKEN);
        let em = format!("{}_{}", BLOCK_END_MARKER, TEST_TOKEN);

        // Case 1: end marker at end of text (no trailing newline) - should work
        let text1 = format!("{}\nold\n{}\nnew\n{}", sm, rm, em);
        let blocks1 = parse_replace_blocks(&text1, TEST_TOKEN).unwrap();
        assert_eq!(blocks1.len(), 1);
        assert_eq!(blocks1[0].replace, "new");

        // Case 2: end marker followed by newline - should work
        let text2 = format!("{}\nold\n{}\nnew\n{}\n", sm, rm, em);
        let blocks2 = parse_replace_blocks(&text2, TEST_TOKEN).unwrap();
        assert_eq!(blocks2.len(), 1);
        assert_eq!(blocks2[0].replace, "new");

        // Case 3: end marker with suffix should NOT match
        let text3 = format!(
            "{}\nold\n{}\nline with {}suffix\n{}\n",
            sm, rm, em, em
        );
        let blocks3 = parse_replace_blocks(&text3, TEST_TOKEN).unwrap();
        assert_eq!(blocks3.len(), 1);
        assert_eq!(blocks3[0].replace, format!("line with {}suffix", em));
    }
    #[test]
    fn test_parse_replace_in_file_no_blocks_becomes_thinking() {
        let raw = format!("{}###-replace_in_file\nsome/file.rs\nno blocks here", SEP);
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Thinking { content } => {
                assert!(content.contains("action解析失败"));
            }
            _ => panic!("Expected Thinking with parse error"),
        }
    }

    #[test]
    fn test_parse_mixed_valid_and_invalid_actions() {
        let raw = format!(
            "{}###-thinking\nplanning\n{}###-send_msg\n{}###-idle",
            SEP, SEP, SEP
        );
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], Action::Thinking { .. }));
        match &actions[1] {
            Action::Thinking { content } => {
                assert!(content.contains("action解析失败"));
            }
            _ => panic!("Expected Thinking with parse error for invalid send_msg"),
        }
        assert!(matches!(actions[2], Action::Idle { timeout_secs: None }));
    }

    #[test]
    fn test_parse_send_msg_trailing_separator() {
        let raw = format!(
            "{}###-send_msg\n24007\nHello!\n{}###-idle",
            SEP, SEP
        );
        let actions = parse_actions(&raw, SEP, TEST_TOKEN).unwrap();
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            Action::SendMsg { content, .. } => {
                assert_eq!(content, "Hello!\n");
            }
            _ => panic!("Expected SendMsg"),
        }
    }

    // -----------------------------------------------------------------------
    // REMEMBER marker tests
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "remember")]
    fn test_extract_remember_no_markers() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        assert_eq!(extract_remember_fragments(content), None);
    }

    #[test]
    #[cfg(feature = "remember")]
    fn test_extract_remember_single_fragment() {
        let content = &format!("use std::io;\n\n{}\nfn main() {{\n    run();\n}}\n{}\n\nfn run() {{\n    // details\n}}\n", REMEMBER_START_MARKER, REMEMBER_END_MARKER);
        let result = extract_remember_fragments(content).unwrap();
        assert_eq!(result, "fn main() {\n    run();\n}");
    }

    #[test]
    #[cfg(feature = "remember")]
    fn test_extract_remember_multiple_fragments() {
        let content = &format!("{}\nfn main() {{}}\n{}\n\nimpl Detail {{\n    // 50 lines\n}}\n\n{}\nfn run() {{}}\n{}\n", REMEMBER_START_MARKER, REMEMBER_END_MARKER, REMEMBER_START_MARKER, REMEMBER_END_MARKER);
        let result = extract_remember_fragments(content).unwrap();
        assert!(result.contains("fn main() {}"));
        assert!(result.contains("---"));
        assert!(result.contains("fn run() {}"));
    }

    #[test]
    #[cfg(feature = "remember")]
    fn test_strip_remember_no_markers() {
        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        assert_eq!(strip_remember_markers(content), content);
    }

    #[test]
    #[cfg(feature = "remember")]
    fn test_strip_remember_with_markers() {
        let content = &format!("use std::io;\n{}\nfn main() {{\n    run();\n}}\n{}\nfn run() {{}}\n", REMEMBER_START_MARKER, REMEMBER_END_MARKER);
        let result = strip_remember_markers(content);
        assert!(!result.contains(REMEMBER_START_MARKER));
        assert!(!result.contains(REMEMBER_END_MARKER));
        assert!(result.contains("use std::io;"));
        assert!(result.contains("fn main()"));
        assert!(result.contains("fn run() {}"));
    }

    #[test]
    #[cfg(feature = "remember")]
    fn test_strip_remember_preserves_content() {
        let content = &format!("header\n{}\nkept content\n{}\nfooter\n", REMEMBER_START_MARKER, REMEMBER_END_MARKER);
        let result = strip_remember_markers(content);
        assert_eq!(result, "header\nkept content\nfooter\n");
    }

    #[test]
    #[cfg(feature = "remember")]
    fn test_write_file_with_remember_extract_in_context() {
        let content = &format!("use std::io;\n{}\nfn main() {{}}\n{}\nfn helper() {{}}\n", REMEMBER_START_MARKER, REMEMBER_END_MARKER);
        let fragments = extract_remember_fragments(content).unwrap();
        assert!(fragments.contains("fn main() {}"));
        assert!(!fragments.contains("fn helper()"));
        assert!(!fragments.contains("std::io"));
    }
}