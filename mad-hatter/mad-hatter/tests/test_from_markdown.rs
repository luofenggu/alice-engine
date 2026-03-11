use mad_hatter::FromMarkdown;
use mad_hatter::llm::FromMarkdown as _;

#[derive(FromMarkdown, Debug, PartialEq)]
enum TestAction {
    /// 阅读收件箱
    ReadMsg,

    /// 记录思考
    Thinking {
        /// 思考内容
        content: String,
    },

    /// 什么都不做
    Idle {
        /// 等待秒数
        timeout_secs: Option<u64>,
    },

    /// 寄出信件
    SendMsg {
        /// 收件人
        recipient: String,
        /// 信件内容
        content: String,
    },

    /// 搜索替换文件内容
    ReplaceInFile {
        /// 文件路径
        path: String,
        /// 搜索文本
        search: String,
        /// 替换文本
        replace: String,
    },
}

#[test]
fn test_schema_markdown() {
    let schema = TestAction::schema_markdown("abc123");

    // Should contain element separator format
    assert!(schema.contains("TestAction-abc123"), "schema should contain element separator");

    // Should contain variant names
    assert!(schema.contains("read_msg"), "schema should contain read_msg");
    assert!(schema.contains("thinking"), "schema should contain thinking");
    assert!(schema.contains("idle"), "schema should contain idle");
    assert!(schema.contains("send_msg"), "schema should contain send_msg");
    assert!(schema.contains("replace_in_file"), "schema should contain replace_in_file");

    // Should contain doc comments
    assert!(schema.contains("// 阅读收件箱"), "schema should contain doc for ReadMsg");
    assert!(schema.contains("// 记录思考"), "schema should contain doc for Thinking");

    // Should contain field separators for multi-field variants
    assert!(schema.contains("recipient-abc123"), "schema should contain field separator");
    assert!(schema.contains("content-abc123"), "schema should contain field separator");

    // Should contain end marker explanation
    assert!(schema.contains("TestAction-end-abc123"), "schema should contain end marker");
}

#[test]
fn test_schema_contains_end_marker() {
    let schema = TestAction::schema_markdown("tok42");
    // End marker should be at the end of schema
    assert!(schema.contains("TestAction-end-tok42"), "schema must mention end marker");
}

#[test]
fn test_zero_field_parse() {
    let input = "TestAction-abc123\nread_msg\nTestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::ReadMsg);
}

#[test]
fn test_one_field_parse() {
    let input = "TestAction-abc123\nthinking\n这是思考内容\n可以多行\nTestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::Thinking {
        content: "这是思考内容\n可以多行".to_string(),
    });
}

#[test]
fn test_option_field_with_value() {
    let input = "TestAction-abc123\nidle\n120\nTestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::Idle {
        timeout_secs: Some(120),
    });
}

#[test]
fn test_option_field_none() {
    let input = "TestAction-abc123\nidle\nTestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::Idle {
        timeout_secs: None,
    });
}

#[test]
fn test_two_field_parse() {
    let input = "\
TestAction-abc123
send_msg
recipient-abc123
user
content-abc123
你好
这是多行内容
TestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::SendMsg {
        recipient: "user".to_string(),
        content: "你好\n这是多行内容".to_string(),
    });
}

#[test]
fn test_three_field_parse() {
    let input = "\
TestAction-abc123
replace_in_file
path-abc123
src/main.rs
search-abc123
old code
replace-abc123
new code
TestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::ReplaceInFile {
        path: "src/main.rs".to_string(),
        search: "old code".to_string(),
        replace: "new code".to_string(),
    });
}

#[test]
fn test_multi_action_parse() {
    let input = "\
TestAction-abc123
thinking
这是思考

TestAction-abc123
send_msg
recipient-abc123
user
content-abc123
你好

TestAction-abc123
idle
120
TestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0], TestAction::Thinking {
        content: "这是思考".to_string(),
    });
    assert_eq!(result[1], TestAction::SendMsg {
        recipient: "user".to_string(),
        content: "你好".to_string(),
    });
    assert_eq!(result[2], TestAction::Idle {
        timeout_secs: Some(120),
    });
}

#[test]
fn test_end_marker_present() {
    // Valid: has end marker
    let input = "TestAction-abc123\nread_msg\nTestAction-end-abc123";
    let result = TestAction::from_markdown(input, "abc123");
    assert!(result.is_ok());
}

#[test]
fn test_end_marker_missing_truncation() {
    // Invalid: missing end marker → truncation error
    let input = "TestAction-abc123\nread_msg";
    let result = TestAction::from_markdown(input, "abc123");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("truncated") || err.contains("end marker"),
        "Error should mention truncation: {}", err);
}

#[test]
fn test_end_marker_missing_multi_action() {
    // Multiple actions but no end marker
    let input = "\
TestAction-abc123
thinking
some thought

TestAction-abc123
read_msg";
    let result = TestAction::from_markdown(input, "abc123");
    assert!(result.is_err());
}

#[test]
fn test_schema_roundtrip() {
    let _schema = TestAction::schema_markdown("test42");

    // The schema itself should be parseable if we construct valid input from it
    let input = "\
TestAction-test42
read_msg

TestAction-test42
thinking
一段思考内容

TestAction-test42
send_msg
recipient-test42
user
content-test42
信件内容

TestAction-test42
idle

TestAction-test42
replace_in_file
path-test42
src/lib.rs
search-test42
old
replace-test42
new
TestAction-end-test42";

    let result = TestAction::from_markdown(input, "test42").unwrap();
    assert_eq!(result.len(), 5);
    assert_eq!(result[0], TestAction::ReadMsg);
    assert_eq!(result[1], TestAction::Thinking { content: "一段思考内容".to_string() });
    assert_eq!(result[2], TestAction::SendMsg {
        recipient: "user".to_string(),
        content: "信件内容".to_string(),
    });
    assert_eq!(result[3], TestAction::Idle { timeout_secs: None });
    assert_eq!(result[4], TestAction::ReplaceInFile {
        path: "src/lib.rs".to_string(),
        search: "old".to_string(),
        replace: "new".to_string(),
    });
}

