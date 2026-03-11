//! # Beat Inference Protocol
//!
//! Defines the request/response protocol for one React cognitive beat.
//! BeatRequest is a pure data struct with ToMarkdown derive.
//! stream_infer() uses to_markdown() + schema_markdown() to build the prompt.


use crate::policy::messages;

use mad_hatter::ToMarkdown;

const RESERVED_SKILL_TEMPLATE: &str = r#"### knowledge: app-development ###
# App 开发指南

当用户要求你开发app时，你拥有以下能力：
- **静态文件托管**：将文件放在工作空间中，通过 http://{host}/serve/{instance}/{路径} 访问
- **公开访问**：将文件放在 workspace/apps/ 目录下，任何人无需登录即可通过 http://{host}/public/{instance}/apps/{路径} 访问
- **本地服务**：启动 Python/Node 等服务监听端口，通过下面两种方式之一让用户访问
- **数据持久化**：系统已预装sqlite3，推荐每个app使用独立的SQLite数据库文件（如 app目录/data.db）

## 网络访问方式

启动本地服务后，用户如何访问取决于网络环境。**请先和用户确认**：

### 情况一：用户可以直达你的机器

如果满足以下任一条件：
- 你运行在用户的本地电脑上（localhost）
- 你在云服务器上，有公网IP，且用户已在安全组/防火墙放行了对应端口

那么用户可以直接访问 `http://{IP或localhost}:{端口}/`。这种情况下代码没有路径限制，正常开发即可。

### 情况二：用户无法直达，需要反向代理

如果用户无法直接访问你的端口（比如没有公网IP、端口未放行、或通过网关中转），可以使用内置的反向代理：`http://{host}/proxy/{端口}/{路径}`（端口范围1024-65535）。

**⚠️ 反向代理下必须使用相对路径**

浏览器地址栏的URL前缀是 `/proxy/{端口}/`。如果代码中使用绝对路径（以 `/` 开头），浏览器会直接访问 `/xxx` 而丢失前缀，导致404。

**原则：所有路径都不带前导 `/`，使用相对路径。**

常见错误和正确写法：

| 场景 | ❌ 错误 | ✅ 正确 |
|------|---------|---------|
| HTML链接 | `<a href="/login">` | `<a href="login">` |
| 表单提交 | `<form action="/api/submit">` | `<form action="api/submit">` |
| JS fetch | `fetch('/api/data')` | `fetch('api/data')` |
| JS跳转 | `location.href = '/dashboard'` | `location.href = 'dashboard'` |
| CSS资源 | `url('/static/bg.png')` | `url('static/bg.png')` |
| 重定向 | `redirect("/login")` | `redirect("login")` |
| 静态文件引用 | `<script src="/js/app.js">` | `<script src="js/app.js">` |

**后端重定向也要注意**：Python Flask 的 `redirect("/login")` 会生成绝对路径的 Location header。用 `redirect("login")` 或返回相对路径。

**自查清单**：写完代码后，全局搜索 `href="/`、`src="/`、`action="/`、`fetch('/`、`url('/`、`redirect("/`，把所有绝对路径改成相对路径。

### 不确定？

如果你不清楚用户的网络环境，主动问一句："你能直接访问我这台机器的端口吗？比如在浏览器打开 http://{IP}:8080 试试。如果不行，我用反向代理给你。"

## 规范日志

开发app时，养成写规范日志的习惯：

**带速查标记前缀：**
```
[AUTH] User login: user_id=123
[ORDER-a1b2] Payment callback: status=success
[DB] Migration applied: v2_add_index
```

每个模块/功能用固定前缀标记，关键业务加上实体ID（如 `[ORDER-{id}]`）。

**为什么这很重要：**
- **运行时定位**：`grep '[ORDER-a1b2]' app.log` 一条命令追踪完整链路
- **代码定位**：日志标记本身就是代码搜索关键词——从日志反查到代码只需一次grep
- **持久中间层**：你的记忆会滚动，但日志不会过期，是你理解系统运行状态最可靠的依据

回信中附上完整URL，用户点击即可打开。

## 图片理解

当你需要理解图片内容时，可以在脚本中调用本机的多模态API：

```bash
curl -s -X POST http://localhost:{port}/api/instances/{instance}/vision \
  -H "Content-Type: application/json" \
  -d '{"prompt":"描述这张图片的内容","image_url":"图片的URL"}'
```

返回格式：`{"text":"图片描述内容"}`

