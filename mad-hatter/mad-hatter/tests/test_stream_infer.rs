use mad_hatter::{ToMarkdown, FromMarkdown, LlmChannel, stream_infer, stream_infer_with_on_text};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;

// --- Test types ---

#[derive(ToMarkdown)]
/// @render Test request
struct StreamReq {
    /// @render prompt
    prompt: String,
}

#[derive(FromMarkdown, PartialEq, Debug)]
enum StreamAction {
    /// @render 思考
    Think {
        /// @render 内容
        content: String,
    },
    /// @render 回复
    Reply {
        /// @render 内容
        content: String,
    },
    /// @render 等待
    Idle,
}

// Helper: extract token from prompt
fn extract_token(prompt: &str, type_name: &str) -> String {
    let marker = format!("{}-", type_name);
    for line in prompt.lines() {
        if let Some(pos) = line.find(&marker) {
            let after = &line[pos + marker.len()..];
            // Token is hex chars until non-hex
            let token: String = after.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if !token.is_empty() {
                return token;
            }
        }
    }
    panic!("Token not found in prompt for type {}", type_name);
}

// Helper: create a mock channel that sends chunks via unbounded_channel
fn mock_channel_rx(chunks: Vec<String>) -> UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for chunk in chunks {
        tx.send(chunk).ok();
    }
    // tx dropped here → channel closed
    rx
}

// --- Tests ---

#[tokio::test]
async fn test_stream_infer_yields_elements_one_by_one() {
    struct ChunkedChannel;

    impl LlmChannel for ChunkedChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let chunks = vec![
                format!("StreamAction-{}\n", token),
                "think\n".to_string(),
                "正在思考...\n".to_string(),
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "回答完毕。\n".to_string(),
                format!("StreamAction-{}\n", token),
                "idle\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer::<StreamReq, StreamAction>(&ChunkedChannel, &request).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result.unwrap());
    }
    assert_eq!(results.len(), 3);
    assert_eq!(results[0], StreamAction::Think { content: "正在思考...".to_string() });
    assert_eq!(results[1], StreamAction::Reply { content: "回答完毕。".to_string() });
    assert_eq!(results[2], StreamAction::Idle);
}

#[tokio::test]
async fn test_stream_infer_single_chunk() {
    // All data arrives in one chunk (like mock channel)
    struct OneChunkChannel;

    impl LlmChannel for OneChunkChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "StreamAction-{t}\nthink\n深度思考中\nStreamAction-{t}\nreply\n最终答案\nStreamAction-end-{t}",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer::<StreamReq, StreamAction>(&OneChunkChannel, &request).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result.unwrap());
    }
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], StreamAction::Think { content: "深度思考中".to_string() });
    assert_eq!(results[1], StreamAction::Reply { content: "最终答案".to_string() });
}

#[tokio::test]
async fn test_stream_infer_missing_end_marker() {
    struct TruncatedChannel;

    impl LlmChannel for TruncatedChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "StreamAction-{t}\nthink\n被截断了",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer::<StreamReq, StreamAction>(&TruncatedChannel, &request).await.unwrap();

    // First next() should return the truncation error
    let result = stream.next().await;
    assert!(result.is_some());
    let err = result.unwrap();
    assert!(err.is_err());
    let err_msg = err.unwrap_err();
    assert!(err_msg.contains("No valid element found") || err_msg.contains("end marker"), "Error should mention missing element or end marker: {}", err_msg);
}

#[tokio::test]
async fn test_stream_infer_channel_error() {
    struct FailChannel;

    impl LlmChannel for FailChannel {
        fn start_stream(&self, _prompt: String) -> Result<UnboundedReceiver<String>, String> {
            Err("Connection refused".to_string())
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let result = stream_infer::<StreamReq, StreamAction>(&FailChannel, &request).await;
    assert!(result.is_err());
    match result {
        Err(e) => assert!(e.contains("Connection refused"), "unexpected error: {}", e),
        Ok(_) => panic!("expected error but got Ok"),
    }
}

#[tokio::test]
async fn test_stream_infer_token_accessible() {
    struct TokenChannel;

    impl LlmChannel for TokenChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!("StreamAction-{t}\nidle\nStreamAction-end-{t}", t = token);
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer::<StreamReq, StreamAction>(&TokenChannel, &request).await.unwrap();

    // Token should be accessible
    let token = stream.token().to_string();
    assert!(!token.is_empty());
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn test_stream_infer_gradual_chunks() {
    // Simulate very small chunks (like real SSE streaming)
    struct GradualChannel;

    impl LlmChannel for GradualChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let full = format!(
                "StreamAction-{t}\nreply\nHello World\nStreamAction-end-{t}",
                t = token
            );
            // Split into 2-char chunks
            let chunks: Vec<String> = full.as_bytes()
                .chunks(2)
                .map(|c| String::from_utf8_lossy(c).to_string())
                .collect();
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer::<StreamReq, StreamAction>(&GradualChannel, &request).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result.unwrap());
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], StreamAction::Reply { content: "Hello World".to_string() });
}

