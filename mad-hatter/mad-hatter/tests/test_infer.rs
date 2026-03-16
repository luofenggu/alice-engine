use mad_hatter::{ToMarkdown, FromMarkdown, LlmChannel, infer, infer_with_on_text, InferError};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;


// ============================================================
// Test types
// ============================================================

#[derive(ToMarkdown)]
/// @render 你是一个助手。请根据用户的请求执行操作。
struct SimpleRequest {
    /// @render 用户消息
    message: String,
    /// @render 上下文信息
    context: String,
}

#[derive(FromMarkdown, PartialEq, Debug)]
enum SimpleAction {
    /// @render 回复用户
    #[allow(dead_code)]
    Reply {
        content: String,
    },
    /// @render 什么都不做
    #[allow(dead_code)]
    Idle,
    /// @render 记录思考
    #[allow(dead_code)]
    Think {
        content: String,
    },
    /// @render 发送消息
    #[allow(dead_code)]
    SendMsg {
        recipient: String,
        content: String,
    },
    /// @render 等待
    #[allow(dead_code)]
    Wait {
        seconds: Option<u64>,
    },
}

// ============================================================
// Helper
// ============================================================

/// Helper to create an UnboundedReceiver from chunks
fn mock_channel_rx(chunks: Vec<String>) -> UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for chunk in chunks {
        tx.send(chunk).ok();
    }
    rx
}

/// Extract the token from a prompt by finding "{type_name}-{token}" pattern
fn extract_token_from_prompt(prompt: &str, type_name: &str) -> String {
    let prefix = format!("{}-", type_name);
    // Find the first occurrence that's not "{type_name}-end-"
    for line in prompt.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            if !rest.starts_with("end-") && !rest.is_empty() {
                let token = rest.split_whitespace().next().unwrap_or(rest);
                return token.to_string();
            }
        }
    }
    panic!("Could not extract token from prompt for type '{}'", type_name);
}

// ============================================================
// Mock LlmChannel
// ============================================================

/// @render Mock channel that captures the prompt for inspection
struct CapturingChannel {
    captured_prompt: Mutex<String>,
    response: String,
}

impl CapturingChannel {
    fn new(response: &str) -> Self {
        Self {
            captured_prompt: Mutex::new(String::new()),
            response: response.to_string(),
        }
    }

    fn get_prompt(&self) -> String {
        self.captured_prompt.lock().unwrap().clone()
    }
}

impl LlmChannel for CapturingChannel {
    fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
        *self.captured_prompt.lock().unwrap() = prompt;
        Ok(mock_channel_rx(vec![self.response.clone()]))
    }
}

/// @render Mock channel that returns an error
struct ErrorChannel;

impl LlmChannel for ErrorChannel {
    fn start_stream(&self, _prompt: String) -> Result<UnboundedReceiver<String>, String> {
        Err("LLM service unavailable".to_string())
    }
}

// ============================================================
// Tests
// ============================================================

#[tokio::test]
async fn test_infer_single_action() {
    struct DynamicMockChannel;

    impl LlmChannel for DynamicMockChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nreply\ncontent-{t}\n你好！我收到了你的消息。\nSimpleAction-end-{t}",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = SimpleRequest {
        message: "你好".to_string(),
        context: "测试上下文".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&DynamicMockChannel, &request).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], SimpleAction::Reply {
        content: "你好！我收到了你的消息。".to_string(),
    });
}

#[tokio::test]
async fn test_infer_multiple_actions() {
    struct MultiActionChannel;

    impl LlmChannel for MultiActionChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nthink\ncontent-{t}\n让我想想...\nSimpleAction-{t}\nreply\ncontent-{t}\n答案是42。\nSimpleAction-{t}\nidle\nSimpleAction-end-{t}",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = SimpleRequest {
        message: "问题".to_string(),
        context: "".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&MultiActionChannel, &request).await.unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0], SimpleAction::Think { content: "让我想想...".to_string() });
    assert_eq!(result[1], SimpleAction::Reply { content: "答案是42。".to_string() });
    assert_eq!(result[2], SimpleAction::Idle);
}

