//! Tests for ToMarkdown derive on enums.

use mad_hatter::ToMarkdown;
use mad_hatter::llm::ToMarkdown as _;


// ============================================================
// Test enums
// ============================================================

#[derive(ToMarkdown)]
enum SimpleAction {
    Idle,
    ReadMsg,
}

#[derive(ToMarkdown)]
enum SingleFieldAction {
    Thinking {
        content: String,
    },
    SendMsg {
        recipient: String,
    },
}

#[derive(ToMarkdown)]
enum MultiFieldAction {
    Script {
        /// @render Standard output
        stdout: String,
        /// @render Exit code
        exit_code: i32,
        /// @render Truncated
        truncated: bool,
    },
}

#[derive(ToMarkdown)]
enum WithOption {
    Idle {
        /// @render Timeout seconds
        timeout_secs: Option<u64>,
    },
    Note {
        text: Option<String>,
    },
}

#[derive(ToMarkdown)]
enum WithVec {
    ReadMsg {
        entries: Vec<String>,
    },
}

#[allow(unused)]
#[derive(ToMarkdown)]
enum WithSkip {
    Example {
        visible: String,
        #[markdown(skip)]
        hidden: String,
    },
}

#[derive(ToMarkdown)]
struct Inner {
    /// @render Name
    name: String,
    /// @render Value
    value: u64,
}

#[derive(ToMarkdown)]
enum WithNested {
    Complex {
        /// @render Description
        desc: String,
        detail: Inner,
    },
}

// ============================================================
// Tests: to_markdown (depth=0)
// ============================================================

#[test]
fn test_unit_variant() {
    let action = SimpleAction::Idle;
    assert_eq!(action.to_markdown(), "idle\n");

    let action2 = SimpleAction::ReadMsg;
    assert_eq!(action2.to_markdown(), "read_msg\n");
}

#[test]
fn test_single_field_inline() {
    let action = SingleFieldAction::Thinking {
        content: "planning next step".to_string(),
    };
    let output = action.to_markdown();
    assert_eq!(output, "thinking\ncontent: planning next step\n");
}

#[test]
fn test_single_field_multiline() {
    let action = SingleFieldAction::Thinking {
        content: "line1\nline2\nline3".to_string(),
    };
    let output = action.to_markdown();
    assert!(output.starts_with("thinking\n"));
    assert!(output.contains("# content #"));
    assert!(output.contains("line1\nline2\nline3"));
}

#[test]
fn test_single_field_empty_string() {
    let action = SingleFieldAction::Thinking {
        content: String::new(),
    };
    let output = action.to_markdown();
    // Empty string skipped, only variant name
    assert_eq!(output, "thinking\n");
}

#[test]
fn test_multi_field_variant() {
    let action = MultiFieldAction::Script {
        stdout: "hello world".to_string(),
        exit_code: 0,
        truncated: false,
    };
    let output = action.to_markdown();
    assert!(output.starts_with("script\n"));
    // stdout is single-line → inline
    assert!(output.contains("Standard output: hello world\n"));
    // exit_code is Numeric → section
    assert!(output.contains("Exit code"));
    assert!(output.contains("0\n"));
    // truncated is Bool → section
    assert!(output.contains("Truncated"));
    assert!(output.contains("false\n"));
}

#[test]
fn test_option_some() {
    let action = WithOption::Idle {
        timeout_secs: Some(120),
    };
    let output = action.to_markdown();
    assert!(output.starts_with("idle\n"));
    assert!(output.contains("Timeout seconds"));
    assert!(output.contains("120"));
}

#[test]
fn test_option_none() {
    let action = WithOption::Idle {
        timeout_secs: None,
    };
    let output = action.to_markdown();
    // None skipped
    assert_eq!(output, "idle\n");
}

#[test]
fn test_option_string_some() {
    let action = WithOption::Note {
        text: Some("a note".to_string()),
    };
    let output = action.to_markdown();
    assert!(output.starts_with("note\n"));
    assert!(output.contains("text: a note\n"));
}

#[test]
fn test_option_string_none() {
    let action = WithOption::Note {
        text: None,
    };
    let output = action.to_markdown();
    assert_eq!(output, "note\n");
}

#[test]
fn test_vec_field() {
    let action = WithVec::ReadMsg {
        entries: vec!["msg1".to_string(), "msg2".to_string()],
    };
    let output = action.to_markdown();
    assert!(output.starts_with("read_msg\n"));
    assert!(output.contains("entries"));
    assert!(output.contains("msg1\n"));
    assert!(output.contains("msg2\n"));
}

#[test]
fn test_vec_empty() {
    let action = WithVec::ReadMsg {
        entries: vec![],
    };
    let output = action.to_markdown();
    // Empty vec skipped
    assert_eq!(output, "read_msg\n");
}

#[test]
fn test_skip_field() {
    let action = WithSkip::Example {
        visible: "shown".to_string(),
        hidden: "secret".to_string(),
    };
    let output = action.to_markdown();
    assert!(output.contains("visible: shown"));
    assert!(!output.contains("secret"));
    assert!(!output.contains("hidden"));
}

#[test]
fn test_nested_struct() {
    let action = WithNested::Complex {
        desc: "test".to_string(),
        detail: Inner {
            name: "foo".to_string(),
            value: 42,
        },
    };
    let output = action.to_markdown();
    assert!(output.starts_with("complex\n"));
    assert!(output.contains("Description: test"));
    assert!(output.contains("detail"));
    assert!(output.contains("Name: foo"));
    assert!(output.contains("42"));
}

// ============================================================
// Tests: to_markdown_item (compact mode)
// ============================================================

#[test]
fn test_item_unit_variant() {
    let action = SimpleAction::Idle;
    assert_eq!(action.to_markdown_item(), "idle\n");
}

#[test]
fn test_item_single_field() {
    let action = SingleFieldAction::SendMsg {
        recipient: "user".to_string(),
    };
    let output = action.to_markdown_item();
    assert_eq!(output, "send_msg\nrecipient: user\n");
}

#[test]
fn test_item_multi_field() {
    let action = MultiFieldAction::Script {
        stdout: "output text".to_string(),
        exit_code: 1,
        truncated: true,
    };
    let output = action.to_markdown_item();
    assert!(output.starts_with("script\n"));
    assert!(output.contains("stdout: output text\n"));
    assert!(output.contains("exit_code: 1\n"));
    assert!(output.contains("truncated: true\n"));
}

#[test]
fn test_item_option_some() {
    let action = WithOption::Idle {
        timeout_secs: Some(60),
    };
    let output = action.to_markdown_item();
    assert!(output.starts_with("idle\n"));
    assert!(output.contains("timeout_secs: 60\n"));
}

#[test]
fn test_item_option_none() {
    let action = WithOption::Idle {
        timeout_secs: None,
    };
    let output = action.to_markdown_item();
    assert_eq!(output, "idle\n");
}