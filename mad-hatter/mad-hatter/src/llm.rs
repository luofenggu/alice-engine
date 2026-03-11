/// LLM推理组件 — trait定义
///
/// ToMarkdown: struct → markdown prompt文本
/// FromMarkdown: markdown输出 → enum解析

/// 输入侧：struct → markdown prompt文本
pub trait ToMarkdown {
    /// Render as markdown with default heading depth (###)
    fn to_markdown(&self) -> String {
        self.to_markdown_depth(2)
    }

    /// Render as markdown with specified heading depth.
    /// depth=2 → ### headings, depth=3 → #### headings, etc.
    fn to_markdown_depth(&self, depth: usize) -> String;

    /// Compact rendering for Vec elements: "field: value" format.
    /// Default implementation falls back to to_markdown_depth.
    fn to_markdown_item(&self) -> String {
        self.to_markdown_depth(2)
    }
}

/// 输出侧：LLM输出 → enum解析
///
/// enum定义是唯一真相源，derive宏同时生成：
/// - schema_markdown: 格式说明（告诉LLM怎么输出）
/// - from_markdown: 解析器（把LLM输出解析回enum）
pub trait FromMarkdown: Sized {
    /// 生成格式说明文本（塞进prompt告诉LLM输出规范）
    fn schema_markdown(token: &str) -> String;
    /// 解析LLM输出为多个action实例
    fn from_markdown(text: &str, token: &str) -> Result<Vec<Self>, String>;
}

// ---------------------------------------------------------------------------
// LLM Channel — engine实现此trait提供流式文本
// ---------------------------------------------------------------------------

/// LLM推理通道。
///
/// Engine实现此trait，内部调用LLM API（OpenAI兼容SSE流式协议），
/// 将SSE chunk中的delta.content提取为纯文本chunk返回。
///
/// 框架不关心HTTP/SSE细节，只消费文本流。
pub trait LlmChannel {
    /// 流式调用LLM，返回逐chunk文本迭代器。
    ///
    /// `prompt` 是框架拼接好的完整prompt（包含输入+格式说明）。
    /// 返回的迭代器每次yield一个文本chunk（对应SSE中的delta.content）。
    fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String>;
}

// ---------------------------------------------------------------------------
// infer — 框架提供的高层推理函数
// ---------------------------------------------------------------------------

/// 执行LLM推理：拼接prompt → 调用channel → 解析响应。
///
/// 开发者只需定义 `Req: ToMarkdown`（输入）和 `Resp: FromMarkdown`（输出），
/// 框架自动完成：
/// 1. 将request序列化为markdown prompt
/// 2. 生成Resp的格式说明（含随机token）
/// 3. 拼接完整prompt并调用LLM
/// 4. 累积流式响应文本
/// 5. 用FromMarkdown解析为结构化结果
///
/// # Example
/// ```ignore
/// let actions = mad_hatter::infer::<BeatRequest, Action>(&channel, &request)?;
/// ```
pub fn infer<Req: ToMarkdown, Resp: FromMarkdown>(
    channel: &dyn LlmChannel,
    request: &Req,
) -> Result<Vec<Resp>, String> {
    let token = generate_token();

    // 1. 拼接prompt
    let input = request.to_markdown();
    let schema = Resp::schema_markdown(&token);
    let prompt = format!(
        "{}\n\n### 输出规范 ###\n{}\n你必须严格按照以上格式输出。\n",
        input, schema
    );

    // 2. 调用channel获取流式响应
    let stream = channel.infer_stream(prompt)?;

    // 3. 累积所有chunk
    let mut full_text = String::new();
    for chunk in stream {
        full_text.push_str(&chunk);
    }

    // 4. 解析
    Resp::from_markdown(&full_text, &token)
}

/// 去除LLM输出中可能的代码块包裹。
///
/// LLM有时会把输出包在 ` ```markdown ``` ` 或 ` ``` ``` ` 里，
/// 此函数自动检测并剥离外层代码块。
/// 框架内置能力，在from_markdown中自动调用，开发者无感。
pub fn strip_code_block(text: &str) -> String {
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

/// 生成随机token（用于分隔符，不需要密码学安全）
fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", nanos)
}