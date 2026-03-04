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
    old: &crate::api::InstanceSettings,
    new: &crate::api::InstanceSettings,
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
        let suffix_len = 4.min(new.api_key.len());
        changes.push(format!("api_key: ...{}", &new.api_key[new.api_key.len() - suffix_len..]));
    }
    if old.model != new.model {
        changes.push(format!("model: {}", new.model));
    }
    if old.privileged != new.privileged {
        changes.push(format!("privileged: {}", new.privileged));
    }
    if old.max_beats != new.max_beats {
        changes.push(format!("max_beats: {:?}", new.max_beats));
    }
    if old.session_blocks_limit != new.session_blocks_limit {
        changes.push(format!("session_blocks_limit: {:?}", new.session_blocks_limit));
    }
    if old.session_block_kb != new.session_block_kb {
        changes.push(format!("session_block_kb: {:?}", new.session_block_kb));
    }
    if old.history_kb != new.history_kb {
        changes.push(format!("history_kb: {:?}", new.history_kb));
    }
    if old.safety_max_consecutive_beats != new.safety_max_consecutive_beats {
        changes.push(format!("safety_max_consecutive_beats: {:?}", new.safety_max_consecutive_beats));
    }
    if old.safety_cooldown_secs != new.safety_cooldown_secs {
        changes.push(format!("safety_cooldown_secs: {:?}", new.safety_cooldown_secs));
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
        changes.push(format!("shell_env: {}", new.shell_env.as_deref().unwrap_or("")));
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
        "安全阀触发：连续{}次推理未进入idle状态，强制冷却{}秒。这可能意味着推理陷入了循环。",
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
    format!("知识: {}/51200字符 🔴 知识超出推荐容量，建议与用户商量裂变", size)
}

pub fn binary_file_description(name: &str, size: u64) -> String {
    format!("[Binary file: {}, {} bytes]", name, size)
}

/// Format beat error for user notification.
pub fn beat_error(e: &anyhow::Error) -> String {
    format!("Beat error: {}", e)
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

// === Roll result messages ===

pub fn roll_deleted_residual(block: &str) -> String {
    format!("deleted residual block {} (already compressed)", block)
}

pub fn roll_deleted_empty(block: &str) -> String {
    format!("deleted empty block {}", block)
}

pub fn roll_llm_empty() -> &'static str {
    "LLM returned empty, roll aborted"
}

pub fn empty_placeholder() -> &'static str {
    "(空)"
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

pub fn chat_message(sender: &str, timestamp: &str, content: &str) -> String {
    format!("{} [{}]: {}", sender, timestamp, content)
}

pub fn knowledge_section(content: &str) -> String {
    format!("### 要点与知识 ###\n{}\n", content)
}

pub fn truncated_content(content: &str) -> String {
    format!("{}...(略)", content)
}

pub fn roll_result(block: &str, usage: Option<(u64, u64)>) -> String {
    let usage_info = if let Some((input, output)) = usage {
        format!(", tokens: {}+{}", input, output)
    } else {
        String::new()
    };
    format!("history rolled: block {} compressed into history.txt{}", block, usage_info)
}
