//! Environment variable configuration.
//!
//! All environment variable names are defined here — the single source of truth
//! for the engine-environment contract. Constructed once at startup, then passed
//! to all components via Arc.

use std::path::PathBuf;

/// Centralized environment configuration.
///
/// Every `std::env::var("ALICE_*")` call in the engine is replaced by a field
/// on this struct. Guardian-exempt (lives in persist/).
#[derive(Clone, Debug)]
pub struct EnvConfig {
    /// Base directory for all relative paths (`ALICE_BASE_DIR`).
    pub base_dir: Option<String>,
    /// Instance storage directory (`ALICE_INSTANCES_DIR`).
    pub instances_dir: Option<String>,
    /// Log storage directory (`ALICE_LOGS_DIR`).
    pub logs_dir: Option<String>,
    /// User ID (`ALICE_USER_ID`, default: `"user"`).
    pub user_id: String,
    /// PID file path (`ALICE_PID_FILE`).
    pub pid_file: Option<PathBuf>,
    /// Public host address (`ALICE_HOST`).
    pub host: Option<String>,
    /// Shell environment description for prompts (`ALICE_SHELL_ENV`).
    pub shell_env: String,
    /// Whether to log inference input (`ALICE_INFER_LOG_IN`).
    pub infer_log_enabled: bool,
    /// Days to retain inference logs (`ALICE_INFER_LOG_RETENTION_DAYS`, default: 7).
    pub infer_log_retention_days: u64,
    /// RPC Unix socket path (`ALICE_RPC_SOCKET`).
    pub rpc_socket: Option<String>,
    /// Default API key for new instances (`ALICE_DEFAULT_API_KEY`).
    pub default_api_key: String,
    /// Default model for new instances (`ALICE_DEFAULT_MODEL`).
    pub default_model: Option<String>,
    /// Graceful shutdown signal file path (`ALICE_SHUTDOWN_SIGNAL_FILE`,
    /// default: `/var/run/alice-engine-shutdown.signal`).
    pub shutdown_signal_file: PathBuf,
}

impl EnvConfig {
    /// Read all `ALICE_*` environment variables and construct the config.
    pub fn from_env() -> Self {
        Self {
            base_dir: std::env::var("ALICE_BASE_DIR").ok(),
            instances_dir: std::env::var("ALICE_INSTANCES_DIR").ok(),
            logs_dir: std::env::var("ALICE_LOGS_DIR").ok(),
            user_id: std::env::var("ALICE_USER_ID")
                .unwrap_or_else(|_| "user".to_string()),
            pid_file: std::env::var("ALICE_PID_FILE").ok().map(PathBuf::from),
            host: std::env::var("ALICE_HOST").ok().filter(|s| !s.is_empty()),
            shell_env: std::env::var("ALICE_SHELL_ENV")
                .unwrap_or_else(|_| "Linux系统，请生成bash脚本".to_string()),
            infer_log_enabled: std::env::var("ALICE_INFER_LOG_IN")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            infer_log_retention_days: std::env::var("ALICE_INFER_LOG_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            rpc_socket: std::env::var("ALICE_RPC_SOCKET").ok(),
            default_api_key: std::env::var("ALICE_DEFAULT_API_KEY")
                .unwrap_or_default(),
            default_model: std::env::var("ALICE_DEFAULT_MODEL").ok(),
            shutdown_signal_file: std::env::var("ALICE_SHUTDOWN_SIGNAL_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/var/run/alice-engine-shutdown.signal")),
        }
    }

    /// Resolve PID file path, using env var or default based on instances directory.
    pub fn pid_file_path(&self, instances_base: &std::path::Path) -> PathBuf {
        self.pid_file.clone().unwrap_or_else(|| {
            instances_base.parent().unwrap_or(instances_base).join("alice-engine.pid")
        })
    }
}