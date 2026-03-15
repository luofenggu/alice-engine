//! Legacy migration logic: file → DB.
//!
//! Each migration function independently checks whether migration is needed
//! (DB table empty + legacy file exists) and is idempotent.

use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use crate::persist::memory::{Memory, SessionBlockEntry};

/// Run all legacy migrations. Called from Instance::open.
///
/// Each sub-migration is independent: failure in one does not prevent others.
/// Errors are logged but not fatal — the instance can still operate with
/// partial migration (missing data will appear empty).
pub fn migrate_all(memory: &Memory, memory_dir: &Path, sessions_dir: &Path) -> Result<()> {
    if let Err(e) = migrate_knowledge(memory, memory_dir) {
        tracing::warn!("[LEGACY] Knowledge migration failed: {}", e);
    }
    if let Err(e) = migrate_history(memory, sessions_dir) {
        tracing::warn!("[LEGACY] History migration failed: {}", e);
    }
    if let Err(e) = migrate_sessions(memory, sessions_dir) {
        tracing::warn!("[LEGACY] Sessions migration failed: {}", e);
    }
    if let Err(e) = migrate_current(memory, sessions_dir) {
        tracing::warn!("[LEGACY] Current migration failed: {}", e);
    }
    Ok(())
}

// ── Knowledge migration ──

/// Migrate knowledge.md → knowledge_store table.
fn migrate_knowledge(memory: &Memory, memory_dir: &Path) -> Result<()> {
    // Skip if DB already has knowledge
    if !memory.read_knowledge().is_empty() {
        return Ok(());
    }

    let knowledge_file = memory_dir.join("knowledge.md");
    if !knowledge_file.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&knowledge_file)
        .with_context(|| format!("Failed to read {}", knowledge_file.display()))?;
    if content.trim().is_empty() {
        return Ok(());
    }

    memory.write_knowledge(&content)?;
    rename_migrated(&knowledge_file)?;
    info!(
        "[LEGACY] Migrated knowledge.md → DB ({} bytes)",
        content.len()
    );

    Ok(())
}

// ── History migration ──

/// Migrate history.txt → history_store table.
fn migrate_history(memory: &Memory, sessions_dir: &Path) -> Result<()> {
    // Skip if DB already has history
    if !memory.read_history().is_empty() {
        return Ok(());
    }

    let history_file = sessions_dir.join("history.txt");
    if !history_file.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&history_file)
        .with_context(|| format!("Failed to read {}", history_file.display()))?;
    if content.trim().is_empty() {
        return Ok(());
    }

    memory.write_history(&content)?;
    rename_migrated(&history_file)?;
    info!(
        "[LEGACY] Migrated history.txt → DB ({} bytes)",
        content.len()
    );

    Ok(())
}

// ── Sessions migration ──

