//! # Inference Module
//!
//! Each LLM inference = RPC function.
//! Request struct + render() = prompt serialization.
//! Response struct + parse() = result deserialization.
//! This module contains protocol definitions only — no execution logic,
//! no LLM API calls, no streaming, no blocking hallucination defense.

pub mod beat;
pub mod capture;
pub mod compress;
pub mod output;

// ---------------------------------------------------------------------------
// Safe template rendering
// ---------------------------------------------------------------------------

/// Safe template rendering: scan template once, replace `{{KEY}}` placeholders
/// by looking up in the provided vars slice. Replacement results are never
/// re-scanned, preventing injection when user content contains `{{...}}` markers.
pub fn safe_render(template: &str, vars: &[(&str, &str)]) -> String {
    use std::collections::HashMap;
    let map: HashMap<&str, &str> = vars.iter().cloned().collect();
    let mut result = String::with_capacity(template.len() * 2);
    let mut chars = template.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch == '{' {
            if let Some(&(_, next_ch)) = chars.peek() {
                if next_ch == '{' {
                    if let Some(end) = template[i + 2..].find("}}") {
                        let key = &template[i..i + 2 + end + 2];
                        if let Some(val) = map.get(key) {
                            result.push_str(val);
                            let skip_to = i + 2 + end + 2;
                            while let Some(&(j, _)) = chars.peek() {
                                if j >= skip_to {
                                    break;
                                }
                                chars.next();
                            }
                            continue;
                        }
                    }
                }
            }
        }
        result.push(ch);
    }
    result
}

// ---------------------------------------------------------------------------
// Action enum
// ---------------------------------------------------------------------------

use mad_hatter::FromMarkdown;
use mad_hatter::llm::FromMarkdown as _;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, FromMarkdown, Serialize, Deserialize)]
pub enum Action {
    /// @render Action：什么都不做（继续等待）
    /// @render idle是终结动作，输出idle后本轮推理结束，不能再输出任何action
    /// @render 礼貌规则: 用户只能看到信件，看不到你的其它行为，因此，进入idle之前需检查已读信件，如有未回复消息，优先回复（寄出信件）
    /// @render ⚠️ sleep vs idle：shell脚本里的sleep是同步阻塞——等待期间你无法响应任何消息；idle是异步等待——来信会立刻唤醒你。需要等待时永远用idle，不要用sleep
    /// @render ⚠️ 默认不加参数：等用户消息时直接idle不带秒数，来信自动唤醒。只有"启动了后台任务、需要过一会儿检查结果"这种场景才加秒数
    /// @render 💡 idle可以紧跟在其他action后面连续输出。比如你send_msg回复了用户后需要等待确认，直接跟一个idle，不用浪费一次推理
    /// @render 💡 idle 120：等待120秒后自动醒来检查状态（期间有来信也会提前醒）。适合异步运维场景
    Idle {
        /// @render 秒数（如120，表示等待120秒后自动醒来）
        timeout_secs: Option<u64>,
    },
    /// @render Action：阅读收件箱（未读来信=0时无效）
    /// @render 来信中sender为"user"代表已鉴权的你的专属用户
    ReadMsg,
    /// @render Action：寄出信件
    /// @render 收件人填"user"代表发给你的专属用户
    /// @render 信件中引用文件路径时使用 [[file:相对路径]] 格式，前端会渲染为可点击的文件链接
    /// @render 信件中的URL会自动识别为可点击链接，无需特殊格式
    /// @render 信件内容支持markdown格式（标题、列表、代码块、表格等），前端会自动渲染
    SendMsg {
        /// @render 收件人
        recipient: String,
        /// @render 信件内容
        content: String,
    },

