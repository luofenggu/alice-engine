//! API Layer End-to-End Test: Episode 2 — File Operations
//!
//! Tests WriteFile + Script + ReplaceInFile + SendMsg through the full chain:
//!   HTTP API → RPC → AliceEngine.run() → Mock LLM → filesystem I/O
//!
//! Flow:
//! 1. User: "帮我写个文件"
//! 2. Agent: WriteFile(hello.txt) + Script(cat hello.txt) — blocking
//! 3. Agent: SendMsg("文件写好了") + Idle
//! 4. Privileged referee: verify workspace/hello.txt

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
fn test_api_episode2_file_ops() {
    let tmp = tempfile::TempDir::new().unwrap();
    let instances_dir = tmp.path().join("instances");
    let logs_dir = tmp.path().join("logs");
    let socket_path = tmp.path().join("test-rpc.sock");
    std::fs::create_dir_all(&instances_dir).unwrap();
    std::fs::create_dir_all(&logs_dir).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Mock LLM: 2 responses
    // Response 1: WriteFile + Script (blocking — beat ends after script)
    // Response 2: SendMsg + Idle
    let mock = rt.block_on(async {
        MockLlmServer::start(vec![
            // Beat after auto-read: write file then run script
            MockScript::with_user_assert(concat!(
                "{ACTION_TOKEN}-thinking\n",
                "用户要写文件，我来操作\n",
                "\n",
                "{ACTION_TOKEN}-write_file\n",
                "hello.txt\n",
                "Hello World\n",
                "第二行\n",
                "\n",
                "{ACTION_TOKEN}-script\n",
                "cat hello.txt && echo done\n",
            ), "写个文件"),
            // Next beat (auto-continues after blocking script): report back
            MockScript::new(concat!(
                "{ACTION_TOKEN}-send_msg\n",
                "user1\n",
                "文件写好了，内容已确认\n",
                "\n",
                "{ACTION_TOKEN}-idle\n",
                "60\n",
            )),
        ]).await
    });

    // Create instance with mock LLM settings
    let settings = alice_rpc::SettingsUpdate {
        model: Some(mock.model_string()),
        api_key: Some("test-api-key".into()),
        ..Default::default()
    };
    let instance = Instance::create(&instances_dir, "user1", Some("TestBot"), None, Some(&settings)).unwrap();
    let instance_id = instance.id.clone();
    drop(instance);

    // Configure engine
    let mut env_config = EnvConfig::from_env();
    env_config.rpc_socket = Some(socket_path.to_string_lossy().to_string());
    env_config.default_model = Some(mock.model_string());
    env_config.default_api_key = "test-api-key".to_string();
    let env_config = Arc::new(env_config);

    // Start RPC server
    let signal_hub = SignalHub::new();
    let engine_state = Arc::new(EngineState::new(
        instances_dir.clone(), logs_dir.clone(), "user1".to_string(),
        signal_hub.clone(), EngineConfig::load(), env_config.clone(),
    ));
    rt.spawn(start_rpc_server(engine_state));

    // Start engine in OS thread
    let engine_instances = instances_dir.clone();
    let engine_logs = logs_dir.clone();
    let engine_env = env_config.clone();
    let engine_signal = signal_hub.clone();
    std::thread::spawn(move || {
        let mut engine = AliceEngine::new(engine_instances, engine_logs, engine_signal, engine_env);
        engine.run().ok();
    });

    std::thread::sleep(Duration::from_millis(500));

    // Build API router
    let api_state = Arc::new(ApiState {
        instances_dir: instances_dir.clone(),
        rpc_socket: socket_path.to_string_lossy().to_string(),
    });
    let router = authenticated_api_routes().with_state(api_state);

    // === Act: Send message ===
    let send_response = rt.block_on(async {
        let body = serde_json::json!({ "content": "帮我写个文件" });
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/instances/{}/messages", instance_id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        router.clone().oneshot(request).await.unwrap()
    });
    assert_eq!(send_response.status(), StatusCode::OK);

    // === Wait for agent reply ===
    let mut found_reply = false;
    for attempt in 1..=20 {
        std::thread::sleep(Duration::from_secs(2));

        let replies_response = rt.block_on(async {
            let request = Request::builder()
                .method("GET")
                .uri(format!("/api/instances/{}/replies?after_id=0", instance_id))
                .body(Body::empty())
                .unwrap();
            router.clone().oneshot(request).await.unwrap()
        });

        if replies_response.status() != StatusCode::OK { continue; }

        let body_bytes = rt.block_on(async {
            axum::body::to_bytes(replies_response.into_body(), usize::MAX).await.unwrap()
        });
        let replies: Vec<alice_rpc::MessageInfo> = serde_json::from_slice(&body_bytes).unwrap_or_default();

        if replies.iter().any(|m| m.role == "agent" && m.content.contains("文件写好了")) {
            println!("✅ Agent reply found after {} attempts (~{}s)", attempt, attempt * 2);
            found_reply = true;
            break;
        }
    }
    assert!(found_reply, "Agent should have replied '文件写好了' within 40 seconds");

    // === Privileged Referee ===

    // 1. Verify workspace file exists with correct content
    let file_path = instances_dir.join(&instance_id).join("workspace").join("hello.txt");
    assert!(file_path.exists(), "hello.txt should exist in workspace");
    let file_content = std::fs::read_to_string(&file_path).unwrap();
    assert!(file_content.trim() == "Hello World
第二行", "file content should match, got: {:?}", file_content);

    // 2. Verify current.txt contains script output
    let current_path = instances_dir.join(&instance_id).join("memory").join("sessions").join("current.txt");
    let current = std::fs::read_to_string(&current_path).unwrap_or_default();
    assert!(current.contains("Hello World"), "current should contain script stdout");
    assert!(current.contains("done"), "current should contain 'done' from echo");
    assert!(current.contains("write_file") || current.contains("write file"),
        "current should record write_file action");

    println!("✅ Episode 2 File Ops: Full chain verified");
    println!("   WriteFile → hello.txt created, Script → output captured, SendMsg → reply delivered");
}

