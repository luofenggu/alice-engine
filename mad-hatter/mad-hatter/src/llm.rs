/// ToMarkdown: struct → markdown prompt for LLM input
pub trait ToMarkdown {
    fn to_markdown(&self) -> String {
        self.to_markdown_depth(2)
    }
    fn to_markdown_depth(&self, depth: usize) -> String;
    fn to_markdown_item(&self) -> String {
        String::new()
    }
}

/// FromMarkdown: parse LLM markdown output → structured data
pub trait FromMarkdown: Sized {
    fn schema_markdown(token: &str) -> String;
    fn from_markdown(text: &str, token: &str) -> Result<Vec<Self>, String>;
    fn type_name() -> &'static str;
}

/// Marker trait: only derived on structs via #[derive(ToMarkdown)]
/// Used to enforce struct-only constraint on infer/stream_infer input
pub trait StructInput {}

/// Marker trait: only derived on enums/structs via #[derive(FromMarkdown)]
/// Used to enforce struct-only constraint on infer/stream_infer output
pub trait StructOutput {}

/// LlmChannel: abstraction for LLM text streaming (async)
///
/// Returns an unbounded receiver that yields text chunks from the LLM stream.
/// Implementations should spawn an async task to read the stream and send chunks.
pub trait LlmChannel: Send + Sync {
    fn start_stream(&self, prompt: String) -> Result<tokio::sync::mpsc::UnboundedReceiver<String>, String>;
}

/// OpenAI-compatible LLM channel configuration
pub struct OpenAiChannel {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub max_tokens: Option<u32>,
}

impl OpenAiChannel {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            api_key: api_key.into(),
            max_tokens: None,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

/// SSE response types for OpenAI streaming
#[derive(serde::Deserialize)]
struct SseResponse {
    choices: Vec<SseChoice>,
}

#[derive(serde::Deserialize)]
struct SseChoice {
    delta: SseDelta,
}

#[derive(serde::Deserialize)]
struct SseDelta {
    content: Option<String>,
}

impl LlmChannel for OpenAiChannel {
    fn start_stream(&self, prompt: String) -> Result<tokio::sync::mpsc::UnboundedReceiver<String>, String> {
        let endpoint = self.endpoint.trim_end_matches('/').to_string();
        let url = if endpoint.ends_with("/v1/chat/completions") {
            endpoint
        } else {
            format!("{}/v1/chat/completions", endpoint)
        };

        let mut body = serde_json::json!({
            "model": &self.model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": true
        });
        if let Some(max_tokens) = self.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        let api_key = self.api_key.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        // Spawn async task to read SSE stream
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let response = match client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    let _ = tx.send(format!("\n[ERROR] HTTP request failed: {}", e));
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body_text = response.text().await.unwrap_or_default();
                let _ = tx.send(format!("\n[ERROR] HTTP {}: {}", status, body_text));
                return;
            }

            // Read SSE stream using bytes streaming
            use futures_util::StreamExt;
            let mut byte_stream = response.bytes_stream();
            let mut line_buf = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let text = String::from_utf8_lossy(&chunk);
                line_buf.push_str(&text);

