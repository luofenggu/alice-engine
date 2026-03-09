//! Environment variable configuration.
//!
//! All environment variable names are defined here — the single source of truth
//! for the engine-environment contract. Constructed once at startup, then passed
//! to all components via Arc.

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

    /// Public host address (`ALICE_HOST`).
    pub host: Option<String>,
    /// Shell environment description for prompts (auto-detected at startup).
    pub shell_env: String,
    /// Whether to log inference input (`ALICE_INFER_LOG_IN`).
    pub infer_log_enabled: bool,
    /// Days to retain inference logs (`ALICE_INFER_LOG_RETENTION_DAYS`, default: 7).
    pub infer_log_retention_days: u64,
    /// Default API key for new instances (`ALICE_DEFAULT_API_KEY`).
    pub default_api_key: String,
    /// Default model for new instances (`ALICE_DEFAULT_MODEL`).
    pub default_model: Option<String>,
    /// Auth secret for HTTP API (`ALICE_AUTH_SECRET`).
    pub auth_secret: String,
    /// Skip auth for development (`ALICE_SKIP_AUTH`).
    pub skip_auth: bool,
    /// HTML frontend directory (`ALICE_HTML_DIR`).
    pub html_dir: Option<String>,
    /// HTTP listen port (`ALICE_HTTP_PORT`, default: 8081).
    pub http_port: u16,

}

impl EnvConfig {
    // ─── CLI / startup defaults ─────────────────────────────────
    /// Default base directory when neither env var nor CLI arg is given.
    pub const DEFAULT_DIR: &str = ".";
    /// Default subdirectory name for instance storage.
    pub const DEFAULT_INSTANCES_DIR: &str = "instances";
    /// Default subdirectory name for log storage.
    pub const DEFAULT_LOGS_DIR: &str = "logs";
    /// Default subdirectory name for HTML static files.
    pub const DEFAULT_HTML_DIR: &str = "html";

    pub const DEFAULT_AUTH_SECRET: &str = "alice-local-default";
    /// CLI positional argument index for instances directory.
    pub const CLI_ARG_INSTANCES: usize = 1;
    /// CLI positional argument index for logs directory.
    pub const CLI_ARG_LOGS: usize = 2;

    /// Read all `ALICE_*` environment variables and construct the config.
    pub fn from_env() -> Self {
        Self {
            base_dir: std::env::var("ALICE_BASE_DIR").ok(),
            instances_dir: std::env::var("ALICE_INSTANCES_DIR").ok(),
            logs_dir: std::env::var("ALICE_LOGS_DIR").ok(),

            host: std::env::var("ALICE_HOST").ok().filter(|s| !s.is_empty()),
            shell_env: Self::detect_shell_env(),
            infer_log_enabled: std::env::var("ALICE_INFER_LOG_IN")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            infer_log_retention_days: std::env::var("ALICE_INFER_LOG_RETENTION_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            default_api_key: std::env::var("ALICE_DEFAULT_API_KEY").unwrap_or_default(),
            default_model: std::env::var("ALICE_DEFAULT_MODEL").ok(),
            auth_secret: std::env::var("ALICE_AUTH_SECRET")
                .unwrap_or_else(|_| Self::DEFAULT_AUTH_SECRET.to_string()),
            skip_auth: std::env::var("ALICE_SKIP_AUTH")
                .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false),
            html_dir: std::env::var("ALICE_HTML_DIR").ok(),
            http_port: std::env::var("ALICE_HTTP_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8081),

        }
    }

    /// Auto-detect shell environment from system info.
    /// Returns a description like "Linux系统（x86_64），请生成bash脚本".
    fn detect_shell_env() -> String {
        let os = std::process::Command::new("uname")
            .arg("-s")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Linux".to_string());

        let arch = std::process::Command::new("uname")
            .arg("-m")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "x86_64".to_string());

        format!("{}系统（{}），请生成bash脚本", os, arch)
    }
}