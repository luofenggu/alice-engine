//! # Concept Model — the .proto of Alice Engine
//!
//! All domain concepts live here as declarative definitions.
//! This is the single source of truth for the engine's vocabulary.
//!
//! Rules:
//! - Literals are legal here (this is the contract definition layer)
//! - Business code imports types from here, never defines its own
//! - Changes here = schema migration (treat with care)

