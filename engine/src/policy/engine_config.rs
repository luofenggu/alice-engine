//! Engine configuration — embedded at compile time from engine.toml.

use serde::Deserialize;
use std::collections::HashMap;

/// Engine configuration loaded from embedded engine.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct EngineConfig {
    pub engine: EnginePolicyConfig,
    pub memory: MemoryPolicyConfig,
    pub streaming: StreamingConfig,
    pub rpc: RpcConfig,
    pub file_browse: FileBrowseConfig,
    pub llm: LlmPolicyConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamingConfig {
    pub poll_interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnginePolicyConfig {
    pub beat_interval_secs: u64,
    pub main_loop_interval_secs: u64,
    pub error_backoff_secs: u64,
    pub log_rotate_max_mb: u64,
    pub disk_check_interval_beats: u32,
    pub disk_min_available_mb: u64,
    pub sandbox_user_prefix: String,
    pub test_instance_prefix: String,
    pub test_instance_max_beats: u32,
    pub inference_backoff_base_secs: u64,
    pub inference_backoff_max_exponent: u32,
    pub inference_backoff_cap_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryPolicyConfig {
    pub session_blocks_limit: u32,
    pub session_block_kb: u32,
    pub history_kb: u32,
    pub message_truncate_length: usize,
    pub safety_max_consecutive_beats: u32,
    pub safety_cooldown_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcConfig {
    pub min_page_size: i64,
    pub max_page_size: i64,
    pub heartbeat_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileBrowseConfig {
    pub binary_extensions: Vec<String>,
    pub hidden_dirs: Vec<String>,
    pub hidden_prefix: String,
    pub max_file_size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmPolicyConfig {
    pub max_tokens: u32,
    pub temperature: f64,
    pub default_model: String,
    pub providers: HashMap<String, String>,
}

impl EngineConfig {
    /// Get the global EngineConfig singleton (initialized on first call).
    pub fn get() -> &'static Self {
        static INSTANCE: std::sync::OnceLock<EngineConfig> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(|| Self::load())
    }

    pub fn load() -> Self {
        let toml_str = include_str!("engine.toml");
        toml::from_str(toml_str).expect("failed to parse embedded engine.toml")
    }
}

impl LlmPolicyConfig {
    /// Resolve a model string "provider@model_id" into (api_url, model_id).
    ///
    /// Looks up the provider in the configured providers map.
    /// If no '@' separator, treats the whole string as model_id and uses
    /// the first provider URL as fallback.
    pub fn resolve_model(&self, model: &str, url_override: Option<&str>) -> (String, String) {
        let model_id = if let Some(pos) = model.find('@') {
            model[pos + 1..].to_string()
        } else {
            model.to_string()
        };

        if let Some(url) = url_override {
            return (url.to_string(), model_id);
        }

        if let Some(pos) = model.find('@') {
            let provider = &model[..pos];
            let api_url = self.providers.get(provider)
                .cloned()
                .unwrap_or_else(|| {
                    tracing::warn!("Unknown provider '{}', using as direct URL", provider);
                    provider.to_string()
                });
            (api_url, model_id)
        } else {
            let fallback_url = self.providers.values().next()
                .cloned()
                .unwrap_or_default();
            (fallback_url, model_id)
        }
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
