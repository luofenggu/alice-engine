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