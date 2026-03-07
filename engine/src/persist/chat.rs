//! # Chat Module
//!
//! Message storage and channel backed by SQLite.
//! All queries go directly to the database — no in-memory caching.
//!
//! @TRACE: MSG — All message I/O operations.

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;

// ---------------------------------------------------------------------------
// Message — the core chat record
// ---------------------------------------------------------------------------

/// A message in the chat history.
///
/// This is the single source of truth for message structure.
/// All message-related queries and mutations operate on this type.
#[derive(Debug, Clone)]
pub struct Message {
    pub id: i64,
    pub sender: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub read_status: String,
    pub msg_type: String,
    /// Recipient instance ID. Empty string means sent to user (default).
    pub recipient: String,
}

impl Message {
    /// Role value: user message
    const ROLE_USER: &'static str = "user";
    /// Role value: agent/assistant message
    const ROLE_AGENT: &'static str = "agent";

    const ROLE_SYSTEM: &'static str = "system";
    /// Read status: already consumed
    const STATUS_READ: &'static str = "read";
    /// Read status: waiting to be consumed
    const STATUS_UNREAD: &'static str = "unread";
    /// Message type: normal chat
    const TYPE_CHAT: &'static str = "chat";
}

// ---------------------------------------------------------------------------
// Query views — projections of Message for specific use cases
// ---------------------------------------------------------------------------

/// A message for display (chat history view).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

impl From<&Message> for ChatMessage {
    fn from(m: &Message) -> Self {
        ChatMessage {
            id: m.id,
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
        }
    }
}

/// A message within a time range (for summary).
#[derive(Debug, Clone)]
pub struct RangeMessage {
    pub sender: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

impl From<&Message> for RangeMessage {
    fn from(m: &Message) -> Self {
        RangeMessage {
            sender: m.sender.clone(),
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
        }
    }
}

/// An unread inbox message from a user.
#[derive(Debug, Clone)]
pub struct InboxMessage {
    pub id: i64,
    pub sender: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub msg_type: String,
}

impl From<&Message> for InboxMessage {
    fn from(m: &Message) -> Self {
        InboxMessage {
            id: m.id,
            sender: m.sender.clone(),
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
            msg_type: m.msg_type.clone(),
        }
    }
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
// ChatHistory — direct SQLite, no caching
// ---------------------------------------------------------------------------

/// @TRACE: MSG
///
/// Chat history and message channel.
/// All queries go directly to SQLite — no in-memory caching.
pub struct ChatHistory {
    conn: Connection,
}

impl ChatHistory {
    /// Open (or create) the chat database at the given path.
    ///
    /// @TRACE: MSG — `[MSG] ChatHistory initialized: {path}`
    pub fn open(db_path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("failed to open chat db: {}", db_path.display()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sender TEXT NOT NULL DEFAULT '',
                role TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL DEFAULT '',
                timestamp TEXT NOT NULL DEFAULT '',
                read_status TEXT NOT NULL DEFAULT '',
                msg_type TEXT NOT NULL DEFAULT '',
                recipient TEXT NOT NULL DEFAULT ''
            );
",
        )
        .context("failed to create chat tables")?;

        // Migration: add recipient column if missing (for existing DBs)
        let has_recipient: bool = conn
            .prepare("SELECT recipient FROM messages LIMIT 0")
            .is_ok();
        if !has_recipient {
            conn.execute(
                "ALTER TABLE messages ADD COLUMN recipient TEXT NOT NULL DEFAULT ''",
                [],
            )
            .context("failed to add recipient column")?;
            info!("[DB] Migration: added recipient column to messages table");
        }

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap_or(0);

        info!(
            "[MSG] ChatHistory initialized: {:?} ({} messages)",
            db_path, count
        );
        Ok(Self { conn })
    }

    // ==================== Chat History ====================

    /// Append a message to history (marked as read, for display).
    ///
    /// @TRACE: MSG
    pub fn append(
        &mut self,
        sender: &str,
        role: &str,
        content: &str,
        timestamp: &str,
    ) -> Result<()> {
        self.insert_message(
            sender,
            role,
            content,
            timestamp,
            Message::STATUS_READ,
            Message::TYPE_CHAT,
            "",
        )?;
        Ok(())
    }

    /// Query messages with pagination.
    ///
    /// @TRACE: MSG
    pub fn query(&self, limit: i64, before: Option<i64>) -> Result<QueryResult> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap_or(0);