#[tokio::test]
async fn test_infer_two_field_variant() {
    struct TwoFieldChannel;

    impl LlmChannel for TwoFieldChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nsend_msg\nrecipient-{t}\nuser\ncontent-{t}\n你好世界！\n第二行。\nSimpleAction-end-{t}",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = SimpleRequest {
        message: "发消息".to_string(),
        context: "".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&TwoFieldChannel, &request).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], SimpleAction::SendMsg {
        recipient: "user".to_string(),
        content: "你好世界！\n第二行。".to_string(),
    });
}

#[tokio::test]
async fn test_infer_option_field() {
    struct OptionChannel {
        with_value: bool,
    }

    impl LlmChannel for OptionChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = if self.with_value {
                format!(
                    "SimpleAction-{t}\nwait\nseconds-{t}\n120\nSimpleAction-end-{t}",
                    t = token
                )
            } else {
                format!(
                    "SimpleAction-{t}\nwait\nSimpleAction-end-{t}",
                    t = token
                )
            };
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = SimpleRequest {
        message: "等待".to_string(),
        context: "".to_string(),
    };

    // With value
    let result: Vec<SimpleAction> = infer(&OptionChannel { with_value: true }, &request).await.unwrap();
    assert_eq!(result[0], SimpleAction::Wait { seconds: Some(120) });

    // Without value
    let result: Vec<SimpleAction> = infer(&OptionChannel { with_value: false }, &request).await.unwrap();
    assert_eq!(result[0], SimpleAction::Wait { seconds: None });
}

#[tokio::test]
async fn test_infer_streaming_chunks() {
    struct ChunkedChannel;

    impl LlmChannel for ChunkedChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let full = format!(
                "SimpleAction-{t}\nreply\ncontent-{t}\nHello World!\nSimpleAction-end-{t}",
                t = token
            );
            let chunks: Vec<String> = full.chars()
                .collect::<Vec<_>>()
                .chunks(5)
                .map(|c| c.iter().collect::<String>())
                .collect();
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = SimpleRequest {
        message: "hi".to_string(),
        context: "".to_string(),
    };

    let result: Vec<SimpleAction> = infer(&ChunkedChannel, &request).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], SimpleAction::Reply { content: "Hello World!".to_string() });
}

#[tokio::test]
async fn test_infer_channel_error() {
    let request = SimpleRequest {
        message: "test".to_string(),
        context: "".to_string(),
    };

    let result: Result<Vec<SimpleAction>, InferError> = infer(&ErrorChannel, &request).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("LLM service unavailable"));
}

#[tokio::test]
async fn test_infer_missing_end_marker() {
    struct NoEndChannel;

    impl LlmChannel for NoEndChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nreply\ncontent-{t}\n截断的响应",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = SimpleRequest {
        message: "test".to_string(),
        context: "".to_string(),
    };

    let result: Result<Vec<SimpleAction>, InferError> = infer(&NoEndChannel, &request).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("No valid element found") || err.message.contains("end marker"), "Error should mention missing element or end marker: {}", err);
}

