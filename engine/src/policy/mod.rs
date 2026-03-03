//! # Policy — Strategy Parameters
//!
//! Design-time configuration parameters decided by the designer (user).
//! Guardian exempts this directory.
//!
//! - `EngineConfig` — Engine behavior & strategy (embedded at compile time)
//! - `EnvConfig` — Environment variables (read once at startup)

pub mod engine_config;
pub mod env_config;
pub use engine_config::EngineConfig;
pub use env_config::EnvConfig;
pub mod action_output;
pub mod messages;