#[tokio::test]
async fn test_stream_infer_empty_response() {
    // Only separator + end marker, no actual elements
    struct EmptyChannel;

    impl LlmChannel for EmptyChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!("StreamAction-end-{t}", t = token);
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer::<StreamReq, StreamAction>(&EmptyChannel, &request).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    assert_eq!(results.len(), 0);
}

#[tokio::test]
async fn test_stream_infer_with_code_block_wrapper() {
    // LLM wraps output in ```
    struct CodeBlockChannel;

    impl LlmChannel for CodeBlockChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "```\nStreamAction-{t}\nreply\n代码块内的回复\nStreamAction-end-{t}\n```",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer::<StreamReq, StreamAction>(&CodeBlockChannel, &request).await.unwrap();
    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    // The ``` lines don't match separator format, so they're ignored as noise
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap(), &StreamAction::Reply { content: "代码块内的回复".to_string() });
}

#[tokio::test]
async fn test_stream_infer_on_text_receives_all_chunks() {
    // Channel that returns response in multiple chunks
    struct ChunkedCallbackChannel;

    impl LlmChannel for ChunkedCallbackChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let chunks = vec![
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "回调测试内容\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let collected = Arc::new(Mutex::new(Vec::<String>::new()));
    let collected_clone = collected.clone();

    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &ChunkedCallbackChannel,
        &request,
        Some(Box::new(move |chunk: &str| {
            collected_clone.lock().unwrap().push(chunk.to_string());
        })),
        None,
        None,
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }

    // Verify parsing works
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap(), &StreamAction::Reply { content: "回调测试内容".to_string() });

    // Verify callback received all 4 chunks
    let chunks = collected.lock().unwrap();
    assert_eq!(chunks.len(), 4);
    assert!(chunks[0].starts_with("StreamAction-"));
    assert_eq!(chunks[1], "reply\n");
    assert_eq!(chunks[2], "回调测试内容\n");
    assert!(chunks[3].starts_with("StreamAction-end-"));
}

#[tokio::test]
async fn test_stream_infer_on_text_none_equivalent() {
    // Same as regular stream_infer
    struct SimpleCallbackChannel;

    impl LlmChannel for SimpleCallbackChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "StreamAction-{t}\nreply\n无回调\nStreamAction-end-{t}\n",
                t = token
            );
            Ok(mock_channel_rx(vec![response]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &SimpleCallbackChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap(), &StreamAction::Reply { content: "无回调".to_string() });
}

// === preamble rejection tests ===

#[tokio::test]
async fn test_stream_infer_preamble_returns_error() {
    // Preamble before first action should cause an error (zero tolerance)
    struct PreambleChannel;

    impl LlmChannel for PreambleChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(mock_channel_rx(vec![
                format!("I need to think about this carefully.\nLet me analyze the situation.\nStreamAction-{}\nidle\nStreamAction-end-{}\n", token, token),
            ]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &PreambleChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await.unwrap();

    let result = stream.next().await;
    assert!(result.is_some(), "should return an error result");
    let err = result.unwrap();
    assert!(err.is_err(), "preamble should cause error");
    assert!(err.unwrap_err().contains("FORMAT VIOLATION"), "error should mention unexpected content");

    // Stream should be done after preamble error
    assert!(stream.next().await.is_none(), "stream should be done after preamble error");
}

#[tokio::test]
async fn test_stream_infer_no_preamble_works_normally() {
    // When there's no preamble, everything works normally
    struct NoPreambleChannel;

    impl LlmChannel for NoPreambleChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(mock_channel_rx(vec![
                format!("StreamAction-{}\nidle\nStreamAction-end-{}\n", token, token),
            ]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &NoPreambleChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
}

#[tokio::test]
async fn test_stream_infer_whitespace_only_preamble_ok() {
    // Whitespace-only content before first separator should be silently ignored (not an error)
    struct WhitespacePreambleChannel;

    impl LlmChannel for WhitespacePreambleChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(mock_channel_rx(vec![
                format!("\n  \n```\nStreamAction-{}\nidle\nStreamAction-end-{}\n", token, token),
            ]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &WhitespacePreambleChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    assert_eq!(results.len(), 1, "whitespace/backtick-only preamble should be ignored");
    assert!(results[0].is_ok());
}

// === Cancel tests ===

#[tokio::test]
async fn test_stream_infer_cancel_mid_stream() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SlowChannel;
    impl LlmChannel for SlowChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let chunks = vec![
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "first action\n".to_string(),
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "second action\n".to_string(),
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "third action\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();

    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &SlowChannel,
        &request,
        None,
        None,
        Some(cancel_clone),
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(item) = stream.next().await {
        results.push(item.unwrap());
        // Cancel after first action
        cancel.store(true, Ordering::Relaxed);
    }

    // Should have gotten first action then stopped
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], StreamAction::Reply { content: "first action".to_string() });
}

#[tokio::test]
async fn test_stream_infer_cancel_none_no_effect() {
    // Verify that cancel=None doesn't affect normal operation
    let request = StreamReq { prompt: "test".to_string() };

    struct NormalChannel;
    impl LlmChannel for NormalChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(mock_channel_rx(vec![
                format!("StreamAction-{}\nreply\nworks\nStreamAction-end-{}\n", token, token),
            ]))
        }
    }

    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &NormalChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].as_ref().unwrap(), &StreamAction::Reply { content: "works".to_string() });
}

