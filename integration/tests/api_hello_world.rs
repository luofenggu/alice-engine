//! API Layer End-to-End Test: Hello World (v2 — Zero-intrusion Mock)
//!
//! Tests the full chain with a real engine loop:
//!   HTTP API → RPC → AliceEngine.run() → Mock LLM HTTP server → I/O
//!
//! The engine runs its normal beat loop, hitting a local mock LLM server
//! instead of a real provider. Zero engine code modifications.
//!
//! Flow:
//! 1. Start mock LLM server with scripted responses
//! 2. Create instance + configure engine to use mock LLM
//! 3. Start AliceEngine.run() in OS thread (real beat loop)
//! 4. Start RPC server in tokio
//! 5. HTTP POST /api/instances/{id}/messages — send user message
//! 6. Poll HTTP GET /api/instances/{id}/replies — wait for agent reply
//! 7. Privileged referee: verify current.txt

use std::sync::Arc;
use std::time::Duration;

use alice_engine::core::signal::SignalHub;
use alice_engine::engine::AliceEngine;
use alice_engine::persist::instance::Instance;
use alice_engine::policy::{EngineConfig, EnvConfig};
use alice_engine::rpc::{EngineState, start_rpc_server};

use alice_frontend::api::{authenticated_api_routes, ApiState};
use alice_integration::mock_llm::{MockLlmServer, MockScript};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[test]
fn test_api_hello_world() {
    // === Setup ===
    let tmp = tempfile::TempDir::new().unwrap();
    let instances_dir = tmp.path().join("instances");
    let logs_dir = tmp.path().join("logs");
    let socket_path = tmp.path().join("test-rpc.sock");
    std::fs::create_dir_all(&instances_dir).unwrap();
    std::fs::create_dir_all(&logs_dir).unwrap();

    // Create tokio runtime for async operations
    let rt = tokio::runtime::Runtime::new().unwrap();

    // 1. Start mock LLM server with scripted response
    // The script uses {ACTION_TOKEN} placeholder — mock server replaces it
    // with the actual token extracted from the engine's system prompt.
    let mock = rt.block_on(async {
        MockLlmServer::start(vec![
            // Response to first inference: Thinking + SendMsg + Idle
            MockScript::with_user_assert(
                concat!(
                    "{ACTION_TOKEN}-thinking\n",
                    "User says hello, I should respond.\n",
                    "\n",
                    "{ACTION_TOKEN}-send_msg\n",
                    "user1\n",
                    "Hello from the zero-intrusion mock test!\n",
                    "\n",
                    "{ACTION_TOKEN}-idle\n",
                ),
                "Hello",
            ),
        ]).await
    });

    // 2. Create instance
    let instance = Instance::create(&instances_dir, "user1", Some("TestBot"), None, None).unwrap();
    let instance_id = instance.id.clone();
    drop(instance); // Close instance — engine will reopen it

    // 3. Configure engine
    let mut env_config = EnvConfig::from_env();
    env_config.rpc_socket = Some(socket_path.to_string_lossy().to_string());
    env_config.default_model = Some(mock.model_string());
    env_config.default_api_key = "test-api-key".to_string();
    let env_config = Arc::new(env_config);

    // 4. Start RPC server
    let signal_hub = SignalHub::new();
    let engine_state = Arc::new(EngineState::new(
        instances_dir.clone(),
        logs_dir.clone(),
        "user1".to_string(),
        signal_hub.clone(),
        EngineConfig::load(),
        env_config.clone(),
    ));
    rt.spawn(start_rpc_server(engine_state));

    // 5. Start AliceEngine.run() in OS thread
    let engine_instances = instances_dir.clone();
    let engine_logs = logs_dir.clone();
    let engine_env = env_config.clone();
    let engine_signal = signal_hub.clone();
    std::thread::spawn(move || {
        let mut engine = AliceEngine::new(engine_instances, engine_logs, engine_signal, engine_env);
        engine.run().ok();
    });

    // Wait for engine + RPC to initialize
    std::thread::sleep(Duration::from_millis(500));

    // 6. Build API router for HTTP requests
    let api_state = Arc::new(ApiState {
        instances_dir: instances_dir.clone(),
        rpc_socket: socket_path.to_string_lossy().to_string(),
    });
    let router = authenticated_api_routes().with_state(api_state);

    // === Act: Send message via HTTP API ===
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

    // === Wait: Poll for agent reply ===
    // Engine beat interval is 3s. Need at least 2 beats (auto-read + inference).
    // Poll every 2s for up to 30s.
    let mut found_reply = false;
    for attempt in 1..=15 {
        std::thread::sleep(Duration::from_secs(2));

        let replies_response = rt.block_on(async {
            let request = Request::builder()
                .method("GET")
                .uri(format!("/api/instances/{}/replies?after_id=0", instance_id))
                .body(Body::empty())
                .unwrap();
            router.clone().oneshot(request).await.unwrap()
        });

        if replies_response.status() != StatusCode::OK {
            continue;
        }

        let body_bytes = rt.block_on(async {
            axum::body::to_bytes(replies_response.into_body(), usize::MAX).await.unwrap()
        });
        let replies: Vec<alice_rpc::MessageInfo> = serde_json::from_slice(&body_bytes).unwrap_or_default();

        let agent_replies: Vec<_> = replies.iter().filter(|m| m.role == "agent").collect();
        if agent_replies.iter().any(|m| m.content.contains("Hello from the zero-intrusion mock test!")) {
            println!("✅ Agent reply found after {} attempts (~{}s)", attempt, attempt * 2);
            found_reply = true;
            break;
        }
    }

    assert!(found_reply, "Agent should have replied within 30 seconds");

    // === Privileged Referee: Verify current.txt ===
    let sessions_dir = instances_dir.join(&instance_id).join("memory").join("sessions");
    let current_path = sessions_dir.join("current.txt");
    let current = std::fs::read_to_string(&current_path).unwrap_or_default();
    // User message verified via HTTP API (chat.db).
    // current.txt records agent action output, not user messages directly.
    assert!(current.contains("Hello from the zero-intrusion mock test!"),
        "current.txt should contain agent's response");

    println!("✅ API Hello World (zero-intrusion): Full chain verified");
    println!("   HTTP POST → RPC → Engine beat loop → Mock LLM → chat.db → HTTP GET");
}
