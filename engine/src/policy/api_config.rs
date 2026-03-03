//! API behavior configuration — embedded at compile time from api.toml.

use serde::Deserialize;

/// API configuration loaded from embedded api.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiConfig {
    pub rpc: RpcConfig,
    pub file_browse: FileBrowseConfig,
    pub action: ActionConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcConfig {
    pub min_page_size: i64,
    pub max_page_size: i64,
    pub heartbeat_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionConfig {
    pub preview_head_lines: usize,
    pub preview_tail_lines: usize,
    pub preview_threshold: usize,
    pub max_result_bytes: usize,
    pub truncate_display: usize,
    pub truncate_detail: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileBrowseConfig {
    pub binary_extensions: Vec<String>,
    pub hidden_dirs: Vec<String>,
    pub hidden_prefix: String,
    pub max_file_size: u64,
}

impl ApiConfig {
    /// Load from the embedded api.toml (compiled into the binary).
    /// Get the global ApiConfig singleton (initialized on first call).
    pub fn get() -> &'static Self {
        static INSTANCE: std::sync::OnceLock<ApiConfig> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(|| Self::load())
    }

    pub fn load() -> Self {
        let toml_str = include_str!("api.toml");
        toml::from_str(toml_str).expect("failed to parse embedded api.toml")
    }
}

impl FileBrowseConfig {
    /// Check if a filename has a binary extension.
    pub fn is_binary_file(&self, name: &str) -> bool {
        self.binary_extensions.iter().any(|ext| name.ends_with(ext.as_str()))
    }

    /// Check if a directory name should be hidden.
    pub fn is_hidden_dir(&self, name: &str) -> bool {
        self.hidden_dirs.iter().any(|d| d == name)
    }

    /// Check if a filename starts with the hidden prefix.
    pub fn is_hidden_file(&self, name: &str) -> bool {
        name.starts_with(&self.hidden_prefix)
    }
}