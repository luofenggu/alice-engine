//! Tests for @parse/@render doc comment prefix classification.
//!
//! Verifies that:
//! - @parse lines only appear in FromMarkdown schema
//! - @render lines only appear in ToMarkdown rendering
//! - No-prefix lines fallback to current behavior (backward compat)

use mad_hatter::{ToMarkdown, FromMarkdown};
use mad_hatter::llm::{ToMarkdown as _, FromMarkdown as _};

// === Enum with @parse/@render prefixes ===

#[derive(FromMarkdown, ToMarkdown, Debug, PartialEq)]
enum ClassifiedAction {
    /// @parse idle终结本轮推理，不再输出任何action
    /// @parse 只在无事可做时使用
    /// @render 空闲等待
    Idle,

    /// @parse 发送消息给指定收件人
    /// @parse 必须填写recipient和content
    /// @render 发消息
    SendMsg {
        /// @parse 收件人ID，必须是有效实例
        /// @render 收件人
        recipient: String,
        /// @parse 消息正文，支持markdown
        /// @render 内容
        content: String,
    },
}

// === Enum without prefixes (backward compat) ===

#[derive(FromMarkdown, ToMarkdown, Debug, PartialEq)]
enum PlainAction {
    /// 什么都不做
    Wait,

    /// 执行脚本
    RunScript {
        /// 脚本内容
        code: String,
    },
}

// === Struct with @parse/@render prefixes ===

#[derive(ToMarkdown, Debug)]
/// @render 环境信息
/// @parse 以下是当前环境的详细配置
struct EnvInfo {
    /// @render 身份
    /// @parse 你的身份标识，格式为"名称（ID）"
    pub identity: String,
    /// @render 系统
    /// @parse 操作系统类型和版本
    pub system: String,
}

// === Struct with mixed: some fields have prefix, some don't ===

#[derive(FromMarkdown, Debug, PartialEq)]
struct MixedStruct {
    /// @parse 用户的显示名称
    pub name: String,
    /// 普通注释，无前缀
    pub age: String,
}

// ---- Tests ----

#[test]
fn test_enum_schema_uses_parse_docs() {
    let schema = ClassifiedAction::schema_markdown("test123");
    // Schema should contain @parse content (without the @parse prefix)
    assert!(schema.contains("idle终结本轮推理"), "schema should have @parse doc for Idle variant: {}", schema);
    assert!(schema.contains("只在无事可做时使用"), "schema should have second @parse line: {}", schema);
    assert!(schema.contains("发送消息给指定收件人"), "schema should have @parse doc for SendMsg variant: {}", schema);
    assert!(schema.contains("收件人ID，必须是有效实例"), "schema should have @parse doc for recipient field: {}", schema);
    assert!(schema.contains("消息正文，支持markdown"), "schema should have @parse doc for content field: {}", schema);
    // Schema should NOT contain @render content
    assert!(!schema.contains("空闲等待"), "schema should NOT have @render doc: {}", schema);
    assert!(!schema.contains("发消息"), "schema should NOT have @render doc for SendMsg: {}", schema);
    assert!(!schema.contains("// 收件人\n"), "schema should NOT have @render doc for recipient field: {}", schema);
}

#[test]
fn test_enum_to_markdown_uses_render_docs() {
    let idle = ClassifiedAction::Idle;
    let _rendered = idle.to_markdown_depth(3);
    // to_markdown for unit variant just outputs snake_name, no doc
    // But let's check SendMsg which has field docs
    let msg = ClassifiedAction::SendMsg {
        recipient: "alice".to_string(),
        content: "hello".to_string(),
    };
    let rendered = msg.to_markdown_depth(3);
    // Field titles should use @render docs
    assert!(rendered.contains("收件人: alice") || rendered.contains("收件人"), 
        "rendered should use @render doc '收件人' for recipient field: {}", rendered);
    assert!(rendered.contains("内容: hello") || rendered.contains("内容"),
        "rendered should use @render doc '内容' for content field: {}", rendered);
    // Should NOT contain @parse docs
    assert!(!rendered.contains("收件人ID"), "rendered should NOT have @parse doc: {}", rendered);
    assert!(!rendered.contains("消息正文"), "rendered should NOT have @parse doc: {}", rendered);
}

#[test]
fn test_plain_enum_schema_fallback() {
    // No @parse/@render prefixes → fallback to all doc comments
    let schema = PlainAction::schema_markdown("test456");
    assert!(schema.contains("什么都不做"), "plain schema should fallback to all docs: {}", schema);
    assert!(schema.contains("执行脚本"), "plain schema should fallback to all docs: {}", schema);
    assert!(schema.contains("脚本内容"), "plain schema should fallback to all docs: {}", schema);
}

#[test]
fn test_plain_enum_to_markdown_fallback() {
    let script = PlainAction::RunScript { code: "echo hi".to_string() };
    let rendered = script.to_markdown_depth(3);
    // Field title should fallback to all docs (joined)
    assert!(rendered.contains("脚本内容: echo hi") || rendered.contains("脚本内容"),
        "plain rendered should fallback to all docs: {}", rendered);
}

#[test]
fn test_struct_header_uses_render_doc() {
    let env = EnvInfo {
        identity: "进化四号（1e268b）".to_string(),
        system: "Linux".to_string(),
    };
    let rendered = env.to_markdown_depth(3);
    // Struct header should use @render doc
    assert!(rendered.contains("环境信息"), "struct header should use @render doc: {}", rendered);
    // Should NOT contain @parse doc for header
    assert!(!rendered.contains("以下是当前环境的详细配置"), "struct header should NOT have @parse doc: {}", rendered);
}

#[test]
fn test_struct_field_uses_render_doc() {
    let env = EnvInfo {
        identity: "进化四号（1e268b）".to_string(),
        system: "Linux".to_string(),
    };
    let rendered = env.to_markdown_depth(3);
    // Field titles should use @render docs
    assert!(rendered.contains("身份: 进化四号") || rendered.contains("身份"),
        "field should use @render doc '身份': {}", rendered);
    assert!(rendered.contains("系统: Linux") || rendered.contains("系统"),
        "field should use @render doc '系统': {}", rendered);
    // Should NOT contain @parse field docs
    assert!(!rendered.contains("你的身份标识"), "field should NOT have @parse doc: {}", rendered);
    assert!(!rendered.contains("操作系统类型"), "field should NOT have @parse doc: {}", rendered);
}

#[test]
fn test_mixed_struct_schema_uses_parse_doc() {
    let schema = MixedStruct::schema_markdown("test789");
    // @parse field should use @parse doc
    assert!(schema.contains("用户的显示名称"), "schema should have @parse doc for name: {}", schema);
    // Plain doc field should fallback to all docs
    assert!(schema.contains("普通注释，无前缀"), "schema should fallback for age field: {}", schema);
}

#[test]
fn test_no_parse_prefix_leaks_to_render() {
    // Verify @parse content doesn't leak into ToMarkdown rendering
    let msg = ClassifiedAction::SendMsg {
        recipient: "bob".to_string(),
        content: "test\nwith\nnewlines".to_string(),
    };
    let rendered = msg.to_markdown_depth(3);
    assert!(!rendered.contains("必须填写"), "no @parse content in render: {}", rendered);
    assert!(!rendered.contains("必须是有效实例"), "no @parse content in render: {}", rendered);
}

#[test]
fn test_no_render_prefix_leaks_to_schema() {
    let schema = ClassifiedAction::schema_markdown("test000");
    assert!(!schema.contains("空闲等待"), "no @render content in schema: {}", schema);
    assert!(!schema.contains("发消息"), "no @render content in schema: {}", schema);
}