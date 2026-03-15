//! Tests for framework resilience features:
//! - Automatic code block stripping in from_markdown
//! - #[markdown(required)] field validation

use mad_hatter::FromMarkdown;
use mad_hatter::llm::FromMarkdown as _;

// ============================================================
// Test types
// ============================================================

/// @render Simple enum for strip code block tests
#[derive(Debug, PartialEq, FromMarkdown)]
enum SimpleAction {
    /// @render 记录思考
    Thinking {
        /// @render thinking内容
        content: String,
    },
    /// @render 发送消息
    SendMsg {
        /// @render 收件人
        #[markdown(required)]
        recipient: String,
        /// @render 消息内容
        content: String,
    },
    /// @render 写入文件
    WriteFile {
        /// @render 文件路径
        #[markdown(required)]
        path: String,
        /// @render 文件内容
        content: String,
    },
    /// @render 空操作
    Idle,
}

/// @render Struct with required field
#[derive(Debug, PartialEq, FromMarkdown)]
struct Config {
    /// @render 名称
    #[markdown(required)]
    name: String,
    /// @render 描述（可选）
    description: String,
}

// ============================================================
// Strip code block tests
// ============================================================

#[test]
fn test_strip_code_block_bare() {
    let token = "tk1";
    let inner = format!(
        "SimpleAction-{token}\nthinking\ncontent-{token}\nHello world\nSimpleAction-end-{token}"
    );
    // Wrap in bare ```
    let input = format!("```\n{inner}\n```");

    let result = SimpleAction::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        SimpleAction::Thinking {
            content: "Hello world".to_string()
        }
    );
}

#[test]
fn test_strip_code_block_with_language() {
    let token = "tk2";
    let inner = format!(
        "SimpleAction-{token}\nidle\nSimpleAction-end-{token}"
    );
    // Wrap in ```markdown
    let input = format!("```markdown\n{inner}\n```");

    let result = SimpleAction::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], SimpleAction::Idle);
}

#[test]
fn test_strip_code_block_no_wrap() {
    let token = "tk3";
    let input = format!(
        "SimpleAction-{token}\nthinking\ncontent-{token}\nNo wrap here\nSimpleAction-end-{token}"
    );

    let result = SimpleAction::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        SimpleAction::Thinking {
            content: "No wrap here".to_string()
        }
    );
}

#[test]
fn test_strip_code_block_multi_action() {
    let token = "tk4";
    let inner = format!(
        "SimpleAction-{token}\nthinking\ncontent-{token}\nFirst\nSimpleAction-{token}\nidle\nSimpleAction-end-{token}"
    );
    let input = format!("```\n{inner}\n```");

    let result = SimpleAction::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(
        result[0],
        SimpleAction::Thinking {
            content: "First".to_string()
        }
    );
    assert_eq!(result[1], SimpleAction::Idle);
}

// ============================================================
// Required field tests — enum
// ============================================================

#[test]
fn test_required_field_present() {
    let token = "tk5";
    let input = format!(
        "SimpleAction-{token}\nsend_msg\nrecipient-{token}\nAlice\ncontent-{token}\nHello!\nSimpleAction-end-{token}"
    );

    let result = SimpleAction::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        SimpleAction::SendMsg {
            recipient: "Alice".to_string(),
            content: "Hello!".to_string(),
        }
    );
}

#[test]
fn test_required_field_empty_recipient() {
    let token = "tk6";
    let input = format!(
        "SimpleAction-{token}\nsend_msg\nrecipient-{token}\n\ncontent-{token}\nHello!\nSimpleAction-end-{token}"
    );

    let result = SimpleAction::from_markdown(&input, token);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("Required field"), "Error: {}", err);
    assert!(err.contains("recipient"), "Error: {}", err);
}

#[test]
fn test_required_field_empty_path() {
    let token = "tk7";
    let input = format!(
        "SimpleAction-{token}\nwrite_file\npath-{token}\n\ncontent-{token}\nsome content\nSimpleAction-end-{token}"
    );

    let result = SimpleAction::from_markdown(&input, token);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("Required field"), "Error: {}", err);
    assert!(err.contains("path"), "Error: {}", err);
}

#[test]
fn test_required_field_whitespace_only() {
    let token = "tk8";
    let input = format!(
        "SimpleAction-{token}\nsend_msg\nrecipient-{token}\n   \ncontent-{token}\nHello!\nSimpleAction-end-{token}"
    );

    let result = SimpleAction::from_markdown(&input, token);
    assert!(result.is_err(), "Whitespace-only required field should fail");
}

#[test]
fn test_non_required_field_empty_ok() {
    // content is NOT required, so empty is fine
    let token = "tk9";
    let input = format!(
        "SimpleAction-{token}\nsend_msg\nrecipient-{token}\nAlice\ncontent-{token}\n\nSimpleAction-end-{token}"
    );

    let result = SimpleAction::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 1);
    // content should be empty string (allowed)
    if let SimpleAction::SendMsg { recipient, content } = &result[0] {
        assert_eq!(recipient, "Alice");
        assert!(content.is_empty() || content.trim().is_empty());
    } else {
        panic!("Expected SendMsg");
    }
}

// ============================================================
// Required field tests — struct
// ============================================================

#[test]
fn test_required_struct_field_present() {
    let token = "tks1";
    let input = format!(
        "Config-{token}\nname-{token}\nMyApp\ndescription-{token}\nA cool app\nConfig-end-{token}"
    );

    let result = Config::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        Config {
            name: "MyApp".to_string(),
            description: "A cool app".to_string(),
        }
    );
}

#[test]
fn test_required_struct_field_empty() {
    let token = "tks2";
    let input = format!(
        "Config-{token}\nname-{token}\n\ndescription-{token}\nA cool app\nConfig-end-{token}"
    );

    let result = Config::from_markdown(&input, token);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("Required field"), "Error: {}", err);
    assert!(err.contains("name"), "Error: {}", err);
}

#[test]
fn test_strip_plus_required_combined() {
    // Code block wrapped + required field present = should work
    let token = "tkc1";
    let inner = format!(
        "SimpleAction-{token}\nsend_msg\nrecipient-{token}\nBob\ncontent-{token}\nHi Bob\nSimpleAction-end-{token}"
    );
    let input = format!("```markdown\n{inner}\n```");

    let result = SimpleAction::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0],
        SimpleAction::SendMsg {
            recipient: "Bob".to_string(),
            content: "Hi Bob".to_string(),
        }
    );
}

#[test]
fn test_strip_plus_required_empty_fails() {
    // Code block wrapped + required field empty = should fail
    let token = "tkc2";
    let inner = format!(
        "SimpleAction-{token}\nsend_msg\nrecipient-{token}\n\ncontent-{token}\nHi\nSimpleAction-end-{token}"
    );
    let input = format!("```\n{inner}\n```");

    let result = SimpleAction::from_markdown(&input, token);
    assert!(result.is_err(), "Empty required field inside code block should fail");
}