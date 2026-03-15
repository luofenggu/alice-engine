//! Stress test: simulate real-world Action enum with 11 variants.
//!
//! This test validates FromMarkdown + ToMarkdown with realistic complexity.

use mad_hatter::{ToMarkdown, FromMarkdown};
use mad_hatter::llm::{FromMarkdown as _, ToMarkdown as _};

// --- Supporting struct ---

#[derive(ToMarkdown, FromMarkdown, PartialEq, Debug, Clone)]
struct ReplaceBlock {
    search: String,
    replace: String,
}

// --- Main Action enum ---

#[derive(FromMarkdown, PartialEq, Debug)]
enum Action {
    /// @render 什么都不做
    Idle,

    /// @render 等待指定秒数
    IdleWithParam {
        seconds: Option<u16>,
    },

    /// @render 记录思考
    Thinking {
        content: String,
    },

    /// @render 执行脚本
    Script {
        content: String,
    },

    /// @render 小结
    Summary {
        content: String,
    },

    /// @render 寄出信件
    SendMsg {
        recipient: String,
        content: String,
    },

    /// @render 写入文件
    WriteFile {
        file_path: String,
        content: String,
    },

    /// @render 搜索替换
    ReplaceInFile {
        file_path: String,
        blocks: Vec<ReplaceBlock>,
    },

    /// @render 提炼
    Distill {
        action_id: String,
        summary: String,
    },

    /// @render 设置个人资料
    SetProfile {
        settings: String,
    },

    /// @render 创建新实例
    CreateInstance {
        name: String,
        knowledge: String,
    },
}

// ============================================================
// Individual variant roundtrip tests
// ============================================================

#[test]
fn test_idle_roundtrip() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nidle\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::Idle]);
}

#[test]
fn test_idle_with_param_some() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nidle_with_param\n120\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::IdleWithParam { seconds: Some(120) }]);
}

#[test]
fn test_idle_with_param_none() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nidle_with_param\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::IdleWithParam { seconds: None }]);
}

#[test]
fn test_thinking_roundtrip() {
    let token = "abc123";
    let content = "这是一段思考内容。\n\n包含空行。\n还有更多内容。";
    let input = format!(
        "Action-{t}\nthinking\n{c}\nAction-end-{t}",
        t = token, c = content
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::Thinking { content: content.to_string() }]);
}

#[test]
fn test_script_with_code_block() {
    let token = "abc123";
    let content = "#!/bin/bash\necho \"hello\"\n\n```\nsome nested code\n```\necho done";
    let input = format!(
        "Action-{t}\nscript\n{c}\nAction-end-{t}",
        t = token, c = content
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::Script { content: content.to_string() }]);
}

#[test]
fn test_summary_roundtrip() {
    let token = "abc123";
    let content = "## 对话小结\n\n### 已完成\n- 任务A\n- 任务B";
    let input = format!(
        "Action-{t}\nsummary\n{c}\nAction-end-{t}",
        t = token, c = content
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::Summary { content: content.to_string() }]);
}

#[test]
fn test_send_msg_roundtrip() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nsend_msg\nrecipient-{t}\nuser\ncontent-{t}\n你好世界！\n第二行消息。\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::SendMsg {
        recipient: "user".to_string(),
        content: "你好世界！\n第二行消息。".to_string(),
    }]);
}

#[test]
fn test_write_file_roundtrip() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nwrite_file\nfile_path-{t}\nsrc/main.rs\ncontent-{t}\nfn main() {{\n    println!(\"hello\");\n}}\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::WriteFile {
        file_path: "src/main.rs".to_string(),
        content: "fn main() {\n    println!(\"hello\");\n}".to_string(),
    }]);
}

#[test]
fn test_replace_in_file_single_block() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nreplace_in_file\nfile_path-{t}\nsrc/lib.rs\nblocks-{t}\nReplaceBlock-{t}\nsearch-{t}\nold code\nreplace-{t}\nnew code\nReplaceBlock-end-{t}\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::ReplaceInFile {
        file_path: "src/lib.rs".to_string(),
        blocks: vec![ReplaceBlock {
            search: "old code".to_string(),
            replace: "new code".to_string(),
        }],
    }]);
}

#[test]
fn test_replace_in_file_multiple_blocks() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nreplace_in_file\nfile_path-{t}\nsrc/lib.rs\nblocks-{t}\nReplaceBlock-{t}\nsearch-{t}\nfn old() {{}}\nreplace-{t}\nfn new() {{}}\nReplaceBlock-{t}\nsearch-{t}\nlet x = 1;\nreplace-{t}\nlet x = 2;\nReplaceBlock-end-{t}\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::ReplaceInFile {
        file_path: "src/lib.rs".to_string(),
        blocks: vec![
            ReplaceBlock {
                search: "fn old() {}".to_string(),
                replace: "fn new() {}".to_string(),
            },
            ReplaceBlock {
                search: "let x = 1;".to_string(),
                replace: "let x = 2;".to_string(),
            },
        ],
    }]);
}

