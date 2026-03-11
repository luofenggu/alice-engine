/// LLM推理组件 — trait定义
///
/// ToMarkdown: struct → markdown prompt文本
/// FromMarkdown: markdown文本 → struct/enum (P1+)

/// 输入侧：struct → markdown prompt文本
pub trait ToMarkdown {
    fn to_markdown(&self) -> String;
}

