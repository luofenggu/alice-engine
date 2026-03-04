//! API Layer End-to-End Test: Hello World
//!
//! Tests the full chain: HTTP API → RPC → Engine → I/O
//!
//! Flow:
//! 1. HTTP POST /api/instances/{id}/messages — send user message
//! 2. alice.beat() — auto-read
//! 3. alice.beat() — mock LLM inference (Thinking + SendMsg + Idle)
//! 4. HTTP GET /api/instances/{id}/replies?after_id=0 — verify reply
//! 5. Privileged referee: read current.txt directly

use std::sync::Arc;
use std::time::Duration;

use alice_engine::core::Alice;
use alice_engine::external::llm::LlmConfig;
use alice_engine::external::llm::StreamItem;
use alice_engine::inference::Action;
use alice_engine::persist::instance::Instance;
use alice_engine::rpc::{EngineState, start_rpc_server};
use alice_engine::core::signal::SignalHub;
use alice_engine::policy::{EngineConfig, EnvConfig};

use alice_frontend::api::{authenticated_api_routes, ApiState};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// Create a test Alice instance in the given instances directory.
/// Returns (Alice, instance_id).
fn create_test_alice(instances_dir: &std::path::Path, env_config: Arc<EnvConfig>) -> (Alice, String) {
    let instance = Instance::create(instances_dir, "user1", Some("TestBot"), None).unwrap();
    let instance_id = instance.id.clone();
    let log_dir = instances_dir.join(&instance_id).join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();
    let llm_config = LlmConfig { model: String::new(), api_key: String::new() };
    let mut alice = Alice::new(instance, log_dir, llm_config, env_config).unwrap();
    alice.privileged = true;
    (alice, instance_id)
}

#[test]
fn test_api_hello_world() {
    // === Setup ===
    let tmp = tempfile::TempDir::new().unwrap();
    let instances_dir = tmp.path().join("instances");
    let logs_dir = tmp.path().join("logs");
    let socket_path = tmp.path().join("test-rpc.sock");
    std::fs::create_dir_all(&instances_dir).unwrap();
    std::fs::create_dir_all(&logs_dir).unwrap();

    // EnvConfig with custom RPC socket path
    let mut env_config = EnvConfig::from_env();
    env_config.rpc_socket = Some(socket_path.to_string_lossy().to_string());
    let env_config = Arc::new(env_config);

    // Create Alice instance (with mock LLM)
    let (mut alice, instance_id) = create_test_alice(&instances_dir, env_config.clone());

    // Set up mock LLM responses
    alice.set_mock_streams(vec![
        // Beat 2: LLM inference — Thinking + SendMsg + Idle
        vec![
            StreamItem::Action(Action::Thinking { content: "User says hello, I'll reply".into() }),
            StreamItem::Action(Action::SendMsg {
                recipient: "user1".into(),
                content: "Hello from API test!".into(),
            }),
            StreamItem::Action(Action::Idle { timeout_secs: None }),
            StreamItem::Done(vec![
                Action::Thinking { content: "User says hello, I'll reply".into() },
                Action::SendMsg {
                    recipient: "user1".into(),
                    content: "Hello from API test!".into(),
                },
                Action::Idle { timeout_secs: None },
            ], Some(alice_engine::external::llm::UsageInfo { input_tokens: 100, output_tokens: 50, total_cost: None })),
        ],
    ]);

    // Create tokio runtime for async operations (RPC server + HTTP requests)
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Start RPC server in background
    let engine_state = Arc::new(EngineState::new(
        instances_dir.clone(),
        logs_dir.clone(),
        "user1".to_string(),
        SignalHub::new(),
        EngineConfig::load(),
        env_config.clone(),
    ));
    rt.spawn(start_rpc_server(engine_state));

    // Wait for RPC server to be ready
    std::thread::sleep(Duration::from_millis(200));

    // Build API router
    let api_state = Arc::new(ApiState {
        instances_dir: instances_dir.clone(),
        rpc_socket: socket_path.to_string_lossy().to_string(),
    });
    let router = authenticated_api_routes().with_state(api_state);

    // === Act 1: Send message via HTTP API ===
    let send_response = rt.block_on(async {
        let body = serde_json::json!({ "content": "Hello, bot!" });
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/instances/{}/messages", instance_id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        router.clone().oneshot(request).await.unwrap()
    });
    assert_eq!(send_response.status(), StatusCode::OK, "Send message should succeed");

    // Verify message was written to chat.db (privileged referee)
    assert_eq!(alice.count_unread_messages(), 1, "Should have 1 unread message after HTTP send");

    // === Act 2: Beat 1 — auto-read ===
    alice.beat().unwrap();
    assert_eq!(alice.count_unread_messages(), 0, "Auto-read should consume the message");

    // Verify current.txt contains the message (privileged referee)
    let current = std::fs::read_to_string(
        alice.instance.memory.sessions_dir().join("current.txt")
    ).unwrap();
    assert!(current.contains("Hello, bot!"), "current.txt should contain user message after auto-read");

    // === Act 3: Beat 2 — mock LLM inference ===
    alice.beat().unwrap();

    // Verify current.txt contains agent's response (privileged referee)
    let current = std::fs::read_to_string(
        alice.instance.memory.sessions_dir().join("current.txt")
    ).unwrap();
    assert!(current.contains("Hello from API test!"), "current.txt should contain agent reply");
    assert!(alice.last_was_idle, "Should be idle after Idle action");

    // === Act 4: Verify reply via HTTP API ===
    let replies_response = rt.block_on(async {
        let request = Request::builder()
            .method("GET")
            .uri(format!("/api/instances/{}/replies?after_id=0", instance_id))
            .body(Body::empty())
            .unwrap();
        router.clone().oneshot(request).await.unwrap()
    });
    assert_eq!(replies_response.status(), StatusCode::OK, "Get replies should succeed");

    // Parse response body
    let body_bytes = rt.block_on(async {
        axum::body::to_bytes(replies_response.into_body(), usize::MAX).await.unwrap()
    });
    let replies: Vec<alice_rpc::MessageInfo> = serde_json::from_slice(&body_bytes).unwrap();

    // Should have at least the agent's reply
    let agent_replies: Vec<_> = replies.iter().filter(|m| m.role == "agent").collect();
    assert!(!agent_replies.is_empty(), "Should have agent replies via HTTP API");
    assert!(
        agent_replies.iter().any(|m| m.content.contains("Hello from API test!")),
        "Agent reply should contain expected content"
    );

    // === Privileged Referee: Final verification ===
    // The full chain worked: HTTP POST → RPC → chat.db → beat(auto-read) → beat(LLM) → chat.db → HTTP GET
    println!("✅ API Hello World: Full chain HTTP → RPC → Engine → I/O verified");
}

