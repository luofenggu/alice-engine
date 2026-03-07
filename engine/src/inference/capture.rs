use crate::external::llm::ChatMessage;

pub struct CaptureRequest {
    pub knowledge_content: String,
    pub recent_content: String,
    pub current_content: String,
    pub summary_content: String,
}

const CAPTURE_SYSTEM: &str = include_str!("../../templates/capture_system.txt");

impl CaptureRequest {
    pub fn render(&self) -> Vec<ChatMessage> {
        let user = format!(
            "## 当前知识\n{}\n\n## 近况\n{}\n\n## 当前增量\n{}\n\n## 本次小结\n{}\n",
            self.knowledge_content, self.recent_content, self.current_content, self.summary_content
        );

        vec![ChatMessage::system(CAPTURE_SYSTEM), ChatMessage::user(&user)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_all_sections() {
        let req = CaptureRequest {
            knowledge_content: "K".into(),
            recent_content: "R".into(),
            current_content: "C".into(),
            summary_content: "S".into(),
        };
        let msgs = req.render();
        assert_eq!(msgs.len(), 2);
        let user = &msgs[1].content;
        assert!(user.contains("## 当前知识\nK"));
        assert!(user.contains("## 近况\nR"));
        assert!(user.contains("## 当前增量\nC"));
        assert!(user.contains("## 本次小结\nS"));
    }

    #[test]
    fn system_prompt_contains_key_concepts() {
        let req = CaptureRequest {
            knowledge_content: "".into(),
            recent_content: "".into(),
            current_content: "".into(),
            summary_content: "".into(),
        };
        let msgs = req.render();
        let system = &msgs[0].content;
        assert!(system.contains("接话"));
        assert!(system.contains("起手"));
        assert!(system.contains("用户知识洞察"));
        assert!(system.contains("自己的理解"));
    }
}