    /// @render Action：执行本地脚本
    /// @render 脚本执行时cwd已经是工作目录(workspace)，脚本中的相对路径基于workspace
    /// @render 使用绝对路径可以访问工作目录之外的文件（需要开启privilege权限，可以跟用户商量）
    Script {
        /// @render 脚本内容
        content: String,
    },
    /// @render Action：写入文件
    /// @render 路径是工作目录中的相对路径。如果需要操作工作目录之外的文件，可以使用绝对路径（需要开启privileged权限，可以跟用户商量）
    /// @render 写入后系统自动提取文件骨架（接口+注释）记入 `current`，不记全文
    /// @render 如果需要记住写入的关键细节，在thinking中提前记录
    WriteFile {
        /// @render file_path
        path: String,
        /// @render 文件完整内容
        content: String,
    },
    /// @render Action：搜索替换文件内容（增量修改）
    /// @render 搜索文本必须在文件中唯一匹配，匹配0次或多次都会报错
    /// @render 比write_file省token：只需输出变更区域，不用全量输出文件
    /// @render 比script中的sed更可靠：引擎内置实现，不依赖系统命令
    /// @render 路径是工作目录中的相对路径。如果需要操作工作目录之外的文件，可以使用绝对路径（需要开启privileged权限，可以跟用户商量）
    ReplaceInFile {
        /// @render file_path
        path: String,
        /// @render 要搜索的精确文本（多行）
        search: String,
        /// @render 要替换成的文本（多行）
        replace: String,
    },
    /// @render Action：小结（回顾对话）
    /// @render 当current变得很长时，用这个action释放空间
    /// @render 执行后current清空、小结合入近况
    /// @render 对话小结按过程顺序记录：关键思考、决策和结论；重要操作及其结果；进行中尚未完成的工作的上下文和指引；新出现的知识术语；读到用户信件时的感受和温度
    Summary {
        /// @render 对话小结
        content: String,
    },
    /// @render Action：设置个人资料
    /// @render 已知key: name（显示名称）, color（主题色，如#FF6B6B）, avatar（头像emoji）
    SetProfile {
        /// @render 设置项（每行一个 key:value）
        content: String,
    },
    /// @render Action：创建新实例（裂变）
    /// @render 创建一个新的agent实例，引擎会自动发现并启动
    /// @render 用于裂变场景：将部分职责和知识委托给新实例
    /// @render 新实例会继承当前用户，获得随机ID和颜色
    /// @render ⚠️ 未经用户授权不得执行裂变
    /// @render knowledge内容与自己当前的知识保持结构基本一致，按用户要求提炼局部内容
    CreateInstance {
        /// @render 实例显示名称
        name: String,
        /// @render knowledge内容（新实例的初始知识）
        knowledge: String,
    },
    /// @render Action：提炼（压缩action块）
    /// @render 将current中指定action的内容替换为你的提炼总结，释放空间
    /// @render 总结直接写入原action位置，前面会自动加[已提炼]标记
    /// @render 💡 时机：脚本执行结果确认后立刻提炼——编译输出、文件列表、curl响应等一次性验证内容，确认结论后就不需要保留原文
    /// @render 💡 效果：信息不丢失，只是从原文压缩为结论。比如100行find输出提炼为"日志在xxx目录，最新文件是xxx"
    /// @render 适用场景：大段脚本输出、重复代码阅读、冗长的中间过程
    /// @render 不能提炼自身action，不能提炼不存在的编号
    /// @render ⚠️ 提炼不可逆，确保总结保留了关键信息再执行
    Distill {
        /// @render 要提炼的action编号（从行为编号标记中获取）
        target_action_id: String,
        /// @render 提炼总结（替换原action的完整内容）
        summary: String,
    },
}