#[tokio::test]
async fn test_infer_prompt_contains_schema() {
    let captured_prompt = Arc::new(Mutex::new(String::new()));
    let captured_clone = captured_prompt.clone();

    struct InspectChannel {
        prompt_holder: Arc<Mutex<String>>,
    }

    impl LlmChannel for InspectChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            *self.prompt_holder.lock().unwrap() = prompt;
            let response = format!(
                "SimpleAction-{t}\nidle\nSimpleAction-end-{t}",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = SimpleRequest {
        message: "检查prompt".to_string(),
        context: "测试上下文".to_string(),
    };

    let channel = InspectChannel { prompt_holder: captured_clone };
    let _result: Vec<SimpleAction> = infer(&channel, &request).await.unwrap();

    let prompt = captured_prompt.lock().unwrap().clone();

    // Prompt should contain request content
    assert!(prompt.contains("检查prompt"), "prompt should contain message");
    assert!(prompt.contains("测试上下文"), "prompt should contain context");

    // Prompt should contain schema
    assert!(prompt.contains("// 回复用户"), "prompt should contain schema");
    assert!(prompt.contains("// 什么都不做"), "prompt should contain idle schema");
    assert!(prompt.contains("SimpleAction-"), "prompt should contain element separator");
    assert!(prompt.contains("SimpleAction-end-"), "prompt should contain end marker");

    // Prompt should contain format instruction
    assert!(prompt.contains("输出规范"), "prompt should contain format section");
}

#[tokio::test]
async fn test_infer_prompt_has_no_token_leak() {
    let captured = CapturingChannel::new("");

    let request = SimpleRequest {
        message: "hello".to_string(),
        context: "world".to_string(),
    };

    // This will fail because the response is empty, but we can still inspect the prompt
    let _ = infer::<SimpleRequest, SimpleAction>(&captured, &request).await;

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

#[tokio::test]
async fn test_infer_with_on_text_receives_chunks() {
    struct OnTextChannel;

    impl LlmChannel for OnTextChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let chunks = vec![
                format!("SimpleAction-{}\n", token),
                "reply\n".to_string(),
                format!("content-{}\n", token),
                "on_text测试\n".to_string(),
                format!("SimpleAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = SimpleRequest { message: "test".to_string(), context: "ctx".to_string() };
    let collected = Arc::new(Mutex::new(Vec::<String>::new()));
    let collected_clone = collected.clone();

    let results = infer_with_on_text::<SimpleRequest, SimpleAction>(
        &OnTextChannel,
        &request,
        Some(Box::new(move |chunk: &str| {
            collected_clone.lock().unwrap().push(chunk.to_string());
        })),
        None,
        None,
        None,
    ).await.unwrap();

    // Verify parsing works
    assert_eq!(results.len(), 1);
    match &results[0] {
        SimpleAction::Reply { content } => assert_eq!(content, "on_text测试"),
        other => panic!("Expected Reply, got {:?}", other),
    }

    // Verify callback received all 4 chunks
    let chunks = collected.lock().unwrap();
    assert_eq!(chunks.len(), 5);
    assert!(chunks[0].starts_with("SimpleAction-"));
    assert_eq!(chunks[1], "reply\n");
    assert!(chunks[2].starts_with("content-"));
    assert_eq!(chunks[3], "on_text测试\n");
    assert!(chunks[4].starts_with("SimpleAction-end-"));
}

#[tokio::test]
async fn test_infer_with_on_text_none_equivalent() {
    struct NoneCallbackChannel;

    impl LlmChannel for NoneCallbackChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            let response = format!(
                "SimpleAction-{t}\nreply\ncontent-{t}\n无回调infer\nSimpleAction-end-{t}\n",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = SimpleRequest { message: "test".to_string(), context: "ctx".to_string() };
    let results = infer_with_on_text::<SimpleRequest, SimpleAction>(
        &NoneCallbackChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await.unwrap();

    assert_eq!(results.len(), 1);
    match &results[0] {
        SimpleAction::Reply { content } => assert_eq!(content, "无回调infer"),
        other => panic!("Expected Reply, got {:?}", other),
    }
}

// === Preamble rejection tests ===

#[tokio::test]
async fn test_infer_with_preamble_returns_error() {
    struct InferPreambleChannel;

    impl LlmChannel for InferPreambleChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            Ok(mock_channel_rx(vec![
                format!("Let me think about this...\nSimpleAction-{}\nidle\nSimpleAction-end-{}\n", token, token),
            ]))
        }
    }

    let request = SimpleRequest { message: "test".to_string(), context: "ctx".to_string() };

    let result = infer_with_on_text::<SimpleRequest, SimpleAction>(
        &InferPreambleChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await;

    assert!(result.is_err(), "preamble should cause error");
    let err = result.unwrap_err();
    assert!(err.message.contains("FORMAT VIOLATION"), "error should mention preamble: {}", err);
}

// === Cancel tests ===

#[tokio::test]
async fn test_infer_with_cancel() {
    use std::sync::atomic::AtomicBool;

    struct InferCancelChannel;
    impl LlmChannel for InferCancelChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token_from_prompt(&prompt, "SimpleAction");
            Ok(mock_channel_rx(vec![
                format!("SimpleAction-{}\ngreet\nhello\nSimpleAction-{}\ngreet\nworld\nSimpleAction-end-{}\n", token, token, token),
            ]))
        }
    }

    let request = SimpleRequest { message: "test".to_string(), context: "ctx".to_string() };
    let cancel = Arc::new(AtomicBool::new(true)); // Pre-cancelled

    let results = infer_with_on_text::<SimpleRequest, SimpleAction>(
        &InferCancelChannel,
        &request,
        None,
        None,
        Some(cancel),
        None,
    ).await.unwrap();

    assert_eq!(results.len(), 0); // Nothing parsed because cancelled immediately
}