/// Migrate sessions/*.jsonl → session_blocks table.
///
/// Each .jsonl file represents a session block. The filename (without extension)
/// is the block_name. Each line in the file is a JSON object with fields:
/// `{"first_msg": "...", "last_msg": "...", "summary": "..."}`.
fn migrate_sessions(memory: &Memory, sessions_dir: &Path) -> Result<()> {
    // Skip if DB already has session blocks
    let existing = memory.list_session_blocks_db().unwrap_or_default();
    if !existing.is_empty() {
        return Ok(());
    }

    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // Directory doesn't exist or not readable
    };

    let mut jsonl_files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map_or(false, |ext| ext == "jsonl")
        })
        .collect();
    jsonl_files.sort_by_key(|e| e.file_name());

    if jsonl_files.is_empty() {
        return Ok(());
    }

    let mut total_entries = 0usize;
    let mut total_blocks = 0usize;

    for file_entry in &jsonl_files {
        let path = file_entry.path();
        let block_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "[LEGACY] Failed to read session file {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };

        let mut block_entries = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionBlockEntry>(line) {
                Ok(entry) => block_entries.push(entry),
                Err(e) => {
                    tracing::warn!(
                        "[LEGACY] Failed to parse session entry in {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }

        if !block_entries.is_empty() {
            memory
                .batch_insert_session_entries(block_name, &block_entries)
                .with_context(|| {
                    format!("[LEGACY] Failed to batch insert session block: {}", block_name)
                })?;
            total_entries += block_entries.len();
            total_blocks += 1;
        }
    }

    // Rename migrated files
    for file_entry in &jsonl_files {
        let _ = rename_migrated(&file_entry.path());
    }

    if total_blocks > 0 {
        info!(
            "[LEGACY] Migrated {} session blocks ({} entries) → DB",
            total_blocks, total_entries
        );
    }

    Ok(())
}

// ── Current migration ──

/// Migrate current.txt → action_log table as a single LegacyNote entry.
///
/// current.txt contains non-structured action history text. Rather than parsing
/// the complex format, we insert the entire content as a single note record.
/// The agent will naturally compress it during the next summary cycle.
fn migrate_current(memory: &Memory, sessions_dir: &Path) -> Result<()> {
    let current_file = sessions_dir.join("current.txt");
    if !current_file.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&current_file)
        .with_context(|| format!("Failed to read {}", current_file.display()))?;
    if content.trim().is_empty() {
        // Empty file, just rename and skip
        let _ = rename_migrated(&current_file);
        return Ok(());
    }

    // Check if action_log already has entries (avoid duplicate import)
    let current_rendered = memory.render_current_from_db();
    if !current_rendered?.is_empty() {
        // DB already has action records, skip current.txt migration
        return Ok(());
    }

    // Generate a unique action_id for the legacy import
    let action_id = format!(
        "{}_legacy",
        chrono::Local::now().format("%Y%m%d%H%M%S")
    );

    memory.insert_done_note(&action_id, "legacy_import", &content)?;
    rename_migrated(&current_file)?;

    info!(
        "[LEGACY] Migrated current.txt → DB as legacy_import ({} bytes)",
        content.len()
    );

    Ok(())
}

// ── Helpers ──

/// Rename a file to .migrated extension (preserving original as backup).
fn rename_migrated(path: &Path) -> Result<()> {
    let mut migrated = path.as_os_str().to_owned();
    migrated.push(".migrated");
    let migrated = std::path::PathBuf::from(migrated);
    std::fs::rename(path, &migrated)
        .with_context(|| format!("Failed to rename {} → {}", path.display(), migrated.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;


    #[test]
    fn test_migrate_knowledge_from_file() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();
        fs::write(memory_dir.join("knowledge.md"), "test knowledge content").unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        migrate_knowledge(&memory, &memory_dir).unwrap();

        assert_eq!(memory.read_knowledge(), "test knowledge content");
        assert!(memory_dir.join("knowledge.md.migrated").exists());
        assert!(!memory_dir.join("knowledge.md").exists());
    }

    #[test]
    fn test_migrate_knowledge_idempotent() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        // Pre-populate DB
        memory.write_knowledge("existing knowledge").unwrap();

        // Write a file that should NOT be migrated
        fs::write(memory_dir.join("knowledge.md"), "new content").unwrap();
        migrate_knowledge(&memory, &memory_dir).unwrap();

        // DB should still have original content
        assert_eq!(memory.read_knowledge(), "existing knowledge");
        // File should NOT be renamed (migration was skipped)
        assert!(memory_dir.join("knowledge.md").exists());
    }

    #[test]
    fn test_migrate_history() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("history.txt"), "test history").unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        migrate_history(&memory, &sessions_dir).unwrap();

        assert_eq!(memory.read_history(), "test history");
        assert!(sessions_dir.join("history.txt.migrated").exists());
    }

    #[test]
    fn test_migrate_sessions() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        // Create a jsonl file with two entries
        let jsonl = r#"{"first_msg":"msg1","last_msg":"msg2","summary":"block summary 1"}
{"first_msg":"msg3","last_msg":"msg4","summary":"block summary 2"}"#;
        fs::write(sessions_dir.join("20260301120000.jsonl"), jsonl).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        migrate_sessions(&memory, &sessions_dir).unwrap();

        let blocks = memory.list_session_blocks_db().unwrap();
        assert_eq!(blocks, vec!["20260301120000"]);

        let entries = memory.read_session_entries_db("20260301120000").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].first_msg, "msg1");
        assert_eq!(entries[1].first_msg, "msg3");

        assert!(sessions_dir.join("20260301120000.jsonl.migrated").exists());
    }

    #[test]
    fn test_migrate_sessions_idempotent() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();

        // Pre-populate DB with a session block
        memory
            .insert_session_block_entry(
                "existing_block",
                &SessionBlockEntry {
                    first_msg: "a".into(),
                    last_msg: "b".into(),
                    summary: "c".into(),
                },
            )
            .unwrap();

        // Write a jsonl file that should NOT be migrated
        let jsonl = r#"{"first_msg":"x","last_msg":"y","summary":"z"}"#;
        fs::write(sessions_dir.join("20260301120000.jsonl"), jsonl).unwrap();
        migrate_sessions(&memory, &sessions_dir).unwrap();

        // Should still only have the pre-existing block
        let blocks = memory.list_session_blocks_db().unwrap();
        assert_eq!(blocks, vec!["existing_block"]);
        // File should NOT be renamed
        assert!(sessions_dir.join("20260301120000.jsonl").exists());
    }

    #[test]
    fn test_migrate_current() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let content = "---行为编号[test]开始---\nsome action content\n---行为编号[test]结束---";
        fs::write(sessions_dir.join("current.txt"), content).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        migrate_current(&memory, &sessions_dir).unwrap();

        // Verify it was inserted as a legacy_import note
        let rendered = memory.render_current_from_db();
        assert!(rendered.unwrap().contains(content));
        assert!(sessions_dir.join("current.txt.migrated").exists());
    }

    #[test]
    fn test_migrate_all_no_files() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        // Should succeed with no files to migrate
        migrate_all(&memory, &memory_dir, &sessions_dir).unwrap();
    }

    #[test]
    fn test_rename_migrated() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        fs::write(&file, "content").unwrap();

        rename_migrated(&file).unwrap();

        assert!(!file.exists());
        assert!(tmp.path().join("test.txt.migrated").exists());
    }

    #[test]
    fn test_migrate_sessions_multiple_files() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        // Create 3 jsonl files — should be migrated in filename order
        let jsonl_a = r#"{"first_msg":"a1","last_msg":"a2","summary":"block A"}"#;
        let jsonl_b = r#"{"first_msg":"b1","last_msg":"b2","summary":"block B1"}
{"first_msg":"b3","last_msg":"b4","summary":"block B2"}"#;
        let jsonl_c = r#"{"first_msg":"c1","last_msg":"c2","summary":"block C"}"#;

        fs::write(sessions_dir.join("20260301000000.jsonl"), jsonl_a).unwrap();
        fs::write(sessions_dir.join("20260302000000.jsonl"), jsonl_b).unwrap();
        fs::write(sessions_dir.join("20260303000000.jsonl"), jsonl_c).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        migrate_sessions(&memory, &sessions_dir).unwrap();

        let blocks = memory.list_session_blocks_db().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], "20260301000000");
        assert_eq!(blocks[1], "20260302000000");
        assert_eq!(blocks[2], "20260303000000");

        // Verify entries count per block
        let entries_a = memory.read_session_entries_db("20260301000000").unwrap();
        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_a[0].summary, "block A");

        let entries_b = memory.read_session_entries_db("20260302000000").unwrap();
        assert_eq!(entries_b.len(), 2);
        assert_eq!(entries_b[0].summary, "block B1");
        assert_eq!(entries_b[1].summary, "block B2");

        // All files renamed
        assert!(sessions_dir.join("20260301000000.jsonl.migrated").exists());
        assert!(sessions_dir.join("20260302000000.jsonl.migrated").exists());
        assert!(sessions_dir.join("20260303000000.jsonl.migrated").exists());
    }

    #[test]
    fn test_migrate_current_idempotent() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();

        // Pre-populate DB with an action_log entry
        memory
            .insert_done_note("existing_001", "some_action", "existing content")
            .unwrap();

        // Write current.txt that should NOT be migrated
        fs::write(
            sessions_dir.join("current.txt"),
            "legacy current content",
        )
        .unwrap();

        migrate_current(&memory, &sessions_dir).unwrap();

        // File should NOT be renamed (migration was skipped)
        assert!(sessions_dir.join("current.txt").exists());
    }

    #[test]
    fn test_migrate_sessions_invalid_json_lines() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        // Mix of valid and invalid lines
        let jsonl = r#"{"first_msg":"ok1","last_msg":"ok2","summary":"valid line 1"}
