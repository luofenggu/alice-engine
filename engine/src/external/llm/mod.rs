//! # LLM Inference Module
//!
//! Provides HTTP client and async inference functions for vision API.
//! Channel management has moved to per-instance (`Alice` struct in `core/mod.rs`).
//!
//! @TRACE: INFER

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{info, warn};
/// Token usage info from LLM response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_cost: Option<f64>,
}



// ---------------------------------------------------------------------------
// LLM Client Configuration
// ---------------------------------------------------------------------------

/// Configuration for the LLM provider.
///
/// @TRACE: INFER
#[derive(Debug, Clone, Default)]
pub struct LlmConfig {
    /// Raw model string (e.g. "openrouter@anthropic/claude-opus-4.6")
    /// Provider resolution and API URL lookup happen at call time in external/llm.
    pub model: String,
    /// API key for authentication
    pub api_key: String,
    /// Optional temperature override (falls back to engine.toml default)
    pub temperature: Option<f64>,
    /// Optional max_tokens override (falls back to engine.toml default)
    pub max_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// SSE Message Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SseChoice {
    delta: SseDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseResponse {
    choices: Vec<SseChoice>,
    usage: Option<SseUsage>,
}

#[derive(Debug, Deserialize)]
struct SseUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// Chat Message
// ---------------------------------------------------------------------------

/// Role in a chat conversation (maps to LLM API role field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A message in the chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// LLM Client
// ---------------------------------------------------------------------------

/// HTTP client for LLM inference via OpenRouter.


// ---------------------------------------------------------------------------
// Sync inference (non-streaming, for background tasks)
// ---------------------------------------------------------------------------

/// Non-streaming response types.
#[derive(Debug, Deserialize)]
struct SyncChoice {
    message: SyncMessage,
}

#[derive(Debug, Deserialize)]
struct SyncMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SyncResponse {
    choices: Vec<SyncChoice>,
    usage: Option<SseUsage>,
}

/// Run a vision (multimodal) inference call. Returns (text, usage).
///
/// Builds an OpenAI-compatible multimodal request with image_url content.
/// The image_url can be a regular URL or a data: URI (base64-encoded).
pub(crate) async fn run_vision_inference(
    config: &LlmConfig,
    http_client: &reqwest::Client,
    prompt: &str,
    image_url: &str,
    instance_id: &str,
) -> Result<(String, Option<UsageInfo>)> {
    use base64::Engine as _;
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};

    let llm_policy = &crate::policy::EngineConfig::get().llm;
    let (api_url, model_id) = llm_policy.resolve_model(&config.model);

    info!(
        "[VISION-{}] Starting vision inference, model={}",
        instance_id, model_id
    );

    // Ensure image is a data URI (base64-encoded).
    // If it's a regular URL, download and convert to base64.
    let data_uri = if image_url.starts_with("data:") {
        image_url.to_string()
    } else {
        info!("[VISION-{}] Downloading image from URL", instance_id);
        let img_response = http_client
            .get(image_url)
            .send()
            .await
            .context("Failed to download image")?;

        let header_ct = img_response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let img_bytes = img_response
            .bytes()
            .await
            .context("Failed to read image bytes")?;

        // Determine media type: prefer URL extension, fall back to header, default to image/png
        let media_type = infer_image_media_type(image_url, &header_ct);

        let b64 = base64::engine::general_purpose::STANDARD.encode(&img_bytes);
        format!("data:{};base64,{}", media_type, b64)
    };

    let body = serde_json::json!({
        "model": model_id,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": data_uri}}
            ]
        }],
        "max_tokens": config.max_tokens.unwrap_or(llm_policy.max_tokens),
        "temperature": config.temperature.unwrap_or(llm_policy.temperature),
        "stream": false,
    });

    let response = http_client
        .post(&api_url)
        .header(AUTHORIZATION, format!("Bearer {}", config.api_key))
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send vision inference request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Vision API error {}: {}", status, body);
    }

    let resp: SyncResponse = response
        .json()
        .await
        .context("Failed to parse vision inference response")?;

    let text = resp
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();

    let usage = resp.usage.map(|u| UsageInfo {
        input_tokens: u.prompt_tokens.unwrap_or(0),
        output_tokens: u.completion_tokens.unwrap_or(0),
        total_cost: None,
    });

    info!(
        "[VISION-{}] Complete, {} chars output",
        instance_id,
        text.len()
    );

    Ok((text, usage))
}

