//! # Policy — Strategy Parameters
//!
//! Design-time configuration parameters decided by the designer (user).
//! Guardian exempts this directory.
//!
//! - `ApiConfig` — API behavior & action strategy (embedded at compile time)
//! - `EnvConfig` — Environment variables (read once at startup)

pub mod api_config;
pub mod env_config;
pub use api_config::ApiConfig;
pub use env_config::EnvConfig;