impl Action {
    /// Returns the snake_case type name for the action_type column.
    pub fn type_name(&self) -> &'static str {
        match self {
            Action::Idle { .. } => "idle",
            Action::ReadMsg => "read_msg",
            Action::SendMsg { .. } => "send_msg",

            Action::Script { .. } => "script",
            Action::WriteFile { .. } => "write_file",
            Action::ReplaceInFile { .. } => "replace_in_file",
            Action::Summary { .. } => "summary",
            Action::SetProfile { .. } => "set_profile",
            Action::CreateInstance { .. } => "create_instance",
            Action::Distill { .. } => "distill",
        }
    }

    /// Returns the ActionType enum for this action.
    pub fn action_type(&self) -> crate::bindings::db::ActionType {
        use crate::bindings::db::ActionType;
        match self {
            Action::Idle { .. } => ActionType::Idle,
            Action::ReadMsg => ActionType::ReadMsg,
            Action::SendMsg { .. } => ActionType::SendMsg,
            Action::Script { .. } => ActionType::Script,
            Action::WriteFile { .. } => ActionType::WriteFile,
            Action::ReplaceInFile { .. } => ActionType::ReplaceInFile,
            Action::Summary { .. } => ActionType::Summary,
            Action::SetProfile { .. } => ActionType::SetProfile,
            Action::CreateInstance { .. } => ActionType::CreateInstance,
            Action::Distill { .. } => ActionType::Distill,
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Idle { timeout_secs: None } => write!(f, "idle"),
            Action::Idle {
                timeout_secs: Some(secs),
            } => write!(f, "idle {}", secs),
            Action::ReadMsg => write!(f, "read_msg"),
            Action::SendMsg { recipient, .. } => write!(f, "send_msg → {}", recipient),

            Action::Script { .. } => write!(f, "script"),
            Action::WriteFile { path, .. } => write!(f, "write_file → {}", path),
            Action::ReplaceInFile { path, .. } => {
                write!(f, "replace_in_file → {}", path)
            }
            Action::Summary { .. } => write!(f, "summary"),
            Action::SetProfile { .. } => write!(f, "set_profile"),
            Action::CreateInstance { name, .. } => write!(f, "create_instance → {}", name),
            Action::Distill {
                target_action_id, ..
            } => write!(f, "distill → {}", target_action_id),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

use anyhow::Result;

/// Parse actions from LLM output using FromMarkdown.
///
/// The raw text is expected to contain action blocks separated by `Action-{token}`
/// markers, ending with `Action-end-{token}`.
///
/// Post-process a single action: strip markdown code blocks, validate required fields.
/// Actions with empty required fields return Err.
fn post_process_action(action: Action) -> Result<Action> {
    match action {
        Action::Script { content } => Ok(Action::Script {
            content: strip_markdown_code_block(&content),
        }),
        Action::WriteFile { path, content } => {
            if path.trim().is_empty() {
                anyhow::bail!("write_file: missing file path")
            } else {
                Ok(Action::WriteFile {
                    path,
                    content: strip_markdown_code_block(&content),
                })
            }
        }
        Action::SendMsg { recipient, content } => {
            if recipient.trim().is_empty() {
                anyhow::bail!("send_msg: missing recipient")
            } else {
                Ok(Action::SendMsg { recipient, content })
            }
        }
        other => Ok(other),
    }
}

/// Post-processing: Script and WriteFile content is stripped of markdown code blocks.
/// Actions with empty required fields return Err.
pub fn parse_actions(raw: &str, separator_token: &str) -> Result<Vec<Action>> {
    let element_sep = format!("Action-{}", separator_token);
    let end_marker = format!("Action-end-{}", separator_token);

    // Check if raw contains any action separator
    if !raw.contains(&element_sep) {
        return Ok(Vec::new());
    }

    // Ensure end marker is present for from_markdown
    let input = if raw.trim().ends_with(&end_marker) {
        raw.to_string()
    } else {
        format!("{}\n{}", raw.trim(), end_marker)
    };

    match Action::from_markdown(&input, separator_token) {
        Ok(actions) => {
            let actions: Result<Vec<Action>> = actions.into_iter().map(post_process_action).collect();
            actions
        }
        Err(e) => {
            let error_msg = format!(
                "action parse failed: {}\nraw output: {}",
                e,
                crate::util::safe_truncate(raw, 200)
            );
            tracing::warn!("[PARSE] {}", error_msg);
            anyhow::bail!("{}", error_msg)
        }
    }
}

/// Parse a single action from a body chunk (used by streaming parser).
/// Wraps the body in Action-{token} and Action-end-{token} markers
/// before calling from_markdown.
pub fn parse_single_action_chunk(body: &str, separator_token: &str) -> Vec<Action> {
    let element_sep = format!("Action-{}", separator_token);
    let end_marker = format!("Action-end-{}", separator_token);
    let input = format!("{}\n{}\n{}", element_sep, body.trim(), end_marker);

    match Action::from_markdown(&input, separator_token) {
        Ok(actions) => {
            actions.into_iter()
                .filter_map(|a| post_process_action(a).ok())
                .collect()
        }
        Err(e) => {
            tracing::warn!("[PARSE] action parse failed: {}", e);
            Vec::new()
        }
    }
}

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "test123";

    fn sep() -> String {
        format!("Action-{}", TOKEN)
    }

    fn end() -> String {
        format!("Action-end-{}", TOKEN)
    }

    #[test]
    fn test_safe_render_basic() {
        let result = safe_render(
            "Hello {{NAME}}, welcome to {{PLACE}}!",
            &[("{{NAME}}", "Alice"), ("{{PLACE}}", "Wonderland")],
        );
        assert_eq!(result, "Hello Alice, welcome to Wonderland!");
    }

    #[test]
    fn test_safe_render_no_injection() {
        let result = safe_render(
            "A={{A}} B={{B}}",
            &[("{{A}}", "contains {{B}} inside"), ("{{B}}", "INJECTED")],
        );
        assert_eq!(result, "A=contains {{B}} inside B=INJECTED");
    }

    #[test]
    fn test_safe_render_unknown_placeholder() {
        let result = safe_render("{{KNOWN}} and {{UNKNOWN}}", &[("{{KNOWN}}", "yes")]);
        assert_eq!(result, "yes and {{UNKNOWN}}");
    }

    #[test]
    fn test_safe_render_empty_value() {
        let result = safe_render("before{{X}}after", &[("{{X}}", "")]);
        assert_eq!(result, "beforeafter");
    }

    #[test]
    fn test_safe_render_chinese() {
        let result = safe_render(
            "你好{{NAME}}，欢迎来到{{PLACE}}",
            &[("{{NAME}}", "小白"), ("{{PLACE}}", "仙境")],
        );
        assert_eq!(result, "你好小白，欢迎来到仙境");
    }

    #[test]
    fn test_safe_render_no_vars() {
        let result = safe_render("no placeholders here", &[]);
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn test_safe_render_adjacent_placeholders() {
        let result = safe_render("{{A}}{{B}}", &[("{{A}}", "hello"), ("{{B}}", "world")]);
        assert_eq!(result, "helloworld");
    }

    #[test]
    fn test_safe_render_single_brace() {
        let result = safe_render("a{b}c", &[]);
        assert_eq!(result, "a{b}c");
    }

    #[test]
    fn test_parse_idle() {
        let raw = format!("{}\nidle\n{}", sep(), end());
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Idle { timeout_secs: None }));
    }

    #[test]
    fn test_parse_idle_with_timeout() {
        let raw = format!("{}\nidle\ntimeout_secs-{}\n120\n{}", sep(), TOKEN, end());
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Idle {
                timeout_secs: Some(120),
            } => {}
            other => panic!("Expected Idle with 120s timeout, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_idle_with_invalid_timeout() {
        let raw = format!("{}\nidle\ntimeout_secs-{}\nabc\n{}", sep(), TOKEN, end());
        // FromMarkdown will fail to parse "abc" as u64, resulting in parse error
        let result = parse_actions(&raw, TOKEN);
        assert!(result.is_err(), "Invalid timeout should cause parse error");
    }

    #[test]
    fn test_idle_display_with_timeout() {
        assert_eq!(
            format!(
                "{}",
                Action::Idle {
                    timeout_secs: Some(60)
                }
            ),
            "idle 60"
        );
        assert_eq!(format!("{}", Action::Idle { timeout_secs: None }), "idle");
    }

    #[test]
    fn test_parse_read_msg() {
        let raw = format!("{}\nread_msg\n{}", sep(), end());
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::ReadMsg));
    }

    #[test]
    fn test_parse_send_msg() {
        let raw = format!(
            "{}\nsend_msg\nrecipient-{}\n24007\ncontent-{}\nHello there!\nSecond line.\n{}",
            sep(), TOKEN, TOKEN, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
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
    fn test_parse_thinking_removed() {
        // Thinking variant removed from Action enum; "thinking" is no longer a valid action type
        let raw = format!(
            "{}\nthinking\nI need to plan this carefully.\nStep 1...\n{}",
            sep(), end()
        );
        let result = parse_actions(&raw, TOKEN);
        assert!(result.is_err(), "thinking is no longer a valid action type");
    }

    #[test]
    fn test_parse_script() {
        let raw = format!("{}\nscript\ncontent-{}\necho hello\nls -la\n{}", sep(), TOKEN, end());
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Script { content } => assert_eq!(content, "echo hello\nls -la"),
            _ => panic!("Expected Script"),
        }
    }

    #[test]
    fn test_parse_write_file() {
        let raw = format!(
            "{}\nwrite_file\npath-{}\ntest.txt\ncontent-{}\nfile content here\nline 2\n{}",
            sep(), TOKEN, TOKEN, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
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
            "{}\nreplace_in_file\npath-{}\nconfig.toml\nsearch-{}\nold text\nreplace-{}\nnew text\n{}",
            sep(), TOKEN, TOKEN, TOKEN, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::ReplaceInFile {
                path,
                search,
                replace,
            } => {
                assert_eq!(path, "config.toml");
                assert_eq!(search, "old text");
                assert_eq!(replace, "new text");
            }
            _ => panic!("Expected ReplaceInFile"),
        }
    }

    #[test]
    fn test_parse_summary() {
        let raw = format!(
            "{}\nsummary\ncontent-{}\nAlice读了代码，修改了配置文件。\n{}",
            sep(), TOKEN, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
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
            "{}\nscript\ncontent-{}\necho planning\n{}\nscript\ncontent-{}\necho test\n{}\nidle\n{}",
            sep(), TOKEN, sep(), TOKEN, sep(), end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], Action::Script { .. }));
        assert!(matches!(actions[1], Action::Script { .. }));
        assert!(matches!(actions[2], Action::Idle { timeout_secs: None }));
    }

    #[test]
    fn test_strip_markdown_code_block_bash() {
        assert_eq!(
            strip_markdown_code_block("```bash\nwhoami\npwd\nls\n```"),
            "whoami\npwd\nls"
        );
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
        assert_eq!(
            strip_markdown_code_block("```\nsome content\nmore content\n```"),
            "some content\nmore content"
        );
    }

    #[test]
    fn test_parse_unknown_action_returns_error() {
        let raw = format!("{}\nunknown_action\n{}", sep(), end());
        let result = parse_actions(&raw, TOKEN);
        assert!(result.is_err(), "Unknown action should return Err");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("action parse failed"), "Error should contain 'action parse failed', got: {}", err);
    }

    #[test]
    fn test_action_display() {
        assert_eq!(format!("{}", Action::Idle { timeout_secs: None }), "idle");
        assert_eq!(
            format!(
                "{}",
                Action::SendMsg {
                    recipient: "24007".to_string(),
                    content: "hi".to_string()
                }
            ),
            "send_msg → 24007"
        );
        assert_eq!(
            format!(
                "{}",
                Action::ReplaceInFile {
                    path: "f.rs".to_string(),
                    search: "a".to_string(),
                    replace: "b".to_string(),
                }
            ),
            "replace_in_file → f.rs"
        );
        assert_eq!(
            format!(
                "{}",
                Action::Summary {
                    content: "test".to_string(),
                }
            ),
            "summary"
        );
    }

    #[test]
    fn test_parse_send_msg_empty_returns_error() {
        // Empty send_msg with no fields should fail
        let raw = format!("{}\nsend_msg\n{}", sep(), end());
        let result = parse_actions(&raw, TOKEN);
        // Either returns Err (from_markdown fails) or Ok with empty recipient → Err from post_process
        assert!(result.is_err(), "Empty send_msg should return Err");
    }

    #[test]
    fn test_parse_write_file_empty_returns_error() {
        // Empty write_file with no fields should fail
        let raw = format!("{}\nwrite_file\n{}", sep(), end());
        let result = parse_actions(&raw, TOKEN);
        // Either returns Err (from_markdown fails) or Ok with empty path → Err from post_process
        assert!(result.is_err(), "Empty write_file should return Err");
    }

    #[test]
    fn test_parse_replace_rust_generics_not_false_match() {
        let rust_code_search = "    connections: RwLock<HashMap<String, Arc<Mutex<Chat>>>>,\n}";
        let rust_code_replace = "    connections: RwLock<HashMap<String, Arc<Mutex<Chat>>>>,\n    extra_field: bool,\n}";
        let raw = format!(
            "{}\nreplace_in_file\npath-{}\nmod.rs\nsearch-{}\n{}\nreplace-{}\n{}\n{}",
            sep(), TOKEN, TOKEN, rust_code_search, TOKEN, rust_code_replace, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::ReplaceInFile {
                path,
                search,
                replace,
            } => {
                assert_eq!(path, "mod.rs");
                assert_eq!(search, rust_code_search);
                assert_eq!(replace, rust_code_replace);
            }
            _ => panic!("Expected ReplaceInFile"),
        }
    }

    #[test]
    fn test_parse_no_actions() {
        let raw = "some random text without any action markers";
        let actions = parse_actions(raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 0);
    }

    #[test]
    fn test_parse_set_profile() {
        let raw = format!(
            "{}\nset_profile\ncontent-{}\nname: TestBot\ncolor: #FF0000\n{}",
            sep(), TOKEN, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::SetProfile { content } => {
                assert!(content.contains("name: TestBot"));
                assert!(content.contains("color: #FF0000"));
            }
            _ => panic!("Expected SetProfile"),
        }
    }

    #[test]
    fn test_parse_distill() {
        let raw = format!(
            "{}\ndistill\ntarget_action_id-{}\n20260101_abc123\nsummary-{}\nThis is the summary.\n{}",
            sep(), TOKEN, TOKEN, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Distill {
                target_action_id,
                summary,
            } => {
                assert_eq!(target_action_id, "20260101_abc123");
                assert_eq!(summary, "This is the summary.");
            }
            _ => panic!("Expected Distill"),
        }
    }

    #[test]
    fn test_parse_create_instance() {
        let raw = format!(
            "{}\ncreate_instance\nname-{}\nMyBot\nknowledge-{}\nSome initial knowledge.\n{}",
            sep(), TOKEN, TOKEN, end()
        );
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::CreateInstance { name, knowledge } => {
                assert_eq!(name, "MyBot");
                assert_eq!(knowledge, "Some initial knowledge.");
            }
            _ => panic!("Expected CreateInstance"),
        }
    }

    #[test]
    fn test_parse_single_action_chunk() {
        let body = &format!("idle\ntimeout_secs-{}\n120", TOKEN);
        let actions = parse_single_action_chunk(body, TOKEN);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::Idle {
                timeout_secs: Some(120),
            } => {}
            other => panic!("Expected Idle with 120s, got {:?}", other),
        }
    }

    #[test]
    fn test_schema_markdown_contains_all_variants() {
        let schema = Action::schema_markdown(TOKEN);
        assert!(schema.contains("idle"));
        assert!(schema.contains("read_msg"));
        assert!(schema.contains("send_msg"));

        assert!(schema.contains("script"));
        assert!(schema.contains("write_file"));
        assert!(schema.contains("replace_in_file"));
        assert!(schema.contains("summary"));
        assert!(schema.contains("set_profile"));
        assert!(schema.contains("create_instance"));
        assert!(schema.contains("distill"));
        assert!(schema.contains(&format!("Action-end-{}", TOKEN)));
    }

    // ─── Format anomaly tests: error messages enter current ─────────

    #[test]
    fn test_format_anomaly_garbage_before_separator() {
        // LLM outputs garbage before the first action separator
        // parse_actions should return Err
        let raw = format!("这是一些废话blah blah\n{}\nidle\n{}", sep(), end());
        let result = parse_actions(&raw, TOKEN);
        assert!(result.is_err(), "Garbage before separator should cause error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("action parse failed"),
            "Error should contain 'action parse failed', got: {}",
            err
        );
    }

    #[test]
    fn test_format_anomaly_complete_garbage_no_separator() {
        // LLM outputs complete garbage without any action separator
        // parse_actions returns empty Vec (no separator found)
        let raw = "completely random garbage text 完全乱码 no actions here";
        let actions = parse_actions(raw, TOKEN).unwrap();
        assert_eq!(
            actions.len(),
            0,
            "Complete garbage without separator should return empty Vec"
        );
    }

    #[test]
    fn test_format_anomaly_misspelled_variant() {
        // LLM outputs correct separator but misspells the variant name
        // parse_actions should return Err
        let raw = format!("{}\nidel\n{}", sep(), end());
        let result = parse_actions(&raw, TOKEN);
        assert!(result.is_err(), "Misspelled variant should cause error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("action parse failed"),
            "Error should contain 'action parse failed', got: {}",
            err
        );
    }

    #[test]
    fn test_format_anomaly_empty_after_separator() {
        // LLM outputs separator but no content before end marker
        let raw = format!("{}\n{}", sep(), end());
        let result = parse_actions(&raw, TOKEN);
        // Empty chunk after separator — should be error or empty
        // The key point: no panic, graceful handling
        // Either Err or Ok(empty vec) is acceptable
        match result {
            Ok(actions) => assert!(actions.is_empty(), "Empty chunk should produce no actions"),
            Err(_) => {} // Error is also acceptable
        }
    }

    #[test]
    fn test_format_anomaly_multiple_actions_with_bad_middle() {
        // Multiple actions where the middle one has an invalid variant
        // from_markdown processes all chunks; bad variant causes error for entire parse
        let raw = format!(
            "{}\nscript\necho ok\n{}\nbad_action_name\n{}\nidle\n{}",
            sep(),
            sep(),
            sep(),
            end()
        );
        let result = parse_actions(&raw, TOKEN);
        // Bad variant causes parse error for entire batch
        assert!(result.is_err(), "Bad variant in middle should cause parse error");
    }

    #[test]
    fn test_format_anomaly_missing_end_marker_auto_appended() {
        // LLM output has separator and valid content but no end marker
        // parse_actions auto-appends end marker, so this should parse successfully
        let raw = format!("{}\nidle\n", sep());
        let actions = parse_actions(&raw, TOKEN).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(
            matches!(actions[0], Action::Idle { .. }),
            "Auto-appended end marker should allow successful parse"
        );
    }

    #[test]
    fn test_format_anomaly_partial_separator() {
        // LLM outputs something that looks like a separator but isn't complete
        let raw = format!("Action-\nidle\n{}", end());
        let actions = parse_actions(&raw, TOKEN).unwrap();
        // "Action-" doesn't match "Action-{TOKEN}", so no separator found → empty
        assert_eq!(
            actions.len(),
            0,
            "Partial separator should not match"
        );
    }

    #[test]
    fn test_format_anomaly_error_message_includes_original_output() {
        // Verify that error messages include truncated original output for debugging
        let raw = format!("废话废话废话\n{}\nnonexistent_action\n{}", sep(), end());
        let result = parse_actions(&raw, TOKEN);
        assert!(result.is_err(), "Garbage + bad action should cause error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("raw output"),
            "Error should include original output snippet, got: {}",
            err
        );
    }

    #[test]
    fn test_format_anomaly_from_markdown_direct_no_separator() {
        // Directly test from_markdown with content that has no separator at all
        let result = Action::from_markdown("just some random text", TOKEN);
        assert!(
            result.is_err(),
            "from_markdown should return Err for text without separator"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("Missing end marker"),
            "Error should mention missing end marker, got: {}",
            err
        );
    }

    #[test]
    fn test_format_anomaly_from_markdown_direct_garbage_before_separator() {
        // Directly test from_markdown with garbage before separator
        let input = format!("garbage here\n{}\nidle\n{}", sep(), end());
        let result = Action::from_markdown(&input, TOKEN);
        assert!(
            result.is_err(),
            "from_markdown should return Err for garbage before separator"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("FORMAT VIOLATION"),
            "Error should contain FORMAT VIOLATION, got: {}",
            err
        );
    }

    #[test]
    fn test_format_anomaly_from_markdown_direct_missing_end_marker() {
        // Directly test from_markdown with separator but no end marker
        let input = format!("{}\nidle\n", sep());
        let result = Action::from_markdown(&input, TOKEN);
        assert!(
            result.is_err(),
            "from_markdown should return Err for missing end marker"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("Missing end marker"),
            "Error should mention missing end marker, got: {}",
            err
        );
    }
}