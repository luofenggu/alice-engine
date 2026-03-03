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
//! - [`rpc`] — RPC server (Unix socket, tarpc)
//! - [`persist`] — Persistence primitives and structs
//! - [`logging`] — Log initialization, rotation, inference logs
//!
//! ## Trace Log Format
//!
//! All trace logs follow: `[TRACE_ID-instance_id] message`
//! Example: `[MEMORY-alice] Reading persistent.txt`

/// Safely truncate a string to at most `max_bytes` bytes,
/// ensuring the cut is on a UTF-8 character boundary.
pub fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Atomic write: write to .tmp file then rename, preventing truncation on crash.
/// @TRACE: MEMORY
pub fn atomic_write(path: &std::path::Path, content: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)
        .map_err(|e| anyhow::anyhow!("Failed to create tmp file {}: {}", tmp.display(), e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| anyhow::anyhow!("Failed to write tmp file {}: {}", tmp.display(), e))?;
    file.sync_all()
        .map_err(|e| anyhow::anyhow!("Failed to fsync tmp file {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| anyhow::anyhow!("Failed to rename {} -> {}: {}", tmp.display(), path.display(), e))?;
    Ok(())
}

/// Re-exported from inference module — safe template rendering.
pub use inference::safe_render;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_truncate_ascii() {
        assert_eq!(safe_truncate("hello world", 5), "hello");
        assert_eq!(safe_truncate("hello", 10), "hello");
        assert_eq!(safe_truncate("hello", 5), "hello");
        assert_eq!(safe_truncate("", 5), "");
    }

    #[test]
    fn test_safe_truncate_chinese() {
        let s = "你好世界";
        assert_eq!(safe_truncate(s, 12), "你好世界");
        assert_eq!(safe_truncate(s, 6), "你好");
        assert_eq!(safe_truncate(s, 7), "你好");
        assert_eq!(safe_truncate(s, 8), "你好");
        assert_eq!(safe_truncate(s, 9), "你好世");
        assert_eq!(safe_truncate(s, 3), "你");
        assert_eq!(safe_truncate(s, 2), "");
        assert_eq!(safe_truncate(s, 1), "");
        assert_eq!(safe_truncate(s, 0), "");
    }

    #[test]
    fn test_safe_truncate_mixed() {
        let s = "hi你好";
        assert_eq!(safe_truncate(s, 8), "hi你好");
        assert_eq!(safe_truncate(s, 5), "hi你");
        assert_eq!(safe_truncate(s, 4), "hi");
        assert_eq!(safe_truncate(s, 3), "hi");
        assert_eq!(safe_truncate(s, 2), "hi");
    }

    #[test]
    fn test_safe_truncate_emoji() {
        let s = "👋hello";
        assert_eq!(safe_truncate(s, 9), "👋hello");
        assert_eq!(safe_truncate(s, 4), "👋");
        assert_eq!(safe_truncate(s, 3), "");
        assert_eq!(safe_truncate(s, 1), "");
    }
}

pub mod logging;
pub mod external;
pub mod policy;
pub mod core;
pub mod action;
pub mod inference;

pub mod prompt;
pub mod engine;
pub mod rpc;
pub mod persist;
pub mod util;