                // Process complete lines
                while let Some(newline_pos) = line_buf.find('\n') {
                    let line = line_buf[..newline_pos].trim().to_string();
                    line_buf = line_buf[newline_pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }
                    if line == "data: [DONE]" {
                        return;
                    }
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(sse) = serde_json::from_str::<SseResponse>(data) {
                            if let Some(choice) = sse.choices.first() {
                                if let Some(content) = &choice.delta.content {
                                    if !content.is_empty() {
                                        if tx.send(content.clone()).is_err() {
                                            return; // receiver dropped
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }
}

/// Strip markdown code block wrapper (```...```) from LLM output
pub fn strip_code_block(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with("```") && trimmed.ends_with("```") {
        let inner = &trimmed[3..trimmed.len() - 3];
        // Skip optional language tag on first line
        let inner = if let Some(newline_pos) = inner.find('\n') {
            let first_line = &inner[..newline_pos];
            if first_line.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                &inner[newline_pos + 1..]
            } else {
                inner
            }
        } else {
            inner
        };
        inner.trim().to_string()
    } else {
        text.to_string()
    }
}

/// Generate a random token from nanosecond timestamp
pub fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:06x}", nanos % 0x1000000)
}

/// Build the full prompt for LLM inference
fn build_prompt<Req: ToMarkdown, Resp: FromMarkdown>(request: &Req, token: &str) -> String {
    let request_text = request.to_markdown();
    let schema = Resp::schema_markdown(token);
    format!(
        "{}\n\n### 输出规范 ###\n你必须严格按照以下格式输出，不要输出任何额外的解释或前言，直接从第一行开始按格式输出。\n\n{}",
        request_text,
        schema
    )
}

/// Stream inference: yields parsed elements one by one as they arrive (async)
///
/// Each time a complete element is detected in the stream (delimited by
/// `{TypeName}-{token}`), it is parsed and yielded immediately.
pub async fn stream_infer<Req, Resp>(
    channel: &dyn LlmChannel,
    request: &Req,
) -> Result<StreamInfer<Resp>, String>
where
    Req: ToMarkdown + StructInput,
    Resp: FromMarkdown + StructOutput,
{
    stream_infer_with_on_text(channel, request, None, None, None).await
}

/// Stream inference with optional text callback (async)
///
/// Like `stream_infer()`, but accepts an optional callback that receives
/// each raw text chunk as it arrives from the LLM stream. This enables
/// real-time logging or forwarding of the raw LLM output.
///
/// # Arguments
/// * `on_text` - Optional callback invoked with each raw chunk before parsing.
///   Pass `None` for no callback (equivalent to `stream_infer()`).
pub async fn stream_infer_with_on_text<Req, Resp>(
    channel: &dyn LlmChannel,
    request: &Req,
    on_text: Option<Box<dyn FnMut(&str) + Send>>,
    on_input: Option<Box<dyn FnOnce(&str) + Send>>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<StreamInfer<Resp>, String>
where
    Req: ToMarkdown + StructInput,
    Resp: FromMarkdown + StructOutput,
{
    let token = generate_token();
    let prompt = build_prompt::<Req, Resp>(request, &token);
    if let Some(cb) = on_input {
        cb(&prompt);
    }
    let receiver = channel.start_stream(prompt)?;

    Ok(StreamInfer {
        receiver,
        buffer: String::new(),
        token,
        type_name: Resp::type_name().to_string(),
        done: false,
        parsed_count: 0,
        on_text,
        cancel,
        _phantom: std::marker::PhantomData,
    })
}

/// Async stream that yields parsed elements from a streaming LLM response
pub struct StreamInfer<T: FromMarkdown> {
    receiver: tokio::sync::mpsc::UnboundedReceiver<String>,
    buffer: String,
    token: String,
    type_name: String,
    done: bool,
    parsed_count: usize,
    on_text: Option<Box<dyn FnMut(&str) + Send>>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: FromMarkdown> StreamInfer<T> {
    /// Get the token used for this inference session
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Async next: yields the next parsed element from the stream
    pub async fn next(&mut self) -> Option<Result<T, String>> {
        if self.done {
            return None;
        }

        let separator = format!("{}-{}", self.type_name, self.token);
        let end_marker = format!("{}-end-{}", self.type_name, self.token);

        loop {
            // Check cancel signal before processing
            if let Some(ref cancel) = self.cancel {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    self.done = true;
                    return None;
                }
            }

            // Priority 1: Try to extract a complete element between two separators
            let first_sep = self.buffer.find(&separator);
            if let Some(first_pos) = first_sep {
                if self.parsed_count == 0 && first_pos > 0 {
                    let before = self.buffer[..first_pos].trim();
                    if !before.is_empty() && !before.lines().all(|l| {
                        let t = l.trim();
                        t.is_empty() || t.starts_with("```")
                    }) {
                        self.done = true;
                        return Some(Err(format!(
                            "Unexpected content before first action separator: {}",
                            if before.len() > 200 { &before[..200] } else { before }
                        )));
                    }
                    self.buffer = self.buffer[first_pos..].to_string();
                    continue;
                }

                let after_first = first_pos + separator.len();
                let next_sep = self.buffer[after_first..].find(&separator);
                let next_end = self.buffer[after_first..].find(&end_marker);

                let boundary = match (next_sep, next_end) {
                    (Some(s), Some(e)) => {
                        if s <= e {
                            Some(("sep", after_first + s))
                        } else {
                            Some(("end", after_first + e))
                        }
                    }
                    (Some(s), None) => Some(("sep", after_first + s)),
                    (None, Some(e)) => Some(("end", after_first + e)),
                    (None, None) => None,
                };

                match boundary {
                    Some(("sep", boundary_pos)) => {
                        let element_text = self.buffer[after_first..boundary_pos].trim();
                        if !element_text.is_empty() {
                            let parse_input = format!("{}\n{}\n{}", separator, element_text, end_marker);
                            self.buffer = self.buffer[boundary_pos..].to_string();
                            match T::from_markdown(&parse_input, &self.token) {
                                Ok(mut items) if !items.is_empty() => {
                                    self.parsed_count += 1;
                                    return Some(Ok(items.remove(0)));
                                }
                                Ok(_) => { continue; }
                                Err(e) => return Some(Err(e)),
                            }
                        } else {
                            self.buffer = self.buffer[boundary_pos..].to_string();
                            continue;
                        }
                    }
                    Some(("end", boundary_pos)) => {
                        let element_text = self.buffer[after_first..boundary_pos].trim();
                        self.done = true;
                        if !element_text.is_empty() {
                            let parse_input = format!("{}\n{}\n{}", separator, element_text, end_marker);
                            match T::from_markdown(&parse_input, &self.token) {
                                Ok(mut items) if !items.is_empty() => {
                                    self.parsed_count += 1;
                                    return Some(Ok(items.remove(0)));
                                }
                                Ok(_) => return None,
                                Err(e) => return Some(Err(e)),
                            }
                        }
                        return None;
                    }
                    _ => {
                        // No boundary found yet — need more data
                    }
                }
            }

            // Need more data - read from async receiver
            match self.receiver.recv().await {
                Some(chunk) => {
                    // Check for error signals from the spawned task
                    if chunk.starts_with("\n[ERROR] ") {
                        self.done = true;
                        return Some(Err(chunk[9..].to_string()));
                    }
                    if let Some(ref mut callback) = self.on_text {
                        callback(&chunk);
                    }
                    self.buffer.push_str(&chunk);
                }
                None => {
                    // Channel closed — stream ended
                    self.done = true;
                    if self.buffer.contains(&end_marker) {
                        return None;
                    }
                    let tail: String = if self.buffer.len() > 200 {
                        format!("...{}", &self.buffer[self.buffer.len() - 200..])
                    } else {
                        self.buffer.clone()
                    };
                    return Some(Err(format!(
                        "[{}] Missing end marker '{}' after {} element(s). Buffer tail (up to 200 chars): {}",
                        self.type_name, end_marker, self.parsed_count, tail
                    )));
                }
            }
        }
    }
}

/// Full inference: collects all elements from stream (async)
///
/// Sends request to LLM and parses the complete response.
/// For streaming element-by-element processing, use `stream_infer()`.
pub async fn infer<Req, Resp>(
    channel: &dyn LlmChannel,
    request: &Req,
) -> Result<Vec<Resp>, String>
where
    Req: ToMarkdown + StructInput,
    Resp: FromMarkdown + StructOutput,
{
    infer_with_on_text(channel, request, None, None, None).await
}

/// Full inference with optional text callback (async)
///
/// Like `infer()`, but accepts an optional callback that receives each raw
/// text chunk as it arrives from the LLM stream.
pub async fn infer_with_on_text<Req, Resp>(
    channel: &dyn LlmChannel,
    request: &Req,
    on_text: Option<Box<dyn FnMut(&str) + Send>>,
    on_input: Option<Box<dyn FnOnce(&str) + Send>>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Vec<Resp>, String>
where
    Req: ToMarkdown + StructInput,
    Resp: FromMarkdown + StructOutput,
{
    let mut stream = stream_infer_with_on_text::<Req, Resp>(channel, request, on_text, on_input, cancel).await?;
    let mut results = Vec::new();
    while let Some(item) = stream.next().await {
        results.push(item?);
    }
    Ok(results)
}

