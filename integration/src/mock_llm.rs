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
//! ## Placeholders
//! - `{ACTION_TOKEN}` — replaced with the actual action token from system prompt
//! - `{FIND_ACTION_ID:keyword}` — replaced with the action_id of the block containing keyword

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::{get, post};
use axum::Router;

/// A scripted response with optional user line assertion.
///
/// - `response`: the LLM response text (with placeholders)
/// - `expected_user_contains`: if Some, assert the last user message contains this keyword
/// - `status_code`: if Some, return this HTTP status instead of 200 (for error simulation)
pub struct MockScript {
    pub response: String,
    pub expected_user_contains: Option<String>,
    pub status_code: Option<u16>,
}

impl MockScript {
    /// Create a script without user assertion.
    pub fn new(response: impl Into<String>) -> Self {
        Self { response: response.into(), expected_user_contains: None, status_code: None }
    }

    /// Create a script with user line assertion.
    pub fn with_user_assert(response: impl Into<String>, expected: impl Into<String>) -> Self {
        Self { response: response.into(), expected_user_contains: Some(expected.into()), status_code: None }
    }

    /// Create a script that returns an HTTP error status.
    pub fn with_error(status_code: u16) -> Self {
        Self { response: String::new(), expected_user_contains: None, status_code: Some(status_code) }
    }
}

/// A mock LLM HTTP server for testing.
pub struct MockLlmServer {
    /// The port the server is listening on.
    pub port: u16,
}

struct MockState {
    scripts: Mutex<VecDeque<MockScript>>,
    total_scripts: usize,
    /// Record of models received in each request (for channel rotation testing).
    request_models: Mutex<Vec<String>>,
}

impl MockLlmServer {
    /// Start the mock server with a list of scripted responses.
    ///
    /// Each script is a complete LLM response text with placeholders.
    /// Scripts are consumed in order, one per request.
    pub async fn start(scripts: Vec<MockScript>) -> Self {
        let total = scripts.len();
        let state = Arc::new(MockState {
            scripts: Mutex::new(VecDeque::from(scripts)),
            total_scripts: total,
            request_models: Mutex::new(Vec::new()),
        });

        let app = Router::new()
            .route("/v1/chat/completions", post(handle_completion))
            .route("/stats", get(handle_stats))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await
            .expect("Failed to bind mock LLM server");
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        Self { port }
    }

