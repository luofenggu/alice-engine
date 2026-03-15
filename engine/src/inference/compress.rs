//! # Compress Inference Protocol
//!
//! Defines the request/response protocol for history compression.
//! CompressRequest uses ToMarkdown for prompt generation;
//! CompressOutput uses FromMarkdown for structured response parsing.
//! End-marker protection is handled automatically by the mad-hatter framework.


/// @render 你是一位小说作家。请将下列内容浓缩为一篇短篇随笔，纪念一个agent和它的用户之间的故事，供agent回忆与用户之间的经历。用第二人称（你）叙述。重要的准则和术语用 > 引用标记。
#[derive(mad_hatter::ToMarkdown)]
pub struct CompressRequest {
    /// @render 压缩要求
    pub requirement: String,
    /// @render 待压缩内容
    pub content: String,
}

/// 压缩结果
#[derive(mad_hatter::FromMarkdown)]
pub struct CompressOutput {
    /// @render 随笔
    #[markdown(required)]
    pub summary: String,
}

#[cfg(test)]
mod tests {
    use mad_hatter::llm::ToMarkdown as _;
    use super::*;

    #[test]
    fn test_compress_request_to_markdown() {
        let req = CompressRequest {
            requirement: "浓缩为不超过10KB".to_string(),
            content: "session data".to_string(),
        };
        let md = req.to_markdown();
        assert!(md.contains("小说作家"));
        // Single-line fields use inline format (smart rendering)
        assert!(md.contains("压缩要求: 浓缩为不超过10KB"));
        assert!(md.contains("待压缩内容: session data"));
    }

    #[test]
    fn test_compress_request_with_combined_content() {
        let req = CompressRequest {
            requirement: "浓缩为不超过10KB".to_string(),
            content: "existing history\n\nnew session".to_string(),
        };
        let md = req.to_markdown();
        assert!(md.contains("existing history"));
        assert!(md.contains("new session"));
    }
}