#[test]
fn test_distill_roundtrip() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\ndistill\naction_id-{t}\n20260311_abc\nsummary-{t}\n这是提炼总结。\n包含多行。\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::Distill {
        action_id: "20260311_abc".to_string(),
        summary: "这是提炼总结。\n包含多行。".to_string(),
    }]);
}

#[test]
fn test_set_profile_roundtrip() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nset_profile\nname: 四号\ncolor: #FF6B6B\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::SetProfile {
        settings: "name: 四号\ncolor: #FF6B6B".to_string(),
    }]);
}

#[test]
fn test_create_instance_roundtrip() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\ncreate_instance\nname-{t}\n新实例\nknowledge-{t}\n# 知识\n\n这是初始知识内容。\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::CreateInstance {
        name: "新实例".to_string(),
        knowledge: "# 知识\n\n这是初始知识内容。".to_string(),
    }]);
}

// ============================================================
// Multi-action mixed test
// ============================================================

#[test]
fn test_multi_action_mixed() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nthinking\n分析问题...\nAction-{t}\nsend_msg\nrecipient-{t}\nuser\ncontent-{t}\n你好\nAction-{t}\nscript\necho hello\nAction-{t}\nidle\nAction-end-{t}",
        t = token
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result.len(), 4);
    assert_eq!(result[0], Action::Thinking { content: "分析问题...".to_string() });
    assert_eq!(result[1], Action::SendMsg { recipient: "user".to_string(), content: "你好".to_string() });
    assert_eq!(result[2], Action::Script { content: "echo hello".to_string() });
    assert_eq!(result[3], Action::Idle);
}

// ============================================================
// Edge cases
// ============================================================

#[test]
fn test_content_with_empty_lines() {
    let token = "abc123";
    let content = "第一段\n\n\n第二段\n\n第三段";
    let input = format!(
        "Action-{t}\nthinking\n{c}\nAction-end-{t}",
        t = token, c = content
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::Thinking { content: content.to_string() }]);
}

#[test]
fn test_content_with_code_blocks() {
    let token = "abc123";
    let content = "执行以下脚本：\n```bash\n#!/bin/bash\nfor i in 1 2 3; do\n  echo $i\ndone\n```\n完成。";
    let input = format!(
        "Action-{t}\nscript\n{c}\nAction-end-{t}",
        t = token, c = content
    );
    let result = Action::from_markdown(&input, token).unwrap();
    assert_eq!(result, vec![Action::Script { content: content.to_string() }]);
}

#[test]
fn test_missing_end_marker() {
    let token = "abc123";
    let input = format!(
        "Action-{t}\nthinking\n一些内容",
        t = token
    );
    let result = Action::from_markdown(&input, token);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("end marker"), "Error should mention end marker: {}", err);
}

// ============================================================
// Schema generation test
// ============================================================

#[test]
fn test_schema_covers_all_variants() {
    let schema = Action::schema_markdown("TOKEN");
    // All variant names should appear
    assert!(schema.contains("idle"), "schema should contain idle");
    assert!(schema.contains("idle_with_param"), "schema should contain idle_with_param");
    assert!(schema.contains("thinking"), "schema should contain thinking");
    assert!(schema.contains("script"), "schema should contain script");
    assert!(schema.contains("summary"), "schema should contain summary");
    assert!(schema.contains("send_msg"), "schema should contain send_msg");
    assert!(schema.contains("write_file"), "schema should contain write_file");
    assert!(schema.contains("replace_in_file"), "schema should contain replace_in_file");
    assert!(schema.contains("distill"), "schema should contain distill");
    assert!(schema.contains("set_profile"), "schema should contain set_profile");
    assert!(schema.contains("create_instance"), "schema should contain create_instance");
    // Separators
    assert!(schema.contains("Action-TOKEN"));
    assert!(schema.contains("Action-end-TOKEN"));
}

// ============================================================
// ReplaceBlock ToMarkdown test
// ============================================================

#[test]
fn test_replace_block_to_markdown_item() {
    let block = ReplaceBlock {
        search: "old code".to_string(),
        replace: "new code".to_string(),
    };
    let item = block.to_markdown_item();
    assert!(item.contains("search: old code"));
}
