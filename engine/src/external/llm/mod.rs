//! # LLM Inference Module
//!
//! Handles communication with LLM providers (OpenRouter) via SSE streaming.
//! Implements the streaming action execution pipeline:
//! SSE chunks → text accumulation → action detection → channel dispatch
//!
//! @TRACE: INFER, STREAM
//!
//! ## Architecture
//!
//! ```text
//! [Inference Thread]              [Consumer Thread (React)]
//!   HTTP POST → SSE stream
//!   accumulate text
//!   detect action separator  ──→  channel.recv() → StreamItem
//!   parse action             ──→  execute action
//!   append to out.log
//!   ...until stream ends     ──→  StreamItem::Done
//! ```
//!
//! ## Key Types
//!
//! - [`LlmClient`] — HTTP client for OpenRouter API
//! - [`InferenceStream`] — Consumer handle for streaming actions
//! - [`StreamItem`] — Items flowing through the channel (Action/Done/Error)

pub mod stream;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use tracing::{error, info, warn};

use crate::inference::beat::BeatRequest;
use crate::inference::compress::CompressRequest;
use crate::inference::parse_actions;
/// Token usage info from LLM response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_cost: Option<f64>,
}

// Re-export key types
pub use stream::{InferenceStream, RecvResult, StreamItem};

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
///
/// Supports multiple channels (provider+key combos) with round-robin rotation.
/// On inference error, `advance_channel()` moves to the next channel so the
/// next retry uses a different provider.
///
/// @TRACE: INFER — `[INFER-{id}] Starting inference`
pub struct LlmClient {
    configs: Vec<LlmConfig>,
    channel_index: AtomicU64,
    http_client: reqwest::Client,
}

impl LlmClient {
    pub fn new(configs: Vec<LlmConfig>) -> Self {
        assert!(
            !configs.is_empty(),
            "LlmClient requires at least one config"
        );
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");
        Self {
            configs,
            channel_index: AtomicU64::new(0),
            http_client,
        }
    }

    /// Get the current channel's config (round-robin by channel_index).
    pub fn current_config(&self) -> LlmConfig {
        let idx = self.channel_index.load(Ordering::Relaxed) as usize % self.configs.len();
        self.configs[idx].clone()
    }

    /// Access primary channel's model (for hot-reload comparison).
    /// Replace all configs (hot-reload channels). Keeps channel_index unchanged.
    pub fn update_configs(&mut self, configs: Vec<LlmConfig>) {
        assert!(
            !configs.is_empty(),
            "LlmClient requires at least one config"
        );
        self.configs = configs;
    }

    /// Clone all configs (for passing to background tasks like RollTask).
    pub fn all_configs(&self) -> Vec<LlmConfig> {
        self.configs.clone()
    }

    /// Get display name for a channel index: 0 → "primary", N → "extraN".
    pub fn channel_display_name(idx: usize) -> String {
        if idx == 0 {
            "primary".to_string()
        } else {
            format!("extra{}", idx)
        }
    }

    /// Advance to the next channel (called on inference error).
    /// Returns (old_name, new_name) if rotation happened (multi-channel), None if single channel.
    pub fn advance_channel(&self) -> Option<(String, String)> {
        let old = self.channel_index.fetch_add(1, Ordering::Relaxed);
        let len = self.configs.len();
        if len > 1 {
            let old_idx = old as usize % len;
            let new_idx = (old + 1) as usize % len;
            let old_name = Self::channel_display_name(old_idx);
            let new_name = Self::channel_display_name(new_idx);
            info!(
                "[CHANNEL] Rotated from {} to {} (model={})",
                old_name, new_name, self.configs[new_idx].model
            );
            Some((old_name, new_name))
        } else {
            None
        }
    }

    /// Synchronous (non-streaming) LLM inference. Returns plain text response.
    ///
    /// Used for background tasks like history rolling where streaming/action
    /// parsing is not needed. Blocks the calling thread.
    ///
    /// @TRACE: INFER
    pub fn infer_sync(
        &self,
        messages: Vec<ChatMessage>,
        instance_id: &str,
    ) -> Result<(String, Option<UsageInfo>)> {
        let config = self.current_config();
        let http_client = self.http_client.clone();
        let instance_id = instance_id.to_string();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to create tokio runtime")?;

        rt.block_on(async {
            run_sync_inference(&config, &http_client, messages, &instance_id).await
        })
    }

    /// Streaming LLM inference that collects full output synchronously.
    ///
    /// Like infer_sync but uses SSE streaming internally — real-time log writing,
    /// chunk-level timeout (60s), no total timeout limit. Ideal for long-running
    /// tasks like knowledge capture where output is large.
    ///
    /// @TRACE: INFER
    pub fn infer_sync_streaming(
        &self,
        messages: Vec<ChatMessage>,
        instance_id: &str,
        log_path: Option<&Path>,
    ) -> Result<(String, Option<UsageInfo>)> {
        let config = self.current_config();
        let http_client = self.http_client.clone();
        let instance_id = instance_id.to_string();
        let log_path = log_path.map(|p| p.to_path_buf());

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to create tokio runtime")?;

        rt.block_on(async {
            run_streaming_collect(
                &config,
                &http_client,
                messages,
                &instance_id,
                log_path.as_deref(),
            )
            .await
        })
    }

