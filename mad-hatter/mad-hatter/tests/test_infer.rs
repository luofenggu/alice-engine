use mad_hatter::{ToMarkdown, FromMarkdown, LlmChannel, infer};


// ============================================================
// Test types
// ============================================================

#[derive(ToMarkdown)]
/// 你是一个助手。请根据用户的请求执行操作。
struct SimpleRequest {
    /// 用户消息
    message: String,
    /// 上下文信息
    context: String,
}

#[derive(FromMarkdown, PartialEq, Debug)]
enum SimpleAction {
    /// 回复用户
    #[allow(dead_code)]
    Reply {
        content: String,
    },
    /// 什么都不做
    #[allow(dead_code)]
    Idle,
    /// 记录思考
    #[allow(dead_code)]
    Think {
        content: String,
    },
    /// 发送消息
    #[allow(dead_code)]
    SendMsg {
        recipient: String,
        content: String,
    },
    /// 等待
    #[allow(dead_code)]
    Wait {
        seconds: Option<u64>,
    },
}

// ============================================================
// Mock LlmChannel
// ============================================================



/// Mock channel that captures the prompt for inspection
struct CapturingChannel {
    captured_prompt: std::cell::RefCell<String>,
    response: String,
}

impl CapturingChannel {
    fn new(response: &str) -> Self {
        Self {
            captured_prompt: std::cell::RefCell::new(String::new()),
            response: response.to_string(),
        }
    }

    fn get_prompt(&self) -> String {
        self.captured_prompt.borrow().clone()
    }
}

impl LlmChannel for CapturingChannel {
    fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
        *self.captured_prompt.borrow_mut() = prompt;
        Ok(Box::new(std::iter::once(self.response.clone())))
    }
}

/// Mock channel that returns an error
struct ErrorChannel;

impl LlmChannel for ErrorChannel {
    fn infer_stream(&self, _prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
        Err("LLM service unavailable".to_string())
    }
}

// ============================================================
// Tests
// ============================================================

#[test]
fn test_infer_single_action() {
    // We need to know the token to construct the response, but infer() generates it internally.
    // Solution: use infer_with_token() or make the response work with any token.
    // Since we can't predict the token, we need a different approach.
    //
    // Actually, the mock channel receives the prompt which contains the token in the schema.
    // We can extract the token from the prompt and construct the response dynamically.
    //
    // Better approach: use a channel that inspects the prompt to find the token.

    struct DynamicMockChannel;

    impl LlmChannel for DynamicMockChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            // Extract token from the schema in the prompt
            // Schema contains "SimpleAction-{token}" as element separator
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nreply\n你好！我收到了你的消息。\nSimpleAction-end-{t}",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = SimpleRequest {
        message: "你好".to_string(),
        context: "测试上下文".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&DynamicMockChannel, &request).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], SimpleAction::Reply {
        content: "你好！我收到了你的消息。".to_string(),
    });
}

#[test]
fn test_infer_multiple_actions() {
    struct MultiActionChannel;

    impl LlmChannel for MultiActionChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nthink\n让我想想...\nSimpleAction-{t}\nreply\n答案是42。\nSimpleAction-{t}\nidle\nSimpleAction-end-{t}",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = SimpleRequest {
        message: "问题".to_string(),
        context: "".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&MultiActionChannel, &request).unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0], SimpleAction::Think { content: "让我想想...".to_string() });
    assert_eq!(result[1], SimpleAction::Reply { content: "答案是42。".to_string() });
    assert_eq!(result[2], SimpleAction::Idle);
}

#[test]
fn test_infer_two_field_variant() {
    struct TwoFieldChannel;

    impl LlmChannel for TwoFieldChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nsend_msg\nrecipient-{t}\nuser\ncontent-{t}\n你好世界！\n第二行。\nSimpleAction-end-{t}",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = SimpleRequest {
        message: "发消息".to_string(),
        context: "".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&TwoFieldChannel, &request).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], SimpleAction::SendMsg {
        recipient: "user".to_string(),
        content: "你好世界！\n第二行。".to_string(),
    });
}

#[test]
fn test_infer_option_field() {
    struct OptionChannel {
        with_value: bool,
    }

    impl LlmChannel for OptionChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = if self.with_value {
                format!(
                    "SimpleAction-{t}\nwait\n120\nSimpleAction-end-{t}",
                    t = token
                )
            } else {
                format!(
                    "SimpleAction-{t}\nwait\nSimpleAction-end-{t}",
                    t = token
                )
            };
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = SimpleRequest {
        message: "等待".to_string(),
        context: "".to_string(),
    };

    // With value
    let result: Vec<SimpleAction> = infer(&OptionChannel { with_value: true }, &request).unwrap();
    assert_eq!(result[0], SimpleAction::Wait { seconds: Some(120) });

    // Without value
    let result: Vec<SimpleAction> = infer(&OptionChannel { with_value: false }, &request).unwrap();
    assert_eq!(result[0], SimpleAction::Wait { seconds: None });
}