    /// Start the mock server on a specific port.
    pub async fn start_on_port(scripts: Vec<MockScript>, port: u16) -> Self {
        let total = scripts.len();
        let state = Arc::new(MockState {
            scripts: Mutex::new(VecDeque::from(scripts)),
            total_scripts: total,
            request_models: Mutex::new(Vec::new()),
        });

        let app = Router::new()
            .route("/v1/chat/completions", post(handle_completion))
            .route("/stats", get(handle_stats))
            .with_state(state);

        let addr = format!("127.0.0.1:{}", port);
        let listener = tokio::net::TcpListener::bind(&addr).await
            .unwrap_or_else(|e| panic!("Failed to bind mock LLM server on {}: {}", addr, e));
        let actual_port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        Self { port: actual_port }
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
/// Supports both streaming (SSE) and sync (JSON) responses,
/// determined by the `stream` field in the request body.
///
/// Compress requests (no action token) are auto-detected and return a fixed
/// response without consuming the script queue.
async fn handle_completion(
    State(state): State<Arc<MockState>>,
    body: String,
) -> axum::response::Response {
    let body_json: serde_json::Value = serde_json::from_str(&body)
        .expect("Mock LLM: invalid JSON request body");

    let is_stream = body_json["stream"].as_bool().unwrap_or(true);

    // Check action token FIRST — compress requests have no token
    let token = match try_extract_action_token(&body_json) {
        Some(t) => t,
        None => {
            // Compress request (no action token) — return fixed response, do NOT consume script
            eprintln!("[MOCK-LLM] No action token (compress request), returning fixed response");
            let compress_response = "Compressed: calculator app development, division feature, system logs review.";
            if is_stream {
                let sse_body = format_sse_response(compress_response);
                return axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(axum::body::Body::from(sse_body))
                    .unwrap();
            } else {
                let json_body = format_sync_response(compress_response);
                return axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(json_body))
                    .unwrap();
            }
        }
    };

    // Extract last user message content (full for assertion, truncated for logging)
    let last_user_content = body_json["messages"].as_array()
        .and_then(|msgs| msgs.iter().rev().find(|m| m["role"].as_str() == Some("user")))
        .and_then(|m| m["content"].as_str())
        .unwrap_or("<no user msg>");
    let last_user_preview: String = last_user_content.chars().take(60).collect();

    // Record the requested model for stats
    let request_model = body_json["model"].as_str().unwrap_or("unknown").to_string();
    state.request_models.lock().unwrap().push(request_model.clone());

    // Now consume the next scripted response
    let mock_script = {
        let mut scripts = state.scripts.lock().unwrap();
        let remaining = scripts.len();
        let script = scripts.pop_front();
        match &script {
            Some(s) => {
                let idx = state.total_scripts - remaining + 1;
                eprintln!("[MOCK-LLM] Consuming script #{} ({} remaining) | model: {} | user: {}", 
                    idx, remaining - 1, request_model, last_user_preview);
                // Assert user line if expected — search full content, not just preview
                if let Some(ref expected) = s.expected_user_contains {
                    assert!(last_user_content.contains(expected.as_str()),
                        "Mock script #{}: expected user message to contain '{}', preview: '{}'",
                        idx, expected, last_user_preview);
                }
            },
            None => {
                // All scripts consumed — return idle instead of panicking
                let user_preview: String = last_user_content.chars().take(60).collect();
                eprintln!("[MOCK-LLM] All {} scripts consumed, returning idle | model: {} | user: {}", 
                    state.total_scripts, request_model, user_preview);
                let idle_content = format!("{}-idle\n", token);
                let sse_body = format_sse_response(&idle_content);
                return axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(axum::body::Body::from(sse_body))
                    .unwrap();
            }
        }
        script.unwrap()
    };

    // If script specifies a non-200 status code, return error response
    if let Some(status_code) = mock_script.status_code {
        eprintln!("[MOCK-LLM] Returning error status {}", status_code);
        let error_body = serde_json::json!({
            "error": {
                "code": status_code.to_string(),
                "type": "mock_error",
                "message": format!("Mock LLM error {}", status_code)
            }
        }).to_string();
        return axum::response::Response::builder()
            .status(status_code)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(error_body))
            .unwrap();
    }

    // Collect all message content for action_id searching
    let all_content = collect_message_content(&body_json);

    // Replace placeholders
    let content = replace_placeholders(&mock_script.response, &token, &all_content);

    if is_stream {
        // SSE streaming response
        let sse_body = format_sse_response(&content);
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(axum::body::Body::from(sse_body))
            .unwrap()
    } else {
        // Sync JSON response
        let json_body = format_sync_response(&content);
        axum::response::Response::builder()
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json_body))
            .unwrap()
    }
}

/// Return request statistics (models seen so far).
async fn handle_stats(
    State(state): State<Arc<MockState>>,
) -> axum::response::Response {
    let models = state.request_models.lock().unwrap().clone();
    let body = serde_json::json!({ "models": models }).to_string();
    axum::response::Response::builder()
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap()
}

/// Replace all placeholders in the script.
fn replace_placeholders(script: &str, token: &str, all_content: &str) -> String {
    // Extract hex portion from token (e.g. "###ACTION_a1b2c3###" → "a1b2c3")
    let token_hex = token
        .strip_prefix("###ACTION_").and_then(|s| s.strip_suffix("###"))
        .unwrap_or("000000");

    let mut result = script.replace("{ACTION_TOKEN}", token);
    result = result.replace("{ACTION_TOKEN_HEX}", token_hex);

    // Replace {FIND_ACTION_ID:keyword} placeholders
    while let Some(start) = result.find("{FIND_ACTION_ID:") {
        let after = &result[start + "{FIND_ACTION_ID:".len()..];
        if let Some(end) = after.find('}') {
            let keyword = &after[..end];
            let action_id = find_action_id_by_keyword(all_content, keyword);
            let placeholder = format!("{{FIND_ACTION_ID:{}}}", keyword);
            result = result.replace(&placeholder, &action_id);
        } else {
            break;
        }
    }

    result
}

