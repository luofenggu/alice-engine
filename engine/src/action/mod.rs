//! # Action Module
//!
//! Action execution logic. Protocol definitions (Action enum, parse, etc.)
//! live in the `inference` module; this module re-exports them for convenience
//! and contains the execution engine.

pub mod execute;

// Re-export protocol types from inference module
pub use crate::inference::{
    Action, ReplaceBlock, ActionRecord,
    parse_actions,
    SEARCH_MARKER, REPLACE_MARKER, BLOCK_END_MARKER,
    REMEMBER_START_MARKER, REMEMBER_END_MARKER,
};

#[cfg(feature = "remember")]
pub use crate::inference::{extract_remember_fragments, strip_remember_markers};
