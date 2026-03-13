//! HTTP bindings — trait与路由的绑定声明
//!
//! 使用 bind_http! 宏将 service/ 中定义的纯业务trait绑定到HTTP路由。
//! 所有路由字符串集中在此文件，业务逻辑在 service/ trait + api/routes.rs impl 中。

use mad_hatter::bind_http;
use crate::api::EngineState;
use crate::service::*;
use crate::api::routes::{CreateInstanceBody, ChannelSelectBody, VisionBody, MessagesQuery, RepliesQuery, FilePathQuery, SendMessageBody, RelayMessageBody, HubEnableBody, HubJoinBody};
use crate::persist::settings::Settings;
use crate::persist::hooks::HooksConfig;

bind_http! {
    InstanceService for EngineState {
        list_instances() => GET "api/instances" -> Response;
        create_instance(body: CreateInstanceBody) => POST "api/instances" -> Response;
        get_instance_by_id(id: String) => GET "api/instances/{id}" -> Response;
        delete_instance_by_id(id: String) => DELETE "api/instances/{id}" -> Response;
    }
}

bind_http! {
    ControlService for EngineState {
        observe_instance(id: String) => GET "api/instances/{id}/observe" -> Response;
        interrupt_instance(id: String) => POST "api/instances/{id}/interrupt" -> Response;
        get_channels(id: String) => GET "api/instances/{id}/channels" -> Response;
        select_channel(id: String, body: ChannelSelectBody) => POST "api/instances/{id}/channels/select" -> Response;
    }
}

bind_http! {
    KnowledgeService for EngineState {
        get_instance_knowledge(id: String) => GET "api/instances/{id}/knowledge" -> Response;
        get_instance_skill(id: String) => GET "api/instances/{id}/skill" -> Response;
        update_instance_skill(id: String, body: String) => PUT "api/instances/{id}/skill" -> Response;
    }
}

bind_http! {
    AuthService for EngineState {
        check_auth() => GET "api/auth/check" -> Response;
    }
}

bind_http! {
    VisionService for EngineState {
        vision_analyze(id: String, body: VisionBody) => POST "api/instances/{id}/vision" -> Response;
    }
}

bind_http! {
    MessageService for EngineState {
        fetch_messages(id: String, query: MessagesQuery) => GET "api/instances/{id}/messages" -> Response;
        post_message(id: String, body: SendMessageBody) => POST "api/instances/{id}/messages" -> Response;
        post_relay(id: String, body: RelayMessageBody) => POST "api/instances/{id}/messages/relay" -> Response;
        post_system_message(id: String, body: SendMessageBody) => POST "api/instances/{id}/system-messages" -> Response;
        fetch_replies(id: String, query: RepliesQuery) => GET "api/instances/{id}/replies" -> Response;
    }
}

bind_http! {
    FileService for EngineState {
        list_instance_files(id: String, query: FilePathQuery) => GET "api/instances/{id}/files/list" -> Response;
        read_instance_file(id: String, query: FilePathQuery) => GET "api/instances/{id}/files/read" -> Response;
        delete_instance_file(id: String, query: FilePathQuery) => DELETE "api/instances/{id}/files/delete" -> Response;
    }
}

bind_http! {
    SettingsService for EngineState {
        fetch_global_settings() => GET "api/settings" -> Response;
        save_global_settings(body: Settings) => POST "api/settings" -> Response;
        fetch_instance_settings(id: String) => GET "api/instances/{id}/settings" -> Response;
        save_instance_settings(id: String, body: Settings) => POST "api/instances/{id}/settings" -> Response;
    }
}

bind_http! {
    HubService for EngineState {
        enable_hub(body: HubEnableBody) => POST "api/hub/enable" -> Response;
        disable_hub() => POST "api/hub/disable" -> Response;
        join_hub(body: HubJoinBody) => POST "api/hub/join" -> Response;
        leave_hub() => POST "api/hub/leave" -> Response;
        hub_status() => GET "api/hub/status" -> Response;
        hub_endpoints() => GET "api/hub/endpoints" -> Response;
        register_hooks(body: HooksConfig) => POST "api/hooks" -> Response;
    }
}

// ── Hub Internal Path Constants ──
// Used by slave.rs for outbound requests and routes.rs for manual handler registration.
// Centralizes all Hub-related URL paths that aren't covered by http_service!/bind_http! macros.

pub const HUB_WS_PATH: &str = "/api/hub/ws";
pub const HUB_HOOKS_PATH: &str = "/api/hooks";
pub const HUB_CONTACTS_PATH_PREFIX: &str = "/api/hub/contacts/";
pub const HUB_RELAY_PATH: &str = "/api/hub/relay";
pub const HUB_TUNNEL_PROXY_CONTACTS_PATH_PREFIX: &str = "/api/hub/tunnel_proxy/contacts/";
pub const HUB_TUNNEL_PROXY_RELAY_PATH: &str = "/api/hub/tunnel_proxy/relay";
