//! API Layer End-to-End Test: Episode 3 — Memory Lifecycle
//!
//! Tests the complete memory lifecycle through the full chain:
//!   Summary → Session Blocks → Forget → Roll (history compression)
//!
//! Settings: session_block_kb=1, session_blocks_limit=3
//!
//! Phases:
//! 1. Write calculator app → Summary with knowledge → 1 block
//! 2. Add division feature → Summary → 2 blocks
//! 3. Large script output → Forget → current shrinks
//! 4. Summary → 3rd block triggers Roll → oldest block compressed into history

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

/// Long summary text to ensure JSONL line > 1KB for block creation.
/// Each entry needs to exceed session_block_kb (1KB) so next summary creates a new block.
fn long_summary(phase: &str) -> String {
    format!(
        "Phase {p}: 用户要求开发Python计算器应用。agent分析了需求，创建了calc.py文件，\
        包含基本的四则运算功能。使用sys.argv接收命令行参数，支持加减乘除操作。\
        agent编写了完整的错误处理逻辑，包括参数数量检查、数值格式验证、除零保护。\
        测试验证了所有运算符的正确性。代码结构清晰，使用字典映射运算符到lambda函数。\
        用户对结果表示满意，确认功能符合预期。这个阶段的工作为后续功能扩展奠定了基础。\
        agent还检查了文件权限和编码格式，确保跨平台兼容性。整个开发过程顺利，\
        没有遇到重大技术障碍。agent在开发过程中保持了良好的代码规范，包括适当的注释、\
        清晰的变量命名和合理的函数划分。用户提出的所有需求都已完整实现并通过测试验证。\
        这次协作展示了agent高效的编码能力和对用户需求的准确理解。\
        后续计划包括添加更多数学运算支持和改进用户界面交互体验。\
        Phase {p} 的所有任务均已完成，等待用户的下一步指示。额外补充一些技术细节：\
        Python版本兼容性已验证（3.6+），f-string格式化输出清晰易读。\
        此外，agent对代码进行了性能优化分析，确认字典查找的时间复杂度为O(1)，\
        整体计算流程的空间复杂度为O(1)，不会随输入规模增长而消耗额外内存。\
        agent还验证了边界条件处理：超大数值运算、负数运算、浮点精度问题均已覆盖。\
        文档方面，agent为每个函数添加了docstring说明，包括参数类型、返回值和异常情况。\
        项目结构遵循Python最佳实践，使用if __name__ == '__main__'作为入口点。\
        测试覆盖率达到100%，包括正常路径和异常路径的所有分支。",
        p = phase
    )
}

/// Helper: send a user message via HTTP API.
fn send_message(rt: &tokio::runtime::Runtime, router: &axum::Router, instance_id: &str, content: &str) {
    let body = serde_json::json!({ "content": content });
    let response = rt.block_on(async {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/instances/{}/messages", instance_id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        router.clone().oneshot(request).await.unwrap()
    });
    assert_eq!(response.status(), StatusCode::OK, "Send message '{}' should succeed", content);
}

/// Helper: poll for agent reply containing expected text.
/// Returns true if found within timeout.
fn wait_for_reply(
    rt: &tokio::runtime::Runtime,
    router: &axum::Router,
    instance_id: &str,
    expected: &str,
    max_attempts: u32,
) -> bool {
    for attempt in 1..=max_attempts {
        std::thread::sleep(Duration::from_secs(2));
        let response = rt.block_on(async {
            let request = Request::builder()
                .method("GET")
                .uri(format!("/api/instances/{}/replies?after_id=0", instance_id))
                .body(Body::empty())
                .unwrap();
            router.clone().oneshot(request).await.unwrap()
        });
        if response.status() != StatusCode::OK { continue; }
        let body_bytes = rt.block_on(async {
            axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()
        });
        let replies: Vec<alice_rpc::MessageInfo> = serde_json::from_slice(&body_bytes).unwrap_or_default();
        if replies.iter().any(|m| m.role == "agent" && m.content.contains(expected)) {
            println!("  ✅ Reply '{}' found after {} attempts (~{}s)", expected, attempt, attempt * 2);
            return true;
        }
    }
    false
}

