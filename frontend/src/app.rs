use leptos::prelude::*;
use leptos_meta::*;
use leptos_router::components::*;
use leptos_router::path;
use serde::{Deserialize, Serialize};

// ============================================================
// 共享类型 — 前后端编译器共享，改了两边一起报错
// ============================================================

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub color: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub id: i64,
    pub sender: String,
    pub role: String, // "user" | "agent"
    pub content: String,
    pub timestamp: String,
}

/// Engine online status enum (mirrors alice_rpc::EngineOnlineStatus)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum EngineOnlineStatus {
    Inferring,
    Online,
    Offline,
}

impl Default for EngineOnlineStatus {
    fn default() -> Self { Self::Offline }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ObserveData {
    pub engine_online: EngineOnlineStatus,
    pub inferring: bool,
    pub idle: bool,
    pub current_action: Option<String>,
    pub executing_script: Option<String>,
    pub infer_output: Option<String>,
    pub recent_actions: Vec<String>,
    pub idle_timeout_secs: Option<i64>,
    pub idle_since: Option<i64>,
    pub active_model: i64,
    pub model_count: i64,
}

// ============================================================
// Server Functions — 通过tarpc RPC调引擎，类型安全
// ============================================================

const USER_ID: &str = "24007";

/// 创建RPC client连接
#[cfg(feature = "ssr")]
async fn rpc_client() -> Result<alice_rpc::AliceEngineClient, ServerFnError> {
    use tokio::net::UnixStream;

    let socket = std::env::var("ALICE_RPC_SOCKET")
        .unwrap_or_else(|_| "/opt/alice/engine/alice-rpc.sock".to_string());
    let stream = UnixStream::connect(&socket)
        .await
        .map_err(|e| ServerFnError::new(format!("[RPC] connect failed: {}", e)))?;
    let transport = tarpc::serde_transport::Transport::from((
        stream,
        tarpc::tokio_serde::formats::Json::default(),
    ));
    let client = alice_rpc::AliceEngineClient::new(tarpc::client::Config::default(), transport).spawn();
    Ok(client)
}

/// 创建RPC调用context
#[cfg(feature = "ssr")]
fn rpc_ctx() -> tarpc::context::Context {
    tarpc::context::current()
}

#[server(InterruptInstance)]
pub async fn interrupt_instance(instance_id: String) -> Result<bool, ServerFnError> {
    let client = rpc_client().await?;
    let result = client.interrupt(rpc_ctx(), instance_id).await
        .map_err(|e| ServerFnError::new(format!("[RPC] interrupt: {}", e)))?;
    Ok(result.success)
}

#[server(SwitchModelInstance)]
pub async fn switch_model_instance(instance_id: String, model_index: i64) -> Result<bool, ServerFnError> {
    let client = rpc_client().await?;
    let result = client.switch_model(rpc_ctx(), instance_id, model_index as u32).await
        .map_err(|e| ServerFnError::new(format!("[RPC] switch_model: {}", e)))?;
    Ok(result.success)
}

#[server(ObserveInstance)]
pub async fn observe_instance(instance_id: String) -> Result<ObserveData, ServerFnError> {
    let client = rpc_client().await?;
    let result = client.observe(rpc_ctx(), instance_id).await
        .map_err(|e| ServerFnError::new(format!("[RPC] observe: {}", e)))?;
    Ok(ObserveData {
        engine_online: match result.engine_online {
            alice_rpc::EngineOnlineStatus::Inferring => EngineOnlineStatus::Inferring,
            alice_rpc::EngineOnlineStatus::Online => EngineOnlineStatus::Online,
            alice_rpc::EngineOnlineStatus::Offline => EngineOnlineStatus::Offline,
        },
        inferring: result.inferring,
        idle: result.idle,
        current_action: result.current_action,
        executing_script: result.executing_script,
        infer_output: result.infer_output,
        recent_actions: result.recent_actions,
        idle_timeout_secs: result.idle_timeout_secs,
        idle_since: result.idle_since,
        active_model: result.active_model,
        model_count: result.model_count,
    })
}

#[server(GetInstances)]
pub async fn get_instances() -> Result<Vec<Instance>, ServerFnError> {
    let client = rpc_client().await?;
    let rpc_instances = client.get_instances(rpc_ctx()).await
        .map_err(|e| ServerFnError::new(format!("[RPC] get_instances: {}", e)))?;
    Ok(rpc_instances.into_iter().map(|i| Instance {
        id: i.id,
        name: i.name,
        avatar: i.avatar,
        color: i.color,
    }).collect())
}

#[server(GetMessages)]
pub async fn get_messages(
    instance_id: String,
    before_id: i64,
    after_id: i64,
    limit: i64,
) -> Result<Vec<ChatMessage>, ServerFnError> {
    let client = rpc_client().await?;
    let rpc_before = if before_id > 0 { Some(before_id) } else { None };
    let rpc_after = if after_id > 0 { Some(after_id) } else { None };
    let result = client.get_messages(rpc_ctx(), instance_id.clone(), rpc_before, rpc_after, limit).await
        .map_err(|e| ServerFnError::new(format!("[RPC] get_messages: {}", e)))?
        .map_err(|e| ServerFnError::new(format!("[RPC] get_messages: {}", e)))?;
    Ok(result.messages.into_iter().map(|m| ChatMessage {
        id: m.id,
        sender: if m.role == "user" { USER_ID.to_string() } else { instance_id.clone() },
        role: m.role,
        content: m.content,
        timestamp: m.timestamp,
    }).collect())
}

#[server(ReportFrontendError)]
pub async fn report_frontend_error(error_type: String, message: String, source: String) -> Result<(), ServerFnError> {
    use std::io::Write;
    let log_path = "/opt/alice/logs/frontend-error.log";
    let timestamp = chrono_now();
    let line = format!("[FRONTEND-ERR] [{}] [{}] {} | source: {}\n", timestamp, error_type, message, source);
    eprintln!("{}", line.trim());
    let mut file = std::fs::OpenOptions::new()
        .create(true).append(true).open(log_path)
        .map_err(|e| ServerFnError::new(format!("open log: {}", e)))?;
    file.write_all(line.as_bytes())
        .map_err(|e| ServerFnError::new(format!("write log: {}", e)))?;
    Ok(())
}

#[server(CreateInstanceFn)]
pub async fn create_instance_fn(display_name: String) -> Result<String, ServerFnError> {
    let client = rpc_client().await?;
    let result = client.create_instance(rpc_ctx(), display_name).await
        .map_err(|e| ServerFnError::new(format!("[RPC] create_instance: {}", e)))?;
    if result.success {
        Ok(result.message.unwrap_or_default())
    } else {
        Err(ServerFnError::new(format!("[RPC] create_instance failed: {}", result.message.unwrap_or_default())))
    }
}

#[server(DeleteInstanceFn)]
pub async fn delete_instance_fn(instance_id: String) -> Result<bool, ServerFnError> {
    let client = rpc_client().await?;
    let result = client.delete_instance(rpc_ctx(), instance_id).await
        .map_err(|e| ServerFnError::new(format!("[RPC] delete_instance: {}", e)))?;
    if result.success {
        Ok(true)
    } else {
        Err(ServerFnError::new(format!("[RPC] delete_instance failed: {}", result.message.unwrap_or_default())))
    }
}

#[server(SendChatMessage)]
pub async fn send_chat_message(instance_id: String, content: String) -> Result<i64, ServerFnError> {
    let client = rpc_client().await?;
    let result = client.send_message(rpc_ctx(), instance_id, content).await
        .map_err(|e| ServerFnError::new(format!("[RPC] send_message: {}", e)))?;
    if result.success {
        // send_message成功，返回0（前端用乐观更新，不依赖真实ID）
        Ok(0)
    } else {
        Err(ServerFnError::new(format!("[RPC] send_message failed: {}", result.message.unwrap_or_default())))
    }
}

/// 获取指定ID之后的agent回复（轮询用）
#[server(GetRepliesAfter)]
pub async fn get_replies_after(instance_id: String, after_id: i64) -> Result<Vec<ChatMessage>, ServerFnError> {
    let client = rpc_client().await?;
    let replies = client.get_replies_after(rpc_ctx(), instance_id, after_id).await
        .map_err(|e| ServerFnError::new(format!("[RPC] get_replies_after: {}", e)))?;
    Ok(replies.into_iter().map(|m| ChatMessage {
        id: m.id,
        sender: if m.role == "user" { USER_ID.to_string() } else { "agent".to_string() },
        role: m.role,
        content: m.content,
        timestamp: m.timestamp,
    }).collect())
}

#[cfg(feature = "ssr")]
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let secs = dur.as_secs();
    let ts = secs + 8 * 3600; // UTC+8
    let time_of_day = ts % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    format!("{:02}{:02}{:02}", h, m, s)
}