    /// High-level beat inference: renders request, writes input log, starts streaming.
    ///
    /// Token is generated internally (self-contained). External code fills
    /// BeatRequest struct fields; this method handles token generation →
    /// render → log → API call → stream parse → action vector internally.
    ///
    /// @TRACE: INFER, STREAM
    pub fn infer_beat(
        &self,
        mut request: BeatRequest,
        log_path: PathBuf,
        log_dir: &Path,
        log_timestamp: &str,
        instance_id: String,
        infer_log_enabled: bool,
    ) -> InferenceStream {
        // Generate token internally (self-contained)
        let token: String = (0..6)
            .map(|_| format!("{:x}", rand::random::<u8>() % 16))
            .collect();
        request.action_token = token;

        let (system_prompt, user_prompt, _snapshot) = request.render();

        // Write input log if enabled
        if infer_log_enabled {
            let llm_policy = &crate::policy::EngineConfig::get().llm;
            let current = self.current_config();
            let (resolved_url, _) = llm_policy.resolve_model(&current.model);
            crate::logging::write_infer_input_log(
                log_dir,
                &instance_id,
                log_timestamp,
                &current.model,
                &resolved_url,
                &system_prompt,
                &user_prompt,
            );
        }

        let messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
        ];

        let separator_token = request.action_token.clone();
        self.infer_async(messages, &separator_token, log_path, instance_id)
    }

    /// High-level compress inference: renders request, calls sync API.
    ///
    /// External code fills CompressRequest struct fields; this method handles
    /// render → API call internally.
    ///
    /// @TRACE: INFER
    pub fn infer_compress(
        &self,
        request: CompressRequest,
        instance_id: &str,
    ) -> Result<(String, Option<UsageInfo>)> {
        let (system_msg, user_msg) = request.render();
        let messages = vec![
            ChatMessage::system(&system_msg),
            ChatMessage::user(&user_msg),
        ];
        self.infer_sync_streaming(messages, instance_id, None)
    }

    /// Start an async inference, returning a stream for consuming actions.
    ///
    /// Low-level method: accepts pre-built messages. Prefer `infer_beat()` which
    /// handles request rendering internally.
    ///
    /// @TRACE: INFER, STREAM
    fn infer_async(
        &self,
        messages: Vec<ChatMessage>,
        separator_token: &str,
        log_path: PathBuf,
        instance_id: String,
    ) -> InferenceStream {
        let (tx, rx) = mpsc::channel();

        let config = self.current_config();
        let http_client = self.http_client.clone();
        let separator = format!("###ACTION_{}###-", separator_token);
        let separator_for_parse = format!("###ACTION_{}", separator_token);
        let separator_token_owned = separator_token.to_string();
        let log_path_clone = log_path.clone();

        // Spawn inference thread
        std::thread::spawn(move || {
            // Create a tokio runtime for this thread (HTTP + SSE are async)
            let _rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create tokio runtime");

            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!(
                        "[INFER-{}] Failed to create tokio runtime: {}",
                        instance_id, e
                    );
                    let _ = tx.send(StreamItem::Error(format!(
                        "Failed to create tokio runtime: {}",
                        e
                    )));
                    return;
                }
            };

            rt.block_on(async {
                if let Err(e) = run_inference(
                    &config,
                    &http_client,
                    messages,
                    &separator,
                    &separator_for_parse,
                    &separator_token_owned,
                    &log_path_clone,
                    &instance_id,
                    &tx,
                )
                .await
                {
                    error!("[INFER-{}] Inference error: {}", instance_id, e);
                    let _ = tx.send(StreamItem::Error(format!("{}", e)));
                }
            });
        });

        InferenceStream::new(rx, log_path)
    }
}

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
// Inference execution (runs in background thread)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_inference(
    config: &LlmConfig,
    http_client: &reqwest::Client,
    messages: Vec<ChatMessage>,
    separator: &str,
    separator_for_parse: &str,
    separator_token: &str,
    log_path: &Path,
    instance_id: &str,
    tx: &mpsc::Sender<StreamItem>,
) -> Result<()> {
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
    use std::io::Write;

    let llm_policy = &crate::policy::EngineConfig::get().llm;
    let (api_url, model_id) = llm_policy.resolve_model(&config.model);

    info!(
        "[INFER-{}] Starting inference, model={}",
        instance_id, model_id
    );

    // Build request body
    let body = serde_json::json!({
        "model": model_id,
        "messages": messages,
        "max_tokens": config.max_tokens.unwrap_or(llm_policy.max_tokens),
        "temperature": config.temperature.unwrap_or(llm_policy.temperature),
        "stream": true,
    });

    // Send request
    let response = http_client
        .post(&api_url)
        .header(AUTHORIZATION, format!("Bearer {}", config.api_key))
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send inference request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("LLM API error {}: {}", status, body);
    }

    // Open log file for append
    let mut log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .context("Failed to open inference log file")?;

    // Read SSE stream
    let mut full_text = String::new();
    let mut last_parsed_pos = 0;
    let mut sse_buffer = String::new();
    let mut collected_usage: Option<UsageInfo> = None;

    use futures_util::StreamExt;
    let mut byte_stream = response.bytes_stream();

    while let Some(chunk_result) = {
        // Per-chunk timeout: 60 seconds without any data = stale connection
        match tokio::time::timeout(tokio::time::Duration::from_secs(60), byte_stream.next()).await {
            Ok(item) => item,
            Err(_) => {
                warn!("[INFER-{}] SSE stream timeout (60s no data)", instance_id);
                anyhow::bail!("SSE stream timeout: no data received for 60 seconds");
            }
        }
    } {
        let chunk = chunk_result.context("SSE stream read error")?;
        let chunk_str = String::from_utf8_lossy(&chunk);

        sse_buffer.push_str(&chunk_str);

        // Process complete SSE lines
        while let Some(line_end) = sse_buffer.find('\n') {
            let line = sse_buffer[..line_end].trim().to_string(); // safe: line_end from find newline
            sse_buffer = sse_buffer[line_end + 1..].to_string();

            if line.is_empty() || line == "data: [DONE]" || line == "data:[DONE]" {
                if line == "data: [DONE]" || line == "data:[DONE]" {
                    info!("[INFER-{}] SSE stream complete", instance_id);
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
                                // Append to full text
                                full_text.push_str(content);

                                // Append to log file (streaming)
                                let _ = log_file.write_all(content.as_bytes());
                                let _ = log_file.flush();
                            }
                        }
                        // Collect usage info (typically in last SSE chunk)
                        if let Some(usage) = &sse.usage {
                            collected_usage = Some(UsageInfo {
                                input_tokens: usage.prompt_tokens.unwrap_or(0),
                                output_tokens: usage.completion_tokens.unwrap_or(0),
                                total_cost: None, // OpenRouter doesn't report cost in SSE
                            });
                            info!(
                                "[INFER-{}] Usage: input={} output={}",
                                instance_id,
                                usage.prompt_tokens.unwrap_or(0),
                                usage.completion_tokens.unwrap_or(0)
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            "[INFER-{}] SSE parse warning: {} for data: {}",
                            instance_id, e, data
                        );
                    }
                }
            }

            // Check for complete actions in accumulated text
            // Look for separator patterns from last_parsed_pos
            while let Some(action_start) = full_text[last_parsed_pos..].find(separator) {
                let abs_start = last_parsed_pos + action_start;

                // Find the next separator or end of text
                let after_start = abs_start + separator.len();
                let next_sep = full_text[after_start..].find(separator);

                if let Some(next_offset) = next_sep {
                    // Complete action found between two separators
                    let action_text = &full_text[abs_start..after_start + next_offset];
                    info!(
                        "[INFER-{}] Stream action detected: {:?}",
                        instance_id,
                        crate::util::safe_truncate(action_text, 80)
                    );
                    let actions = parse_actions(action_text, separator_for_parse, separator_token)
                        .unwrap_or_default();
                    info!(
                        "[INFER-{}] Parsed {} actions from stream chunk",
                        instance_id,
                        actions.len()
                    );
                    for action in actions {
                        let _ = tx.send(StreamItem::Action(action));
                    }
                    last_parsed_pos = after_start + next_offset;
                } else {
                    // No next separator yet — wait for more data
                    break;
                }
            }
        }
    }

    // Parse any remaining actions after stream ends
    if last_parsed_pos < full_text.len() {
        let remaining = &full_text[last_parsed_pos..];
        info!(
            "[INFER-{}] Remaining text ({} chars): {:?}",
            instance_id,
            remaining.len(),
            crate::util::safe_truncate(remaining, 100)
        );
        if remaining.contains(separator) {
            info!("[INFER-{}] Parsing remaining actions", instance_id);
            let actions =
                parse_actions(remaining, separator_for_parse, separator_token).unwrap_or_default();
            info!(
                "[INFER-{}] Parsed {} remaining actions",
                instance_id,
                actions.len()
            );
            for action in actions {
                let _ = tx.send(StreamItem::Action(action));
            }
        } else {
            info!("[INFER-{}] No separator in remaining text", instance_id);
        }
    } else {
        info!(
            "[INFER-{}] No remaining text (last_parsed_pos={}, full_text.len={})",
            instance_id,
            last_parsed_pos,
            full_text.len()
        );
    }

    // Signal completion — all actions already sent via streaming
    let _ = tx.send(StreamItem::Done(Vec::new(), collected_usage));

    Ok(())
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
