//! Centralized human-readable messages for frontend/user feedback.
//!
//! Simple template functions have been migrated to `crate::bindings::i18n`.
//! This file retains only functions with complex logic (branching, loops, pattern matching).

use crate::bindings::i18n;

// === RPC operation feedback ===

/// Compare old and new settings, return human-readable description of changes.
/// Returns None if nothing changed.
pub fn describe_settings_change(
    old: &crate::persist::Settings,
    new: &crate::persist::Settings,
) -> Option<String> {
    let mut changes = Vec::new();

    if old.name != new.name {
        changes.push(format!("name: {}", new.name.as_deref().unwrap_or("")));
    }
    if old.avatar != new.avatar {
        changes.push(format!("avatar: {}", new.avatar.as_deref().unwrap_or("")));
    }
    if old.color != new.color {
        changes.push(format!("color: {}", new.color.as_deref().unwrap_or("")));
    }
    if old.api_key != new.api_key {
        let key = new.api_key.as_deref().unwrap_or("");
        let suffix_len = 4.min(key.len());
        changes.push(format!("api_key: ...{}", &key[key.len() - suffix_len..]));
    }
    if old.model != new.model {
        changes.push(format!("model: {}", new.model.as_deref().unwrap_or("")));
    }
    if old.privileged != new.privileged {
        changes.push(format!("privileged: {:?}", new.privileged));
    }
    if old.max_beats != new.max_beats {
        changes.push(format!("max_beats: {:?}", new.max_beats));
    }
    if old.session_blocks_limit != new.session_blocks_limit {
        changes.push(format!(
            "session_blocks_limit: {:?}",
            new.session_blocks_limit
        ));
    }
    if old.session_block_kb != new.session_block_kb {
        changes.push(format!("session_block_kb: {:?}", new.session_block_kb));
    }
    if old.history_kb != new.history_kb {
        changes.push(format!("history_kb: {:?}", new.history_kb));
    }
    if old.safety_max_consecutive_beats != new.safety_max_consecutive_beats {
        changes.push(format!(
            "safety_max_consecutive_beats: {:?}",
            new.safety_max_consecutive_beats
        ));
    }
    if old.safety_cooldown_secs != new.safety_cooldown_secs {
        changes.push(format!(
            "safety_cooldown_secs: {:?}",
            new.safety_cooldown_secs
        ));
    }
    if old.temperature != new.temperature {
        changes.push(format!("temperature: {:?}", new.temperature));
    }
    if old.max_tokens != new.max_tokens {
        changes.push(format!("max_tokens: {:?}", new.max_tokens));
    }
    if old.host != new.host {
        changes.push(format!("host: {}", new.host.as_deref().unwrap_or("")));
    }
    if old.shell_env != new.shell_env {
        changes.push(format!(
            "shell_env: {}",
            new.shell_env.as_deref().unwrap_or("")
        ));
    }

    if changes.is_empty() {
        None
    } else {
        Some(changes.join(", "))
    }
}

// === LLM error translation ===

/// Translate raw LLM error strings into user-friendly messages.
///
/// Recognizes:
/// - `"LLM API error {status} ...:"` → status-code based mapping
/// - Known non-API errors (timeout, connection, stream)
/// - Fallback: return original string unchanged
pub fn humanize_llm_error(raw: &str) -> String {
    // Pattern: "LLM API error {status} {reason}: {body}"
    if let Some(rest) = raw.strip_prefix("LLM API error ") {
        // Extract status code (first token before space)
        if let Some(status_str) = rest.split_whitespace().next() {
            if let Ok(status) = status_str.parse::<u16>() {
                return match status {
                    401 => i18n::llm_err_invalid_key(),
                    402 => i18n::llm_err_quota_exceeded(),
                    403 => i18n::llm_err_access_denied(),
                    429 => i18n::llm_err_rate_limited(),
                    500 | 502 | 503 => i18n::llm_err_service_unavailable(),
                    _ => i18n::llm_err_unknown_status(status),
                };
            }
        }
    }

    // Known non-API errors
    if raw.contains("SSE stream timeout") {
        return i18n::llm_err_sse_timeout();
    }
    if raw.contains("Failed to send") && raw.contains("request") {
        return i18n::llm_err_connection_failed();
    }
    if raw.contains("SSE stream read error") {
        return i18n::llm_err_stream_read_error();
    }
    if raw.contains("Failed to create tokio runtime") {
        return i18n::llm_err_runtime_failed();
    }

    // Format/parse errors from FromMarkdown (preamble violation)
    if raw.contains("FORMAT VIOLATION") {
        return i18n::llm_err_format_violation();
    }
    if raw.contains("Missing end marker") {
        return i18n::llm_err_missing_end_marker();
    }

    // Fallback: return as-is
    raw.to_string()
}

/// Format beat error for user notification.
/// This is a thin wrapper — inference errors are already formatted by
/// `inference_error`/`inference_disconnected`, so we just pass through.
pub fn beat_error(e: &anyhow::Error) -> String {
    format!("{}", e)
}

/// Format inference error with optional channel rotation info.
pub fn inference_error(
    error: &str,
    backoff_secs: u64,
    rotation: Option<&(String, String)>,
) -> String {
    let friendly = humanize_llm_error(error);
    match rotation {
        Some((from, to)) => i18n::inference_error_with_rotation(&friendly, from, to, backoff_secs),
        None => i18n::inference_error_no_rotation(&friendly, backoff_secs),
    }
}

