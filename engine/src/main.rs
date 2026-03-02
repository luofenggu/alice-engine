//! Alice Engine entry point.
//!
//! Runs both the Engine heartbeat loop and the embedded Web server
//! in the same process.
//!
//! All behavior is configured via environment variables and/or CLI arguments.
//! The engine itself makes no assumptions about its deployment environment.
//!
//! ## Configuration (env > CLI args > defaults)
//!
//! | Variable | CLI arg | Default | Description |
//! |----------|---------|---------|-------------|
//! | ALICE_BASE_DIR | - | exe dir | Root for all relative paths |
//! | ALICE_INSTANCES_DIR | $1 | base/instances | Instance storage |
//! | ALICE_LOGS_DIR | $2 | base/logs | Log storage |
//! | ALICE_PORT | $3 | 8080 | Web server port |
//! | ALICE_WEB_DIR | $4 | base/web | Static web files |
//! | ALICE_BIND_ADDR | - | 0.0.0.0 | Bind address |
//! | ALICE_AUTO_BROWSER | - | false | Open browser on start |
//! | ALICE_SETUP_ENABLED | - | false | Enable setup page |
//! | ALICE_SKIP_AUTH | - | false | Skip authentication |
//! | ALICE_AUTH_SECRET | - | alice-local-default | Auth secret |
//! | ALICE_USER_ID | - | user | Default user ID |
//! | ALICE_PID_FILE | - | base/alice-engine.pid | PID file path |
//!
//! @TRACE: INSTANCE, BEAT

use std::path::PathBuf;
use std::sync::Arc;

use alice_engine::engine::AliceEngine;
use alice_engine::web::{self, AppState};

/// Read a config value: env var > CLI arg > None.
fn env_or_arg(env_key: &str, arg: Option<&String>) -> Option<String> {
    std::env::var(env_key).ok().or_else(|| arg.cloned())
}

/// Open the default browser to the given URL.
fn open_browser(url: &str) {
    let _result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", url]).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
}

/// Parse a boolean env var (true/1/yes → true, anything else → false).
fn env_bool(key: &str) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(false)
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
                // Infer from first CLI arg (instances_dir's parent)
                PathBuf::from(&args[1])
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            } else {
                // Fallback: directory containing the binary
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

    let web_port: u16 = env_or_arg("ALICE_PORT", args.get(3))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);

    let web_dir = env_or_arg("ALICE_WEB_DIR", args.get(4))
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join("web"));

    // Behavior flags from environment
    let bind_addr = std::env::var("ALICE_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0".to_string());
    let auto_browser = env_bool("ALICE_AUTO_BROWSER");
    let setup_enabled = env_bool("ALICE_SETUP_ENABLED");
    let skip_auth = env_bool("ALICE_SKIP_AUTH");

    // Auth config
    let auth_secret = std::env::var("ALICE_AUTH_SECRET")
        .unwrap_or_else(|_| "alice-local-default".to_string());
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
    tracing::info!("  Web: {}", web_dir.display());
    tracing::info!("  Bind: {}:{}", bind_addr, web_port);

    // Create shared web state
    let app_state = Arc::new(AppState::new(
        instances_dir.clone(),
        logs_dir.clone(),
        auth_secret,
        user_id,
        web_dir,
        bind_addr,
        setup_enabled,
        skip_auth,
        base_dir.clone(),
    ));

    // Start RPC server (Unix socket, for Leptos frontend)
    let rpc_state = app_state.clone();
    tokio::spawn(async move {
        alice_engine::rpc::start_rpc_server(rpc_state).await;
    });

    // Start Engine in a dedicated OS thread
    let engine_instances_dir = instances_dir.clone();
    let engine_logs_dir = logs_dir.clone();
    let engine_handle = std::thread::spawn(move || {
        let mut engine = AliceEngine::new(engine_instances_dir, engine_logs_dir);
        if let Err(e) = engine.run() {
            tracing::error!("Engine error: {}", e);
        }
        tracing::info!("Engine thread exited, terminating process.");
        std::process::exit(0);
    });

    // Auto-open browser if configured
    if auto_browser {
        let url = format!("http://localhost:{}", web_port);
        tracing::info!("Opening browser: {}", url);
        let url_clone = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            open_browser(&url_clone);
        });
    }

    // Start Web server on tokio runtime
    tracing::info!("Starting web server on port {}...", web_port);
    web::start_server(app_state, web_port).await?;

    // If web server exits, wait for engine thread
    engine_handle.join().ok();

    tracing::info!("Alice Engine shut down.");
    Ok(())
}
