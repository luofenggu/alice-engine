//! # Chat Module
//!
//! SQLite-based chat history and message channel.
//! Shared between Java Web and Rust Engine via WAL mode.
//!
//! @TRACE: MSG — All message I/O operations.
//!
//! ## Schema
//!
//! - `messages` table: chat history + inbox/outbox
//! - `engine_status` table: engine runtime status (e.g. current_infer_log_path)
//!
//! ## Concurrency
//!
//! SQLite WAL mode allows concurrent readers with one writer.
//! `busy_timeout=5000` prevents immediate lock failures.
//! All write operations are serialized through `&mut self`.

use std::path::Path;
use anyhow::{Result, Context};
use rusqlite::{Connection, params};
use tracing::info;

// ---------------------------------------------------------------------------
// Data models
// ---------------------------------------------------------------------------

/// A chat message for display (query results).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

/// A message within a time range (for summary).
#[derive(Debug, Clone)]
pub struct RangeMessage {
    pub sender: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

/// An unread inbox message from a user.
#[derive(Debug, Clone)]
pub struct InboxMessage {
    pub id: i64,
    pub sender: String,
    pub content: String,
    pub timestamp: String,
    pub msg_type: String,
}

/// Paginated query result.
#[derive(Debug)]
pub struct QueryResult {
    pub messages: Vec<ChatMessage>,
    pub total: i64,
    pub start_id: i64,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// ChatHistory — the unified SQLite interface
// ---------------------------------------------------------------------------

/// @TRACE: MSG
///
/// Unified chat history and message channel backed by SQLite.
/// Java Web writes user messages; Rust Engine reads them and writes replies.
/// Both sides share the same `chat.db` file via WAL mode.
pub struct ChatHistory {
    conn: Connection,
}

impl ChatHistory {
    /// Open (or create) the chat database at the given path.
    ///
    /// @TRACE: MSG — `[MSG] ChatHistory initialized: {path}`
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open chat.db at {:?}", db_path))?;

        // WAL mode for cross-process concurrency
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;

        // Create tables
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sender TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                read_status TEXT NOT NULL DEFAULT 'read',
                msg_type TEXT NOT NULL DEFAULT 'chat'
            );
            CREATE INDEX IF NOT EXISTS idx_messages_id ON messages(id);
            CREATE INDEX IF NOT EXISTS idx_messages_unread ON messages(role, read_status);
            CREATE TABLE IF NOT EXISTS engine_status (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );"
        )?;

