//! Centralized human-readable messages for frontend/user feedback.
//!
//! All natural-language strings that are returned to users (via ActionResult,
//! notify_anomaly, or prompt hints) should be defined here.
//! Guardian exempts this file — it serves as the "message catalog".

// === RPC operation feedback ===

pub fn empty_message() -> &'static str {
    "Empty message"
}

pub fn instance_deleted(id: &str) -> String {
    format!("Deleted: {}", id)
}

pub fn no_valid_fields() -> &'static str {
    "No valid fields to update"
}

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

// === Engine anomaly notifications ===

pub fn safety_valve_triggered(consecutive: u32, cooldown: u64) -> String {
    format!(
        "连续工作保护已启动：已连续推理{}次，暂停{}秒让系统休息一下。这是正常的保护机制，不是故障。稍等片刻即可自动恢复。如需调整触发阈值，可在 Settings 中修改 safety_max_consecutive_beats。",
        consecutive, cooldown
    )
}

pub fn beat_limit_reached(current: u32, max: u32) -> String {
    format!("推理次数已达上限（{}/{}），实例已停止推理。", current, max)
}

pub fn disk_space_low(avail_mb: u64) -> String {
    format!(
        "磁盘空间不足：仅剩 {}MB 可用。请清理磁盘空间，否则可能导致数据损坏。",
        avail_mb
    )
}

// === Knowledge capacity hints (shown in prompt) ===

pub fn knowledge_capacity_ok(size: usize) -> String {
    format!("知识: {}/51200字符 🟢", size)
}

pub fn knowledge_capacity_warning(size: usize) -> String {
    format!("知识: {}/51200字符 ⚠️ 知识接近上限，summary时请精简", size)
}

pub fn knowledge_capacity_critical(size: usize) -> String {
    format!(
        "知识: {}/51200字符 🔴 知识超出推荐容量，建议与用户商量裂变",
        size
    )
}

pub fn binary_file_description(name: &str, size: u64) -> String {
    format!("[Binary file: {}, {} bytes]", name, size)
}

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
                    401 => "API密钥无效".to_string(),
                    402 => "通道额度用完了".to_string(),
                    403 => "API访问被拒绝".to_string(),
                    429 => "请求太频繁".to_string(),
                    500 | 502 | 503 => "LLM服务暂时不可用".to_string(),
                    _ => format!("LLM服务返回错误({})", status),
                };
            }
        }
    }

    // Known non-API errors
    if raw.contains("SSE stream timeout") {
        return "连接超时（60秒无响应）".to_string();
    }
    if raw.contains("Failed to send") && raw.contains("request") {
        return "无法连接LLM服务".to_string();
    }
    if raw.contains("SSE stream read error") {
        return "数据流读取异常".to_string();
    }
    if raw.contains("Failed to create tokio runtime") {
        return "内部运行时创建失败".to_string();
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
        Some((from, to)) => format!(
            "⚡ {}（{}），已轮换到 {}，{}秒后重试",
            friendly, from, to, backoff_secs
        ),
        None => format!("⚡ {}，{}秒后重试", friendly, backoff_secs),
    }
}

/// Format inference disconnection with optional channel rotation info.
pub fn inference_disconnected(backoff_secs: u64, rotation: Option<&(String, String)>) -> String {
    match rotation {
        Some((from, to)) => format!(
            "⚡ 推理连接异常断开（{}），已轮换到 {}，{}秒后重试",
            from, to, backoff_secs
        ),
        None => format!("⚡ 推理连接异常断开，{}秒后重试", backoff_secs),
    }
}

// === Sequence defense (hallucination defense) ===

pub fn sequence_reject_after_blocking(instance_id: &str, action: &str) -> String {
    format!(
        "[SEQUENCE-{}] Non-blocking action '{}' after blocking action — aborting inference",
        instance_id, action
    )
}

pub fn sequence_reject_after_idle(instance_id: &str, action: &str) -> String {
    format!(
        "[SEQUENCE-{}] Action '{}' after idle — zero tolerance, aborting inference",
        instance_id, action
    )
}

// === Roll / Capture result messages ===

pub fn roll_deleted_residual(block: &str) -> String {
    format!("记忆整理：发现已压缩的残留记录，已清理（{}）", block)
}

pub fn roll_deleted_empty(block: &str) -> String {
    format!("记忆整理：发现空记录，已清理（{}）", block)
}

pub fn roll_llm_empty() -> &'static str {
    "记忆整理中断：压缩结果为空，已跳过本次整理"
}

pub fn empty_placeholder() -> &'static str {
    "(空)"
}

pub fn global_settings_updated() -> String {
    "Global settings updated".to_string()
}

pub fn session_summary(summary: &str) -> String {
    format!("[总结] {}", summary)
}

pub fn memory_over_limit(kb: usize) -> String {
    format!(
        "\n⚠️ prompt总量已达{}KB（上限200K）！建议执行 summary 整理记忆。",
        kb
    )
}

pub fn host_info(host: &str) -> String {
    format!("公网地址：{}", host)
}

pub fn chat_message(role: &str, sender: &str, self_id: &str, timestamp: &str, content: &str) -> String {
    let prefix = match role {
        "user" => "user".to_string(),
        "system" => "system".to_string(),
        _ => {
            if sender == self_id {
                format!("you[{}]", self_id)
            } else {
                format!("agent[{}]", sender)
            }
        }
    };
    format!("{} [{}]: {}", prefix, timestamp, content)
}

pub fn knowledge_section(content: &str) -> String {
    format!("{}\n", content)
}

pub fn truncated_content(content: &str) -> String {
    format!("{}...(略)", content)
}

pub fn roll_result(old_kb: u64, new_kb: u64) -> String {
    format!(
        "记忆整理完成：旧记录已压缩归档（{} KB → {} KB）",
        old_kb, new_kb
    )
}

pub fn capture_result(old_kb: u64, new_kb: u64) -> String {
    format!("知识更新完成（{} KB → {} KB）", old_kb, new_kb)
}

pub fn capture_failed(error: &str) -> String {
    format!("知识更新失败：{}", error)
}

pub fn roll_failed(error: &str) -> String {
    format!("记忆整理失败：{}", error)
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
        assert_eq!(humanize_llm_error(raw), "some completely unknown error message");
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
        assert_eq!(result, "⚡ 通道额度用完了（primary），已轮换到 extra1，30秒后重试");
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
        assert_eq!(result, "⚡ 通道额度用完了（primary），已轮换到 extra1，30秒后重试");
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
        assert_eq!(result, "⚡ 推理连接异常断开（extra2），已轮换到 primary，20秒后重试");
    }

    #[test]
    fn test_inference_disconnected_without_rotation() {
        let result = inference_disconnected(10, None);
        assert_eq!(result, "⚡ 推理连接异常断开，10秒后重试");
    }
}
