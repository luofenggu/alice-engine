//! Memory subsystem — manages all persistent memory files for an Alice instance.
//!
//! Memory is not a single Document but a composite of multiple persistence handles:
//! - knowledge.md (TextFile) — persistent cognitive framework
//! - history.txt (TextFile) — compressed long-term narrative
//! - current.txt (TextFile) — current session incremental memory
//! - sessions/*.jsonl — session block files (directory-based)
//!
//! Multiple commit methods control flush order for crash safety:
//! - append_current() — normal beat, write current to disk
//! - commit_summary() — summary transaction: write session → clear current
//! - commit_history() — roll transaction: write history → delete old session block

use crate::bindings::db::{
    self, ActionLogRow, MemoryCursorRow, NewActionLog, ACTION_STATUS_DISTILLED,
    ACTION_STATUS_DONE, ACTION_STATUS_EXECUTING,
};
use crate::persist::TextFile;
use crate::policy::action_output as out;
use anyhow::{Context, Result};
use chrono::Local;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A single entry in a session block JSONL file.
/// This struct is the single source of truth for session block format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBlockEntry {
    pub first_msg: String,
    pub last_msg: String,
    pub summary: String,
}

/// Session block file extension
const SESSION_EXT: &str = "jsonl";

/// Memory subsystem for an Alice instance.
#[derive(Clone)]
pub struct Memory {
    /// Root memory directory (instance_dir/memory)
    memory_dir: PathBuf,
    /// Sessions directory (memory_dir/sessions)
    sessions_dir: PathBuf,
    /// knowledge.md — persistent cognitive framework
    pub knowledge: TextFile,
    /// history.txt — compressed long-term narrative
    pub history: TextFile,
    /// current.txt — current session incremental memory
    pub current: TextFile,
    /// Database connection for action_log and other tables
    pub db: Arc<Mutex<SqliteConnection>>,
    /// Instance ID for scoping DB queries
    instance_id: String,
}

impl Memory {
    /// Open memory from the given memory directory.
    ///
    /// Initializes TextFile handles for knowledge, history, and current.
    /// Creates directories if they don't exist.
    ///
    /// Note: One-time migrations (e.g. keypoints.md → knowledge.md) should be
    /// performed BEFORE calling this, so TextFile::open reads the migrated content.
    pub fn open(memory_dir: impl Into<PathBuf>, instance_id: &str) -> Result<Self> {
        let memory_dir = memory_dir.into();
        let sessions_dir = memory_dir.join("sessions");

        // Ensure directories exist
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&sessions_dir).with_context(|| {
            format!("Failed to create sessions dir: {}", sessions_dir.display())
        })?;

        // Clean up .tmp residuals from atomic_write after crash (self-contained cleanup)
        Self::cleanup_tmp_residuals(&memory_dir);

        let knowledge = TextFile::open(memory_dir.join("knowledge.md"))?;
        let history = TextFile::open(sessions_dir.join("history.txt"))?;
        let current = TextFile::open(sessions_dir.join("current.txt"))?;

        // Initialize memory database
        let db_path = memory_dir.join("memory.db");
        let mut conn = SqliteConnection::establish(
            db_path.to_str().ok_or_else(|| anyhow::anyhow!("Invalid db path"))?,
        )
        .with_context(|| format!("Failed to open memory db: {}", db_path.display()))?;

        // Create tables
        diesel::sql_query(db::CREATE_ACTION_LOG_TABLE)
            .execute(&mut conn)
            .context("Failed to create action_log table")?;
        diesel::sql_query(db::CREATE_ACTION_LOG_IDX_INSTANCE)
            .execute(&mut conn)
            .context("Failed to create action_log instance index")?;
        diesel::sql_query(db::CREATE_ACTION_LOG_IDX_ACTION_ID)
            .execute(&mut conn)
            .context("Failed to create action_log action_id index")?;
        diesel::sql_query(db::CREATE_ACTION_LOG_IDX_TYPE)
            .execute(&mut conn)
            .context("Failed to create action_log type index")?;
        diesel::sql_query(db::CREATE_MEMORY_CURSOR_TABLE)
            .execute(&mut conn)
            .context("Failed to create memory_cursor table")?;
        diesel::sql_query(db::CREATE_KNOWLEDGE_STORE_TABLE)
            .execute(&mut conn)
            .context("Failed to create knowledge_store table")?;
        diesel::sql_query(db::CREATE_HISTORY_STORE_TABLE)
            .execute(&mut conn)
            .context("Failed to create history_store table")?;
        diesel::sql_query(db::CREATE_SESSION_BLOCKS_TABLE)
            .execute(&mut conn)
            .context("Failed to create session_blocks table")?;
        diesel::sql_query(db::CREATE_SESSION_BLOCKS_INDEX)
            .execute(&mut conn)
            .context("Failed to create session_blocks instance index")?;