        info!("[MSG] ChatHistory initialized: {:?}", db_path);
        Ok(Self { conn })
    }

    // ==================== Chat History ====================

    /// Append a message to history (marked as read, for display).
    ///
    /// @TRACE: MSG
    pub fn append(&mut self, sender: &str, role: &str, content: &str, timestamp: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages (sender, role, content, timestamp, read_status, msg_type) VALUES (?1, ?2, ?3, ?4, 'read', 'chat')",
            params![sender, role, content, timestamp],
        )?;
        Ok(())
    }

    /// Query messages with pagination.
    ///
    /// @TRACE: MSG
    pub fn query(&self, limit: i64, before: i64) -> Result<QueryResult> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages", [], |row| row.get(0)
        )?;

        let (messages, start_id, has_more) = if before <= 0 {
            let msgs = self.query_last_n(limit)?;
            let sid = msgs.first().map(|m| m.id).unwrap_or(0);
            let more = sid > 1;
            (msgs, sid, more)
        } else {
            let msgs = self.query_before_id(before, limit)?;
            let sid = msgs.first().map(|m| m.id).unwrap_or(0);
            let more = sid > 1;
            (msgs, sid, more)
        };

        Ok(QueryResult { messages, total, start_id, has_more })
    }

    fn query_last_n(&self, limit: i64) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content, timestamp FROM messages ORDER BY id DESC LIMIT ?1"
        )?;
        let mut rows: Vec<ChatMessage> = stmt.query_map(params![limit], |row| {
            Ok(ChatMessage {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                timestamp: row.get(3)?,
            })
        })?.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    fn query_before_id(&self, before_id: i64, limit: i64) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content, timestamp FROM messages WHERE id < ?1 ORDER BY id DESC LIMIT ?2"
        )?;
        let mut rows: Vec<ChatMessage> = stmt.query_map(params![before_id, limit], |row| {
            Ok(ChatMessage {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                timestamp: row.get(3)?,
            })
        })?.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Get the timestamp of the last message (as epoch millis, 0 if empty).
    pub fn get_last_message_time(&self) -> Result<i64> {
        let result: Option<String> = self.conn.query_row(
            "SELECT timestamp FROM messages ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        ).ok();

        match result {
            Some(ts) => {
                // Parse "yyyyMMddHHmmss" format to epoch millis
                Ok(parse_timestamp_to_millis(&ts).unwrap_or(0))
            }
            None => Ok(0),
        }
    }

    // ==================== Message Channel (Inbox/Outbox) ====================

    /// Write a user message (unread, for engine to pick up).
    /// Called by Java Web when user sends a message.
    ///
    /// @TRACE: MSG
    pub fn write_user_message(
        &mut self, sender: &str, content: &str, timestamp: &str, msg_type: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO messages (sender, role, content, timestamp, read_status, msg_type) VALUES (?1, 'user', ?2, ?3, 'unread', ?4)",
            params![sender, content, timestamp, msg_type],
        )?;
        let id = self.conn.last_insert_rowid();
        info!("[MSG] User message written: id={}, sender={}, type={}", id, sender, msg_type);
        Ok(id)
    }

    /// Read all unread user messages (inbox) and mark them as read.
    ///
    /// @TRACE: MSG
    pub fn read_unread_user_messages(&mut self) -> Result<Vec<InboxMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sender, content, timestamp, msg_type FROM messages WHERE role='user' AND read_status='unread' ORDER BY id ASC"
        )?;
        let messages: Vec<InboxMessage> = stmt.query_map([], |row| {
            Ok(InboxMessage {
                id: row.get(0)?,
                sender: row.get(1)?,
                content: row.get(2)?,
                timestamp: row.get(3)?,
                msg_type: row.get(4)?,
            })
        })?.collect::<std::result::Result<Vec<_>, _>>()?;

        if !messages.is_empty() {
            let ids: Vec<String> = messages.iter().map(|m| m.id.to_string()).collect();
            let id_list = ids.join(",");
            self.conn.execute(
                &format!("UPDATE messages SET read_status='read' WHERE id IN ({})", id_list),
                [],
            )?;
            info!("[MSG] Read {} unread user messages", messages.len());
        }

        Ok(messages)
    }

    /// Count unread user messages.
    pub fn count_unread_user_messages(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE role='user' AND read_status='unread'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Write an agent reply (unread, for Web to pick up and display).
    ///
    /// @TRACE: MSG
    pub fn write_agent_reply(&mut self, sender: &str, content: &str, timestamp: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages (sender, role, content, timestamp, read_status, msg_type) VALUES (?1, 'agent', ?2, ?3, 'unread', 'chat')",
            params![sender, content, timestamp],
        )?;
        info!("[MSG] Agent reply written: sender={}", sender);
        Ok(())
    }

    /// Read all unread agent replies and mark them as read.
    /// Returns Vec<(id, content, timestamp)> for deduplication on frontend.
    pub fn read_unread_agent_replies(&mut self) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, timestamp FROM messages WHERE role='agent' AND read_status='unread' ORDER BY id ASC"
        )?;
        let rows: Vec<(i64, String, String)> = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?.collect::<std::result::Result<Vec<_>, _>>()?;

        if !rows.is_empty() {
            let ids: Vec<String> = rows.iter().map(|(id, _, _)| id.to_string()).collect();
            let id_list = ids.join(",");
            self.conn.execute(
                &format!("UPDATE messages SET read_status='read' WHERE id IN ({})", id_list),
                [],
            )?;
        }

        Ok(rows)
    }

    /// Get agent replies with id > after_id (for polling without read_status).
    /// Returns Vec<(id, content, timestamp)>.
    pub fn get_agent_replies_after(&self, after_id: i64) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, timestamp FROM messages WHERE role='agent' AND id > ?1 ORDER BY id ASC"
        )?;
        let rows: Vec<(i64, String, String)> = stmt.query_map([after_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get all messages (any role) after a given id, for multi-client sync
    pub fn get_messages_after(&self, after_id: i64) -> Result<Vec<(i64, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content, timestamp FROM messages WHERE id > ?1 ORDER BY id ASC"
        )?;
        let rows: Vec<(i64, String, String, String)> = stmt.query_map([after_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ==================== Time Range Query ====================

    /// Read all messages within a timestamp range (inclusive).
    /// Timestamps are in "yyyyMMddHHmmss" format.
    pub fn read_messages_in_range(&self, start: &str, end: &str) -> Result<Vec<RangeMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT sender, role, content, timestamp FROM messages WHERE timestamp >= ?1 AND timestamp <= ?2 ORDER BY id ASC LIMIT 50"
        )?;
        let messages: Vec<RangeMessage> = stmt.query_map(params![start, end], |row| {
            Ok(RangeMessage {
                sender: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                timestamp: row.get(3)?,
            })
        })?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    // ==================== Engine Status ====================

    /// Update engine status (JSON string stored under "status" key).
    ///
    /// @TRACE: MSG
    pub fn update_status(&mut self, status_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO engine_status (key, value) VALUES ('status', ?1)",
            params![status_json],
        )?;
        Ok(())
    }

    /// Read engine status JSON.
    pub fn read_status(&self) -> Result<Option<String>> {
        let result: Option<String> = self.conn.query_row(
            "SELECT value FROM engine_status WHERE key='status'",
            [],
            |row| row.get(0),
        ).ok();
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse "yyyyMMddHHmmss" timestamp to epoch millis.
fn parse_timestamp_to_millis(ts: &str) -> Option<i64> {
    if ts.len() < 14 {
        return None;
    }
    let year: i32 = ts[0..4].parse().ok()?;
    let month: u32 = ts[4..6].parse().ok()?;
    let day: u32 = ts[6..8].parse().ok()?;
    let hour: u32 = ts[8..10].parse().ok()?;
    let min: u32 = ts[10..12].parse().ok()?;
    let sec: u32 = ts[12..14].parse().ok()?;

    use chrono::{NaiveDate, NaiveTime, NaiveDateTime};
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let time = NaiveTime::from_hms_opt(hour, min, sec)?;
    let dt = NaiveDateTime::new(date, time);
    Some(dt.and_utc().timestamp_millis())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (ChatHistory, TempDir) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test_chat.db");
        let ch = ChatHistory::open(&db_path).unwrap();
        (ch, tmp)
    }

    #[test]
    fn test_open_creates_tables() {
        let (ch, _tmp) = setup();
        // Verify tables exist by querying them
        let count: i64 = ch.conn.query_row(
            "SELECT COUNT(*) FROM messages", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_append_and_query() {
        let (mut ch, _tmp) = setup();
        ch.append("user1", "user", "hello", "20260220120000").unwrap();
        ch.append("agent", "assistant", "hi there", "20260220120001").unwrap();

        let result = ch.query(10, 0).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[1].role, "assistant");
    }

    #[test]
    fn test_query_pagination() {
        let (mut ch, _tmp) = setup();
        for i in 0..10 {
            ch.append("user", "user", &format!("msg{}", i), "20260220120000").unwrap();
        }

        // Get last 3
        let result = ch.query(3, 0).unwrap();
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.total, 10);
        assert!(result.has_more);

        // Get 3 before the start_id
        let result2 = ch.query(3, result.start_id).unwrap();
        assert_eq!(result2.messages.len(), 3);
    }

    #[test]
    fn test_user_message_inbox() {
        let (mut ch, _tmp) = setup();

        // Write user messages (unread)
        ch.write_user_message("24007", "hello agent", "20260220120000", "chat").unwrap();
        ch.write_user_message("24007", "are you there?", "20260220120001", "chat").unwrap();

        // Count unread
        assert_eq!(ch.count_unread_user_messages().unwrap(), 2);

        // Read unread (marks as read)
        let msgs = ch.read_unread_user_messages().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello agent");
        assert_eq!(msgs[1].content, "are you there?");

        // Now count should be 0
        assert_eq!(ch.count_unread_user_messages().unwrap(), 0);

        // Read again should return empty
        let msgs2 = ch.read_unread_user_messages().unwrap();
        assert!(msgs2.is_empty());
    }

    #[test]
    fn test_agent_reply_outbox() {
        let (mut ch, _tmp) = setup();

        ch.write_agent_reply("alice", "hello user!", "20260220120000").unwrap();

        let replies = ch.read_unread_agent_replies().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1, "hello user!");

        // Read again should be empty
        let replies2 = ch.read_unread_agent_replies().unwrap();
        assert!(replies2.is_empty());
    }

    #[test]
    fn test_engine_status() {
        let (mut ch, _tmp) = setup();

        // Initially no status
        assert!(ch.read_status().unwrap().is_none());

        // Update status
        ch.update_status(r#"{"inferring": true, "log": "/tmp/out.log"}"#).unwrap();
        let status = ch.read_status().unwrap().unwrap();
        assert!(status.contains("inferring"));

        // Update again (upsert)
        ch.update_status(r#"{"inferring": false}"#).unwrap();
        let status2 = ch.read_status().unwrap().unwrap();
        assert!(status2.contains("false"));
    }

    #[test]
    fn test_get_last_message_time() {
        let (mut ch, _tmp) = setup();

        // Empty db
        assert_eq!(ch.get_last_message_time().unwrap(), 0);

        // Add a message
        ch.append("user", "user", "test", "20260220120000").unwrap();
        let time = ch.get_last_message_time().unwrap();
        assert!(time > 0);
    }

    #[test]
    fn test_parse_timestamp() {
        let millis = parse_timestamp_to_millis("20260220120000").unwrap();
        assert!(millis > 0);

        // Invalid
        assert!(parse_timestamp_to_millis("short").is_none());
    }

    #[test]
    fn test_mixed_read_write() {
        let (mut ch, _tmp) = setup();

        // User sends message
        ch.write_user_message("24007", "hi", "20260220120000", "chat").unwrap();

        // Agent reads it
        let inbox = ch.read_unread_user_messages().unwrap();
        assert_eq!(inbox.len(), 1);

        // Agent replies
        ch.write_agent_reply("alice", "hello!", "20260220120001").unwrap();

        // Append to history (display)
        ch.append("24007", "user", "hi", "20260220120000").unwrap();
        ch.append("alice", "assistant", "hello!", "20260220120001").unwrap();

        // Query should show all messages (inbox + history)
        let result = ch.query(100, 0).unwrap();
        // 1 user msg (inbox, now read) + 1 agent reply + 2 appended = 4
        assert_eq!(result.total, 4);
    }
}