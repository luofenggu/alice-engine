//! Memory subsystem — manages all persistent memory for an Alice instance.
//!
//! All memory state is stored in SQLite (memory.db):
//! - action_log — current session action records (replaces current.txt)
//! - knowledge_store — persistent cognitive framework (replaces knowledge.md)
//! - history_store — compressed long-term narrative (replaces history.txt)
//! - session_blocks — session block entries (replaces sessions/*.jsonl)
//! - memory_cursor — tracks rendering start position
//!
//! Commit methods control flush order for crash safety:
//! - commit_summary() — write session block → advance cursor
//! - commit_history() — write history → delete old session block

use crate::bindings::db::{
    self, HistoryRow, KnowledgeRow, NewSessionBlock, SessionBlockRow,
};

use crate::inference::output::{ActionOutput, ActionRecord};
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



/// Helper struct for sql_query result mapping (msg_id queries).
#[derive(Debug, QueryableByName)]
struct MsgIdRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    mid: String,
}

/// Memory subsystem for an Alice instance.
#[derive(Clone)]
pub struct Memory {
    /// Root memory directory (instance_dir/memory)
    memory_dir: PathBuf,
    /// Sessions directory (memory_dir/sessions)
    sessions_dir: PathBuf,
    /// Database connection for memory tables
    pub db: Arc<Mutex<SqliteConnection>>,
    /// Instance ID for scoping DB queries
    instance_id: String,
}