/// Find an action_id in the content by searching for a block containing the keyword.
///
/// Searches for `行为编号[ACTION_ID]` patterns and returns the action_id
/// of the block that contains the given keyword.
fn find_action_id_by_keyword(content: &str, keyword: &str) -> String {
    // Find all action block boundaries
    let block_start_marker = "行为编号[";
    let mut last_action_id = String::new();

    for (pos, _) in content.match_indices(block_start_marker) {
        let rest = &content[pos + block_start_marker.len()..];
        if let Some(end) = rest.find(']') {
            let id = &rest[..end];
            // Check if this is a "开始" marker
            let after_bracket = &rest[end + 1..];
            if after_bracket.starts_with("开始") {
                // Find the end of this block
                let end_marker = format!("行为编号[{}]结束", id);
                let block_text = if let Some(end_pos) = content[pos..].find(&end_marker) {
                    &content[pos..pos + end_pos]
                } else {
                    &content[pos..]
                };

                if block_text.contains(keyword) {
                    last_action_id = id.to_string();
                    // Don't break — find the LAST matching block
                }
            }
        }
    }

    if last_action_id.is_empty() {
        panic!("Mock LLM: could not find action block containing keyword '{}'", keyword);
    }

    last_action_id
}

/// Extract the action token from the request's system prompt.
fn try_extract_action_token(body: &serde_json::Value) -> Option<String> {
    let messages = body["messages"].as_array()?;

    for msg in messages {
        let content = msg["content"].as_str().unwrap_or_default();
        if let Some(start) = content.find("###ACTION_") {
            let after = &content[start + 10..];
            if let Some(end) = after.find("###") {
                let token_hex = &after[..end];
                return Some(format!("###ACTION_{}###", token_hex));
            }
        }
    }

    None
}

/// Collect all message content into a single string for searching.
fn collect_message_content(body: &serde_json::Value) -> String {
    let messages = body["messages"].as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    messages
}

/// Format content as an SSE response (single delta + DONE).
fn format_sse_response(content: &str) -> String {
    let content_json = serde_json::to_string(content).unwrap();
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n",
        content_json
    )
}

/// Format content as a sync JSON response.
fn format_sync_response(content: &str) -> String {
    serde_json::json!({
        "choices": [{"message": {"content": content}}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 50}
    }).to_string()
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
        assert_eq!(try_extract_action_token(&body), Some("###ACTION_a1b2c3###".to_string()));
    }

    #[test]
    fn test_format_sse_response() {
        let response = format_sse_response("hello\nworld");
        assert!(response.contains("data: "));
        assert!(response.contains("[DONE]"));
        assert!(response.contains("hello\\nworld"));
    }

    #[test]
    fn test_format_sync_response() {
        let response = format_sync_response("compressed history text");
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["choices"][0]["message"]["content"], "compressed history text");
        assert!(parsed["usage"]["prompt_tokens"].is_number());
    }

    #[test]
    fn test_find_action_id_by_keyword() {
        let content = concat!(
            "---------行为编号[20260301_aaa111]开始---------\n",
            "execute script: ls\n",
            "result: file1.txt\n",
            "---------行为编号[20260301_aaa111]结束---------\n",
            "\n",
            "---------行为编号[20260301_bbb222]开始---------\n",
            "execute script: echo [LOG] service healthy\n",
            "result: [LOG] service healthy\n",
            "---------行为编号[20260301_bbb222]结束---------\n",
        );
        assert_eq!(find_action_id_by_keyword(content, "[LOG]"), "20260301_bbb222");
    }

    #[test]
    fn test_replace_placeholders() {
        let script = "{ACTION_TOKEN}-forget\n{FIND_ACTION_ID:[LOG]}\nsummary text";
        let token = "###ACTION_abc123###";
        let content = concat!(
            "---------行为编号[20260301_xyz789]开始---------\n",
            "[LOG] service check\n",
            "---------行为编号[20260301_xyz789]结束---------\n",
        );
        let result = replace_placeholders(script, token, content);
        assert_eq!(result, "###ACTION_abc123###-forget\n20260301_xyz789\nsummary text");
    }
}