/// 格式化时间戳显示：
/// "20260301091413" → "09:14"
/// "sending..." / "sent" / "error:..." → 原样返回
fn format_timestamp(ts: &str) -> String {
    if ts.len() == 14 && ts.chars().all(|c| c.is_ascii_digit()) {
        // YYYYMMDDHHmmss → HH:MM
        let h = &ts[8..10];
        let m = &ts[10..12];
        format!("{}:{}", h, m)
    } else {
        ts.to_string()
    }
}

// ============================================================
// Markdown渲染
// ============================================================

fn preprocess_file_links(content: &str, instance_id: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut remaining = content;
    while let Some(start) = remaining.find("[[file:") {
        result.push_str(&remaining[..start]);
        let after_prefix = &remaining[start + 7..];
        if let Some(end) = after_prefix.find("]]") {
            let path = after_prefix[..end].trim();
            result.push_str(&format!(
                "[📄 {}](/serve/{}/{})", path, instance_id, path
            ));
            remaining = &after_prefix[end + 2..];
        } else {
            result.push_str(&remaining[start..start + 7]);
            remaining = after_prefix;
        }
    }
    result.push_str(remaining);
    result
}

fn render_markdown(content: &str, instance_id: &str) -> String {
    use pulldown_cmark::{Parser, Options, html};
    let content = preprocess_file_links(content, instance_id);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&content, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

// ============================================================
// Shell & App
// ============================================================

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
                <link rel="stylesheet" href="/main.css?v=9"/>
                <script src="/error-reporter.js"></script>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Stylesheet href="/main.css?v=9"/>
        <Title text="Alice"/>
        <Router>
            <Routes fallback=|| "Page not found">
                <Route path=path!("/") view=ChatPage/>
            </Routes>
        </Router>
    }
}