- 此API使用你自己的LLM channel，需要模型支持vision（如Claude、GPT-4o等）
- image_url 支持公网URL或base64格式（`data:image/png;base64,...`）
- 适用场景：用户让你看图片、理解截图、分析上传的图像等

## 用户上传文件

用户可能会上传文件到云端。上传的文件在你的工作目录中可通过 `uploads/` 访问：

- 文件按日期分目录：`uploads/YYYYMMDD/filename`
- 文本文件直接用 `cat uploads/YYYYMMDD/filename` 读取
- 图片文件用上面的多模态API理解内容（先用 `ls uploads/` 查看有哪些文件）"#;

pub const INITIAL_HISTORY: &str = include_str!("../../templates/initial_history.txt");

const SOFT_LIMIT: usize = 180_000;

use crate::policy::action_output as out;
use crate::policy::EngineConfig;

// ---------------------------------------------------------------------------
// Data structs — raw data extracted from Alice's state
// ---------------------------------------------------------------------------

/// A single chat message (raw data, not yet formatted).
pub struct PromptMessage {
    pub role: String,
    pub sender: String,
    pub timestamp: String,
    pub content: String,
}

/// One session entry: chat messages + summary (raw data).
pub struct SessionEntryData {
    pub messages: Vec<PromptMessage>,
    pub summary: String,
}

/// One session block: block name + entries (raw data).
pub struct SessionBlockData {
    pub block_name: String,
    pub entries: Vec<SessionEntryData>,
}

// ---------------------------------------------------------------------------
// BeatRequest — pure data struct for one beat's prompt rendering
// ---------------------------------------------------------------------------

/// 你醒了，你发现自己身处一个密闭房间，桌子上摆放着几样东西。
/// 收件箱：你可以在此收到来信
/// 寄件箱：你可以在此寄出信件
/// 工作目录：你可以在此读写文件、执行脚本以完成任务
#[derive(ToMarkdown)]
pub struct BeatRequest {
    /// skill
    pub skill: String,
    /// 知识
    pub knowledge: String,
    /// 经历
    pub history: String,
    /// 近况
    pub sessions: String,
    /// 环境信息
    pub environment: String,
    /// current
    pub current: String,
    /// 当前状态
    pub status: String,
}

