//! Alice Engine entry point.
//!
//! Runs the Engine heartbeat loop and HTTP API server.
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
//! | ALICE_HTTP_PORT | - | 8081 | HTTP server port |
//! | ALICE_HTML_DIR | - | base/html | Static HTML directory |
//! | ALICE_AUTH_SECRET | - | (none) | Auth password |
//!
//! @TRACE: INSTANCE, BEAT

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use alice_engine::engine::AliceEngine;
use alice_engine::core::signal::SignalHub;
use alice_engine::policy::EnvConfig;
use alice_engine::api::state::EngineState;
use alice_engine::api::routes;

/// Resolve a path: env value > CLI arg > None.
fn env_or_arg(env_val: Option<&str>, arg: Option<&String>) -> Option<String> {
    env_val.map(|s| s.to_string()).or_else(|| arg.cloned())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing with local timestamps
    alice_engine::logging::init_tracing();

    let args: Vec<String> = std::env::args().collect();

    // Load all environment variables once
    let env_config = Arc::new(EnvConfig::from_env());

    // Base directory: all relative paths resolve from here
    let base_dir = env_config.base_dir.as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if args.len() > EnvConfig::CLI_ARG_INSTANCES {
                PathBuf::from(&args[EnvConfig::CLI_ARG_INSTANCES])
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from(EnvConfig::DEFAULT_DIR))
            } else {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_else(|| PathBuf::from(EnvConfig::DEFAULT_DIR))
            }
        });

    // Derive paths: env > CLI arg > base_dir default
    let instances_dir = env_or_arg(env_config.instances_dir.as_deref(), args.get(EnvConfig::CLI_ARG_INSTANCES))
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join(EnvConfig::DEFAULT_INSTANCES_DIR));

    let logs_dir = env_or_arg(env_config.logs_dir.as_deref(), args.get(EnvConfig::CLI_ARG_LOGS))
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join(EnvConfig::DEFAULT_LOGS_DIR));

    // HTML directory for static file serving
    let html_dir = env_config.html_dir.as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join(EnvConfig::DEFAULT_HTML_DIR));

    // HTTP port
    let http_port = env_config.http_port;

    // Ensure directories exist
    let uploads_dir = alice_engine::persist::instance::InstanceStore::resolve_uploads_dir(&base_dir);
    std::fs::create_dir_all(&instances_dir).ok();
    std::fs::create_dir_all(&logs_dir).ok();
    std::fs::create_dir_all(&uploads_dir).ok();

    // Set up crash log hook
    alice_engine::logging::setup_crash_hook(&logs_dir);

    tracing::info!("Alice Engine (Rust) starting...");
    tracing::info!("  Base dir: {}", base_dir.display());
    tracing::info!("  Instances: {}", instances_dir.display());
    tracing::info!("  Logs: {}", logs_dir.display());
    tracing::info!("  HTML dir: {}", html_dir.display());
    tracing::info!("  HTTP port: {}", http_port);

    // Create shared signal hub (memory-based inter-thread signaling)
    let signal_hub = SignalHub::new();

    // ── Global Settings: three-layer merge ──
    // seed = env vars ∪ engine.toml defaults (env wins)
    // global = seed ∪ persisted global_settings.json (persisted wins)
    let (_global_settings, global_settings_store) =
        alice_engine::persist::GlobalSettingsStore::init(&base_dir, &env_config)?;
    tracing::info!("Global settings initialized");

    // Create engine state (shared between HTTP server and engine)
    let engine_config = alice_engine::policy::EngineConfig::load();
    let engine_state = Arc::new(EngineState::new(
        instances_dir.clone(),
        logs_dir.clone(),
        html_dir,
        env_config.user_id.clone(),
        signal_hub.clone(),
        engine_config,
        env_config.clone(),
        global_settings_store.clone(),
    ));

    // Build HTTP router (routes, auth, embedded HTML — all in api/)
    let app = routes::build_router(engine_state.clone());

    // Start HTTP server
    let addr = SocketAddr::from(([0, 0, 0, 0], http_port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("HTTP server listening on {}", addr);

    let http_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
            tracing::error!("HTTP server error: {}", e);
        }
    });

    // Start Engine in a dedicated OS thread
    let engine_instances_dir = instances_dir.clone();
    let engine_logs_dir = logs_dir.clone();
    let engine_env_config = env_config.clone();
    let engine_gs_store = global_settings_store.clone();
    let engine_handle = std::thread::spawn(move || {
        let mut engine = AliceEngine::new(engine_instances_dir, engine_logs_dir, signal_hub, engine_env_config, engine_gs_store);
        if let Err(e) = engine.run() {
            tracing::error!("Engine error: {}", e);
        }
        tracing::info!("Engine thread exited, terminating process.");
        std::process::exit(0);
    });

    // Wait for engine thread (HTTP server runs in background)
    engine_handle.join().ok();

    // Cancel HTTP server if engine exits
    http_handle.abort();

    tracing::info!("Alice Engine shut down.");
    Ok(())
}
