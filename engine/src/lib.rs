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
//! - [`prompt`] — Prompt template construction
//! - [`engine`] — Multi-instance management (@TRACE: INSTANCE, RESTART)
//! - [`rpc`] — RPC server (Unix socket, tarpc)
//! - [`chat`] — Chat message storage (persist layer)
//! - [`logging`] — Log initialization, rotation, inference logs
//!
//! ## Trace Log Format
//!
//! All trace logs follow: `[TRACE_ID-instance_id] message`
//! Example: `[MEMORY-alice] Reading persistent.txt`

/// Safely truncate a string to at most `max_bytes` bytes,
/// ensuring the cut is on a UTF-8 character boundary.
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

pub fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the last char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end] // safe: end verified by is_char_boundary
}

/// Safe template rendering: scan template once, replace `{{KEY}}` placeholders
/// by looking up in the provided vars slice. Replacement results are never
/// re-scanned, preventing injection when user content contains `{{...}}` markers.
///
/// This is the root fix for the "replacement phase problem" — chained
/// `.replace("{{A}}", a).replace("{{B}}", b)` lets content from `a` that
/// contains `{{B}}` get replaced in the second step.
pub fn safe_render(template: &str, vars: &[(&str, &str)]) -> String {
    use std::collections::HashMap;
    let map: HashMap<&str, &str> = vars.iter().cloned().collect();
    let mut result = String::with_capacity(template.len() * 2);
    let mut chars = template.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        if ch == '{' {
            // Check for {{ at current position
            if let Some(&(_, next_ch)) = chars.peek() {
                if next_ch == '{' {
                    // Found "{{", look for "}}"
                    if let Some(end) = template[i + 2..].find("}}") {
                        let key = &template[i..i + 2 + end + 2]; // "{{XXX}}"
                        if let Some(val) = map.get(key) {
                            result.push_str(val);
                            // Advance past the entire "{{XXX}}" in the char iterator
                            let skip_to = i + 2 + end + 2;
                            while let Some(&(j, _)) = chars.peek() {
                                if j >= skip_to {
                                    break;
                                }
                                chars.next();
                            }
                            continue;
                        }
                    }
                }
            }
        }
        result.push(ch);
    }
    result
}

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
        // 中文UTF-8: 3 bytes per char
        let s = "你好世界"; // 12 bytes
        assert_eq!(safe_truncate(s, 12), "你好世界");
        assert_eq!(safe_truncate(s, 6), "你好");
        assert_eq!(safe_truncate(s, 7), "你好"); // 7 is mid-char, back to 6
        assert_eq!(safe_truncate(s, 8), "你好"); // 8 is mid-char, back to 6
        assert_eq!(safe_truncate(s, 9), "你好世");
        assert_eq!(safe_truncate(s, 3), "你");
        assert_eq!(safe_truncate(s, 2), ""); // can't fit a single Chinese char in 2 bytes
        assert_eq!(safe_truncate(s, 1), "");
        assert_eq!(safe_truncate(s, 0), "");
    }

    #[test]
    fn test_safe_truncate_mixed() {
        let s = "hi你好"; // 2 + 6 = 8 bytes
        assert_eq!(safe_truncate(s, 8), "hi你好");
        assert_eq!(safe_truncate(s, 5), "hi你");
        assert_eq!(safe_truncate(s, 4), "hi"); // 4 is mid-char of 你, back to 2
        assert_eq!(safe_truncate(s, 3), "hi"); // 3 is mid-char of 你, back to 2
        assert_eq!(safe_truncate(s, 2), "hi");
    }

    #[test]
    fn test_safe_truncate_emoji() {
        let s = "👋hello"; // 4 + 5 = 9 bytes
        assert_eq!(safe_truncate(s, 9), "👋hello");
        assert_eq!(safe_truncate(s, 4), "👋");
        assert_eq!(safe_truncate(s, 3), ""); // can't fit emoji in 3 bytes
        assert_eq!(safe_truncate(s, 1), "");
    }
    #[test]
    fn test_safe_render_basic() {
        let result = safe_render("Hello {{NAME}}, welcome to {{PLACE}}!", &[
            ("{{NAME}}", "Alice"),
            ("{{PLACE}}", "Wonderland"),
        ]);
        assert_eq!(result, "Hello Alice, welcome to Wonderland!");
    }

    #[test]
    fn test_safe_render_no_injection() {
        // Key test: value contains another placeholder — must NOT be replaced
        let result = safe_render("A={{A}} B={{B}}", &[
            ("{{A}}", "contains {{B}} inside"),
            ("{{B}}", "INJECTED"),
        ]);
        // {{B}} inside A's value should remain literal, not replaced
        assert_eq!(result, "A=contains {{B}} inside B=INJECTED");
    }

    #[test]
    fn test_safe_render_unknown_placeholder() {
        // Unknown placeholders are left as-is
        let result = safe_render("{{KNOWN}} and {{UNKNOWN}}", &[
            ("{{KNOWN}}", "yes"),
        ]);
        assert_eq!(result, "yes and {{UNKNOWN}}");
    }

    #[test]
    fn test_safe_render_empty_value() {
        let result = safe_render("before{{X}}after", &[
            ("{{X}}", ""),
        ]);
        assert_eq!(result, "beforeafter");
    }

    #[test]
    fn test_safe_render_chinese() {
        let result = safe_render("你好{{NAME}}，欢迎来到{{PLACE}}", &[
            ("{{NAME}}", "小白"),
            ("{{PLACE}}", "Alice引擎"),
        ]);
        assert_eq!(result, "你好小白，欢迎来到Alice引擎");
    }

    #[test]
    fn test_safe_render_no_vars() {
        let result = safe_render("no placeholders here", &[]);
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn test_safe_render_adjacent_placeholders() {
        let result = safe_render("{{A}}{{B}}", &[
            ("{{A}}", "hello"),
            ("{{B}}", "world"),
        ]);
        assert_eq!(result, "helloworld");
    }

    #[test]
    fn test_safe_render_single_brace() {
        // Single braces should not trigger replacement
        let result = safe_render("{not a placeholder} and {{REAL}}", &[
            ("{{REAL}}", "yes"),
        ]);
        assert_eq!(result, "{not a placeholder} and yes");
    }

}

pub mod logging;
pub mod external;
pub mod core;
pub mod action;
pub mod llm;
pub mod prompt;
pub mod engine;
pub mod chat;
pub mod messages;
pub mod rpc;
pub mod persist;
