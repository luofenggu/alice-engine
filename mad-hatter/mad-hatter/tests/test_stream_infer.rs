use mad_hatter::{ToMarkdown, FromMarkdown, LlmChannel, stream_infer};

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
    assert!(err.unwrap_err().contains("Missing end marker"));
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