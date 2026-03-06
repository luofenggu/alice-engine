use crate::external::llm::ChatMessage;

pub struct CaptureRequest {
    pub knowledge_content: String,
    pub recent_content: String,
    pub current_content: String,
    pub summary_content: String,
}

impl CaptureRequest {
    pub fn render(&self) -> Vec<ChatMessage> {
        let system = r#"你是一个知识维护者。你的任务是基于当前知识、近况、当前增量和本次小结，产出新的完整知识文件。

要求：
- 输出完整知识文件，不要解释，不要加代码块
- 删除语义要克制：不能因为“本轮没提到”就删
- 优先删除或压缩：重复内容、可grep细节、一次性过程、过时冗余
- 保留“用户知识洞察”
- 结构保持为：
  - 用户知识洞察——
  - 自己的理解——
- 如果知识过长，必须主动压缩合并，在容量上限内完成"#;

        let user = format!(
            "## 当前知识\n{}\n\n## 近况\n{}\n\n## 当前增量\n{}\n\n## 本次小结\n{}\n",
            self.knowledge_content, self.recent_content, self.current_content, self.summary_content
        );

        vec![ChatMessage::system(system), ChatMessage::user(&user)]
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
}