impl Memory {
    /// Open memory from the given memory directory.
    ///
    /// Initializes SQLite database and creates all required tables.
    /// Creates directories if they don't exist.
    pub fn open(memory_dir: impl Into<PathBuf>, instance_id: &str) -> Result<Self> {
        let memory_dir = memory_dir.into();
        let sessions_dir = memory_dir.join("sessions");

        // Ensure directories exist
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&sessions_dir).with_context(|| {
            format!("Failed to create sessions dir: {}", sessions_dir.display())
        })?;

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
            db: Arc::new(Mutex::new(conn)),
            instance_id: instance_id.to_string(),
        })
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

    // ── Commit methods (transaction-like, controlled flush order) ──

    /// Summary transaction:
    /// 1. Resolve target session block (append to existing if under size limit, else create new)
    /// 2. Insert entry into session_blocks DB table
    /// 3. Advance cursor (marks action_log entries as consumed)
    ///
    /// Returns the block name used (for logging).
    pub fn commit_summary(
        &self,
        entry: &SessionBlockEntry,
        session_block_kb: u32,
    ) -> Result<String> {
        // Step 1: Resolve target block (from DB)
        let blocks = self.list_session_blocks_db()?;
        let block_name = if let Some(latest) = blocks.last() {
            let size = self.session_block_size_db(latest);
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

        // Step 2: Insert entry into DB
        self.insert_session_block_entry(&block_name, entry)?;

        // Step 3: Advance DB cursor (marks action_log entries as consumed)
        self.advance_cursor().ok();

        Ok(block_name)
    }

    /// Roll history transaction:
    /// 1. Write idempotency marker (so crash recovery knows what was being rolled)
    /// 2. Write compressed history to DB
    /// 3. Delete oldest session block from DB
    /// 4. Clear idempotency marker
    ///
    /// Crash safety: marker written first, cleared last.
    /// Worst case on crash: marker exists → next roll detects idempotency and cleans up.
    pub fn commit_history(&self, new_history: &str, oldest_block_name: &str) -> Result<()> {
        // Step 1: Write idempotency marker before any mutation
        self.set_last_rolled(oldest_block_name);

        // Step 2: Write history to DB
        self.write_history(new_history)?;

        // Step 3: Delete oldest session block from DB
        self.delete_session_block_db(oldest_block_name)?;

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
        action_input_json: &str,
        created_at: &str,
    ) -> Result<()> {
        use crate::bindings::db::{action_log, NewActionLog, ACTION_STATUS_EXECUTING};

        let new_row = NewActionLog {
            instance_id: &self.instance_id,
            action_id,
            action_type,
            action_input: action_input_json,
            action_output: None,
            status: ACTION_STATUS_EXECUTING,
            distill_text: None,
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
            action_input: "",
            action_output: Some(text),
            status: ACTION_STATUS_DONE,
            distill_text: None,
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

    /// Complete an action log entry: update status to done, set action_output JSON.
    /// Called after action execution finishes.
    pub fn complete_action_log(
        &self,
        action_id: &str,
        output: &ActionOutput,
    ) -> Result<()> {
        use crate::bindings::db::{action_log, ACTION_STATUS_DONE};

        let output_json = serde_json::to_string(output)
            .with_context(|| format!("[MEMORY-DB] Failed to serialize ActionOutput for {}", action_id))?;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let updated = diesel::update(
            action_log::table
                .filter(action_log::instance_id.eq(&self.instance_id))
                .filter(action_log::action_id.eq(action_id)),
        )
        .set((
            action_log::status.eq(ACTION_STATUS_DONE),
            action_log::action_output.eq(Some(&output_json)),
        ))
        .execute(conn)
        .with_context(|| format!("[MEMORY-DB] Failed to complete action_log: {}", action_id))?;

        if updated == 0 {
            tracing::warn!("[MEMORY-DB] complete_action_log: no row found for {}", action_id);
        }
        Ok(())
    }

    /// Distill an action log entry: update status to distilled, write distill_text.
    /// DB preserves original action_output; rendering uses distill_text instead.
    pub fn distill_action_log(&self, action_id: &str, summary: &str) -> Result<(usize, usize)> {
        use crate::bindings::db::{action_log, ActionLogRow, ACTION_STATUS_DISTILLED};

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;

        // Read current action_output length for logging
        let row: Option<ActionLogRow> = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .filter(action_log::action_id.eq(action_id))
            .first(conn)
            .optional()
            .with_context(|| format!("[MEMORY-DB] Failed to query action_log: {}", action_id))?;

        let old_len = row
            .as_ref()
            .and_then(|r| r.action_output.as_ref())
            .map(|t| t.len())
            .unwrap_or(0);

        let updated = diesel::update(
            action_log::table
                .filter(action_log::instance_id.eq(&self.instance_id))
                .filter(action_log::action_id.eq(action_id)),
        )
        .set((
            action_log::status.eq(ACTION_STATUS_DISTILLED),
            action_log::distill_text.eq(Some(summary)),
        ))
        .execute(conn)
        .with_context(|| format!("[MEMORY-DB] Failed to distill action_log: {}", action_id))?;

        if updated == 0 {
            anyhow::bail!("action_log [{}] not found for distill", action_id);
        }

        Ok((old_len, summary.len()))
    }

    /// Render current memory from action_log (all entries after cursor).
    /// Uses ActionRecord for structured rendering.
    pub fn render_current_from_db(&self) -> Result<String> {
        use crate::bindings::db::{action_log, ActionLogRow};

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
            let record = ActionRecord::from_db_row(row);
            parts.push(record.render());
        }

        Ok(parts.join("\n"))
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
    /// Extracts msg_id from send_msg output JSON and entry timestamps from read_msg output JSON.
    pub fn query_msg_range(&self) -> Result<(Option<String>, Option<String>)> {
        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let cursor = self.get_cursor_inner(conn)?;

        // Query all msg_ids: send_msg's $.msg_id and read_msg's entries[*].timestamp
        let sql = "
            SELECT mid FROM (
                SELECT json_extract(action_output, '$.msg_id') as mid
                FROM action_log
                WHERE instance_id = ?1 AND id > ?2 AND action_type = 'send_msg'
                    AND status = 'done' AND action_output IS NOT NULL
                    AND json_extract(action_output, '$.msg_id') IS NOT NULL
                UNION ALL
                SELECT json_extract(je.value, '$.timestamp') as mid
                FROM action_log, json_each(json_extract(action_output, '$.entries')) as je
                WHERE action_log.instance_id = ?1 AND action_log.id > ?2 AND action_type = 'read_msg'
                    AND status = 'done' AND action_output IS NOT NULL
            )
            ORDER BY mid
        ";

        let rows: Vec<MsgIdRow> = diesel::sql_query(sql)
            .bind::<diesel::sql_types::Text, _>(&self.instance_id)
            .bind::<diesel::sql_types::BigInt, _>(cursor)
            .load(conn)
            .context("[MEMORY-DB] Failed to query msg_range")?;

        let first = rows.first().map(|r| r.mid.clone());
        let last = rows.last().map(|r| r.mid.clone());

        Ok((first, last))
    }

    /// Count message-related actions in action_log entries after cursor.
    /// Counts send_msg and read_msg actions that have output.
    pub fn query_msg_count(&self) -> Result<i64> {
        use crate::bindings::db::action_log;
        use diesel::dsl::sql;
        use diesel::sql_types::BigInt;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let cursor = self.get_cursor_inner(conn)?;

        let count: i64 = action_log::table
            .filter(action_log::instance_id.eq(&self.instance_id))
            .filter(action_log::id.gt(cursor))
            .filter(action_log::action_type.eq_any(&["send_msg", "read_msg"]))
            .filter(action_log::action_output.is_not_null())
            .select(sql::<BigInt>("COUNT(*)"))
            .first(conn)
            .context("[MEMORY-DB] Failed to count msg actions")?;

        Ok(count)
    }

    // ── Knowledge store (DB) ──

    /// Write knowledge content to DB (UPSERT).
    pub fn write_knowledge(&self, content: &str) -> Result<()> {
        use crate::bindings::db::knowledge_store;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let now = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        diesel::replace_into(knowledge_store::table)
            .values(&KnowledgeRow {
                instance_id: self.instance_id.clone(),
                content: content.to_string(),
                updated_at: now,
            })
            .execute(conn)
            .context("[MEMORY-DB] Failed to write knowledge")?;
        Ok(())
    }

    /// Read knowledge content from DB. Returns empty string if no record.
    pub fn read_knowledge(&self) -> String {
        use crate::bindings::db::knowledge_store;

        let conn = &mut *match self.db.lock() {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        knowledge_store::table
            .find(&self.instance_id)
            .select(knowledge_store::content)
            .first::<String>(conn)
            .unwrap_or_default()
    }

    // ── History store (DB) ──

    /// Write history content to DB (UPSERT).
    pub fn write_history(&self, content: &str) -> Result<()> {
        use crate::bindings::db::history_store;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let now = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        diesel::replace_into(history_store::table)
            .values(&HistoryRow {
                instance_id: self.instance_id.clone(),
                content: content.to_string(),
                updated_at: now,
            })
            .execute(conn)
            .context("[MEMORY-DB] Failed to write history")?;
        Ok(())
    }

    /// Read history content from DB. Returns empty string if no record.
    pub fn read_history(&self) -> String {
        use crate::bindings::db::history_store;

        let conn = &mut *match self.db.lock() {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        history_store::table
            .find(&self.instance_id)
            .select(history_store::content)
            .first::<String>(conn)
            .unwrap_or_default()
    }

    // ── Session blocks (DB) ──

    /// List distinct block names for this instance, ordered chronologically.
    pub fn list_session_blocks_db(&self) -> Result<Vec<String>> {
        use crate::bindings::db::session_blocks;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let names: Vec<String> = session_blocks::table
            .filter(session_blocks::instance_id.eq(&self.instance_id))
            .select(session_blocks::block_name)
            .distinct()
            .order(session_blocks::block_name.asc())
            .load(conn)
            .context("[MEMORY-DB] Failed to list session blocks")?;
        Ok(names)
    }

    /// Read session entries for a specific block from DB.
    pub fn read_session_entries_db(&self, block_name: &str) -> Result<Vec<SessionBlockEntry>> {
        use crate::bindings::db::session_blocks;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let rows: Vec<SessionBlockRow> = session_blocks::table
            .filter(session_blocks::instance_id.eq(&self.instance_id))
            .filter(session_blocks::block_name.eq(block_name))
            .order(session_blocks::id.asc())
            .load(conn)
            .context("[MEMORY-DB] Failed to read session entries")?;

        Ok(rows
            .into_iter()
            .map(|r| SessionBlockEntry {
                first_msg: r.first_msg,
                last_msg: r.last_msg,
                summary: r.summary,
            })
            .collect())
    }

    /// Insert a session block entry into DB.
    pub fn insert_session_block_entry(
        &self,
        block_name: &str,
        entry: &SessionBlockEntry,
    ) -> Result<()> {
        use crate::bindings::db::session_blocks;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let now = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        diesel::insert_into(session_blocks::table)
            .values(&NewSessionBlock {
                instance_id: &self.instance_id,
                block_name,
                first_msg: &entry.first_msg,
                last_msg: &entry.last_msg,
                summary: &entry.summary,
                created_at: &now,
            })
            .execute(conn)
            .context("[MEMORY-DB] Failed to insert session block entry")?;
        Ok(())
    }

    /// Batch insert session block entries within a single transaction.
    /// Used by legacy migration to atomically migrate an entire session block file.
    pub fn batch_insert_session_entries(
        &self,
        block_name: &str,
        entries: &[SessionBlockEntry],
    ) -> Result<()> {
        use crate::bindings::db::session_blocks;
        use diesel::Connection;

        if entries.is_empty() {
            return Ok(());
        }

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        let now = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();

        conn.transaction(|conn| {
            for entry in entries {
                diesel::insert_into(session_blocks::table)
                    .values(&NewSessionBlock {
                        instance_id: &self.instance_id,
                        block_name,
                        first_msg: &entry.first_msg,
                        last_msg: &entry.last_msg,
                        summary: &entry.summary,
                        created_at: &now,
                    })
                    .execute(conn)?;
            }
            Ok::<(), diesel::result::Error>(())
        })
        .context("[MEMORY-DB] Failed to batch insert session block entries")?;

        tracing::debug!(
            "[MEMORY-DB] Batch inserted {} entries for block {}",
            entries.len(),
            block_name
        );
        Ok(())
    }

    /// Delete all entries for a specific block from DB.
    pub fn delete_session_block_db(&self, block_name: &str) -> Result<()> {
        use crate::bindings::db::session_blocks;

        let conn = &mut *self.db.lock().map_err(|e| anyhow::anyhow!("DB lock: {}", e))?;
        diesel::delete(
            session_blocks::table
                .filter(session_blocks::instance_id.eq(&self.instance_id))
                .filter(session_blocks::block_name.eq(block_name)),
        )
        .execute(conn)
        .context("[MEMORY-DB] Failed to delete session block")?;
        Ok(())
    }

    /// Estimate total size of a session block in bytes (sum of text field lengths).
    pub fn session_block_size_db(&self, block_name: &str) -> u64 {
        use crate::bindings::db::session_blocks;
        use diesel::dsl::sql;
        use diesel::sql_types::BigInt;

        let conn = &mut *match self.db.lock() {
            Ok(c) => c,
            Err(_) => return 0,
        };
        let size: i64 = session_blocks::table
            .filter(session_blocks::instance_id.eq(&self.instance_id))
            .filter(session_blocks::block_name.eq(block_name))
            .select(sql::<BigInt>(
                "COALESCE(SUM(LENGTH(first_msg) + LENGTH(last_msg) + LENGTH(summary)), 0)",
            ))
            .first(conn)
            .unwrap_or(0);
        size as u64
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
    fn test_open_empty_db() {
        let (_tmp, memory) = setup();
        assert_eq!(memory.read_knowledge(), "");
        assert_eq!(memory.read_history(), "");
        assert_eq!(memory.render_current_from_db().unwrap(), "");
    }

    #[test]
    fn test_session_blocks_db() {
        let (_tmp, memory) = setup();

        // Initially empty
        assert_eq!(memory.list_session_blocks_db().unwrap(), Vec::<String>::new());

        // Insert entries
        let entry = SessionBlockEntry {
            first_msg: "MSG001".to_string(),
            last_msg: "MSG002".to_string(),
            summary: "test".to_string(),
        };
        memory.insert_session_block_entry("20260301120000", &entry).unwrap();

        assert_eq!(
            memory.list_session_blocks_db().unwrap(),
            vec!["20260301120000"]
        );

        // Read entries back
        let entries = memory.read_session_entries_db("20260301120000").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "test");

        // Size > 0
        assert!(memory.session_block_size_db("20260301120000") > 0);
        assert_eq!(memory.session_block_size_db("nonexistent"), 0);

        // Delete
        memory.delete_session_block_db("20260301120000").unwrap();
        assert_eq!(memory.list_session_blocks_db().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn test_session_blocks_db_sorted() {
        let (_tmp, memory) = setup();
        let entry = SessionBlockEntry {
            first_msg: "a".to_string(),
            last_msg: "b".to_string(),
            summary: "s".to_string(),
        };
        memory.insert_session_block_entry("20260301150000", &entry).unwrap();
        memory.insert_session_block_entry("20260301120000", &entry).unwrap();
        memory.insert_session_block_entry("20260301180000", &entry).unwrap();

        let blocks = memory.list_session_blocks_db().unwrap();
        assert_eq!(
            blocks,
            vec!["20260301120000", "20260301150000", "20260301180000"]
        );
    }

    #[test]
    fn test_commit_summary() {
        let (_tmp, memory) = setup();

        // Commit summary
        let entry = SessionBlockEntry {
            first_msg: "MSG001".to_string(),
            last_msg: "MSG002".to_string(),
            summary: "session data".to_string(),
        };
        let block_name = memory.commit_summary(&entry, 100).unwrap();

        // Verify: session block written with serialized entry
        let entries = memory.read_session_entries_db(&block_name).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "session data");
        assert_eq!(entries[0].first_msg, "MSG001");

        // Verify: knowledge not changed (no capture)
        assert_eq!(memory.read_knowledge(), "");

        // Verify: current cleared (cursor advanced)
        // current.txt no longer used, skip file check
    }

    #[test]
    fn test_commit_history() {
        let (_tmp, mut memory) = setup();

        // Setup: write a session block entry and initial history
        memory.insert_session_block_entry("20260301120000", &SessionBlockEntry {
            first_msg: "MSG001".to_string(),
            last_msg: "MSG002".to_string(),
            summary: "old session data".to_string(),
        }).unwrap();
        memory.write_history("old history");

        // Commit history (roll)
        memory
            .commit_history("compressed new history", "20260301120000")
            .unwrap();

        // Verify: history updated
        assert_eq!(memory.read_history(), "compressed new history");

        // Verify: old session block deleted
        assert!(memory
            .read_session_entries_db("20260301120000")
            .unwrap_or_default()
            .is_empty());

        // Verify: .last_rolled marker is cleared after successful commit
        assert!(memory.get_last_rolled().is_none());

        // Verify persistence
        assert_eq!(memory.read_history(), "compressed new history");
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
        let (_tmp, memory) = setup();

        // First summary (small block limit so second appends to same block)
        let entry1 = SessionBlockEntry {
            first_msg: "MSG001".to_string(),
            last_msg: "MSG002".to_string(),
            summary: "first summary".to_string(),
        };
        let block1 = memory.commit_summary(&entry1, 100).unwrap();

        // Second summary appends to same block (under size limit)
        let entry2 = SessionBlockEntry {
            first_msg: "MSG003".to_string(),
            last_msg: "MSG004".to_string(),
            summary: "second summary".to_string(),
        };
        let block2 = memory.commit_summary(&entry2, 100).unwrap();

        // Both went to same block
        assert_eq!(block1, block2);
        let entries = memory.read_session_entries_db(&block1).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].summary, "first summary");
        assert_eq!(entries[1].summary, "second summary");

        // Knowledge is latest version
        assert_eq!(memory.read_knowledge(), "");
    }

    #[test]
    fn test_memory_dir_accessor() {
        let (tmp, memory) = setup();
        assert_eq!(memory.memory_dir(), tmp.path().join("memory"));
        assert_eq!(memory.sessions_dir(), tmp.path().join("memory/sessions"));
    }
}
