//! # Compress Inference Protocol
//!
//! Defines the request/response protocol for history compression.
//! CompressRequest renders the compression prompt; response is plain text.

use crate::inference::safe_render;

const HISTORY_COMPRESS_PROMPT: &str = include_str!("../../templates/history_compress.txt");

/// Request for history compression inference.
pub struct CompressRequest {
    /// Target size in KB for the compressed history.
    pub history_kb: usize,
    /// The rendered session block content to compress.
    pub session_content: String,
    /// The current history content to append to.
    pub current_history: String,
}

impl CompressRequest {
    /// Render the compression prompt.
    /// Returns `(system_prompt, user_content)` for the LLM call.
    pub fn render(&self) -> (String, String) {
        let system_msg = safe_render(HISTORY_COMPRESS_PROMPT, &[
            ("{{HISTORY_KB}}", &self.history_kb.to_string()),
        ]);

        let user_content = if self.current_history.is_empty() {
            self.session_content.clone()
        } else {
            format!("{}\n\n{}", self.current_history, self.session_content)
        };

        (system_msg, user_content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_request_render() {
        let req = CompressRequest {
            history_kb: 10,
            session_content: "session data".to_string(),
            current_history: String::new(),
        };
        let (system, user) = req.render();
        assert!(system.contains("10"));
        assert_eq!(user, "session data");
    }

    #[test]
    fn test_compress_request_with_existing_history() {
        let req = CompressRequest {
            history_kb: 10,
            session_content: "new session".to_string(),
            current_history: "existing history".to_string(),
        };
        let (_, user) = req.render();
        assert!(user.contains("existing history"));
        assert!(user.contains("new session"));
    }
}
