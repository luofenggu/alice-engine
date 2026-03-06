//! HTTP API layer — routes, auth, and shared types.
//!
//! This module provides the HTTP API server that serves both
//! JSON API endpoints and static HTML pages.

pub mod auth;
pub mod http_protocol;
pub mod routes;
pub mod state;
pub mod types;

pub use state::EngineState;
pub use types::*;