/// Run a non-streaming inference call. Returns (text, usage).
/// Infer image media type from URL extension, falling back to HTTP header, then default.
fn infer_image_media_type(url: &str, header_content_type: &str) -> &'static str {
    // Try URL path extension first
    if let Some(path) = url.split('?').next() {
        let lower = path.to_lowercase();
        if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            return "image/jpeg";
        } else if lower.ends_with(".png") {
            return "image/png";
        } else if lower.ends_with(".gif") {
            return "image/gif";
        } else if lower.ends_with(".webp") {
            return "image/webp";
        }
    }
    // Fall back to HTTP content-type header if it's an image type
    let ct = header_content_type.split(';').next().unwrap_or("").trim();
    if ct == "image/jpeg" {
        "image/jpeg"
    } else if ct == "image/png" {
        "image/png"
    } else if ct == "image/gif" {
        "image/gif"
    } else if ct == "image/webp" {
        "image/webp"
    } else {
        "image/png"
    }
}

pub(crate) async fn run_sync_inference(
    config: &LlmConfig,
    http_client: &reqwest::Client,
    messages: Vec<ChatMessage>,
    instance_id: &str,
) -> Result<(String, Option<UsageInfo>)> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};

    let llm_policy = &crate::policy::EngineConfig::get().llm;
    let (api_url, model_id) = llm_policy.resolve_model(&config.model);

    info!(
        "[INFER-SYNC-{}] Starting sync inference, model={}",
        instance_id, model_id
    );

    let body = serde_json::json!({
        "model": model_id,
        "messages": messages,
        "max_tokens": config.max_tokens.unwrap_or(llm_policy.max_tokens),
        "temperature": config.temperature.unwrap_or(llm_policy.temperature),
        "stream": false,
    });

    let response = http_client
        .post(&api_url)
        .header(AUTHORIZATION, format!("Bearer {}", config.api_key))
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send sync inference request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("LLM API error {}: {}", status, body);
    }

    let resp: SyncResponse = response
        .json()
        .await
        .context("Failed to parse sync inference response")?;

    let text = resp
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default();

    let usage = resp.usage.map(|u| UsageInfo {
        input_tokens: u.prompt_tokens.unwrap_or(0),
        output_tokens: u.completion_tokens.unwrap_or(0),
        total_cost: None,
    });

    info!(
        "[INFER-SYNC-{}] Complete, {} chars output",
        instance_id,
        text.len()
    );

    Ok((text, usage))
}

// ---------------------------------------------------------------------------
// Streaming collect inference (for capture and other long-running tasks)
// ---------------------------------------------------------------------------

