use mad_hatter::llm::FromMarkdown;
use mad_hatter::FromMarkdown;

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
        /// 秒数
        timeout_secs: Option<u64>,
    },

    /// 寄出信件
    SendMsg {
        /// 收件人
        recipient: String,
        /// 信件内容
        content: String,
    },

    /// 搜索替换
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
    // Verify key elements are present
    assert!(schema.contains("TestAction-abc123"), "schema should contain element separator");
    assert!(schema.contains("read_msg"), "schema should contain read_msg variant");
    assert!(schema.contains("thinking"), "schema should contain thinking variant");
    assert!(schema.contains("send_msg"), "schema should contain send_msg variant");
    assert!(schema.contains("idle"), "schema should contain idle variant");
    assert!(schema.contains("replace_in_file"), "schema should contain replace_in_file variant");
    // Verify field separators for multi-field variants
    assert!(schema.contains("recipient-abc123"), "schema should contain recipient separator");
    assert!(schema.contains("content-abc123"), "schema should contain content separator");
    assert!(schema.contains("path-abc123"), "schema should contain path separator");
    assert!(schema.contains("search-abc123"), "schema should contain search separator");
    assert!(schema.contains("replace-abc123"), "schema should contain replace separator");
    // Verify doc comments
    assert!(schema.contains("阅读收件箱"), "schema should contain ReadMsg doc");
    assert!(schema.contains("记录思考"), "schema should contain Thinking doc");
    assert!(schema.contains("寄出信件"), "schema should contain SendMsg doc");
    // Print for manual inspection
    println!("=== Schema ===\n{}", schema);
}

#[test]
fn test_zero_field_parse() {
    let input = "TestAction-abc123\nread_msg\n";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::ReadMsg);
}

#[test]
fn test_one_field_parse() {
    let input = "TestAction-abc123\nthinking\n这是思考内容\n可以多行\n";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::Thinking {
        content: "这是思考内容\n可以多行".to_string(),
    });
}

#[test]
fn test_option_field_with_value() {
    let input = "TestAction-abc123\nidle\n120\n";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::Idle {
        timeout_secs: Some(120),
    });
}

#[test]
fn test_option_field_none() {
    let input = "TestAction-abc123\nidle\n";
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
";
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
";
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
read_msg

TestAction-abc123
thinking
先分析一下问题

TestAction-abc123
send_msg
recipient-abc123
user
content-abc123
分析完成，结果如下：
1. 第一点
2. 第二点

TestAction-abc123
idle
60
";
    let result = TestAction::from_markdown(input, "abc123").unwrap();
    assert_eq!(result.len(), 4);
    assert_eq!(result[0], TestAction::ReadMsg);
    assert_eq!(result[1], TestAction::Thinking {
        content: "先分析一下问题".to_string(),
    });
    assert_eq!(result[2], TestAction::SendMsg {
        recipient: "user".to_string(),
        content: "分析完成，结果如下：\n1. 第一点\n2. 第二点".to_string(),
    });
    assert_eq!(result[3], TestAction::Idle {
        timeout_secs: Some(60),
    });
}

#[test]
fn test_schema_roundtrip() {
    // Generate schema, then verify the format description can guide correct parsing
    let schema = TestAction::schema_markdown("test42");
    // Schema should mention the separator format
    assert!(schema.contains("TestAction-test42"));
    // Verify a simple action can be parsed with the token from schema
    let input = "TestAction-test42\nread_msg\n";
    let result = TestAction::from_markdown(input, "test42").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], TestAction::ReadMsg);
}
