use mad_hatter::{ToMarkdown, FromMarkdown, LlmChannel, stream_infer, stream_infer_with_on_text};
use std::cell::RefCell;
use std::rc::Rc;

// --- Test types ---

#[derive(ToMarkdown)]
/// Test request
struct StreamReq {
    /// prompt
    prompt: String,
}

#[derive(FromMarkdown, PartialEq, Debug)]
enum StreamAction {
    /// 思考
    Think {
        /// 内容
        content: String,
    },
    /// 回复
    Reply {
        /// 内容
        content: String,
    },
    /// 等待
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

// --- Tests ---

#[test]
fn test_stream_infer_yields_elements_one_by_one() {
    struct ChunkedChannel;

    impl LlmChannel for ChunkedChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            // Simulate streaming: each chunk arrives separately
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
            Ok(Box::new(chunks.into_iter()))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer::<StreamReq, StreamAction>(&ChunkedChannel, &request).unwrap();

    let results: Vec<StreamAction> = stream.map(|r| r.unwrap()).collect();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0], StreamAction::Think { content: "正在思考...".to_string() });
    assert_eq!(results[1], StreamAction::Reply { content: "回答完毕。".to_string() });
    assert_eq!(results[2], StreamAction::Idle);
}

#[test]
fn test_stream_infer_single_chunk() {
    // All data arrives in one chunk (like mock channel)
    struct OneChunkChannel;

    impl LlmChannel for OneChunkChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "StreamAction-{t}\nthink\n深度思考中\nStreamAction-{t}\nreply\n最终答案\nStreamAction-end-{t}",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer::<StreamReq, StreamAction>(&OneChunkChannel, &request).unwrap();

    let results: Vec<StreamAction> = stream.map(|r| r.unwrap()).collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], StreamAction::Think { content: "深度思考中".to_string() });
    assert_eq!(results[1], StreamAction::Reply { content: "最终答案".to_string() });
}

#[test]
fn test_stream_infer_missing_end_marker() {
    struct TruncatedChannel;

    impl LlmChannel for TruncatedChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "StreamAction-{t}\nthink\n被截断了",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let mut stream = stream_infer::<StreamReq, StreamAction>(&TruncatedChannel, &request).unwrap();

    // First next() should return the truncation error
    let result = stream.next();
    assert!(result.is_some());
    let err = result.unwrap();
    assert!(err.is_err());
    let err_msg = err.unwrap_err();
    assert!(err_msg.contains("No valid element found") || err_msg.contains("end marker"), "Error should mention missing element or end marker: {}", err_msg);
}

#[test]
fn test_stream_infer_channel_error() {
    struct FailChannel;

    impl LlmChannel for FailChannel {
        fn infer_stream(&self, _prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            Err("Connection refused".to_string())
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let result = stream_infer::<StreamReq, StreamAction>(&FailChannel, &request);
    assert!(result.is_err());
    match result {
        Err(e) => assert!(e.contains("Connection refused"), "unexpected error: {}", e),
        Ok(_) => panic!("expected error but got Ok"),
    }
}

#[test]
fn test_stream_infer_token_accessible() {
    struct TokenChannel;

    impl LlmChannel for TokenChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!("StreamAction-{t}\nidle\nStreamAction-end-{t}", t = token);
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer::<StreamReq, StreamAction>(&TokenChannel, &request).unwrap();

    // Token should be accessible
    let token = stream.token().to_string();
    assert!(!token.is_empty());
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_stream_infer_gradual_chunks() {
    // Simulate very small chunks (like real SSE streaming)
    struct GradualChannel;

    impl LlmChannel for GradualChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
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
            Ok(Box::new(chunks.into_iter()))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer::<StreamReq, StreamAction>(&GradualChannel, &request).unwrap();

    let results: Vec<StreamAction> = stream.map(|r| r.unwrap()).collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], StreamAction::Reply { content: "Hello World".to_string() });
}

#[test]
fn test_stream_infer_empty_response() {
    // Only separator + end marker, no actual elements
    struct EmptyChannel;

    impl LlmChannel for EmptyChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!("StreamAction-end-{t}", t = token);
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer::<StreamReq, StreamAction>(&EmptyChannel, &request).unwrap();

    let results: Vec<Result<StreamAction, String>> = stream.collect();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_stream_infer_with_code_block_wrapper() {
    // LLM wraps output in ```
    struct CodeBlockChannel;

    impl LlmChannel for CodeBlockChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "```\nStreamAction-{t}\nreply\n代码块内的回复\nStreamAction-end-{t}\n```",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    // Note: stream_infer doesn't strip code blocks (that's from_markdown's job via strip_code_block)
    // But from_markdown is called per-element with synthetic input, so code block stripping
    // happens at a different level. This test verifies the behavior.
    let stream = stream_infer::<StreamReq, StreamAction>(&CodeBlockChannel, &request).unwrap();
    let results: Vec<Result<StreamAction, String>> = stream.collect();
    // The ``` lines don't match separator format, so they're ignored as noise
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap(), &StreamAction::Reply { content: "代码块内的回复".to_string() });
}
#[test]
fn test_stream_infer_on_text_receives_all_chunks() {
    // Channel that returns response in multiple chunks
    struct ChunkedCallbackChannel;

    impl LlmChannel for ChunkedCallbackChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let chunks = vec![
                format!("StreamAction-{}\n", token),
                "reply\n".to_string(),
                "回调测试内容\n".to_string(),
                format!("StreamAction-end-{}\n", token),
            ];
            Ok(Box::new(chunks.into_iter()))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let collected = Rc::new(RefCell::new(Vec::<String>::new()));
    let collected_clone = collected.clone();

    let stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &ChunkedCallbackChannel,
        &request,
        Some(Box::new(move |chunk: &str| {
            collected_clone.borrow_mut().push(chunk.to_string());
        })),
        None,
        None,
    ).unwrap();

    let results: Vec<Result<StreamAction, String>> = stream.collect();

    // Verify parsing works
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap(), &StreamAction::Reply { content: "回调测试内容".to_string() });

    // Verify callback received all 4 chunks
    let chunks = collected.borrow();
    assert_eq!(chunks.len(), 4);
    assert!(chunks[0].starts_with("StreamAction-"));
    assert_eq!(chunks[1], "reply\n");
    assert_eq!(chunks[2], "回调测试内容\n");
    assert!(chunks[3].starts_with("StreamAction-end-"));
}