// ============================================================
// ChatPage — 主聊天界面
// ============================================================

const PAGE_SIZE: i64 = 50;

#[component]
fn ChatPage() -> impl IntoView {
    // 当前选中的实例
    let (current_instance, set_current_instance) = signal(Option::<Instance>::None);

    // 消息列表
    let (messages, set_messages) = signal(Vec::<ChatMessage>::new());

    // 输入框内容
    let (input, set_input) = signal(String::new());

    // 最后一条消息的ID（用于增量轮询）
    let (last_id, set_last_id) = signal(0i64);

    // 最老消息ID（用于加载更多历史）
    let (oldest_id, set_oldest_id) = signal(0i64);

    // 是否还有更多历史消息
    let (has_more, set_has_more) = signal(false);

    // 是否正在加载更多
    let (loading_more, set_loading_more) = signal(false);

    // 是否在底部附近（用于自动滚动）
    let (is_near_bottom, set_is_near_bottom) = signal(true);

    // 是否需要滚动到底部（触发器）
    let (scroll_to_bottom_tick, set_scroll_to_bottom_tick) = signal(0u32);

    // 消息容器引用
    let messages_ref = NodeRef::<leptos::html::Div>::new();

    // 输入框引用（用于自适应高度）
    let textarea_ref = NodeRef::<leptos::html::Textarea>::new();

    // 推理面板状态
    let (observe_data, set_observe_data) = signal(Option::<ObserveData>::None);
    let (inference_expanded, set_inference_expanded) = signal(false);
    let inference_content_ref = NodeRef::<leptos::html::Div>::new();
    let (inference_near_bottom, set_inference_near_bottom) = signal(true);

    // 侧边栏实例状态（instance_id -> "inferring"/"idle"/"offline"）
    let (sidebar_statuses, set_sidebar_statuses) = signal(std::collections::HashMap::<String, String>::new());

    // 设置Modal
    let (show_settings, set_show_settings) = signal(false);
    // 移动端侧边栏
    let (show_sidebar, set_show_sidebar) = signal(false);

    // 应用就绪状态（WASM加载+实例列表加载完成后为true）
    let (app_ready, set_app_ready) = signal(false);

    // 加载实例列表
    let (instance_refresh, set_instance_refresh) = signal(0u32);
    let instances = Resource::new(move || instance_refresh.get(), |_| get_instances());

    // 刷新后自动进入最近对话的agent
    #[cfg(feature = "hydrate")]
    {
        Effect::new(move |_| {
            if current_instance.get().is_some() {
                set_app_ready.set(true);
                return;
            }
            if let Some(result) = instances.get() {
                if let Ok(list) = result {
                    if let Ok(val) = js_sys::eval("localStorage.getItem('last_instance')") {
                        if let Some(last_id) = val.as_string() {
                            if let Some(inst) = list.iter().find(|i| i.id == last_id) {
                                set_current_instance.set(Some(inst.clone()));
                            }
                        }
                    }
                }
                // 无论instances加载成功还是失败，都标记app就绪
                set_app_ready.set(true);
            }
        });
    }

    // 轮询触发器
    let (poll_tick, _set_poll_tick) = signal(0u32);

    // 选择实例时加载消息
    Effect::new(move |_| {
        let inst = current_instance.get();
        if let Some(inst) = inst {
            let id = inst.id.clone();
            set_messages.set(Vec::new());
            set_last_id.set(0);
            set_oldest_id.set(0);
            set_has_more.set(false);
            set_is_near_bottom.set(true);
            leptos::task::spawn_local(async move {
                if let Ok(msgs) = get_messages(id, 0, 0, PAGE_SIZE).await {
                    let count = msgs.len() as i64;
                    if let Some(first) = msgs.first() {
                        set_oldest_id.set(first.id);
                    }
                    if let Some(last) = msgs.last() {
                        set_last_id.set(last.id);
                    }
                    set_has_more.set(count >= PAGE_SIZE);
                    set_messages.set(msgs);
                    // 初始加载后滚到底部
                    set_scroll_to_bottom_tick.update(|n| *n = n.wrapping_add(1));
                }
            });
        }
    });

    // 轮询新消息
    Effect::new(move |_| {
        let _tick = poll_tick.get();
        let inst = current_instance.get();
        let aid = last_id.get_untracked();
        if let Some(inst) = inst {
            if aid > 0 {
                let id = inst.id.clone();
                leptos::task::spawn_local(async move {
                    if let Ok(new_msgs) = get_messages(id, 0, aid, 200).await {
                        if !new_msgs.is_empty() {
                            if let Some(last) = new_msgs.last() {
                                set_last_id.set(last.id);
                            }
                            set_messages.update(|msgs| {
                                // 清除乐观消息（id < 0），真实消息到了
                                if msgs.iter().any(|m| m.id < 0) {
                                    msgs.retain(|m| m.id >= 0);
                                }
                                for msg in new_msgs {
                                    if !msgs.iter().any(|m| m.id == msg.id) {
                                        msgs.push(msg);
                                    }
                                }
                            });
                            // 如果用户在底部附近，自动滚到底
                            if is_near_bottom.get_untracked() {
                                set_scroll_to_bottom_tick.update(|n| *n = n.wrapping_add(1));
                            }
                        }
                    }
                });
            }
        }
    });

    // 启动定时轮询（仅在浏览器端）
    #[cfg(feature = "hydrate")]
    {
        Effect::new(move |_| {
            leptos::task::spawn_local(async move {
                loop {
                    gloo_timers::future::TimeoutFuture::new(3_000).await;
                    _set_poll_tick.update(|n| *n = n.wrapping_add(1));
                }
            });
        });
    }

    // 推理面板轮询（仅在浏览器端）
    #[cfg(feature = "hydrate")]
    {
        Effect::new(move |_| {
            let inst = current_instance.get();
            if let Some(inst) = inst {
                let id = inst.id.clone();
                // 切换实例时清空旧的observe数据
                set_observe_data.set(None);
                leptos::task::spawn_local(async move {
                    loop {
                        // 检查当前实例是否已切换，是则退出旧循环
                        let current_id = current_instance.get_untracked()
                            .map(|i| i.id.clone())
                            .unwrap_or_default();
                        if current_id != id {
                            break;
                        }
                        if let Ok(data) = observe_instance(id.clone()).await {
                            // 再次检查，防止请求返回时实例已切换
                            let current_id = current_instance.get_untracked()
                                .map(|i| i.id.clone())
                                .unwrap_or_default();
                            if current_id != id {
                                break;
                            }
                            let is_inferring = data.inferring;
                            set_observe_data.set(Some(data));
                            // 推理面板自动滚动：推理中且用户没手动上滚时，滚到底部
                            if is_inferring && inference_near_bottom.get_untracked() {
                                if let Some(el) = inference_content_ref.get() {
                                    leptos::task::spawn_local(async move {
                                        gloo_timers::future::TimeoutFuture::new(20).await;
                                        el.set_scroll_top(el.scroll_height());
                                    });
                                }
                            }
                            // 推理中1秒轮询，空闲5秒
                            let delay = if is_inferring { 1_000 } else { 5_000 };
                            gloo_timers::future::TimeoutFuture::new(delay).await;
                        } else {
                            set_observe_data.set(None);
                            gloo_timers::future::TimeoutFuture::new(5_000).await;
                        }
                    }
                });
            } else {
                set_observe_data.set(None);
            }
        });
    }

    // 侧边栏实例状态轮询（仅浏览器端）
    #[cfg(feature = "hydrate")]
    {
        Effect::new(move |_| {
            leptos::task::spawn_local(async move {
                loop {
                    // 获取实例列表
                    if let Ok(list) = get_instances().await {
                        let mut statuses = std::collections::HashMap::new();
                        for inst in &list {
                            if let Ok(data) = observe_instance(inst.id.clone()).await {
                                let status = match data.engine_online {
                                    EngineOnlineStatus::Inferring => "inferring",
                                    EngineOnlineStatus::Online => "idle",
                                    EngineOnlineStatus::Offline => "offline",
                                };
                                statuses.insert(inst.id.clone(), status.to_string());
                            } else {
                                statuses.insert(inst.id.clone(), "offline".to_string());
                            }
                        }
                        set_sidebar_statuses.set(statuses);
                    }
                    gloo_timers::future::TimeoutFuture::new(5_000).await;
                }
            });
        });
    }

    // 自动滚动到底部的Effect（仅浏览器端）
    #[cfg(feature = "hydrate")]
    {
        Effect::new(move |_| {
            let _tick = scroll_to_bottom_tick.get();
            if let Some(el) = messages_ref.get() {
                leptos::task::spawn_local(async move {
                    // 等DOM更新
                    gloo_timers::future::TimeoutFuture::new(20).await;
                    el.set_scroll_top(el.scroll_height());
                });
            }
        });
    }

    // 滚动事件：检测是否在底部附近
    let on_scroll = move |_: leptos::ev::Event| {
        if let Some(el) = messages_ref.get() {
            let at_bottom = el.scroll_top() + el.client_height() >= el.scroll_height() - 60;
            set_is_near_bottom.set(at_bottom);
        }
    };

    // 加载更多历史消息
    let load_more = move || {
        if loading_more.get_untracked() || !has_more.get_untracked() {
            return;
        }
        let inst = current_instance.get_untracked();
        let bid = oldest_id.get_untracked();
        if let Some(inst) = inst {
            if bid > 0 {
                set_loading_more.set(true);
                let id = inst.id.clone();
                leptos::task::spawn_local(async move {
                    if let Ok(older_msgs) = get_messages(id, bid, 0, PAGE_SIZE).await {
                        let count = older_msgs.len() as i64;
                        if let Some(first) = older_msgs.first() {
                            set_oldest_id.set(first.id);
                        }
                        set_has_more.set(count >= PAGE_SIZE);

                        // 记录旧scrollHeight用于位置恢复
                        #[cfg(feature = "hydrate")]
                        let old_scroll_height = messages_ref.get().map(|el| el.scroll_height()).unwrap_or(0);

                        set_messages.update(|msgs| {
                            let mut new_msgs = older_msgs;
                            new_msgs.extend(msgs.drain(..));
                            *msgs = new_msgs;
                        });

                        // 恢复滚动位置
                        #[cfg(feature = "hydrate")]
                        {
                            gloo_timers::future::TimeoutFuture::new(20).await;
                            if let Some(el) = messages_ref.get() {
                                let new_scroll_height = el.scroll_height();
                                el.set_scroll_top(new_scroll_height - old_scroll_height);
                            }
                        }
                    }
                    set_loading_more.set(false);
                });
            }
        }
    };

    // 发送消息
    let send = move || {
        let msg = input.get_untracked();
        let msg = msg.trim().to_string();
        if msg.is_empty() {
            return;
        }
        let inst = current_instance.get_untracked();
        if let Some(inst) = inst {
            set_input.set(String::new());
            // 重置textarea高度
            #[cfg(feature = "hydrate")]
            {
                use wasm_bindgen::JsCast;
                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                    if let Some(el) = doc.query_selector("textarea").ok().flatten() {
                        if let Ok(el) = el.dyn_into::<web_sys::HtmlElement>() {
                            let _ = el.set_attribute("style", "height: auto");
                        }
                    }
                }
            }
            let content = msg.clone();
            let id = inst.id.clone();
            // 乐观更新
            set_messages.update(|msgs| {
                msgs.push(ChatMessage {
                    id: -1,
                    sender: USER_ID.to_string(),
                    role: "user".to_string(),
                    content: content.clone(),
                    timestamp: "sending...".to_string(),
                });
            });
            // 发送后强制滚到底部
            set_scroll_to_bottom_tick.update(|n| *n = n.wrapping_add(1));
            leptos::task::spawn_local(async move {
                match send_chat_message(id, content).await {
                    Ok(_new_id) => {
                        // 不更新last_id（server fn返回0，会破坏轮询）
                        // 乐观消息保持id=-1，轮询拉到真实消息后会替换
                        set_messages.update(|msgs| {
                            if let Some(last) = msgs.last_mut() {
                                if last.id == -1 {
                                    last.timestamp = "sent".to_string();
                                }
                            }
                        });
                    }
                    Err(e) => {
                        set_messages.update(|msgs| {
                            if let Some(last) = msgs.last_mut() {
                                if last.id == -1 {
                                    last.timestamp = format!("error: {}", e);
                                }
                            }
                        });
                    }
                }
            });
        }
    };

    // 自适应高度：通过event target操作DOM style
    let auto_resize_from_event = move |ev: &leptos::ev::Event| {
        #[cfg(feature = "hydrate")]
        {
            use wasm_bindgen::JsCast;
            if let Some(target) = ev.target() {
                if let Ok(el) = target.dyn_into::<web_sys::HtmlTextAreaElement>() {
                    let _ = el.set_attribute("style", "height: auto");
                    let scroll_h = el.scroll_height();
                    let _ = el.set_attribute("style", &format!("height: {}px", scroll_h));
                }
            }
        }
    };

    let on_keydown = {
        let send = send.clone();
        move |ev: leptos::ev::KeyboardEvent| {
            if ev.key() == "Enter" && !ev.shift_key() {
                ev.prevent_default();
                send();
                // 重置textarea高度
                #[cfg(feature = "hydrate")]
                {
                    use wasm_bindgen::JsCast;
                    if let Some(target) = ev.target() {
                        if let Ok(el) = target.dyn_into::<web_sys::HtmlTextAreaElement>() {
                            let _ = el.set_attribute("style", "height: auto");
                        }
                    }
                }
            }
        }
    };

    let on_click_send = {
        let send = send.clone();
        move |_: leptos::ev::MouseEvent| {
            send();
        }
    };

    let load_more_click = move |_: leptos::ev::MouseEvent| {
        load_more();
    };

    view! {
        <div class="chat-app">
            // Loading overlay — WASM下载+hydration期间显示
            <Show when=move || !app_ready.get()>
                <div id="app-loading-overlay" class="app-loading-overlay">
                    <div class="loading-spinner"></div>
                    <div class="loading-text">"Loading..."</div>
                </div>
            </Show>
            // 侧边栏
            // 移动端sidebar overlay
            <div
                class="sidebar-overlay"
                class:open=move || show_sidebar.get()
                on:click=move |_| set_show_sidebar.set(false)
            ></div>
            <aside class="sidebar" class:open=move || show_sidebar.get()>
                <div class="sidebar-header">
                    <h2>"ALICE"</h2>
                    <button class="add-instance-btn" title="New Instance" on:click=move |_| {
                        #[cfg(feature = "hydrate")]
                        {
                            use wasm_bindgen::JsCast;
                            let name_result = js_sys::eval("prompt('Enter instance name:')");
                            if let Ok(val) = name_result {
                                if let Some(name_str) = val.as_string() {
                                    let name = name_str.trim().to_string();
                                    if !name.is_empty() {
                                        let set_refresh = set_instance_refresh;
                                        leptos::task::spawn_local(async move {
                                            match create_instance_fn(name).await {
                                                Ok(_) => {
                                                    set_refresh.update(|n| *n += 1);
                                                }
                                                Err(e) => {
                                                    let _ = js_sys::eval(&format!(
                                                        "alert('Failed: {}')",
                                                        e.to_string().replace('\'', "\\'")
                                                    ));
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                        }
                    }>"+"</button>
                </div>
                <Suspense fallback=move || view! { <p class="loading">"Loading..."</p> }>
                    {move || instances.get().map(|result| match result {
                        Ok(list) => {
                            // 按状态排序：推理中 > 空闲 > 离线
                            let statuses = sidebar_statuses.get();
                            let mut sorted_list = list;
                            sorted_list.sort_by(|a, b| {
                                let priority = |id: &str| -> u8 {
                                    match statuses.get(id).map(|s| s.as_str()) {
                                        Some("inferring") => 0,
                                        Some("idle") => 1,
                                        _ => 2,
                                    }
                                };
                                priority(&a.id).cmp(&priority(&b.id))
                                    .then_with(|| a.name.cmp(&b.name))
                            });
                            sorted_list.into_iter().map(|inst| {
                                let inst_clone = inst.clone();
                                let is_active = {
                                    let inst_id = inst.id.clone();
                                    move || current_instance.get().as_ref().map(|c| c.id == inst_id).unwrap_or(false)
                                };
                                let avatar = inst.avatar.clone();
                                let name = inst.name.clone();
                                let id_display = inst.id.clone();
                                let color = inst.color.clone();
                                let status_id1 = inst.id.clone();
                                let status_id2 = inst.id.clone();
                                let status_emoji = move || {
                                    let statuses = sidebar_statuses.get();
                                    match statuses.get(&status_id1).map(|s| s.as_str()) {
                                        Some("inferring") => "🟢",
                                        Some("idle") => "⚪",
                                        _ => "○",
                                    }
                                };
                                let status_title = move || {
                                    let statuses = sidebar_statuses.get();
                                    match statuses.get(&status_id2).map(|s| s.as_str()) {
                                        Some("inferring") => "Reasoning".to_string(),
                                        Some("idle") => "Idle".to_string(),
                                        _ => "Offline".to_string(),
                                    }
                                };
                                let storage_id = inst.id.clone();

                                view! {
                                    <div
                                        class="instance-item"
                                        class:active=is_active
                                        style=format!("border-left-color: {}", color)
                                        on:click=move |_| {
                                            set_current_instance.set(Some(inst_clone.clone()));
                                            set_show_sidebar.set(false);
                                            // 记住最后选择的实例
                                            #[cfg(feature = "hydrate")]
                                            {
                                                if let Some(storage) = web_sys::window()
                                                {
                                                    let _ = js_sys::eval(&format!(
                                                        "localStorage.setItem('last_instance','{}')", storage_id
                                                    ));
                                                }
                                            }
                                        }
                                    >
                                        <span class="avatar">{avatar.clone()}</span>
                                        <div class="instance-info">
                                            <span class="name">{name.clone()}</span>
                                            <span class="instance-status">
                                                <span class="status-dot" title=status_title>{status_emoji}</span>
                                                <span class="id">{id_display.clone()}</span>
                                            </span>
                                        </div>

                                    </div>
                                }
                            }).collect::<Vec<_>>()
                        }.into_any(),
                        Err(e) => view! { <p class="error">{format!("{}", e)}</p> }.into_any(),
                    })}
                </Suspense>
            </aside>

            // 聊天区
            <main class="chat-main">
                {move || {
                    if let Some(inst) = current_instance.get() {
                        let header_name = format!("{} {}", inst.avatar, inst.name);
                        let settings_name = inst.name.clone();
                        let settings_avatar = inst.avatar.clone();
                        let settings_color = inst.color.clone();
                        let agent_color_style = format!("--agent-color: {}", settings_color);
                        let settings_id = inst.id.clone();
                        view! {
                            <div class="chat-container" style=agent_color_style>
                                <div class="chat-header">
                                    <button class="hamburger-btn" title="Menu" on:click=move |_| {
                                        set_show_sidebar.update(|v| *v = !*v);
                                    }>"☰"</button>
                                    <h3>{header_name}</h3>
                                    <button class="settings-btn" title="Settings" on:click=move |_| {
                                        set_show_settings.update(|v| *v = !*v);
                                    }>"⚙"</button>
                                </div>
                                <div class="messages" node_ref=messages_ref on:scroll=on_scroll.clone()>
                                    // 加载更多按钮
                                    {move || {
                                        if has_more.get() {
                                            view! {
                                                <div class="load-more">
                                                    <button
                                                        on:click=load_more_click.clone()
                                                        disabled=move || loading_more.get()
                                                    >
                                                        {move || if loading_more.get() { "Loading..." } else { "↑ Load earlier messages" }}
                                                    </button>
                                                </div>
                                            }.into_any()
                                        } else {
                                            view! { <span></span> }.into_any()
                                        }
                                    }}
                                    <For
                                        each=move || messages.get()
                                        key=|msg| msg.id
                                        children=move |msg| {
                                            let is_user = msg.role == "user";
                                            let bubble_class = if is_user { "bubble user" } else { "bubble agent" };
                                            let time_display = format_timestamp(&msg.timestamp);
                                            if is_user {
                                                // 用户消息：纯文本
                                                let content = msg.content.clone();
                                                view! {
                                                    <div class=bubble_class>
                                                        <div class="bubble-content">{content}</div>
                                                        <span class="timestamp">{time_display}</span>
                                                    </div>
                                                }.into_any()
                                            } else {
                                                // Agent消息：Markdown渲染
                                                let inst_id = current_instance.get_untracked().map(|i| i.id.clone()).unwrap_or_default();
                                                let html = render_markdown(&msg.content, &inst_id);
                                                view! {
                                                    <div class=bubble_class>
                                                        <div class="bubble-content markdown-body" inner_html=html></div>
                                                        <span class="timestamp">{time_display}</span>
                                                    </div>
                                                }.into_any()
                                            }
                                        }
                                    />
                                </div>
                                // 推理面板
                                <div class="inference-bar">
                                    <div class="inference-header" on:click=move |_| {
                                        set_inference_expanded.update(|v| *v = !*v);
                                    }>
                                        <span class="inference-icon">
                                            {move || {
                                                match observe_data.get() {
                                                    Some(ref d) if d.inferring => "🧠",
                                                    Some(ref d) if d.idle => "💤",
                                                    Some(_) => "🧠",
                                                    None => "⚪",
                                                }
                                            }}
                                        </span>
                                        <span class="inference-label">
                                            {move || {
                                                match observe_data.get() {
                                                    Some(ref d) if d.inferring => {
                                                        if let Some(ref output) = d.infer_output {
                                                            if output.contains("memory_summary") || output.contains("summary_confirm") {
                                                                "Reasoning (整理记忆中)".to_string()
                                                            } else {
                                                                format!("Reasoning ({} chars)", output.len())
                                                            }
                                                        } else {
                                                            "Reasoning".to_string()
                                                        }
                                                    },
                                                    Some(ref d) if d.idle => {
                                                        if let (Some(timeout), Some(since)) = (d.idle_timeout_secs, d.idle_since) {
                                                            let now = std::time::SystemTime::now()
                                                                .duration_since(std::time::UNIX_EPOCH)
                                                                .unwrap_or_default()
                                                                .as_secs() as i64;
                                                            let elapsed = now.saturating_sub(since);
                                                            let remaining = timeout.saturating_sub(elapsed);
                                                            format!("idle {}s", remaining)
                                                        } else {
                                                            "idle".to_string()
                                                        }
                                                    },
                                                    Some(ref d) if d.engine_online == EngineOnlineStatus::Offline => "Engine offline".to_string(),
                                                    Some(_) => "Ready".to_string(),
                                                    None => "Connecting...".to_string(),
                                                }
                                            }}
                                        </span>

                                        <span class={move || if inference_expanded.get() { "inference-toggle expanded" } else { "inference-toggle" }}>
                                            "▼"
                                        </span>
                                    </div>
                                    <div
                                        node_ref=inference_content_ref
                                        class={move || if inference_expanded.get() { "inference-content expanded" } else { "inference-content" }}
                                        on:scroll=move |_| {
                                            if let Some(el) = inference_content_ref.get() {
                                                let at_bottom = el.scroll_height() - el.scroll_top() - el.client_height() < 30;
                                                set_inference_near_bottom.set(at_bottom);
                                            }
                                        }
                                    >
                                        {move || {
                                            match observe_data.get() {
                                                Some(ref d) if d.inferring => {
                                                    if let Some(ref output) = d.infer_output {
                                                        view! { <pre class="infer-output">{output.clone()}</pre> }.into_any()
                                                    } else {
                                                        view! { <span class="inference-empty">"Waiting for output..."</span> }.into_any()
                                                    }
                                                },
                                                _ => view! { <span class="inference-empty">"No active reasoning"</span> }.into_any(),
                                            }
                                        }}
                                    </div>
                                    // 脚本执行区域
                                    {move || {
                                        if let Some(ref d) = observe_data.get() {
                                            if let Some(ref script) = d.executing_script {
                                                let script_text = script.clone();
                                                return view! {
                                                    <div class="observe-script active">
                                                        <div class="observe-script-header">
                                                            <span class="observe-script-icon">"🔧"</span>
                                                            <span class="observe-script-label">"Executing script..."</span>
                                                        </div>
                                                        <pre class="observe-script-content">{script_text}</pre>
                                                    </div>
                                                }.into_any();
                                            }
                                        }
                                        view! { <span></span> }.into_any()
                                    }}
                                </div>
                                <div class="input-area">
                                    <textarea
                                        node_ref=textarea_ref
                                        placeholder="Type a message..."
                                        prop:value=input
                                        on:input=move |ev| {
                                            set_input.set(event_target_value(&ev));
                                            auto_resize_from_event(&ev);
                                        }
                                        on:keydown=on_keydown.clone()
                                        rows="1"
                                    />
                                    <button class="send-btn" on:click=on_click_send.clone()>
                                        "➤"
                                    </button>
                                </div>

                                // 设置抽屉
                                {move || {
                                    if show_settings.get() {
                                        let drawer_name = settings_name.clone();
                                        let drawer_avatar = settings_avatar.clone();
                                        let drawer_color = settings_color.clone();
                                        let drawer_id = settings_id.clone();
                                        let del_id = settings_id.clone();
                                        let del_name = settings_name.clone();
                                        let model_info = observe_data.get().map(|d| {
                                            format!("Model {}/{}", d.active_model + 1, d.model_count)
                                        }).unwrap_or_else(|| "—".to_string());
                                        let is_inferring = observe_data.get().as_ref().map(|d| d.inferring).unwrap_or(false);
                                        let has_multi_model = observe_data.get().as_ref().map(|d| d.model_count > 1).unwrap_or(false);
                                        view! {
                                            <div class="settings-overlay" on:click=move |_| set_show_settings.set(false)>
                                                <div class="settings-drawer" on:click=move |ev| {
                                                    ev.stop_propagation();
                                                }>
                                                    <div class="settings-header">
                                                        <h3>"Settings"</h3>
                                                        <button class="settings-close" on:click=move |_| set_show_settings.set(false)>"✕"</button>
                                                    </div>
                                                    <div class="settings-body">
                                                        <div class="settings-row">
                                                            <span class="settings-label">"Avatar"</span>
                                                            <span class="settings-value avatar-large">{drawer_avatar}</span>
                                                        </div>
                                                        <div class="settings-row">
                                                            <span class="settings-label">"Name"</span>
                                                            <span class="settings-value">{drawer_name}</span>
                                                        </div>
                                                        <div class="settings-row">
                                                            <span class="settings-label">"ID"</span>
                                                            <span class="settings-value mono">{drawer_id}</span>
                                                        </div>
                                                        <div class="settings-row">
                                                            <span class="settings-label">"Color"</span>
                                                            <span class="settings-value">
                                                                <span class="color-swatch" style=format!("background:{}", drawer_color)></span>
                                                                {drawer_color}
                                                            </span>
                                                        </div>
                                                        <div class="settings-row">
                                                            <span class="settings-label">"Model"</span>
                                                            <span class="settings-value">{model_info}</span>
                                                        </div>
                                                    </div>
                                                    <div class="settings-ops">
                                                        {if is_inferring {
                                                            view! {
                                                                <button class="ops-menu-item ops-stop" on:click=move |_| {
                                                                    if let Some(inst) = current_instance.get() {
                                                                        let id = inst.id.clone();
                                                                        leptos::task::spawn_local(async move {
                                                                            let _ = interrupt_instance(id).await;
                                                                            set_show_settings.set(false);
                                                                        });
                                                                    }
                                                                }>
                                                                    <span class="ops-icon">"🛑"</span>
                                                                    <span>"Stop Reasoning"</span>
                                                                </button>
                                                            }.into_any()
                                                        } else {
                                                            view! { <span></span> }.into_any()
                                                        }}
                                                        {if has_multi_model {
                                                            let (switching, set_switching) = signal(false);
                                                            view! {
                                                                <button class="ops-menu-item ops-switch"
                                                                    disabled=move || switching.get()
                                                                    on:click=move |_| {
                                                                    if switching.get() { return; }
                                                                    if let Some(inst) = current_instance.get() {
                                                                        if let Some(ref d) = observe_data.get() {
                                                                            let id = inst.id.clone();
                                                                            let next = (d.active_model + 1) % d.model_count;
                                                                            set_switching.set(true);
                                                                            leptos::task::spawn_local(async move {
                                                                                let _ = switch_model_instance(id, next).await;
                                                                                set_switching.set(false);
                                                                                set_show_settings.set(false);
                                                                            });
                                                                        }
                                                                    }
                                                                }>
                                                                    <span class="ops-icon">"🤖"</span>
                                                                    <span>{move || {
                                                                        if switching.get() {
                                                                            "Switching...".to_string()
                                                                        } else {
                                                                            observe_data.get().map(|d| {
                                                                                format!("Switch Model ({}→{})", d.active_model + 1, (d.active_model + 1) % d.model_count + 1)
                                                                            }).unwrap_or_else(|| "Switch Model".to_string())
                                                                        }
                                                                    }}</span>
                                                                </button>
                                                            }.into_any()
                                                        } else {
                                                            view! { <span></span> }.into_any()
                                                        }}
                                                        <button class="ops-menu-item danger" on:click=move |_| {
                                                            #[cfg(feature = "hydrate")]
                                                            {
                                                                let confirmed = js_sys::eval(&format!(
                                                                    "confirm('Delete instance \"{}\" ({})?')",
                                                                    del_name, del_id
                                                                )).map(|v| v.as_bool().unwrap_or(false)).unwrap_or(false);
                                                                if confirmed {
                                                                    let id = del_id.clone();
                                                                    leptos::task::spawn_local(async move {
                                                                        if let Ok(true) = delete_instance_fn(id).await {
                                                                            set_instance_refresh.update(|n| *n += 1);
                                                                            set_show_settings.set(false);
                                                                            set_current_instance.set(None);
                                                                            let _ = js_sys::eval("localStorage.removeItem('last_instance')");
                                                                        }
                                                                    });
                                                                }
                                                            }
                                                        }>
                                                            <span class="ops-icon">"🗑"</span>
                                                            <span>"Delete Instance"</span>
                                                        </button>
                                                    </div>
                                                </div>
                                            </div>
                                        }.into_any()
                                    } else {
                                        view! { <div></div> }.into_any()
                                    }
                                }}
                            </div>
                        }.into_any()
                    } else {
                        view! {
                            <div class="no-selection">
                                <button class="hamburger-btn" on:click=move |_| {
                                    set_show_sidebar.update(|v| *v = !*v);
                                }>"☰"</button>
                                <p>"← Select an agent to start chatting"</p>
                            </div>
                        }.into_any()
                    }
                }}
            </main>
        </div>
    }
}