/// Format inference disconnection with optional channel rotation info.
pub fn inference_disconnected(backoff_secs: u64, rotation: Option<&(String, String)>) -> String {
    match rotation {
        Some((from, to)) => {
            i18n::inference_disconnected_with_rotation(from, to, backoff_secs)
        }
        None => i18n::inference_disconnected_no_rotation(backoff_secs),
    }
}

/// Format a chat message for prompt rendering.
pub fn chat_message(
    role: &str,
    sender: &str,
    self_id: &str,
    timestamp: &str,
    content: &str,
) -> String {
    let prefix = match role {
        "user" => "user".to_string(),
        "system" => "system".to_string(),
        _ => {
            if sender == self_id {
                i18n::chat_prefix_self(self_id)
            } else {
                i18n::chat_prefix_agent(sender)
            }
        }
    };
    i18n::chat_message_fmt(&prefix, timestamp, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    // === humanize_llm_error tests ===

    #[test]
    fn test_humanize_401() {
        let raw = r#"LLM API error 401 Unauthorized: {"error":"invalid api key"}"#;
        assert_eq!(humanize_llm_error(raw), "API密钥无效");
    }

    #[test]
    fn test_humanize_402() {
        let raw = r#"LLM API error 402 Payment Required: {"error":{"code":"402","type":"quote_exceeded","message":"You have reached your subscription quota limit..."}}"#;
        assert_eq!(humanize_llm_error(raw), "通道额度用完了");
    }

    #[test]
    fn test_humanize_403() {
        let raw = r#"LLM API error 403 Forbidden: {"error":"access denied"}"#;
        assert_eq!(humanize_llm_error(raw), "API访问被拒绝");
    }

    #[test]
    fn test_humanize_429() {
        let raw = r#"LLM API error 429 Too Many Requests: {"code": 429, "reason": "RATE_LIMIT_EXCEEDED"}"#;
        assert_eq!(humanize_llm_error(raw), "请求太频繁");
    }

    #[test]
    fn test_humanize_500() {
        let raw = "LLM API error 500 Internal Server Error: something broke";
        assert_eq!(humanize_llm_error(raw), "LLM服务暂时不可用");
    }

    #[test]
    fn test_humanize_502() {
        let raw = "LLM API error 502 Bad Gateway: ";
        assert_eq!(humanize_llm_error(raw), "LLM服务暂时不可用");
    }

    #[test]
    fn test_humanize_503() {
        let raw = "LLM API error 503 Service Unavailable: overloaded";
        assert_eq!(humanize_llm_error(raw), "LLM服务暂时不可用");
    }

    #[test]
    fn test_humanize_unknown_status() {
        let raw = "LLM API error 418 I'm a Teapot: {}";
        assert_eq!(humanize_llm_error(raw), "LLM服务返回错误(418)");
    }

    #[test]
    fn test_humanize_sse_timeout() {
        let raw = "SSE stream timeout: no data received for 60 seconds";
        assert_eq!(humanize_llm_error(raw), "连接超时（60秒无响应）");
    }

    #[test]
    fn test_humanize_connection_failure() {
        let raw = "Failed to send sync inference request";
        assert_eq!(humanize_llm_error(raw), "无法连接LLM服务");
    }

    #[test]
    fn test_humanize_stream_read_error() {
        let raw = "SSE stream read error";
        assert_eq!(humanize_llm_error(raw), "数据流读取异常");
    }

    #[test]
    fn test_humanize_tokio_runtime() {
        let raw = "Failed to create tokio runtime: some OS error";
        assert_eq!(humanize_llm_error(raw), "内部运行时创建失败");
    }

    #[test]
    fn test_humanize_fallback() {
        let raw = "some completely unknown error message";
        assert_eq!(
            humanize_llm_error(raw),
            "some completely unknown error message"
        );
    }

    #[test]
    fn test_humanize_empty_string() {
        assert_eq!(humanize_llm_error(""), "");
    }

    // === beat_error tests ===

    #[test]
    fn test_beat_error_passthrough() {
        let err = anyhow::anyhow!("⚡ 通道额度用完了（primary），已轮换到 extra1，30秒后重试");
        let result = beat_error(&err);
        assert_eq!(
            result,
            "⚡ 通道额度用完了（primary），已轮换到 extra1，30秒后重试"
        );
    }

    // === inference_error tests ===

    #[test]
    fn test_inference_error_with_rotation() {
        let rotation = ("primary".to_string(), "extra1".to_string());
        let result = inference_error(
            r#"LLM API error 402 Payment Required: {"error":"quota"}"#,
            30,
            Some(&rotation),
        );
        assert_eq!(
            result,
            "⚡ 通道额度用完了（primary），已轮换到 extra1，30秒后重试"
        );
    }

    #[test]
    fn test_inference_error_without_rotation() {
        let result = inference_error(
            "LLM API error 429 Too Many Requests: rate limited",
            15,
            None,
        );
        assert_eq!(result, "⚡ 请求太频繁，15秒后重试");
    }

    #[test]
    fn test_inference_error_unknown_error() {
        let result = inference_error("weird error", 10, None);
        assert_eq!(result, "⚡ weird error，10秒后重试");
    }

    // === inference_disconnected tests ===

    #[test]
    fn test_inference_disconnected_with_rotation() {
        let rotation = ("extra2".to_string(), "primary".to_string());
        let result = inference_disconnected(20, Some(&rotation));
        assert_eq!(
            result,
            "⚡ 推理连接异常断开（extra2），已轮换到 primary，20秒后重试"
        );
    }

    #[test]
    fn test_inference_disconnected_without_rotation() {
        let result = inference_disconnected(10, None);
        assert_eq!(result, "⚡ 推理连接异常断开，10秒后重试");
    }
}