//! Internationalized message catalog.
//!
//! All user/agent-facing message strings are defined here via the i18n! macro.
//! This is the single source of truth for human-readable text in the engine.

macro_rules! i18n {
    ( $( $name:ident( $($arg:ident : $ty:ty),* ) = $msg:expr ; )* ) => {
        $(
            #[allow(dead_code)]
            pub fn $name( $($arg : $ty),* ) -> String {
                format!($msg)
            }
        )*
    };
}

i18n! {
    // === RPC/API ===
    empty_message() = "Empty message";
    instance_deleted(id: &str) = "Deleted: {id}";
    no_valid_fields() = "No valid fields to update";
    global_settings_updated() = "Global settings updated";

    // === Engine Safety ===
    safety_valve_triggered(consecutive: u32, cooldown: u64) = "连续工作保护已启动：已连续推理{consecutive}次，暂停{cooldown}秒让系统休息一下。这是正常的保护机制，不是故障。稍等片刻即可自动恢复。如需调整触发阈值，可在 Settings 中修改 safety_max_consecutive_beats。";
    beat_limit_reached(current: u32, max: u32) = "推理次数已达上限（{current}/{max}），实例已停止推理。";
    disk_space_low(avail_mb: u64) = "磁盘空间不足：仅剩 {avail_mb}MB 可用。请清理磁盘空间，否则可能导致数据损坏。";

    // === Knowledge ===
    knowledge_capacity_ok(size: usize) = "知识: {size}/51200字符 🟢";
    knowledge_capacity_warning(size: usize) = "知识: {size}/51200字符 ⚠️ 知识接近上限，summary时请精简";
    knowledge_capacity_critical(size: usize) = "知识: {size}/51200字符 🔴 知识超出推荐容量，建议与用户商量裂变";
    knowledge_section(content: &str) = "{content}\n";

    // === Memory/Prompt ===
    session_summary(summary: &str) = "[总结] {summary}";
    memory_over_limit(kb: usize) = "\n⚠️ prompt总量已达{kb}KB（上限200K）！建议执行 summary 整理记忆。";
    empty_placeholder() = "(空)";
    truncated_content(content: &str) = "{content}...(略)";
    host_info(host: &str) = "公网地址：{host}";
    roll_requirement(kb: u32) = "不超过{kb}KB";

    // === Roll/Capture ===
    roll_result(old_kb: u64, new_kb: u64) = "记忆整理完成：旧记录已压缩归档（{old_kb} KB → {new_kb} KB）";
    roll_deleted_residual(block: &str) = "记忆整理：发现已压缩的残留记录，已清理（{block}）";
    roll_deleted_empty(block: &str) = "记忆整理：发现空记录，已清理（{block}）";
    roll_llm_empty() = "记忆整理中断：压缩结果为空，已跳过本次整理";
    roll_failed(error: &str) = "记忆整理失败：{error}";
    capture_result(old_kb: u64, new_kb: u64) = "知识更新完成（{old_kb} KB → {new_kb} KB）";
    capture_failed(error: &str) = "知识更新失败：{error}";

    // === Sequence Defense ===
    sequence_reject_after_blocking(instance_id: &str, action: &str) = "[SEQUENCE-{instance_id}] Non-blocking action '{action}' after blocking action — aborting inference";
    sequence_reject_after_idle(instance_id: &str, action: &str) = "[SEQUENCE-{instance_id}] Action '{action}' after idle — zero tolerance, aborting inference";

    // === Action Execution ===
    send_failed_no_extension(recipient: &str) = "发送失败：通讯服务不可用，无法联系 \"{recipient}\"";
    send_failed_not_in_contacts(recipient: &str, contacts: &str) = "发送失败：收件人 \"{recipient}\" 不在联系人列表中。可用联系人：{contacts}";
    send_failed_rejected(recipient: &str) = "发送失败：消息转发到 \"{recipient}\" 时被拒绝";
    file_content_with_header(content: &str) = "---file content---\n{content}";
    skeleton_with_header(content: &str) = "--- skeleton (auto-extracted, showing interface & comments only, not full content) ---\n{content}";
    preview_with_header(content: &str) = "--- preview (first 10 + last 5 lines, not full content) ---\n{content}";
    preview_ellipsis() = "     ...";
    idle_cancelled() = "idle cancelled: send_msg failed earlier in this beat";
    binary_file_description(name: &str, size: u64) = "[Binary file: {name}, {size} bytes]";

    // === LLM Errors ===
    llm_err_invalid_key() = "API密钥无效";
    llm_err_quota_exceeded() = "通道额度用完了";
    llm_err_access_denied() = "API访问被拒绝";
    llm_err_rate_limited() = "请求太频繁";
    llm_err_service_unavailable() = "LLM服务暂时不可用";
    llm_err_unknown_status(status: u16) = "LLM服务返回错误({status})";
    llm_err_sse_timeout() = "连接超时（60秒无响应）";
    llm_err_connection_failed() = "无法连接LLM服务";
    llm_err_stream_read_error() = "数据流读取异常";
    llm_err_runtime_failed() = "内部运行时创建失败";
    llm_err_format_violation() = "你的输出格式不正确——Action分隔符前有多余内容。思考过程会自动收集，无需特殊格式";
    llm_err_missing_end_marker() = "你的输出不完整——缺少结束标记。请确保每个action都有完整的开始和结束";

    // === Inference Status ===
    inference_error_with_rotation(friendly: &str, from: &str, to: &str, backoff: u64) = "⚡ {friendly}（{from}），已轮换到 {to}，{backoff}秒后重试";
    inference_error_no_rotation(friendly: &str, backoff: u64) = "⚡ {friendly}，{backoff}秒后重试";
    inference_disconnected_with_rotation(from: &str, to: &str, backoff: u64) = "⚡ 推理连接异常断开（{from}），已轮换到 {to}，{backoff}秒后重试";
    inference_disconnected_no_rotation(backoff: u64) = "⚡ 推理连接异常断开，{backoff}秒后重试";

    // === Chat Message ===
    chat_prefix_self(id: &str) = "you[{id}]";
    chat_prefix_agent(sender: &str) = "agent[{sender}]";
    chat_message_fmt(prefix: &str, timestamp: &str, content: &str) = "{prefix} [{timestamp}]: {content}";

    // === Hub ===
    hub_join_token_empty() = "Join token cannot be empty";
    hub_already_host() = "Already in host mode";
    hub_not_host() = "Not in host mode";
    hub_already_joined() = "Already joined to a host. Leave first.";
    hub_slaves_connected(count: usize) = "{count} slaves connected";
    hub_connected_to(host: &str) = "connected to {host}";
    hub_instance_not_found(id: &str, err: &str) = "Instance {id} not found: {err}";
    hub_relay_write_failed(err: &str) = "Failed to write relay message: {err}";
    hub_tunnel_relay_failed(err: &str) = "Tunnel relay failed: {err}";
    hub_slave_endpoint_unavailable() = "Slave tunnel endpoint not available";
    hub_not_connected() = "Not connected to host";
    hub_host_unavailable() = "Host state no longer available";
    hub_instance_not_found_anywhere(id: &str) = "Instance {id} not found on any connected engine";
    hub_connection_timeout(host: &str) = "Connection timeout (5s) to {host}";
    hub_ws_connect_failed(err: &str) = "WebSocket connect failed: {err}";
    hub_send_register_failed(err: &str) = "Failed to send register: {err}";
    hub_tunnel_error(err: &str) = "Tunnel error: {err}";
    hub_joined_leave_first() = "Currently joined to a host. Leave first.";
    hub_host_disable_first() = "Currently in host mode. Disable first.";
    hub_mode_changed() = "Mode changed during connection attempt";
    hub_not_joined() = "Not joined to any host";

    // === Extension ===
    no_extension_handler() = "No extension handler configured";
}