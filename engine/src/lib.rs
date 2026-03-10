//! # Alice Engine (Rust)
//!
//! The soul container, rewritten in Rust for compile-time safety.
//! Rust's borrow checker is the "免死金牌" — implicit contracts
//! that cause 贯穿伤 in Java are caught at compile time here.
//!
//! ## @HUB - Crate Root
//!
//! Module map:
//! - [`shell`] — Shell command execution (@TRACE: SHELL)
//! - [`core`] — Alice instance, Transaction, React loop (@TRACE: BEAT, INSTANCE)
//! - [`action`] — Action type system and execution (@TRACE: ACTION)
//! - [`llm`] — LLM inference and streaming (@TRACE: INFER, STREAM)
//! - [`inference`] — LLM inference protocol definitions (request/response)
//! - [`prompt`] — Prompt data extraction from Alice state
//! - [`engine`] — Multi-instance management (@TRACE: INSTANCE, RESTART)
//! - [`persist`] — Persistence primitives and structs
//! - [`logging`] — Log initialization, rotation, inference logs
//!
//! ## Trace Log Format
//!
//! All trace logs follow: `[TRACE_ID-instance_id] message`
//! Example: `[MEMORY-alice] Reading persistent.txt`

/// Re-exported from inference module — safe template rendering.
pub use inference::safe_render;

pub mod action;
pub mod core;
pub mod external;
pub mod inference;
pub mod logging;
pub mod policy;

pub mod api;
pub mod engine;
pub mod hub;
pub mod persist;
pub mod bindings;
pub mod prompt;
pub mod util;
pub mod service;
pub mod bindings;
