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

pub fn field_changed(field: &str, value: &str) -> String {
    format!("{}: {}", field, value)
}

pub fn field_separator() -> &'static str {
    ", "
}

pub fn api_key_changed(suffix: &str) -> String {
    format!("api_key: ...{}", suffix)
}

pub fn extra_models_changed(count: usize) -> String {
    format!("extra_models: {} items", count)
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
