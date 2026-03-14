//! Service traits — 纯业务接口定义
//!
//! 每个trait定义一组业务操作，impl在 api/routes.rs 中。
//! trait方法签名中不出现HTTP框架类型（除Response作为统一返回类型）。

use axum::response::Response;
use crate::api::routes::{
    CreateInstanceBody, ChannelSelectBody, VisionBody,
    MessagesQuery, RepliesQuery, FilePathQuery,
    SendMessageBody, RelayMessageBody,
    HubEnableBody, HubJoinBody,
};
use crate::persist::settings::Settings;

pub trait InstanceService {
    fn list_instances(&self) -> impl std::future::Future<Output = Response> + Send;
    fn create_instance(&self, body: CreateInstanceBody) -> impl std::future::Future<Output = Response> + Send;
    fn get_instance_by_id(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
    fn delete_instance_by_id(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
}

pub trait ControlService {
    fn observe_instance(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
    fn interrupt_instance(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
    fn get_channels(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
    fn select_channel(&self, id: String, body: ChannelSelectBody) -> impl std::future::Future<Output = Response> + Send;
}

pub trait KnowledgeService {
    fn get_instance_knowledge(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
    fn get_instance_skill(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
    fn update_instance_skill(&self, id: String, body: String) -> impl std::future::Future<Output = Response> + Send;
}

pub trait AuthService {
    fn check_auth(&self) -> impl std::future::Future<Output = Response> + Send;
}

pub trait VisionService {
    fn vision_analyze(&self, id: String, body: VisionBody) -> impl std::future::Future<Output = Response> + Send;
}

pub trait MessageService {
    fn fetch_messages(&self, id: String, query: MessagesQuery) -> impl std::future::Future<Output = Response> + Send;
    fn post_message(&self, id: String, body: SendMessageBody) -> impl std::future::Future<Output = Response> + Send;
    fn post_relay(&self, id: String, body: RelayMessageBody) -> impl std::future::Future<Output = Response> + Send;
    fn post_system_message(&self, id: String, body: SendMessageBody) -> impl std::future::Future<Output = Response> + Send;
    fn fetch_replies(&self, id: String, query: RepliesQuery) -> impl std::future::Future<Output = Response> + Send;
}

pub trait FileService {
    fn list_instance_files(&self, id: String, query: FilePathQuery) -> impl std::future::Future<Output = Response> + Send;
    fn read_instance_file(&self, id: String, query: FilePathQuery) -> impl std::future::Future<Output = Response> + Send;
    fn delete_instance_file(&self, id: String, query: FilePathQuery) -> impl std::future::Future<Output = Response> + Send;
}

pub trait SettingsService {
    fn fetch_global_settings(&self) -> impl std::future::Future<Output = Response> + Send;
    fn save_global_settings(&self, body: Settings) -> impl std::future::Future<Output = Response> + Send;
    fn fetch_instance_settings(&self, id: String) -> impl std::future::Future<Output = Response> + Send;
    fn save_instance_settings(&self, id: String, body: Settings) -> impl std::future::Future<Output = Response> + Send;
}

pub trait HubService {
    fn enable_hub(&self, body: HubEnableBody) -> impl std::future::Future<Output = Response> + Send;
    fn disable_hub(&self) -> impl std::future::Future<Output = Response> + Send;
    fn join_hub(&self, body: HubJoinBody) -> impl std::future::Future<Output = Response> + Send;
    fn leave_hub(&self) -> impl std::future::Future<Output = Response> + Send;
    fn hub_status(&self) -> impl std::future::Future<Output = Response> + Send;
    fn hub_endpoints(&self) -> impl std::future::Future<Output = Response> + Send;
}

pub mod extension;