this is not valid json
{"first_msg":"ok3","last_msg":"ok4","summary":"valid line 2"}
{"broken json
"#;
        fs::write(sessions_dir.join("20260301000000.jsonl"), jsonl).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        migrate_sessions(&memory, &sessions_dir).unwrap();

        // Only valid lines should be imported
        let entries = memory.read_session_entries_db("20260301000000").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "valid line 1");
        assert_eq!(entries[1].summary, "valid line 2");

        // File still renamed (partial import is OK)
        assert!(sessions_dir.join("20260301000000.jsonl.migrated").exists());
    }

    #[test]
    fn test_migrate_all_full() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        // Set up all 4 data types
        fs::write(memory_dir.join("knowledge.md"), "full test knowledge").unwrap();
        fs::write(sessions_dir.join("history.txt"), "full test history").unwrap();

        let jsonl = r#"{"first_msg":"m1","last_msg":"m2","summary":"session entry"}"#;
        fs::write(sessions_dir.join("20260301000000.jsonl"), jsonl).unwrap();

        let current = "action content from current.txt";
        fs::write(sessions_dir.join("current.txt"), current).unwrap();

        let memory = Memory::open(tmp.path(), "test").unwrap();
        migrate_all(&memory, &memory_dir, &sessions_dir).unwrap();

        // Verify all 4 data types migrated
        assert_eq!(memory.read_knowledge(), "full test knowledge");
        assert_eq!(memory.read_history(), "full test history");

        let blocks = memory.list_session_blocks_db().unwrap();
        assert_eq!(blocks, vec!["20260301000000"]);
        let entries = memory.read_session_entries_db("20260301000000").unwrap();
        assert_eq!(entries.len(), 1);

        let rendered = memory.render_current_from_db().unwrap();
        assert!(rendered.contains(current));

        // All files renamed
        assert!(memory_dir.join("knowledge.md.migrated").exists());
        assert!(sessions_dir.join("history.txt.migrated").exists());
        assert!(sessions_dir.join("20260301000000.jsonl.migrated").exists());
        assert!(sessions_dir.join("current.txt.migrated").exists());
    }
}