        Ok(Self {
            memory_dir,
            sessions_dir,
            knowledge,
            history,
            current,
            db: Arc::new(Mutex::new(conn)),
            instance_id: instance_id.to_string(),
        })
    }

    /// Clean up .tmp files left by atomic_write after crash.
    /// Recursively scans memory directory and subdirectories.
    fn cleanup_tmp_residuals(dir: &Path) {
        let mut cleaned = 0u32;
        Self::cleanup_tmp_in_dir(dir, &mut cleaned);
        if cleaned > 0 {
            tracing::info!(
                "[MEMORY] Cleaned {} .tmp residual files from previous crash",
                cleaned
            );
        }
    }

    fn cleanup_tmp_in_dir(dir: &Path, cleaned: &mut u32) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "tmp") {
                    if std::fs::remove_file(&path).is_ok() {
                        tracing::info!("[MEMORY] Cleaned tmp file: {:?}", path);
                        *cleaned += 1;
                    }
                } else if path.is_dir() {
                    Self::cleanup_tmp_in_dir(&path, cleaned);
                }
            }
        }
    }

    /// Root memory directory path.
    pub fn memory_dir(&self) -> &Path {
        &self.memory_dir
    }

    /// Sessions directory path.
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Read the .last_rolled idempotency marker.
    /// Returns Some(block_name) if marker exists, None otherwise.
    pub fn get_last_rolled(&self) -> Option<String> {
        let path = self.last_rolled_path();
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .map(|s| s.trim().to_string())
        } else {
            None
        }
    }

    /// Write the .last_rolled idempotency marker.
    pub fn set_last_rolled(&self, block_name: &str) {
        let path = self.last_rolled_path();
        let _ = std::fs::write(&path, block_name.as_bytes());
    }

    /// Clear (delete) the .last_rolled idempotency marker.
    pub fn clear_last_rolled(&self) {
        let path = self.last_rolled_path();
        let _ = std::fs::remove_file(&path);
    }

    /// Path to the .last_rolled idempotency marker file (internal).
    fn last_rolled_path(&self) -> PathBuf {
        self.sessions_dir.join(".last_rolled")
    }

    // ── Current: convenience methods ──

    /// Append text to current memory, with newline separator if not empty.
    pub fn append_current(&self, text: &str) -> Result<()> {
        let existing = self.current.read()?;
        if existing.is_empty() {
            self.current.write(text)
        } else {
            // Read existing + newline + new text, write atomically
            let combined = format!("{}\n{}", existing, text);
            self.current.write(&combined)
        }
    }

    /// Replace current content.
    pub fn write_current(&self, content: &str) -> Result<()> {
        self.current.write(content)
    }



    // ── Session blocks ──

    /// Read a session block file content.
    pub fn read_session_block(&self, name: &str) -> Result<String> {
        let path = self.session_block_path(name);
        if !path.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read session block: {}", path.display()))
    }

    /// Append lines to a session block file (creates if not exists).
    /// Uses fsync for durability.
    pub fn append_session_block(&self, name: &str, lines: &str) -> Result<()> {
        use std::io::Write;
        let path = self.session_block_path(name);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "Failed to open session block for append: {}",
                    path.display()
                )
            })?;
        file.write_all(lines.as_bytes())
            .with_context(|| format!("Failed to append to session block: {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to fsync session block: {}", path.display()))?;
        Ok(())
    }

    /// Write (overwrite) a session block file.
    pub fn write_session_block(&self, name: &str, content: &str) -> Result<()> {
        let path = self.session_block_path(name);
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write session block: {}", path.display()))
    }

    /// Delete a session block file.
    pub fn delete_session_block(&self, name: &str) -> Result<()> {
        let path = self.session_block_path(name);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to delete session block: {}", path.display()))?;
        }
        Ok(())
    }

    /// Get the size of a session block file in bytes.
    pub fn session_block_size(&self, name: &str) -> u64 {
        let path = self.session_block_path(name);
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    }

    /// List all session block names (sorted), without extension.
    pub fn list_session_blocks(&self) -> Result<Vec<String>> {
        let mut blocks = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == SESSION_EXT) {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        blocks.push(stem.to_string());
                    }
                }
            }
        }
        blocks.sort();
        Ok(blocks)
    }

    // ── Commit methods (transaction-like, controlled flush order) ──



    /// Read and deserialize all entries from a session block.
    pub fn read_session_entries(&self, block_name: &str) -> Result<Vec<SessionBlockEntry>> {
        let content = self.read_session_block(block_name)?;
        let mut entries = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<SessionBlockEntry>(line) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Summary transaction:
    /// 1. Resolve target session block (append to existing if under size limit, else create new)
    /// 2. Serialize entry and append to session block
    /// 3. Clear current
    ///
    /// Returns the block name used (for logging).
    ///
    /// Crash safety: write targets first, clear source last.
    /// Worst case on crash: duplicate session entry + stale current (no data loss).
    pub fn commit_summary(
        &self,
        entry: &SessionBlockEntry,
        session_block_kb: u32,
    ) -> Result<String> {
        // Step 1: Resolve target block
        let blocks = self.list_session_blocks()?;
        let block_name = if let Some(latest) = blocks.last() {
            let size = self.session_block_size(latest);
            if size < (session_block_kb as u64 * 1024) {
                latest.clone()
            } else {
                // Block full — create new one with unique name
                let mut name = Local::now().format("%Y%m%d%H%M%S").to_string();
                if blocks.contains(&name) {
                    // Same-second collision: append suffix
                    let mut suffix = 2u32;
                    loop {
                        let candidate = format!("{}_{}", name, suffix);
                        if !blocks.contains(&candidate) {
                            name = candidate;
                            break;
                        }
                        suffix += 1;
                    }
                }
                name
            }
        } else {
            Local::now().format("%Y%m%d%H%M%S").to_string()
        };

        // Step 2: Serialize and append
        let jsonl_line =
            serde_json::to_string(entry).context("Failed to serialize session block entry")? + "\n";
        self.append_session_block(&block_name, &jsonl_line)?;

        // Step 3: Clear current
        self.current.clear()?;

        // Step 4: Advance DB cursor (marks action_log entries as consumed)
        self.advance_cursor().ok();

        Ok(block_name)
    }

    /// Roll history transaction:
    /// 1. Write idempotency marker (so crash recovery knows what was being rolled)
    /// 2. Write compressed history (atomic: tmp + rename)
    /// 3. Delete oldest session block
    /// 4. Clear idempotency marker
    ///
    /// Crash safety: marker written first, cleared last.
    /// Worst case on crash: marker exists → next roll detects idempotency and cleans up.
    pub fn commit_history(&self, new_history: &str, oldest_block_name: &str) -> Result<()> {
        // Step 1: Write idempotency marker before any mutation
        self.set_last_rolled(oldest_block_name);

        // Step 2: Write history (atomic: tmp + rename)
        self.history.write(new_history)?;

        // Step 3: Delete oldest session block
        self.delete_session_block(oldest_block_name)?;

        // Step 4: Clear marker after successful commit
        self.clear_last_rolled();

        Ok(())
    }

    // ── Action log (database) ──

    /// Insert a new action log entry with status=executing (Write-Ahead Doing).
    /// Called at the start of action execution.
    pub fn insert_action_log(
        &self,
        action_id: &str,
        action_type: &str,
        action_data_json: &str,
        created_at: &str,
    ) -> Result<()> {
        use crate::bindings::db::{action_log, NewActionLog, ACTION_STATUS_EXECUTING};

        let new_row = NewActionLog {
            instance_id: &self.instance_id,
            action_id,
            action_type,
            action_data: action_data_json,
            result_text: None,
            status: ACTION_STATUS_EXECUTING,
            msg_id_first: None,
            msg_id_last: None,
            created_at,
        };

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        diesel::insert_into(action_log::table)
            .values(&new_row)
            .execute(conn)
            .with_context(|| format!("[MEMORY-DB] Failed to insert action_log: {}", action_id))?;

        tracing::debug!("[MEMORY-DB] Inserted action_log: {} ({})", action_id, action_type);
        Ok(())
    }

    /// Complete an action log entry: update status to done, set result_text and msg_ids.
    /// Called after action execution finishes.
    /// Insert a completed note directly (for interrupt/reject events).
    /// These are not Action enum variants but control flow markers.
    pub fn insert_done_note(
        &self,
        action_id: &str,
        action_type: &str,
        text: &str,
    ) -> Result<()> {
        use crate::bindings::db::{action_log, NewActionLog, ACTION_STATUS_DONE};

        let created_at = if action_id.len() >= 14 { &action_id[..14] } else { action_id };

        let new_row = NewActionLog {
            instance_id: &self.instance_id,
            action_id,
            action_type,
            action_data: "",
            result_text: Some(text),
            status: ACTION_STATUS_DONE,
            msg_id_first: None,
            msg_id_last: None,
            created_at,
        };

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        diesel::insert_into(action_log::table)
            .values(&new_row)
            .execute(conn)
            .with_context(|| format!("[MEMORY-DB] Failed to insert done note: {}", action_id))?;

        tracing::debug!("[MEMORY-DB] Inserted done note: {} ({})", action_id, action_type);
        Ok(())
    }

    pub fn complete_action_log(
        &self,
        action_id: &str,
        result_text: &str,
        msg_id_first: Option<&str>,
        msg_id_last: Option<&str>,
    ) -> Result<()> {
        use crate::bindings::db::{action_log, ACTION_STATUS_DONE};

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let updated = diesel::update(
            action_log::table
                .filter(action_log::instance_id.eq(&self.instance_id))
                .filter(action_log::action_id.eq(action_id)),
        )
        .set((
            action_log::status.eq(ACTION_STATUS_DONE),
            action_log::result_text.eq(Some(result_text)),
            action_log::msg_id_first.eq(msg_id_first),
            action_log::msg_id_last.eq(msg_id_last),
        ))
        .execute(conn)
        .with_context(|| format!("[MEMORY-DB] Failed to complete action_log: {}", action_id))?;

        if updated == 0 {
            tracing::warn!("[MEMORY-DB] complete_action_log: no row found for {}", action_id);
        }
        Ok(())
    }

    /// Distill an action log entry: update status to distilled, replace result_text with summary.
    pub fn distill_action_log(&self, action_id: &str, summary: &str) -> Result<(usize, usize)> {
        use crate::bindings::db::{action_log, ActionLogRow, ACTION_STATUS_DISTILLED};

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;

        // Read current result_text length for logging
        let row: Option<ActionLogRow> = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .filter(action_log::action_id.eq(action_id))
            .first(conn)
            .optional()
            .with_context(|| format!("[MEMORY-DB] Failed to query action_log: {}", action_id))?;

        let old_len = row
            .as_ref()
            .and_then(|r| r.result_text.as_ref())
            .map(|t| t.len())
            .unwrap_or(0);

        let updated = diesel::update(
            action_log::table
                .filter(action_log::instance_id.eq(&self.instance_id))
                .filter(action_log::action_id.eq(action_id)),
        )
        .set((
            action_log::status.eq(ACTION_STATUS_DISTILLED),
            action_log::result_text.eq(Some(summary)),
        ))
        .execute(conn)
        .with_context(|| format!("[MEMORY-DB] Failed to distill action_log: {}", action_id))?;

        if updated == 0 {
            anyhow::bail!("action_log [{}] not found for distill", action_id);
        }

        Ok((old_len, summary.len()))
    }

    /// Render current memory from action_log (all entries after cursor).
    /// Returns the rendered string in the same format as current.txt.
    pub fn render_current_from_db(&self) -> Result<String> {
        use crate::bindings::db::{action_log, ActionLogRow, ACTION_STATUS_EXECUTING, ACTION_STATUS_DISTILLED};

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;

        let cursor = self.get_cursor_inner(conn)?;

        let rows: Vec<ActionLogRow> = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .filter(action_log::id.gt(cursor))
            .order(action_log::id.asc())
            .load(conn)
            .context("[MEMORY-DB] Failed to load action_log for render")?;

        let mut parts: Vec<String> = Vec::new();
        for row in &rows {
            parts.push(self.render_action_log_entry(row));
        }

        Ok(parts.join("\n"))
    }

    /// Render a single action_log entry in the current.txt format.
    fn render_action_log_entry(&self, row: &crate::bindings::db::ActionLogRow) -> String {
        use crate::bindings::db::{ACTION_STATUS_EXECUTING, ACTION_STATUS_DISTILLED};

        let id = &row.action_id;
        let start = out::action_block_start(id);
        let end = out::action_block_end(id);

        if row.status == ACTION_STATUS_DISTILLED {
            // Distilled: show [已提炼] + summary
            let summary = row.result_text.as_deref().unwrap_or("");
            out::distilled_block(id, summary)
        } else if row.status == ACTION_STATUS_EXECUTING {
            // Executing: show doing description from action_data
            let doing_desc = self.doing_description_from_data(&row.action_data, &row.action_type);
            format!("{}\n{}\n---action executing, result pending---\n{}", start, doing_desc, end)
        } else {
            // Done: show result_text (which contains doing+done)
            let result = row.result_text.as_deref().unwrap_or("");
            format!("{}\n{}\n{}", start, result, end)
        }
    }

    /// Extract doing description from action_data JSON.
    /// Falls back to action_type if deserialization fails.
    fn doing_description_from_data(&self, action_data: &str, action_type: &str) -> String {
        use crate::inference::Action;
        match serde_json::from_str::<Action>(action_data) {
            Ok(action) => out::build_doing_description(&action),
            Err(_) => {
                tracing::warn!("[MEMORY-DB] Failed to deserialize action_data for doing description, action_type={}", action_type);
                format!("execute {} (details unavailable)", action_type)
            }
        }
    }

    /// Advance cursor to the current maximum action_log id.
    /// Called during summary to mark "current" as consumed.
    pub fn advance_cursor(&self) -> Result<()> {
        use crate::bindings::db::action_log;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;

        let max_id: Option<i64> = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .select(diesel::dsl::max(action_log::id))
            .first(conn)
            .context("[MEMORY-DB] Failed to query max action_log id")?;

        let new_cursor = max_id.unwrap_or(0);
        self.set_cursor_inner(conn, new_cursor)?;

        tracing::info!("[MEMORY-DB] Advanced cursor to {}", new_cursor);
        Ok(())
    }

    /// Query message ID range from action_log entries after cursor.
    /// Returns (first_msg_id, last_msg_id) from read_msg and send_msg actions.
    pub fn query_msg_range(&self) -> Result<(Option<String>, Option<String>)> {
        use crate::bindings::db::action_log;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let cursor = self.get_cursor_inner(conn)?;

        // Find first non-null msg_id_first (ordered by id asc)
        let first: Option<String> = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .filter(action_log::id.gt(cursor))
            .filter(action_log::msg_id_first.is_not_null())
            .order(action_log::id.asc())
            .select(action_log::msg_id_first)
            .first::<Option<String>>(conn)
            .optional()
            .context("[MEMORY-DB] Failed to query first msg_id")?
            .flatten();

        // Find last non-null msg_id_last (ordered by id desc)
        let last: Option<String> = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .filter(action_log::id.gt(cursor))
            .filter(action_log::msg_id_last.is_not_null())
            .order(action_log::id.desc())
            .select(action_log::msg_id_last)
            .first::<Option<String>>(conn)
            .optional()
            .context("[MEMORY-DB] Failed to query last msg_id")?
            .flatten();

        Ok((first, last))
    }

    /// Count distinct message IDs in action_log entries after cursor.
    /// Used by summary to report how many messages are covered.
    pub fn query_msg_count(&self) -> Result<i64> {
        use crate::bindings::db::action_log;
        use diesel::dsl::sql;
        use diesel::sql_types::BigInt;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let cursor = self.get_cursor_inner(conn)?;

        // Count rows that have at least one non-null msg_id field
        let count: i64 = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .filter(action_log::id.gt(cursor))
            .filter(action_log::msg_id_first.is_not_null())
            .select(sql::<BigInt>("COUNT(*)"))
            .first(conn)
            .context("[MEMORY-DB] Failed to count msg_ids")?;

        Ok(count)
    }

    // ── Cursor helpers (internal, conn already locked) ──

    fn get_cursor_inner(&self, conn: &mut SqliteConnection) -> Result<i64> {
        use crate::bindings::db::memory_cursor;

        let row: Option<crate::bindings::db::MemoryCursorRow> = memory_cursor::table
            .find(&self.instance_id)
            .first(conn)
            .optional()
            .context("[MEMORY-DB] Failed to query cursor")?;

        Ok(row.map(|r| r.current_cursor).unwrap_or(0))
    }

    fn set_cursor_inner(&self, conn: &mut SqliteConnection, cursor: i64) -> Result<()> {
        use crate::bindings::db::memory_cursor;

        let now = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        diesel::replace_into(memory_cursor::table)
            .values(&crate::bindings::db::MemoryCursorRow {
                instance_id: self.instance_id.clone(),
                current_cursor: cursor,
                updated_at: now,
            })
            .execute(conn)
            .context("[MEMORY-DB] Failed to update cursor")?;

        Ok(())
    }

    // ── Internal helpers ──

    fn session_block_path(&self, name: &str) -> PathBuf {
        self.sessions_dir.join(format!("{}.{}", name, SESSION_EXT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Memory) {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let memory = Memory::open(&memory_dir, "test-instance").unwrap();
        (tmp, memory)
    }

    #[test]
    fn test_open_creates_directories() {
        let (tmp, _memory) = setup();
        assert!(tmp.path().join("memory").exists());
        assert!(tmp.path().join("memory/sessions").exists());
    }

    #[test]
    fn test_open_empty_files() {
        let (_tmp, memory) = setup();
        assert_eq!(memory.knowledge.read().unwrap(), "");
        assert_eq!(memory.history.read().unwrap(), "");
        assert_eq!(memory.current.read().unwrap(), "");
    }

    #[test]
    fn test_open_existing_files() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let sessions_dir = memory_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(memory_dir.join("knowledge.md"), "existing knowledge").unwrap();
        std::fs::write(sessions_dir.join("history.txt"), "existing history").unwrap();
        std::fs::write(sessions_dir.join("current.txt"), "existing current").unwrap();

        let memory = Memory::open(&memory_dir, "test-instance").unwrap();
        assert_eq!(memory.knowledge.read().unwrap(), "existing knowledge");
        assert_eq!(memory.history.read().unwrap(), "existing history");
        assert_eq!(memory.current.read().unwrap(), "existing current");
    }

    #[test]
    fn test_append_current() {
        let (_tmp, mut memory) = setup();
        memory.append_current("line 1\n").unwrap();
        memory.append_current("line 2\n").unwrap();
        assert_eq!(memory.current.read().unwrap(), "line 1\n\nline 2\n");

        // Verify persisted
        assert_eq!(memory.current.read().unwrap(), "line 1\n\nline 2\n");
    }

    #[test]
    fn test_write_current() {
        let (_tmp, mut memory) = setup();
        memory.append_current("old content").unwrap();
        memory.write_current("new content").unwrap();
        assert_eq!(memory.current.read().unwrap(), "new content");
        assert_eq!(memory.current.read().unwrap(), "new content");
    }

    #[test]
    fn test_session_blocks() {
        let (_tmp, memory) = setup();

        // Initially empty
        assert_eq!(memory.list_session_blocks().unwrap(), Vec::<String>::new());

        // Write a block
        memory
            .write_session_block("20260301120000", "line1\nline2\n")
            .unwrap();
        assert_eq!(
            memory.list_session_blocks().unwrap(),
            vec!["20260301120000"]
        );

        // Read it back
        let content = memory.read_session_block("20260301120000").unwrap();
        assert_eq!(content, "line1\nline2\n");

        // Append to it
        memory
            .append_session_block("20260301120000", "line3\n")
            .unwrap();
        let content = memory.read_session_block("20260301120000").unwrap();
        assert_eq!(content, "line1\nline2\nline3\n");

        // Size
        assert!(memory.session_block_size("20260301120000") > 0);
        assert_eq!(memory.session_block_size("nonexistent"), 0);

        // Delete
        memory.delete_session_block("20260301120000").unwrap();
        assert_eq!(memory.list_session_blocks().unwrap(), Vec::<String>::new());

        // Delete nonexistent is ok
        memory.delete_session_block("nonexistent").unwrap();
    }

    #[test]
    fn test_session_blocks_sorted() {
        let (_tmp, memory) = setup();
        memory.write_session_block("20260301150000", "b").unwrap();
        memory.write_session_block("20260301120000", "a").unwrap();
        memory.write_session_block("20260301180000", "c").unwrap();

        let blocks = memory.list_session_blocks().unwrap();
        assert_eq!(
            blocks,
            vec!["20260301120000", "20260301150000", "20260301180000"]
        );
    }

    #[test]
    fn test_commit_summary() {
        let (_tmp, mut memory) = setup();

        // Setup: some current content
        memory.append_current("thinking about stuff\n").unwrap();

        // Commit summary
        let entry = SessionBlockEntry {
            first_msg: "MSG001".to_string(),
            last_msg: "MSG002".to_string(),
            summary: "session data".to_string(),
        };
        let block_name = memory.commit_summary(&entry, 100).unwrap();

        // Verify: session block written with serialized entry
        let entries = memory.read_session_entries(&block_name).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "session data");
        assert_eq!(entries[0].first_msg, "MSG001");

        // Verify: knowledge updated
        assert_eq!(memory.knowledge.read().unwrap(), "");

        // Verify: current cleared
        assert_eq!(memory.current.read().unwrap(), "");
    }

    #[test]
    fn test_commit_history() {
        let (_tmp, mut memory) = setup();

        // Setup: write a session block and initial history
        memory
            .write_session_block("20260301120000", "old session data\n")
            .unwrap();
        memory.history.write("old history").unwrap();

        // Commit history (roll)
        memory
            .commit_history("compressed new history", "20260301120000")
            .unwrap();

        // Verify: history updated
        assert_eq!(memory.history.read().unwrap(), "compressed new history");

        // Verify: old session block deleted
        assert!(memory
            .read_session_block("20260301120000")
            .unwrap()
            .is_empty());

        // Verify: .last_rolled marker is cleared after successful commit
        assert!(memory.get_last_rolled().is_none());

        // Verify persistence
        assert_eq!(memory.history.read().unwrap(), "compressed new history");
    }

    #[test]
    fn test_last_rolled_marker() {
        let (_tmp, memory) = setup();

        // Initially no marker
        assert!(memory.get_last_rolled().is_none());

        // Set marker
        memory.set_last_rolled("20260301120000");
        assert_eq!(memory.get_last_rolled(), Some("20260301120000".to_string()));

        // Clear marker
        memory.clear_last_rolled();
        assert!(memory.get_last_rolled().is_none());

        // Clear nonexistent is ok
        memory.clear_last_rolled();
    }

    #[test]
    fn test_commit_summary_appends_to_existing_session() {
        let (_tmp, mut memory) = setup();

        // First summary (small block limit so second appends to same block)
        let entry1 = SessionBlockEntry {
            first_msg: "MSG001".to_string(),
            last_msg: "MSG002".to_string(),
            summary: "first summary".to_string(),
        };
        let block1 = memory.commit_summary(&entry1, 100).unwrap();

        // Second summary appends to same block (under size limit)
        memory.append_current("more thinking\n").unwrap();
        let entry2 = SessionBlockEntry {
            first_msg: "MSG003".to_string(),
            last_msg: "MSG004".to_string(),
            summary: "second summary".to_string(),
        };
        let block2 = memory.commit_summary(&entry2, 100).unwrap();

        // Both went to same block
        assert_eq!(block1, block2);
        let entries = memory.read_session_entries(&block1).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "first summary");
        assert_eq!(entries[1].summary, "second summary");

        // Knowledge is latest version
        assert_eq!(memory.knowledge.read().unwrap(), "");
    }

    #[test]
    fn test_memory_dir_accessor() {
        let (tmp, memory) = setup();
        assert_eq!(memory.memory_dir(), tmp.path().join("memory"));
        assert_eq!(memory.sessions_dir(), tmp.path().join("memory/sessions"));
    }
}
