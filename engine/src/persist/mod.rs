//! # Persistence Isolator — the ORM layer
//!
//! Generic framework that bridges in-memory structs to storage backends.
//! Business code only reads/writes memory; this layer handles durability.
//!
//! Supported backends (configured declaratively per type):
//! - SQLite (tables)
//! - JSON files
//! - Raw text files
//!
//! Rules:
//! - Only generic framework code lives here (like protoc, serde, tarpc)
//! - No business concepts — those belong in model/
//! - SQL, file paths, serialization formats are contained here