#[test]
fn test_stream_infer_on_text_none_equivalent() {
    // Same as regular stream_infer
    struct SimpleCallbackChannel;

    impl LlmChannel for SimpleCallbackChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            let response = format!(
                "StreamAction-{t}\nreply\n无回调\nStreamAction-end-{t}\n",
                t = token
            );
            Ok(Box::new(std::iter::once(response)))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &SimpleCallbackChannel,
        &request,
        None,
        None,
        None,
    ).unwrap();

    let results: Vec<Result<StreamAction, String>> = stream.collect();
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
    assert_eq!(results[0].as_ref().unwrap(), &StreamAction::Reply { content: "无回调".to_string() });
}

// === on_preamble tests ===

#[test]
fn test_stream_infer_preamble_callback_receives_content() {
    struct PreambleChannel;

    impl LlmChannel for PreambleChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(Box::new(vec![
                format!("I need to think about this carefully.\nLet me analyze the situation.\nStreamAction-{}\nidle\nStreamAction-end-{}\n", token, token),
            ].into_iter()))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let preamble_text = Rc::new(RefCell::new(String::new()));
    let preamble_clone = preamble_text.clone();

    let stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &PreambleChannel,
        &request,
        None,
        None,
        Some(Box::new(move |text: &str| {
            *preamble_clone.borrow_mut() = text.to_string();
        })),
    ).unwrap();

    let results: Vec<Result<StreamAction, String>> = stream.collect();
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());

    let preamble = preamble_text.borrow();
    assert!(preamble.contains("I need to think about this carefully"), "preamble should contain LLM waste text: {}", preamble);
    assert!(preamble.contains("Let me analyze the situation"), "preamble should contain all waste lines: {}", preamble);
}

#[test]
fn test_stream_infer_preamble_none_no_error() {
    // When on_preamble is None and there's preamble text, should still not error
    // (preamble is silently discarded)
    struct PreambleNoneChannel;

    impl LlmChannel for PreambleNoneChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(Box::new(vec![
                format!("Some thinking here\nStreamAction-{}\nidle\nStreamAction-end-{}\n", token, token),
            ].into_iter()))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &PreambleNoneChannel,
        &request,
        None,
        None,
        None,
    ).unwrap();

    let results: Vec<Result<StreamAction, String>> = stream.collect();
    assert_eq!(results.len(), 1, "should parse 1 action despite preamble: {:?}", results);
    assert!(results[0].is_ok(), "should succeed: {:?}", results[0]);
}

#[test]
fn test_stream_infer_no_preamble_no_callback() {
    // When there's no preamble, on_preamble callback should not be called
    struct NoPreambleChannel;

    impl LlmChannel for NoPreambleChannel {
        fn infer_stream(&self, prompt: String) -> Result<Box<dyn Iterator<Item = String> + '_>, String> {
            let token = extract_token(&prompt, "StreamAction");
            Ok(Box::new(vec![
                format!("StreamAction-{}\nidle\nStreamAction-end-{}\n", token, token),
            ].into_iter()))
        }
    }

    let request = StreamReq { prompt: "test".to_string() };
    let was_called = Rc::new(RefCell::new(false));
    let was_called_clone = was_called.clone();

    let stream = stream_infer_with_on_text::<StreamReq, StreamAction>(
        &NoPreambleChannel,
        &request,
        None,
        None,
        Some(Box::new(move |_text: &str| {
            *was_called_clone.borrow_mut() = true;
        })),
    ).unwrap();

    let results: Vec<Result<StreamAction, String>> = stream.collect();
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());
    assert!(!*was_called.borrow(), "on_preamble should NOT be called when there's no preamble");
}