impl From<&crate::persist::SessionBlockEntry> for SessionEntryData {
    fn from(entry: &crate::persist::SessionBlockEntry) -> Self {
        SessionEntryData {
            messages: vec![],
            summary: entry.summary.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Public formatting — used by both prompt building and compress
// ---------------------------------------------------------------------------

/// Format session entries into display text.
/// Used by prompt builder for beat prompts and by compress for history rolling.
pub fn format_session_entries(entries: &[SessionEntryData], self_id: &str) -> String {
    let truncate_len = EngineConfig::get().memory.message_truncate_length;

    let mut sections = Vec::new();
    for entry in entries {
        let mut parts = Vec::new();

        for msg in &entry.messages {
            let content_display = if msg.content.len() > truncate_len {
                messages::truncated_content(&crate::util::safe_truncate(&msg.content, truncate_len))
            } else {
                msg.content.clone()
            };
            parts.push(messages::chat_message(
                &msg.role,
                &msg.sender,
                self_id,
                &msg.timestamp,
                &content_display,
            ));
        }

        if !entry.summary.is_empty() {
            parts.push(messages::session_summary(&entry.summary));
        }

        if !parts.is_empty() {
            sections.push(parts.join("\n"));
        }
    }
    sections.join("\n\n")
}

/// Render session blocks into display text.
pub fn render_session_blocks(blocks: &[SessionBlockData], self_id: &str) -> String {
    if blocks.is_empty() {
        return String::new();
    }

    let mut sections = Vec::new();
    for block in blocks {
        let rendered = format_session_entries(&block.entries, self_id);
        if !rendered.is_empty() {
            sections.push(format!("[{}]\n{}", block.block_name, rendered));
        }
    }
    sections.join("\n\n")
}

// ---------------------------------------------------------------------------
// Response parsing — second-layer parsing of specific action content
// ---------------------------------------------------------------------------

/// Parse dual-output summary: split into summary text and knowledge text.
/// The knowledge_marker contains the current beat's random token, ensuring
/// it cannot match any historical content (self-reference defense).
pub fn parse_summary_dual_output(
    raw: &str,
    summary_marker: &str,
    knowledge_marker: &str,
) -> (String, String) {
    // Find first knowledge marker on its own line (token ensures uniqueness)
    let lines: Vec<&str> = raw.lines().collect();
    let mut knowledge_line_idx: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if line.trim() == knowledge_marker {
            knowledge_line_idx = Some(i);
            break;
        }
    }

    let (summary_part, knowledge_part) = match knowledge_line_idx {
        Some(idx) => {
            let summary = lines[..idx].join("\n");
            let knowledge = lines[idx + 1..].join("\n");
            (summary, knowledge)
        }
        None => {
            // No knowledge marker found, treat entire output as summary
            (raw.to_string(), String::new())
        }
    };

    // Strip leading ===SUMMARY=== marker if present
    let summary_part = {
        let trimmed = summary_part.trim_start();
        if trimmed.starts_with(summary_marker) {
            let after = &trimmed[summary_marker.len()..];
            after.trim_start_matches('\n').to_string()
        } else {
            summary_part
        }
    };

    (summary_part, knowledge_part)
}

/// Extract MSG IDs from current.txt content.
/// Only matches trusted markers (send success / read_msg format).
/// Returns IDs in **appearance order** (not sorted), to avoid
/// stale timestamps in exec results from expanding the range.
pub fn extract_msg_ids(text: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let marker = "[MSG:";
    let send_context = out::MSG_SEND_CONTEXT;
    let read_context = out::MSG_READ_CONTEXT;
    let mut search_from = 0;

    while let Some(start) = text[search_from..].find(marker) {
        let bracket_pos = search_from + start;
        let abs_start = bracket_pos + marker.len();
        if let Some(end) = text[abs_start..].find(']') {
            let candidate = &text[abs_start..abs_start + end];
            if candidate.len() == 14 && candidate.chars().all(|c| c.is_ascii_digit()) {
                let after_bracket = abs_start + end + 1;
                let is_send = bracket_pos >= send_context.len()
                    && text
                        .get(bracket_pos - send_context.len()..bracket_pos)
                        .map_or(false, |s| s == send_context);
                let is_read = text
                    .get(after_bracket..)
                    .map_or(false, |s| s.starts_with(read_context));
                if (is_send || is_read) && !ids.contains(&candidate.to_string()) {
                    ids.push(candidate.to_string());
                }
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }

    ids
}

// ---------------------------------------------------------------------------
// Helper functions (pure, no Alice dependency)
// ---------------------------------------------------------------------------

/// Build skill content: default skill (app guide) + instance custom skill + extra skills.
/// Returns combined content WITHOUT section header (caller/ToMarkdown adds it).
pub fn build_skill_content(
    host: Option<&str>,
    instance_id: &str,
    http_port: u16,
    skill_content: &str,
    extra_skills: &str,
) -> String {
    let default_skill = make_reserved_skill(host, instance_id, http_port);

    let mut parts: Vec<&str> = Vec::new();
    if !default_skill.is_empty() {
        parts.push(&default_skill);
    }
    if !skill_content.trim().is_empty() {
        parts.push(skill_content);
    }
    if !extra_skills.trim().is_empty() {
        parts.push(extra_skills);
    }

    parts.join("\n\n")
}

/// Build knowledge content with sub-section header.
/// Returns content WITH "### 要点与知识 ###" sub-header, or empty string.
pub fn build_knowledge_content(knowledge_content: &str) -> String {
    if knowledge_content.trim().is_empty() {
        String::new()
    } else {
        messages::knowledge_section(knowledge_content)
    }
}

/// Build environment info string.
pub fn build_environment(
    instance_id: &str,
    instance_name: Option<&str>,
    contacts_info: &str,
    shell_env: &str,
    host: Option<&str>,
) -> String {
    let name_part = match instance_name {
        Some(name) if !name.is_empty() => format!("{}（{}）", name, instance_id),
        _ => instance_id.to_string(),
    };
    let mut lines = vec![format!("你是{}", name_part)];
    if !contacts_info.is_empty() {
        lines.push(contacts_info.to_string());
    }
    lines.push(format!("脚本环境：{}", shell_env));
    let host_line = make_host_line(host);
    if !host_line.is_empty() {
        lines.push(host_line);
    }
    lines.join("\n")
}

/// Build status string for the "当前状态" section.
pub fn build_status(
    instance_id: &str,
    instance_name: Option<&str>,
    system_start_time: chrono::DateTime<chrono::Local>,
    unread_count: usize,
    history_size: usize,
    sessions_size: usize,
    current_size: usize,
    knowledge_size: usize,
    skill_size: usize,
) -> String {
    let current_time = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let start_time = system_start_time.format("%Y%m%d%H%M%S").to_string();

    let memory_status = make_memory_status(
        instance_id,
        instance_name,
        history_size,
        sessions_size,
        current_size,
        knowledge_size + skill_size,
    );

    format!(
        "现在时刻：[{}]\n系统启动时刻：[{}]\n收件箱未读来信：[{}] 条\n{}",
        current_time, start_time, unread_count, memory_status
    )
}

/// Build memory status report with character counts and optional warning.
fn make_memory_status(
    instance_id: &str,
    instance_name: Option<&str>,
    history_size: usize,
    daily_size: usize,
    current_size: usize,
    knowledge_size: usize,
) -> String {
    let total = history_size + daily_size + current_size + knowledge_size;
    let knowledge_indicator = if knowledge_size < 51200 {
        messages::knowledge_capacity_ok(knowledge_size)
    } else if knowledge_size < 61440 {
        messages::knowledge_capacity_warning(knowledge_size)
    } else {
        messages::knowledge_capacity_critical(knowledge_size)
    };

    let instance_line = match instance_name {
        Some(name) if !name.is_empty() => format!("实例名：{}（{}）", name, instance_id),
        _ => format!("实例名：{}", instance_id),
    };

    let mut status = format!(
        "{}\ncurrent: {}字符 | 经历: {}字符 | 近况: {}字符 | {} | 合计: {}字符",
        instance_line, current_size, history_size, daily_size, knowledge_indicator, total
    );

    if total > SOFT_LIMIT {
        let kb = total / 1000;
        status.push_str(&messages::memory_over_limit(kb));
    }

    status
}

/// Build host info line (just the public address, if available).
fn make_host_line(host: Option<&str>) -> String {
    match host {
        Some(h) if !h.is_empty() => messages::host_info(h),
        _ => String::new(),
    }
}

/// Build reserved skill section (app guide + vision API + uploads).
/// Returns empty string if no host is configured.
fn make_reserved_skill(host: Option<&str>, instance_id: &str, port: u16) -> String {
    let h = match host {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => format!("localhost:{}", port),
    };
    RESERVED_SKILL_TEMPLATE
        .replace("{host}", &h)
        .replace("{instance}", instance_id)
        .replace("{port}", &port.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_summary_dual_output_with_knowledge() {
        let raw = "This is summary\n===KNOWLEDGE_abc123===\nknowledge content here";
        let (summary, knowledge) =
            parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_abc123===");
        assert_eq!(summary, "This is summary");
        assert_eq!(knowledge, "knowledge content here");
    }

    #[test]
    fn test_parse_summary_dual_output_no_knowledge() {
        let raw = "Just summary, no knowledge marker";
        let (summary, knowledge) =
            parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_xyz===");
        assert_eq!(summary, "Just summary, no knowledge marker");
        assert_eq!(knowledge, "");
    }

    #[test]
    fn test_parse_summary_dual_output_strip_summary_marker() {
        let raw = "===SUMMARY===\nActual summary\n===KNOWLEDGE_t1===\nknowledge";
        let (summary, knowledge) =
            parse_summary_dual_output(raw, "===SUMMARY===", "===KNOWLEDGE_t1===");
        assert_eq!(summary, "Actual summary");
        assert_eq!(knowledge, "knowledge");
    }

    #[test]
    fn test_extract_msg_ids_send() {
        let text = "send success [MSG:20260303120000]\nsome other text";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260303120000"]);
    }

    #[test]
    fn test_extract_msg_ids_read() {
        let text = "[MSG:20260303130000]发来一条消息：\nhello";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260303130000"]);
    }

    #[test]
    fn test_extract_msg_ids_mixed() {
        let text = "send success [MSG:20260303120000]\n[MSG:20260303130000]发来一条消息：\nhello\nsend success [MSG:20260303120000]";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260303120000", "20260303130000"]);
    }

    #[test]
    fn test_extract_msg_ids_ignores_untrusted() {
        let text = "random [MSG:20260303120000] text without context";
        let ids = extract_msg_ids(text);
        assert!(ids.is_empty());
    }

    #[test]
    fn test_extract_msg_ids_system_message() {
        let text = "[系统通知] [MSG:20260307103449]发来一条消息：\n\n[验证] system消息测试\n";
        let ids = extract_msg_ids(text);
        assert_eq!(ids, vec!["20260307103449"]);
    }

    #[test]
    fn test_make_memory_status_basic() {
        let status = make_memory_status("test-id", Some("TestBot"), 1000, 2000, 3000, 4000);
        assert!(status.contains("TestBot"));
        assert!(status.contains("test-id"));
        assert!(status.contains("3000"));
    }

    #[test]
    fn test_make_memory_status_over_limit() {
        let status = make_memory_status("id", None, 50000, 50000, 50000, 50000);
        assert!(status.contains("⚠️"));
    }

    #[test]
    fn test_make_host_line() {
        assert_eq!(
            make_host_line(Some("1.2.3.4:8080")),
            messages::host_info("1.2.3.4:8080")
        );
        assert_eq!(make_host_line(None), "");
        assert_eq!(make_host_line(Some("")), "");
    }

    #[test]
    fn test_make_reserved_skill_no_host() {
        // When host is None or empty, falls back to localhost:{port}
        let result = make_reserved_skill(None, "test", 8081);
        assert!(result.contains("localhost:8081"), "should fallback to localhost:port");
        assert!(result.contains("test"), "should contain instance_id");

        let result2 = make_reserved_skill(Some(""), "test", 8081);
        assert!(result2.contains("localhost:8081"), "empty host should also fallback");
    }

    #[test]
    fn test_format_session_entries_basic() {
        let entries = vec![SessionEntryData {
            messages: vec![PromptMessage {
                role: "user".into(),
                sender: "user".into(),
                timestamp: "20260303120000".into(),
                content: "hello".into(),
            }],
            summary: "User said hello".into(),
        }];
        let rendered = format_session_entries(&entries, "test_self");
        assert!(rendered.contains("user"));
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("User said hello"));
    }

    #[test]
    fn test_format_session_entries_truncates() {
        let long_content = "x".repeat(300);
        let entries = vec![SessionEntryData {
            messages: vec![PromptMessage {
                role: "agent".into(),
                sender: "test_self".into(),
                timestamp: "20260303120000".into(),
                content: long_content,
            }],
            summary: String::new(),
        }];
        let rendered = format_session_entries(&entries, "test_self");
        assert!(!rendered.contains(&"x".repeat(300)));
    }

    #[test]
    fn test_format_session_entries_empty() {
        let entries: Vec<SessionEntryData> = vec![];
        assert_eq!(format_session_entries(&entries, "test_self"), "");
    }

    #[test]
    fn test_build_skill_content() {
        let result = build_skill_content(Some("1.2.3.4:8080"), "test", 8081, "custom skill", "extra");
        assert!(result.contains("custom skill"));
        assert!(result.contains("extra"));
    }

    #[test]
    fn test_build_skill_content_empty() {
        let result = build_skill_content(None, "test", 8081, "", "");
        // Should still have reserved skill (app guide)
        assert!(result.contains("localhost:8081"));
    }

    #[test]
    fn test_build_knowledge_content() {
        let result = build_knowledge_content("# 泛准则\n- 谨慎加信任");
        assert!(result.contains("### 要点与知识 ###"));
        assert!(result.contains("谨慎加信任"));
    }

    #[test]
    fn test_build_knowledge_content_empty() {
        assert_eq!(build_knowledge_content(""), "");
        assert_eq!(build_knowledge_content("  "), "");
    }

    #[test]
    fn test_build_environment() {
        let env = build_environment("abc123", Some("TestBot"), "联系人列表", "Linux x86_64", Some("1.2.3.4:8080"));
        assert!(env.contains("TestBot（abc123）"));
        assert!(env.contains("联系人列表"));
        assert!(env.contains("脚本环境：Linux x86_64"));
        assert!(env.contains("1.2.3.4:8080"));
    }

    #[test]
    fn test_build_environment_no_contacts() {
        let env = build_environment("abc123", None, "", "Linux x86_64", None);
        assert!(env.contains("你是abc123"));
        assert!(!env.contains("联系人"));
    }

    #[test]
    fn test_to_markdown_output() {
        use mad_hatter::llm::ToMarkdown;
        let request = BeatRequest {
            skill: String::new(),
            knowledge: String::new(),
            history: "(空)".to_string(),
            sessions: String::new(),
            environment: "你是test".to_string(),
            current: "(空)".to_string(),
            status: "现在时刻：[20260311120000]".to_string(),
        };
        let output = request.to_markdown();
        // Empty skill and knowledge should be skipped by ToMarkdown
        assert!(!output.contains("### skill ###"));
        assert!(!output.contains("### 知识 ###"));
        // Non-empty fields should have headers
        assert!(output.contains("### 经历 ###"));
        assert!(output.contains("### 环境信息 ###"));
        assert!(output.contains("### current ###"));
        assert!(output.contains("### 当前状态 ###"));
        // Scene description (struct-level doc comment) should appear at the top
        assert!(output.contains("你醒了"));
    }
}