        let rows = match before {
            None => {
                // Last N messages
                let mut stmt = self.conn.prepare(
                    "SELECT id, role, content, timestamp FROM messages ORDER BY id DESC LIMIT ?",
                )?;
                let rows: Vec<ChatMessage> = stmt
                    .query_map([limit], |row| {
                        Ok(ChatMessage {
                            id: row.get(0)?,
                            role: row.get(1)?,
                            content: row.get(2)?,
                            timestamp: row.get(3)?,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                let mut rows = rows;
                rows.reverse(); // Back to ascending order
                rows
            }
            Some(id) => {
                // Messages with id < before, last N
                let mut stmt = self.conn.prepare(
                    "SELECT id, role, content, timestamp FROM messages WHERE id < ? ORDER BY id DESC LIMIT ?"
                )?;
                let rows: Vec<ChatMessage> = stmt
                    .query_map(rusqlite::params![id, limit], |row| {
                        Ok(ChatMessage {
                            id: row.get(0)?,
                            role: row.get(1)?,
                            content: row.get(2)?,
                            timestamp: row.get(3)?,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                let mut rows = rows;
                rows.reverse();
                rows
            }
        };

        let start_id = rows.first().map(|m| m.id).unwrap_or(0);
        let has_more = start_id > 1;

        Ok(QueryResult {
            messages: rows,
            total,
            start_id,
            has_more,
        })
    }

    /// Get the timestamp of the last message (as epoch millis, 0 if empty).
    pub fn get_last_message_time(&self) -> Result<i64> {
        let ts: Option<String> = self
            .conn
            .query_row(
                "SELECT timestamp FROM messages ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();

        match ts {
            Some(t) => Ok(parse_timestamp_to_millis(&t).unwrap_or(0)),
            None => Ok(0),
        }
    }

    // ==================== Message Channel (Inbox/Outbox) ====================

    /// Write a user message (unread, for engine to pick up).
    ///
    /// @TRACE: MSG
    /// Generate a timestamp string in Alice's standard format.
    pub fn now_timestamp() -> String {
        chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
    }

    pub fn write_user_message(
        &mut self,
        sender: &str,
        content: &str,
        timestamp: &str,
    ) -> Result<i64> {
        let id = self.insert_message(
            sender,
            Message::ROLE_USER,
            content,
            timestamp,
            Message::STATUS_UNREAD,
            Message::TYPE_CHAT,
            "",
        )?;
        info!(
            "[MSG] User message written: id={}, sender={}, type={}",
            id,
            sender,
            Message::TYPE_CHAT
        );
        Ok(id)
    }

    /// Read all unread user messages (inbox) and mark them as read.
    ///
    /// @TRACE: MSG
    pub fn read_unread_user_messages(&mut self) -> Result<Vec<InboxMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sender, role, content, timestamp, msg_type FROM messages WHERE role IN ('user', 'system') AND read_status = ?"
        )?;
        let inbox: Vec<InboxMessage> = stmt
            .query_map(
                rusqlite::params![Message::STATUS_UNREAD],
                |row| {
                    Ok(InboxMessage {
                        id: row.get(0)?,
                        sender: row.get(1)?,
                        role: row.get(2)?,
                        content: row.get(3)?,
                        timestamp: row.get(4)?,
                        msg_type: row.get(5)?,
                    })
                },
            )?
            .filter_map(|r| r.ok())
            .collect();

        if !inbox.is_empty() {
            self.conn.execute(
                "UPDATE messages SET read_status = ? WHERE role IN ('user', 'system') AND read_status = ?",
                rusqlite::params![
                    Message::STATUS_READ,
                    Message::STATUS_UNREAD
                ],
            )?;
            info!("[MSG] Read {} unread messages (user+system)", inbox.len());
        }

        Ok(inbox)
    }

    /// Count unread user messages.
    pub fn count_unread_user_messages(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE role IN ('user', 'system') AND read_status = ?",
                rusqlite::params![Message::STATUS_UNREAD],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(count)
    }

    /// Write an agent reply (unread, for Web to pick up and display).
    ///
    /// @TRACE: MSG
    pub fn write_agent_reply(
        &mut self,
        sender: &str,
        content: &str,
        timestamp: &str,
        recipient: &str,
    ) -> Result<()> {
        self.insert_message(
            sender,
            Message::ROLE_AGENT,
            content,
            timestamp,
            Message::STATUS_UNREAD,
            Message::TYPE_CHAT,
            recipient,
        )?;
        info!(
            "[MSG] Agent reply written: sender={}, recipient={}",
            sender,
            if recipient.is_empty() { "user" } else { recipient }
        );
        Ok(())
    }

    /// Write a system message (role=system, read_status=unread).
    /// System messages are delivered to the agent's inbox alongside user messages.
    pub fn write_system_message(
        &mut self,
        content: &str,
        timestamp: &str,
    ) -> Result<i64> {
        let id = self.insert_message(
            "system",
            Message::ROLE_SYSTEM,
            content,
            timestamp,
            Message::STATUS_UNREAD,
            Message::TYPE_CHAT,
            "",
        )?;
        info!(
            "[MSG] System message written: id={}, type={}",
            id,
            Message::TYPE_CHAT
        );
        Ok(id)
    }

    /// Read all unread agent replies and mark them as read.
    /// Returns Vec<(id, content, timestamp)> for deduplication on frontend.
    pub fn read_unread_agent_replies(&mut self) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content, timestamp FROM messages WHERE role = ? AND read_status = ?",
        )?;
        let replies: Vec<(i64, String, String)> = stmt
            .query_map(
                rusqlite::params![Message::ROLE_AGENT, Message::STATUS_UNREAD],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?
            .filter_map(|r| r.ok())
            .collect();

        if !replies.is_empty() {
            self.conn.execute(
                "UPDATE messages SET read_status = ? WHERE role = ? AND read_status = ?",
                rusqlite::params![
                    Message::STATUS_READ,
                    Message::ROLE_AGENT,
                    Message::STATUS_UNREAD
                ],
            )?;
        }

        Ok(replies)
    }

    /// Get agent replies with id > after_id (for polling without read_status).
    /// Returns Vec<(id, role, content, timestamp)>.
    pub fn get_agent_replies_after(
        &self,
        after_id: i64,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content, timestamp FROM messages WHERE role = ? AND id > ? ORDER BY id"
        )?;
        let replies: Vec<(i64, String, String, String)> = stmt
            .query_map(rusqlite::params![Message::ROLE_AGENT, after_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(replies)
    }

    /// Get messages (any role) after a given id, with limit for pagination
    pub fn get_messages_after(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, content, timestamp FROM messages WHERE id > ? ORDER BY id LIMIT ?",
        )?;
        let rows: Vec<(i64, String, String, String)> = stmt
            .query_map(rusqlite::params![after_id, limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    // ==================== Time Range Query ====================

    /// Read all messages within a timestamp range (inclusive).
    /// Timestamps are in "yyyyMMddHHmmss" format.
    pub fn read_messages_in_range(&self, start: &str, end: &str) -> Result<Vec<RangeMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT sender, role, content, timestamp FROM messages WHERE timestamp >= ? AND timestamp <= ? ORDER BY id LIMIT 50"
        )?;
        let messages: Vec<RangeMessage> = stmt
            .query_map(rusqlite::params![start, end], |row| {
                Ok(RangeMessage {
                    sender: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    timestamp: row.get(3)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(messages)
    }

    // ==================== Engine Status ====================

    // ==================== Private helpers ====================

    fn insert_message(
        &self,
        sender: &str,
        role: &str,
        content: &str,
        timestamp: &str,
        read_status: &str,
        msg_type: &str,
        recipient: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO messages (sender, role, content, timestamp, read_status, msg_type, recipient) VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![sender, role, content, timestamp, read_status, msg_type, recipient],
        ).context("failed to insert message")?;
        Ok(self.conn.last_insert_rowid())
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

    use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
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

    fn setup() -> ChatHistory {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("chat_test_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).ok();
        let db_path = dir.join("test_chat.db");
        ChatHistory::open(&db_path).unwrap()
    }

    #[test]
    fn test_append_and_query() {
        let mut ch = setup();
        ch.append("user1", "user", "hello", "20260220120000")
            .unwrap();
        ch.append("agent", "agent", "hi there", "20260220120001")
            .unwrap();

        let result = ch.query(10, None).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "user");
        assert_eq!(result.messages[1].role, "agent");
    }

    #[test]
    fn test_query_pagination() {
        let mut ch = setup();
        for i in 0..10 {
            ch.append("user", "user", &format!("msg{}", i), "20260220120000")
                .unwrap();
        }

        // Get last 3
        let result = ch.query(3, None).unwrap();
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.total, 10);
        assert!(result.has_more);

        // Get 3 before the start_id
        let result2 = ch.query(3, Some(result.start_id)).unwrap();
        assert_eq!(result2.messages.len(), 3);
    }

    #[test]
    fn test_user_message_inbox() {
        let mut ch = setup();

        ch.write_user_message("24007", "hello agent", "20260220120000")
            .unwrap();
        ch.write_user_message("24007", "are you there?", "20260220120001")
            .unwrap();

        assert_eq!(ch.count_unread_user_messages().unwrap(), 2);

        let msgs = ch.read_unread_user_messages().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello agent");
        assert_eq!(msgs[1].content, "are you there?");

        assert_eq!(ch.count_unread_user_messages().unwrap(), 0);

        let msgs2 = ch.read_unread_user_messages().unwrap();
        assert!(msgs2.is_empty());
    }

    #[test]
    fn test_agent_reply_outbox() {
        let mut ch = setup();

        ch.write_agent_reply("alice", "hello user!", "20260220120000", "")
            .unwrap();

        let replies = ch.read_unread_agent_replies().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1, "hello user!");

        let replies2 = ch.read_unread_agent_replies().unwrap();
        assert!(replies2.is_empty());
    }

    #[test]
    fn test_get_last_message_time() {
        let mut ch = setup();

        assert_eq!(ch.get_last_message_time().unwrap(), 0);

        ch.append("user", "user", "test", "20260220120000").unwrap();
        let time = ch.get_last_message_time().unwrap();
        assert!(time > 0);
    }

    #[test]
    fn test_get_messages_after() {
        let mut ch = setup();

        ch.append("user", "user", "msg1", "20260220120000").unwrap();
        ch.append("agent", "agent", "msg2", "20260220120001")
            .unwrap();
        ch.append("user", "user", "msg3", "20260220120002").unwrap();

        let after = ch.get_messages_after(1, 100).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].2, "msg2");
        assert_eq!(after[1].2, "msg3");
    }
}