#[test]
fn test_infer_streaming_chunks() {
    // Test that chunked responses are correctly assembled
    struct ChunkedChannel;

    impl LlmChannel for ChunkedChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let full = format!(
                "SimpleAction-{t}\nreply\nHello World!\nSimpleAction-end-{t}",
                t = token
            );
            // Split into small chunks to simulate streaming
            let chunks: Vec<String> = full.chars()
                .collect::<Vec<_>>()
                .chunks(5)
                .map(|c| c.iter().collect::<String>())
                .collect();
            Ok(Box::new(chunks.into_iter()))
        }
    }

    let request = SimpleRequest {
        message: "hi".to_string(),
        context: "".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&ChunkedChannel, &request).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], SimpleAction::Reply { content: "Hello World!".to_string() });
}

#[test]
fn test_infer_channel_error() {
    let request = SimpleRequest {
        message: "test".to_string(),
        context: "".to_string(),
    };

    let result: Result<Vec<SimpleAction>, String> = infer(&ErrorChannel, &request);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("LLM service unavailable"));
}

#[test]
fn test_infer_missing_end_marker() {
    struct NoEndChannel;

    impl LlmChannel for NoEndChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            // Missing end marker
            let response = format!(
                "SimpleAction-{t}\nreply\ncontent-{t}\n截断的响应",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = SimpleRequest {
        message: "test".to_string(),
        context: "".to_string(),
    };

    let result: Result<Vec<SimpleAction>, String> = infer(&NoEndChannel, &request);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Missing end marker"));
}

#[test]
fn test_infer_prompt_contains_schema() {
    // Verify that the prompt sent to channel contains both request content and schema
    let response_holder = std::cell::RefCell::new(String::new());

    struct InspectChannel<'a> {
        prompt_holder: &'a std::cell::RefCell<String>,
    }

    impl<'a> LlmChannel for InspectChannel<'a> {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            *self.prompt_holder.borrow_mut() = prompt;
            let response = format!(
                "SimpleAction-{t}\nidle\nSimpleAction-end-{t}",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = SimpleRequest {
        message: "检查prompt".to_string(),
        context: "测试上下文".to_string(),
    };

    let channel = InspectChannel { prompt_holder: &response_holder };
    let _result: Vec<SimpleAction> = infer(&channel, &request).unwrap();

    let prompt = response_holder.borrow().clone();

    // Prompt should contain request content
    assert!(prompt.contains("检查prompt"), "prompt should contain message");
    assert!(prompt.contains("测试上下文"), "prompt should contain context");

    // Prompt should contain schema
    assert!(prompt.contains("action 回复用户"), "prompt should contain schema");
    assert!(prompt.contains("action 什么都不做"), "prompt should contain idle schema");
    assert!(prompt.contains("SimpleAction-"), "prompt should contain element separator");
    assert!(prompt.contains("SimpleAction-end-"), "prompt should contain end marker");

    // Prompt should contain format instruction
    assert!(prompt.contains("输出规范"), "prompt should contain format section");
}

#[test]
fn test_infer_prompt_has_no_token_leak() {
    // The token should only appear in the schema section, not in the request section
    let captured = CapturingChannel::new("");

    let request = SimpleRequest {
        message: "hello".to_string(),
        context: "world".to_string(),
    };

    // This will fail because the response is empty, but we can still inspect the prompt
    let _ = infer::<SimpleRequest, SimpleAction>(&captured, &request);

    let prompt = captured.get_prompt();

    // Find the token by looking for SimpleAction-{something}
    if let Some(pos) = prompt.find("SimpleAction-") {
        let after = &prompt[pos + "SimpleAction-".len()..];
        let token_end = after.find('\n').unwrap_or(after.len());
        let token = &after[..token_end];
        // Token should not be "end" (that's the end marker pattern)
        if !token.starts_with("end") {
            // The request section (before "### 输出规范 ###") should not contain the token
            if let Some(schema_pos) = prompt.find("### 输出规范 ###") {
                let request_section = &prompt[..schema_pos];
                assert!(!request_section.contains(token),
                    "Token '{}' leaked into request section", token);
            }
        }
    }
}

// ============================================================
// Helper
// ============================================================

/// Extract the token from a prompt by finding "{type_name}-{token}" pattern
fn extract_token_from_prompt(prompt: &str, type_name: &str) -> String {
    let prefix = format!("{}-", type_name);
    // Find the first occurrence that's not "{type_name}-end-"
    for line in prompt.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            if !rest.starts_with("end-") && !rest.is_empty() {
                // The token might have more text after it, take until whitespace or end
                let token = rest.split_whitespace().next().unwrap_or(rest);
                return token.to_string();
            }
        }
    }
    panic!("Could not extract token from prompt for type '{}'", type_name);
}