#[tokio::test]
async fn test_stream_infer_cancel_before_start() {
    use std::sync::atomic::AtomicBool;

    struct AnyChannel;
    impl LlmChannel for AnyChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(mock_channel_rx(vec![
                format!("StreamAction-{}\nreply\nshould not see\nStreamAction-end-{}\n", token, token),
            ]))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let cancel = Arc::new(AtomicBool::new(true)); // Already cancelled

    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &AnyChannel,
        &request,
        None,
        None,
        Some(cancel),
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result);
    }
    assert_eq!(results.len(), 0); // Nothing parsed
}

// --- Thinking tests ---

#[tokio::test]
async fn test_stream_thinking_then_action() {
    struct ThinkingChannel;

    impl LlmChannel for ThinkingChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let chunks = vec![
format!("<think-{}>\nI need to think about this carefully.\n</think-{}>\n", token, token),
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "hello world\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let collected_thinking = Arc::new(Mutex::new(Vec::<String>::new()));
    let thinking_clone = collected_thinking.clone();

    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &ThinkingChannel,
        &request,
        None,
        None,
        None,
        Some(Box::new(move |thinking: &str| {
            thinking_clone.lock().unwrap().push(thinking.to_string());
        })),
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result.unwrap());
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], StreamAction::Reply { content: "hello world".to_string() });

    let thinking = collected_thinking.lock().unwrap();
    assert_eq!(thinking.len(), 1);
    assert!(thinking[0].contains("think about this carefully"));
}

#[tokio::test]
async fn test_stream_thinking_no_callback() {
    struct ThinkingNoCallbackChannel;

    impl LlmChannel for ThinkingNoCallbackChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let chunks = vec![
format!("<think-{}>\nSome internal reasoning\n</think-{}>\n", token, token),
                format!("StreamAction-{}\n", token),
                "idle\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    // on_thinking = None — thinking should be silently discarded
    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &ThinkingNoCallbackChannel,
        &request,
        None,
        None,
        None,
        None,
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result.unwrap());
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], StreamAction::Idle);
}

#[tokio::test]
async fn test_stream_thinking_with_leading_whitespace() {
    struct ThinkingWhitespaceChannel;

    impl LlmChannel for ThinkingWhitespaceChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let chunks = vec![
                "\n\n".to_string(),
format!("<think-{}>\nthinking with leading whitespace\n</think-{}>\n", token, token),
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "result\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let collected_thinking = Arc::new(Mutex::new(Vec::<String>::new()));
    let thinking_clone = collected_thinking.clone();

    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &ThinkingWhitespaceChannel,
        &request,
        None,
        None,
        None,
        Some(Box::new(move |thinking: &str| {
            thinking_clone.lock().unwrap().push(thinking.to_string());
        })),
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result.unwrap());
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], StreamAction::Reply { content: "result".to_string() });

    let thinking = collected_thinking.lock().unwrap();
    assert_eq!(thinking.len(), 1);
    assert!(thinking[0].contains("leading whitespace"));
}

#[tokio::test]
async fn test_stream_thinking_gradual_chunks() {
    struct ThinkingGradualChannel;

    impl LlmChannel for ThinkingGradualChannel {
        fn start_stream(&self, prompt: String) -> Result<UnboundedReceiver<String>, String> {
            let token = extract_token(&prompt, "StreamAction");
            // Thinking arrives in small chunks
            let think_open = format!("<think-{}>", token);
                let think_close = format!("</think-{}>", token);
                let mid = think_open.len() / 2;
                let chunks = vec![
                think_open[..mid].to_string(),
                format!("{}\nLine 1\n", &think_open[mid..]),
                format!("Line 2\n{}\n", think_close),
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "done\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(mock_channel_rx(chunks))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let collected_thinking = Arc::new(Mutex::new(Vec::<String>::new()));
    let thinking_clone = collected_thinking.clone();

    let mut stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &ThinkingGradualChannel,
        &request,
        None,
        None,
        None,
        Some(Box::new(move |thinking: &str| {
            thinking_clone.lock().unwrap().push(thinking.to_string());
        })),
    ).await.unwrap();

    let mut results = Vec::new();
    while let Some(result) = stream.next().await {
        results.push(result.unwrap());
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], StreamAction::Reply { content: "done".to_string() });

    let thinking = collected_thinking.lock().unwrap();
    assert_eq!(thinking.len(), 1);
    assert!(thinking[0].contains("Line 1"));
    assert!(thinking[0].contains("Line 2"));
}

