//! HTTP API layer — routes, auth, and shared types.
//!
//! This module provides the HTTP API server that serves both
//! JSON API endpoints and static HTML pages.

pub mod types;
pub mod auth;
pub mod routes;
pub mod state;

pub use types::*;
pub use state::EngineState;