/// Helper: poll until a condition on a file is met.
fn wait_for_file_condition(
    path: &std::path::Path,
    condition: impl Fn(&str) -> bool,
    desc: &str,
    max_attempts: u32,
) -> bool {
    for attempt in 1..=max_attempts {
        std::thread::sleep(Duration::from_secs(2));
        if let Ok(content) = std::fs::read_to_string(path) {
            if condition(&content) {
                println!("  ✅ File condition '{}' met after {} attempts (~{}s)", desc, attempt, attempt * 2);
                return true;
            }
        }
    }
    false
}

/// Helper: poll until session block count reaches expected.
fn wait_for_block_count(
    sessions_dir: &std::path::Path,
    expected: usize,
    desc: &str,
    max_attempts: u32,
) -> bool {
    for attempt in 1..=max_attempts {
        std::thread::sleep(Duration::from_secs(2));
        let count = std::fs::read_dir(sessions_dir)
            .map(|entries| {
                entries.filter_map(|e| e.ok())
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
                    .count()
            }).unwrap_or(0);
        if count >= expected {
            println!("  ✅ Block count '{}': {} blocks after {} attempts (~{}s)", desc, count, attempt, attempt * 2);
            return true;
        }
    }
    false
}

#[test]
fn test_api_episode3_memory_lifecycle() {
    // === Setup ===
    let tmp = tempfile::TempDir::new().unwrap();
    let instances_dir = tmp.path().join("instances");
    let logs_dir = tmp.path().join("logs");
    let socket_path = tmp.path().join("test-rpc.sock");
    std::fs::create_dir_all(&instances_dir).unwrap();
    std::fs::create_dir_all(&logs_dir).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Calculator source code
    let calc_py = "import sys\nops = {'+': lambda a,b: a+b, '-': lambda a,b: a-b, '*': lambda a,b: a*b}\na, op, b = float(sys.argv[1]), sys.argv[2], float(sys.argv[3])\nprint(f\"{a} {op} {b} = {ops[op](a, b)}\")\n";

    // Knowledge content for Phase 1
    let knowledge_v1 = "# Agent Knowledge\n\n## Project\n- Developing Python calculator app calc.py\n- Supports add, subtract, multiply\n";

    // Build all mock responses upfront
    // Mock 1: Phase 1 — WriteFile + Script (blocking)
    let mock_1 = format!(
        "{{ACTION_TOKEN}}-thinking\nCreating calculator app\n\n\
         {{ACTION_TOKEN}}-write_file\ncalc.py\n{}\n\n\
         {{ACTION_TOKEN}}-script\npython3 calc.py 2 + 3\n",
        calc_py
    );

    // Mock 2: Phase 1 — SendMsg + Summary(with knowledge) + Idle
    let mock_2 = format!(
        "{{ACTION_TOKEN}}-send_msg\nuser1\nCalculator ready! 2+3=5 verified\n\n\
         {{ACTION_TOKEN}}-summary\n{}\n===KNOWLEDGE_{{ACTION_TOKEN_HEX}}===\n{}\n\n\
         {{ACTION_TOKEN}}-idle\n",
        long_summary("1"), knowledge_v1
    );

    // Mock 3: Phase 2 — ReplaceInFile + Script (blocking)
    let mock_3 = format!(
        "{{ACTION_TOKEN}}-thinking\nAdding division support\n\n\
         {{ACTION_TOKEN}}-replace_in_file\ncalc.py\n\
         <<<SEARCH_{{ACTION_TOKEN_HEX}}\n\
         '*': lambda a,b: a*b}}\n\
         ===REPLACE_{{ACTION_TOKEN_HEX}}\n\
         '*': lambda a,b: a*b, '/': lambda a,b: a/b}}\n\
         >>>END_{{ACTION_TOKEN_HEX}}\n\n\
         {{ACTION_TOKEN}}-script\npython3 calc.py 10 / 3\n"
    );

    // Mock 4: Phase 2 — SendMsg + Summary + Idle
    let mock_4 = format!(
        "{{ACTION_TOKEN}}-send_msg\nuser1\nDivision added! 10/3=3.333 verified\n\n\
         {{ACTION_TOKEN}}-summary\n{}\n\n\
         {{ACTION_TOKEN}}-idle\n",
        long_summary("2")
    );

    // Mock 5: Phase 3 — Script with large output (blocking)
    let mock_5 = concat!(
        "{ACTION_TOKEN}-thinking\nChecking system logs\n\n",
        "{ACTION_TOKEN}-script\n",
        "for i in $(seq 1 50); do echo \"[LOG] Service heartbeat #$i: status=healthy\"; done\n",
    ).to_string();

    // Mock 6: Phase 3 — Forget + SendMsg + Idle
    let mock_6 = concat!(
        "{ACTION_TOKEN}-thinking\nForgetting the large log output\n\n",
        "{ACTION_TOKEN}-forget\n",
        "{FIND_ACTION_ID:[LOG]}\n",
        "Checked 50 heartbeat logs, all services healthy\n\n",
        "{ACTION_TOKEN}-send_msg\nuser1\nDone! Logs show all services healthy\n\n",
        "{ACTION_TOKEN}-idle\n",
    ).to_string();

    // Mock 7: Phase 4 — Summary + SendMsg + Idle (triggers 3rd block → roll)
    let mock_7 = format!(
        "{{ACTION_TOKEN}}-summary\n{}\n\n\
         {{ACTION_TOKEN}}-send_msg\nuser1\nAll phases complete\n\n\
         {{ACTION_TOKEN}}-idle\n",
        long_summary("3")
    );

    let mock = rt.block_on(async {
        MockLlmServer::start(vec![
            MockScript::with_user_assert(mock_1, "计算器"),
            MockScript::new(mock_2),
            MockScript::with_user_assert(mock_3, "除法"),
            MockScript::new(mock_4),
            MockScript::with_user_assert(mock_5, "日志"),
            MockScript::new(mock_6),
            MockScript::new(mock_7),
        ]).await
    });

    // Create instance with small memory settings
    let initial_settings = serde_json::json!({
        "model": mock.model_string(),
        "api_key": "test-api-key",
        "session_block_kb": 1,
        "session_blocks_limit": 3
    });
    let settings_update: alice_rpc::SettingsUpdate = serde_json::from_value(initial_settings).unwrap();
    let instance = Instance::create(
        &instances_dir, "user1", Some("MemoryBot"), None, Some(&settings_update),
    ).unwrap();
    let instance_id = instance.id.clone();
    let sessions_dir = instances_dir.join(&instance_id).join("memory").join("sessions");
    let knowledge_path = instances_dir.join(&instance_id).join("memory").join("knowledge.md");
    let history_path = sessions_dir.join("history.txt");
    let workspace_dir = instances_dir.join(&instance_id).join("workspace");
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

    // ================================================================
    // Phase 1: Write calculator
    // ================================================================
    println!("\n=== Phase 1: Write calculator ===");
    send_message(&rt, &router, &instance_id, "帮我写个Python计算器，支持加减乘");

    assert!(wait_for_reply(&rt, &router, &instance_id, "Calculator ready", 20),
        "Phase 1: should get calculator reply within 40s");

    // Privileged referee: verify calc.py
    let calc_path = workspace_dir.join("calc.py");
    assert!(calc_path.exists(), "calc.py should exist in workspace");
    let calc_content = std::fs::read_to_string(&calc_path).unwrap();
    assert!(calc_content.contains("lambda"), "calc.py should contain lambda functions");

    // Wait for session block to be created (summary triggers block creation)
    assert!(wait_for_block_count(&sessions_dir, 1, "1+ blocks", 10),
        "Phase 1: should have at least 1 session block");

    // Knowledge should exist
    assert!(wait_for_file_condition(
        &knowledge_path,
        |content| content.contains("calculator") || content.contains("Calculator") || content.contains("calc"),
        "knowledge contains calculator",
        5,
    ), "Phase 1: knowledge should mention calculator");

    println!("  ✅ Phase 1 complete: calc.py + 1 block + knowledge");

    // ================================================================
    // Phase 2: Add division
    // ================================================================
    println!("\n=== Phase 2: Add division ===");
    send_message(&rt, &router, &instance_id, "加个除法功能");

    assert!(wait_for_reply(&rt, &router, &instance_id, "Division added", 20),
        "Phase 2: should get division reply within 40s");

    // Privileged referee: calc.py should have division
    let calc_content = std::fs::read_to_string(&calc_path).unwrap();
    assert!(calc_content.contains("'/': lambda") || calc_content.contains("\"/\": lambda"),
        "calc.py should have division operator");

    // Wait for 2 session blocks
    // Debug: print session blocks info
    std::thread::sleep(Duration::from_secs(4));
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            println!("  [DEBUG] Session file: {} ({} bytes)", name, size);
        }
    }
    assert!(wait_for_block_count(&sessions_dir, 2, "2+ blocks", 10),
        "Phase 2: should have at least 2 session blocks");

    println!("  ✅ Phase 2 complete: division added + 2 blocks");

    // ================================================================
    // Phase 3: Forget
    // ================================================================
    println!("\n=== Phase 3: Forget ===");
    send_message(&rt, &router, &instance_id, "查个日志然后forget释放空间");

    assert!(wait_for_reply(&rt, &router, &instance_id, "all services healthy", 20),
        "Phase 3: should get forget confirmation within 40s");

    // Privileged referee: current should contain [已提炼]
    let current_path = sessions_dir.join("current.txt");
    assert!(wait_for_file_condition(
        &current_path,
        |content| content.contains("已提炼"),
        "current contains 已提炼",
        5,
    ), "Phase 3: current should contain forgotten marker.\nCurrent content:\n{}", 
        std::fs::read_to_string(&current_path).unwrap_or_default());

    println!("  ✅ Phase 3 complete: forget verified");

    // ================================================================
    // Phase 4: Summary triggers Roll
    // ================================================================
    println!("\n=== Phase 4: Summary + Roll ===");
    send_message(&rt, &router, &instance_id, "继续吧");

    assert!(wait_for_reply(&rt, &router, &instance_id, "All phases complete", 20),
        "Phase 4: should get completion reply within 40s");

    // Wait for roll to complete (async background thread)
    // Roll deletes oldest block and creates history.txt
    assert!(wait_for_file_condition(
        &history_path,
        |content| content.contains("calculator") || content.contains("Calculator") || content.contains("calc"),
        "history.txt contains calculator",
        15,
    ), "Phase 4: history.txt should exist with compressed content");

    // After roll, should have 2 blocks (3rd created, 1st deleted)
    let block_count = std::fs::read_dir(&sessions_dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
                .count()
        }).unwrap_or(0);
    assert_eq!(block_count, 2, "After roll: should have 2 session blocks (oldest deleted)");

    // Knowledge should still exist
    let knowledge = std::fs::read_to_string(&knowledge_path).unwrap();
    assert!(!knowledge.is_empty(), "Knowledge should persist through all phases");

    // calc.py should still exist
    assert!(calc_path.exists(), "calc.py should still exist after roll");

    println!("  ✅ Phase 4 complete: roll verified, history.txt created");
    println!("\n✅ Episode 3 Memory Lifecycle: All phases passed!");
    println!("   Summary → Blocks → Forget → Roll — full lifecycle verified");
}
