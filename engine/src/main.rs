//! Alice Engine entry point.
//!
//! Runs the Engine heartbeat loop and RPC server.
//! The engine itself makes no assumptions about its deployment environment.
//!
//! ## Configuration (env > CLI args > defaults)
//!
//! | Variable | CLI arg | Default | Description |
//! |----------|---------|---------|-------------|
//! | ALICE_BASE_DIR | - | exe dir | Root for all relative paths |
//! | ALICE_INSTANCES_DIR | $1 | base/instances | Instance storage |
//! | ALICE_LOGS_DIR | $2 | base/logs | Log storage |
//! | ALICE_USER_ID | - | user | Default user ID |
//! | ALICE_PID_FILE | - | base/alice-engine.pid | PID file path |
//!
//! @TRACE: INSTANCE, BEAT

use std::path::PathBuf;
use std::sync::Arc;

use alice_engine::engine::AliceEngine;
use alice_engine::core::signal::SignalHub;
use alice_engine::rpc::EngineState;

/// Read a config value: env var > CLI arg > None.
fn env_or_arg(env_key: &str, arg: Option<&String>) -> Option<String> {
    std::env::var(env_key).ok().or_else(|| arg.cloned())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing with local timestamps
    alice_engine::logging::init_tracing();

    let args: Vec<String> = std::env::args().collect();

    // Base directory: all relative paths resolve from here
    let base_dir = std::env::var("ALICE_BASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            if args.len() > 1 {
                PathBuf::from(&args[1])
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            } else {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_else(|| PathBuf::from("."))
            }
        });

    // Derive paths: env > CLI arg > base_dir default
    let instances_dir = env_or_arg("ALICE_INSTANCES_DIR", args.get(1))
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join("instances"));

    let logs_dir = env_or_arg("ALICE_LOGS_DIR", args.get(2))
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join("logs"));

    let user_id = std::env::var("ALICE_USER_ID")
        .unwrap_or_else(|_| "user".to_string());

    // Ensure directories exist
    std::fs::create_dir_all(&instances_dir).ok();
    std::fs::create_dir_all(&logs_dir).ok();

    // Set up crash log hook
    alice_engine::logging::setup_crash_hook(&logs_dir);

    tracing::info!("Alice Engine (Rust) starting...");
    tracing::info!("  Base dir: {}", base_dir.display());
    tracing::info!("  Instances: {}", instances_dir.display());
    tracing::info!("  Logs: {}", logs_dir.display());

    // Create shared signal hub (memory-based inter-thread signaling)
    let signal_hub = SignalHub::new();

    // Create engine state (shared between RPC and engine)
    let engine_state = Arc::new(EngineState::new(
        instances_dir.clone(),
        logs_dir.clone(),
        user_id,
        signal_hub.clone(),
    ));

    // Start RPC server (Unix socket, for Leptos frontend)
    let rpc_state = engine_state.clone();
    tokio::spawn(async move {
        alice_engine::rpc::start_rpc_server(rpc_state).await;
    });

    // Start Engine in a dedicated OS thread
    let engine_instances_dir = instances_dir.clone();
    let engine_logs_dir = logs_dir.clone();
    let engine_handle = std::thread::spawn(move || {
        let mut engine = AliceEngine::new(engine_instances_dir, engine_logs_dir, signal_hub);
        if let Err(e) = engine.run() {
            tracing::error!("Engine error: {}", e);
        }
        tracing::info!("Engine thread exited, terminating process.");
        std::process::exit(0);
    });

    // Wait for engine thread (RPC server runs in background)
    engine_handle.join().ok();

    tracing::info!("Alice Engine shut down.");
    Ok(())
}
