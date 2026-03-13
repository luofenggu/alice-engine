//! Legacy file-to-DB migration module.
//!
//! This module handles one-time migration of memory data from legacy file formats
//! to the new SQLite database tables. It is designed to be called during instance
//! initialization and is idempotent — safe to call multiple times.
//!
//! Guardian exemption: This directory (along with `bindings/`) contains code that
//! interfaces with legacy data formats and is exempt from standard refactoring rules.

pub mod migrate;
