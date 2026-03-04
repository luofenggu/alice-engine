//! Zero-intrusion Mock LLM Server
//!
//! Starts a local HTTP server that impersonates an OpenAI-compatible LLM provider.
//! The engine's existing provider URL configuration points to this server,
//! requiring zero code changes in the engine.
//!
//! ## Usage
//! ```ignore
//! let mock = MockLlmServer::start(vec![
//!     "response for first request".into(),
//!     "response for second request".into(),
//! ]).await;
//! // Set ALICE_DEFAULT_MODEL to mock.model_string()
//! ```
//!
//! Scripts use `{ACTION_TOKEN}` as placeholder — replaced with the actual
//! action token extracted from the system prompt at request time.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::Router;

/// A mock LLM HTTP server for testing.
pub struct MockLlmServer {
    /// The port the server is listening on.
    pub port: u16,
}

struct MockState {
    scripts: Mutex<VecDeque<String>>,
}

impl MockLlmServer {
    /// Start the mock server with a list of scripted responses.
    ///
    /// Each script is a complete LLM response text with `{ACTION_TOKEN}` placeholders.
    /// Scripts are consumed in order, one per request.
    pub async fn start(scripts: Vec<String>) -> Self {
        let state = Arc::new(MockState {
            scripts: Mutex::new(VecDeque::from(scripts)),
        });

        let app = Router::new()
            .route("/v1/chat/completions", post(handle_completion))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await
            .expect("Failed to bind mock LLM server");
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        Self { port }
    }

    /// Returns the model string for engine configuration.
    ///
    /// Format: `http://127.0.0.1:{port}/v1/chat/completions@test-model`
    /// The engine's `resolve_model` treats the part before `@` as the provider URL.
    pub fn model_string(&self) -> String {
        format!("http://127.0.0.1:{}/v1/chat/completions@test-model", self.port)
    }
}

/// Handle a chat completion request.
///
/// 1. Extract action token from system prompt in request body
/// 2. Pop next scripted response
/// 3. Replace `{ACTION_TOKEN}` with actual token
/// 4. Return as SSE stream
async fn handle_completion(
    State(state): State<Arc<MockState>>,
    body: String,
) -> axum::response::Response {
    let body_json: serde_json::Value = serde_json::from_str(&body)
        .expect("Mock LLM: invalid JSON request body");

    // Extract action token from system prompt
    let token = extract_action_token(&body_json);

    // Get next scripted response
    let script = state.scripts.lock().unwrap().pop_front()
        .expect("Mock LLM: no more scripted responses available");

    // Replace placeholder with actual token
    let content = script.replace("{ACTION_TOKEN}", &token);

    // Format as SSE response
    let sse_body = format_sse_response(&content);

    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(axum::body::Body::from(sse_body))
        .unwrap()
}

/// Extract the action token (e.g. `###ACTION_abc123###`) from the request's system prompt.
fn extract_action_token(body: &serde_json::Value) -> String {
    let messages = body["messages"].as_array()
        .expect("Mock LLM: request missing 'messages' array");

    for msg in messages {
        let content = msg["content"].as_str().unwrap_or_default();
        // Look for ###ACTION_{hex}###
        if let Some(start) = content.find("###ACTION_") {
            let after = &content[start + 10..];
            if let Some(end) = after.find("###") {
                let token_hex = &after[..end];
                return format!("###ACTION_{}###", token_hex);
            }
        }
    }

    panic!("Mock LLM: could not find action token in system prompt");
}

/// Format content as an SSE response (single delta + DONE).
fn format_sse_response(content: &str) -> String {
    // Serialize content with proper JSON escaping
    let content_json = serde_json::to_string(content).unwrap();

    // Build SSE: one data event with the full content, then DONE
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n",
        content_json
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_action_token() {
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "You are an agent. Use ###ACTION_a1b2c3### to act."},
                {"role": "user", "content": "Hello"}
            ]
        });
        assert_eq!(extract_action_token(&body), "###ACTION_a1b2c3###");
    }

    #[test]
    fn test_format_sse_response() {
        let response = format_sse_response("hello\nworld");
        assert!(response.contains("data: "));
        assert!(response.contains("[DONE]"));
        assert!(response.contains("hello\\nworld")); // JSON escaped
    }
}