/// Streaming inference that collects full output. Real-time log writing,
/// chunk-level timeout (60s no data), no total timeout.
///
/// @TRACE: INFER — `[INFER-STREAM-COLLECT-{id}]`
async fn run_streaming_collect(
    config: &LlmConfig,
    http_client: &reqwest::Client,
    messages: Vec<ChatMessage>,
    instance_id: &str,
    log_path: Option<&Path>,
) -> Result<(String, Option<UsageInfo>)> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
    use std::io::Write;

    let llm_policy = &crate::policy::EngineConfig::get().llm;
    let (api_url, model_id) = llm_policy.resolve_model(&config.model);

    info!(
        "[INFER-STREAM-COLLECT-{}] Starting streaming collect, model={}",
        instance_id, model_id
    );

    let body = serde_json::json!({
        "model": model_id,
        "messages": messages,
        "max_tokens": config.max_tokens.unwrap_or(llm_policy.max_tokens),
        "temperature": config.temperature.unwrap_or(llm_policy.temperature),
        "stream": true,
    });

    let response = http_client
        .post(&api_url)
        .header(AUTHORIZATION, format!("Bearer {}", config.api_key))
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send streaming collect request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("LLM API error {}: {}", status, body);
    }

    // Open log file if provided
    let mut log_file = log_path.and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });

    let mut full_text = String::new();
    let mut sse_buffer = String::new();
    let mut collected_usage: Option<UsageInfo> = None;

    use futures_util::StreamExt;
    let mut byte_stream = response.bytes_stream();

    while let Some(chunk_result) = {
        // Per-chunk timeout: 60 seconds without any data
        match tokio::time::timeout(tokio::time::Duration::from_secs(60), byte_stream.next()).await {
            Ok(item) => item,
            Err(_) => {
                warn!(
                    "[INFER-STREAM-COLLECT-{}] Stream timeout (60s no data)",
                    instance_id
                );
                anyhow::bail!("Streaming collect timeout: no data received for 60 seconds");
            }
        }
    } {
        let chunk = chunk_result.context("SSE stream read error")?;
        let chunk_str = String::from_utf8_lossy(&chunk);

        sse_buffer.push_str(&chunk_str);

        // Process complete SSE lines
        while let Some(line_end) = sse_buffer.find('\n') {
            let line = sse_buffer[..line_end].trim().to_string();
            sse_buffer = sse_buffer[line_end + 1..].to_string();

            if line.is_empty() || line == "data: [DONE]" || line == "data:[DONE]" {
                if line == "data: [DONE]" || line == "data:[DONE]" {
                    info!("[INFER-STREAM-COLLECT-{}] SSE stream complete", instance_id);
                }
                continue;
            }

            if let Some(data) = line
                .strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
            {
                match serde_json::from_str::<SseResponse>(data) {
                    Ok(sse) => {
                        for choice in &sse.choices {
                            if let Some(content) = &choice.delta.content {
                                full_text.push_str(content);

                                // Real-time log writing
                                if let Some(ref mut f) = log_file {
                                    let _ = f.write_all(content.as_bytes());
                                    let _ = f.flush();
                                }
                            }
                        }
                        if let Some(usage) = &sse.usage {
                            collected_usage = Some(UsageInfo {
                                input_tokens: usage.prompt_tokens.unwrap_or(0),
                                output_tokens: usage.completion_tokens.unwrap_or(0),
                                total_cost: None,
                            });
                        }
                    }
                    Err(e) => {
                        warn!(
                            "[INFER-STREAM-COLLECT-{}] SSE parse warning: {} for data: {}",
                            instance_id, e, data
                        );
                    }
                }
            }
        }
    }

    let usage_info = if let Some(ref u) = collected_usage {
        format!(" (tokens: {}+{})", u.input_tokens, u.output_tokens)
    } else {
        String::new()
    };

    info!(
        "[INFER-STREAM-COLLECT-{}] Complete, {} chars{}",
        instance_id,
        full_text.len(),
        usage_info
    );

    Ok((full_text, collected_usage))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_constructors() {
        let sys = ChatMessage::system("You are helpful");
        assert_eq!(sys.role, Role::System);
        assert_eq!(sys.content, "You are helpful");

        let user = ChatMessage::user("Hello");
        assert_eq!(user.role, Role::User);

        let asst = ChatMessage::assistant("Hi there");
        assert_eq!(asst.role, Role::Assistant);
    }

    #[test]
    fn test_llm_config() {
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            max_tokens: None,
        };
        assert_eq!(config.model, "test-model");
        assert_eq!(config.api_key, "test-key");
        assert!(config.temperature.is_none());
        assert!(config.max_tokens.is_none());
    }

    #[test]
    fn test_sse_response_parse() {
        let json = r#"{"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}"#;
        let resp: SseResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].delta.content.as_deref(), Some("hello"));
    }

    #[test]
    fn test_sse_response_empty_delta() {
        let json = r#"{"choices":[{"delta":{},"finish_reason":null}]}"#;
        let resp: SseResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices[0].delta.content.is_none());
    }

    #[test]
    fn test_sse_response_finish() {
        let json = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let resp: SseResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
    }
}
