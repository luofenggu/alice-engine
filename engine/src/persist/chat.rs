//! # Chat Module
//!
//! Message storage and channel backed by SQLite.
//! All queries go directly to the database via Diesel — no in-memory caching.
//!
//! @TRACE: MSG — All message I/O operations.

use anyhow::{Context, Result};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use tracing::info;

use crate::bindings::db::{self, messages, MessageRow, NewMessage};

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

impl From<MessageRow> for Message {
    fn from(row: MessageRow) -> Self {
        Self {
            id: row.id,
            sender: row.sender,
            role: row.role,
            content: row.content,
            timestamp: row.timestamp,
            read_status: row.read_status,
            msg_type: row.msg_type,
            recipient: row.recipient,
        }
    }
}

impl Message {
    pub const ROLE_USER: &'static str = db::ROLE_USER;
    pub const ROLE_AGENT: &'static str = db::ROLE_AGENT;
    pub const ROLE_SYSTEM: &'static str = db::ROLE_SYSTEM;
    pub const STATUS_READ: &'static str = db::STATUS_READ;
    pub const STATUS_UNREAD: &'static str = db::STATUS_UNREAD;
    pub const TYPE_CHAT: &'static str = db::TYPE_CHAT;
}

// ---------------------------------------------------------------------------
// View types — projections for specific use-cases
// ---------------------------------------------------------------------------

/// A message for display (chat history view).
#[derive(Debug)]
pub struct ChatMessage {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub sender: String,
    pub recipient: String,
}

impl From<&Message> for ChatMessage {
    fn from(m: &Message) -> Self {
        Self {
            id: m.id,
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
            sender: m.sender.clone(),
            recipient: m.recipient.clone(),
        }
    }
}

/// A message within a time range (for summary).
#[derive(Debug)]
pub struct RangeMessage {
    pub sender: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

impl From<&Message> for RangeMessage {
    fn from(m: &Message) -> Self {
        Self {
            sender: m.sender.clone(),
            role: m.role.clone(),
            content: m.content.clone(),
            timestamp: m.timestamp.clone(),
        }
    }
}

/// An unread inbox message from a user.
#[derive(Debug)]
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
        Self {
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
// ChatHistory — direct SQLite via Diesel, no caching
// ---------------------------------------------------------------------------

/// @TRACE: MSG
///
/// Chat history and message channel.
/// All queries go directly to SQLite via Diesel — no in-memory caching.
pub struct ChatHistory {
    conn: SqliteConnection,
}

impl ChatHistory {
    /// Open (or create) the chat database at the given path.
    ///
    /// @TRACE: MSG — `[MSG] ChatHistory initialized: {path}`
    pub fn open(db_path: &std::path::Path) -> Result<Self> {
        let path_str = db_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid db path: {}", db_path.display()))?;

        let mut conn = SqliteConnection::establish(path_str)
            .map_err(|e| anyhow::anyhow!("failed to open chat db {}: {}", db_path.display(), e))?;

        // Create table if not exists
        diesel::sql_query(db::CREATE_MESSAGES_TABLE)
            .execute(&mut conn)
            .context("failed to create chat tables")?;

        // Migration: add recipient column if missing (for existing DBs)
        let has_recipient = diesel::sql_query(db::CHECK_RECIPIENT_COLUMN)
            .execute(&mut conn)
            .is_ok();
        if !has_recipient {
            diesel::sql_query(db::ADD_RECIPIENT_COLUMN)
                .execute(&mut conn)
                .context("failed to add recipient column")?;
            info!("[DB] Migration: added recipient column to messages table");
        }

        let count: i64 = messages::table
            .count()
            .get_result(&mut conn)
            .unwrap_or(0);

        info!(
            "[MSG] ChatHistory initialized: {:?} ({} messages)",
            db_path, count
        );
        Ok(Self { conn })
    }

    /// Append a message to history (marked as read, for display).
    ///
    /// @TRACE: MSG
    pub fn append(
        &mut self,
        role: &str,
        sender: &str,
        content: &str,
        timestamp: &str,
        recipient: &str,
    ) -> Result<()> {
        self.insert_message(
            sender,
            role,
            content,
            timestamp,
            Message::STATUS_READ,
            Message::TYPE_CHAT,
            recipient,
        )?;
        Ok(())
    }

    /// Query messages with pagination.
    ///
    /// @TRACE: MSG
    pub fn query(&mut self, limit: i64, before: Option<i64>) -> Result<QueryResult> {
        let total: i64 = messages::table
            .count()
            .get_result(&mut self.conn)
            .context("failed to count messages")?;

        if total == 0 {
            return Ok(QueryResult {
                messages: vec![],
                total: 0,
                start_id: 0,
                has_more: false,
            });
        }

        let rows: Vec<MessageRow> = match before {
            None => {
                messages::table
                    .order(messages::id.desc())
                    .limit(limit)
                    .load(&mut self.conn)
                    .context("failed to query messages")?
            }
            Some(before_id) => {
                messages::table
                    .filter(messages::id.lt(before_id))
                    .order(messages::id.desc())
                    .limit(limit)
                    .load(&mut self.conn)
                    .context("failed to query messages with before")?
            }
        };

        let msgs: Vec<Message> = rows.into_iter().map(Message::from).collect();
        let start_id = msgs.last().map(|m| m.id).unwrap_or(0);
        let has_more = if start_id > 0 {
            let older_count: i64 = messages::table
                .filter(messages::id.lt(start_id))
                .count()
                .get_result(&mut self.conn)
                .unwrap_or(0);
            older_count > 0
        } else {
            false
        };

        let chat_messages: Vec<ChatMessage> = msgs.iter().rev().map(ChatMessage::from).collect();

        Ok(QueryResult {
            messages: chat_messages,
            total,
            start_id,
            has_more,
        })
    }

    /// Get the timestamp of the last message (as epoch millis, 0 if empty).
    pub fn get_last_message_time(&mut self) -> Result<i64> {
        let result: Option<String> = messages::table
            .select(messages::timestamp)
            .order(messages::id.desc())
            .first::<String>(&mut self.conn)
            .optional()
            .context("failed to get last message time")?;

        match result {
            Some(ts) => Ok(parse_timestamp_to_millis(&ts).unwrap_or(0)),
            None => Ok(0),
        }
    }

    /// Generate a timestamp string in Alice's standard format.
    pub fn now_timestamp() -> String {
        chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
    }

    /// Unified message write function.
    /// All message types (user/agent/system) go through this single entry point.
    pub fn write_message(
        &mut self,
        role: &str,
        sender: &str,
        content: &str,
        timestamp: &str,
        recipient: &str,
    ) -> Result<i64> {
        let read_status = db::STATUS_UNREAD;
        self.insert_message(
            sender,
            role,
            content,
            timestamp,
            read_status,
            Message::TYPE_CHAT,
            recipient,
        )
    }

    pub fn write_user_message(
        &mut self,
        content: &str,
        timestamp: &str,
    ) -> Result<i64> {
        self.write_message(Message::ROLE_USER, "user", content, timestamp, "")
    }

    /// Read all unread inbox messages and mark them as read.
    /// Reads all unread messages except agent messages sent by self.
    ///
    /// @TRACE: MSG
    pub fn read_unread_user_messages(&mut self, self_instance_id: &str) -> Result<Vec<InboxMessage>> {
        // Filter: unread AND NOT (role='agent' AND sender=self)
        let rows: Vec<MessageRow> = messages::table
            .filter(
                messages::read_status.eq(Message::STATUS_UNREAD)
                    .and(diesel::dsl::not(
                        messages::role.eq(Message::ROLE_AGENT)
                            .and(messages::sender.eq(self_instance_id))
                    ))
            )
            .load(&mut self.conn)
            .context("failed to read unread messages")?;

        let msgs: Vec<Message> = rows.into_iter().map(Message::from).collect();
        let inbox: Vec<InboxMessage> = msgs.iter().map(InboxMessage::from).collect();

        // Mark as read
        if !inbox.is_empty() {
            diesel::update(
                messages::table.filter(
                    messages::read_status.eq(Message::STATUS_UNREAD)
                        .and(diesel::dsl::not(
                            messages::role.eq(Message::ROLE_AGENT)
                                .and(messages::sender.eq(self_instance_id))
                        ))
                )
            )
            .set(messages::read_status.eq(Message::STATUS_READ))
            .execute(&mut self.conn)
            .context("failed to mark messages as read")?;
        }

        Ok(inbox)
    }

    /// Count unread inbox messages (excluding agent messages sent by self).
    pub fn count_unread_user_messages(&mut self, self_instance_id: &str) -> Result<i64> {
        let count: i64 = messages::table
            .filter(
                messages::read_status.eq(Message::STATUS_UNREAD)
                    .and(diesel::dsl::not(
                        messages::role.eq(Message::ROLE_AGENT)
                            .and(messages::sender.eq(self_instance_id))
                    ))
            )
            .count()
            .get_result(&mut self.conn)
            .context("failed to count unread messages")?;
        Ok(count)
    }

    /// Write an agent reply (unread, for Web to pick up and display).
    ///
    /// @TRACE: MSG
    pub fn write_agent_reply(
        &mut self,
        instance_id: &str,
        content: &str,
        timestamp: &str,
        recipient: &str,
    ) -> Result<i64> {
        self.insert_message(
            instance_id,
            Message::ROLE_AGENT,
            content,
            timestamp,
            Message::STATUS_UNREAD,
            Message::TYPE_CHAT,
            recipient,
        )
    }

    /// Write a system message (role=system, read_status=unread).
    /// System messages are delivered to the agent's inbox alongside user messages.
    pub fn write_system_message(
        &mut self,
        content: &str,
        timestamp: &str,
    ) -> Result<i64> {
        self.write_message(Message::ROLE_SYSTEM, "system", content, timestamp, "")
    }

    /// Read all unread agent replies and mark them as read.
    /// Returns Vec<(id, content, timestamp)> for deduplication on frontend.
    pub fn read_unread_agent_replies(&mut self) -> Result<Vec<(i64, String, String)>> {
        let rows: Vec<MessageRow> = messages::table
            .filter(
                messages::role.eq(Message::ROLE_AGENT)
                    .and(messages::read_status.eq(Message::STATUS_UNREAD))
            )
            .load(&mut self.conn)
            .context("failed to read unread agent replies")?;

        let result: Vec<(i64, String, String)> = rows
            .iter()
            .map(|r| (r.id, r.content.clone(), r.timestamp.clone()))
            .collect();

        if !result.is_empty() {
            diesel::update(
                messages::table.filter(
                    messages::role.eq(Message::ROLE_AGENT)
                        .and(messages::read_status.eq(Message::STATUS_UNREAD))
                )
            )
            .set(messages::read_status.eq(Message::STATUS_READ))
            .execute(&mut self.conn)
            .context("failed to mark agent replies as read")?;
        }

        Ok(result)
    }

    /// Get agent replies with id > after_id (for polling without read_status).
    /// Returns Vec<(id, sender, role, content, timestamp, recipient)>.
    pub fn get_agent_replies_after(
        &mut self,
        after_id: i64,
    ) -> Result<Vec<(i64, String, String, String, String, String)>> {
        let rows: Vec<MessageRow> = messages::table
            .filter(
                messages::role.eq(Message::ROLE_AGENT)
                    .and(messages::id.gt(after_id))
            )
            .order(messages::id.asc())
            .load(&mut self.conn)
            .context("failed to get agent replies after")?;

        Ok(rows
            .into_iter()
            .map(|r| (r.id, r.sender, r.role, r.content, r.timestamp, r.recipient))
            .collect())
    }

    /// Get messages (any role) after a given id, with limit for pagination
    pub fn get_messages_after(
        &mut self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, String, String, String, String, String)>> {
        let rows: Vec<MessageRow> = messages::table
            .filter(messages::id.gt(after_id))
            .order(messages::id.asc())
            .limit(limit)
            .load(&mut self.conn)
            .context("failed to get messages after")?;

        Ok(rows
            .into_iter()
            .map(|r| (r.id, r.sender, r.role, r.content, r.timestamp, r.recipient))
            .collect())
    }

    /// Read all messages within a timestamp range (inclusive).
    /// Timestamps are in "yyyyMMddHHmmss" format.
    pub fn read_messages_in_range(&mut self, start: &str, end: &str) -> Result<Vec<RangeMessage>> {
        let rows: Vec<MessageRow> = messages::table
            .filter(
                messages::timestamp.ge(start)
                    .and(messages::timestamp.le(end))
            )
            .order(messages::id.asc())
            .limit(50)
            .load(&mut self.conn)
            .context("failed to read messages in range")?;

        let msgs: Vec<Message> = rows.into_iter().map(Message::from).collect();
        Ok(msgs.iter().map(RangeMessage::from).collect())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn insert_message(
        &mut self,
        sender: &str,
        role: &str,
        content: &str,
        timestamp: &str,
        read_status: &str,
        msg_type: &str,
        recipient: &str,
    ) -> Result<i64> {
        let new_msg = NewMessage {
            sender,
            role,
            content,
            timestamp,
            read_status,
            msg_type,
            recipient,
        };

        diesel::insert_into(messages::table)
            .values(&new_msg)
            .execute(&mut self.conn)
            .context("failed to insert message")?;

        // Get the auto-generated id
        #[derive(QueryableByName)]
        struct LastId {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            #[diesel(column_name = "last_insert_rowid()")]
            id: i64,
        }

        let last_id = diesel::sql_query(db::SELECT_LAST_INSERT_ROWID)
            .get_result::<LastId>(&mut self.conn)
            .map(|r| r.id)
            .unwrap_or(0);

        Ok(last_id)
    }
}

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

    use chrono::{Local, TimeZone};
    let dt = Local
        .with_ymd_and_hms(year, month, day, hour, min, sec)
        .single()?;
    Some(dt.timestamp_millis())
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
        let path = std::env::temp_dir().join(format!("chat_test_diesel_{}.db", n));
        // Clean up any previous test file
        let _ = std::fs::remove_file(&path);
        ChatHistory::open(&path).unwrap()
    }

    #[test]
    fn test_append_and_query() {
        let mut ch = setup();
        ch.append("user", "user", "hello", "20250101120000", "").unwrap();
        ch.append("agent", "bot1", "hi there", "20250101120001", "").unwrap();
        let result = ch.query(10, None).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].content, "hello");
        assert_eq!(result.messages[1].content, "hi there");
    }

    #[test]
    fn test_query_pagination() {
        let mut ch = setup();
        for i in 0..5 {
            ch.append("user", "user", &format!("msg{}", i), "20250101120000", "").unwrap();
        }
        let page1 = ch.query(3, None).unwrap();
        assert_eq!(page1.messages.len(), 3);
        assert!(page1.has_more);
        let page2 = ch.query(3, Some(page1.start_id)).unwrap();
        assert_eq!(page2.messages.len(), 2);
        assert!(!page2.has_more);
    }

    #[test]
    fn test_user_message_inbox() {
        let mut ch = setup();
        let id = ch.write_user_message("hello from user", "20250101120000").unwrap();
        assert!(id > 0);
        let unread = ch.count_unread_user_messages("bot1").unwrap();
        assert_eq!(unread, 1);
        let msgs = ch.read_unread_user_messages("bot1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello from user");
        let unread_after = ch.count_unread_user_messages("bot1").unwrap();
        assert_eq!(unread_after, 0);
    }

    #[test]
    fn test_agent_reply_outbox() {
        let mut ch = setup();
        ch.write_agent_reply("bot1", "reply content", "20250101120000", "").unwrap();
        let replies = ch.read_unread_agent_replies().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1, "reply content");
        let replies_after = ch.read_unread_agent_replies().unwrap();
        assert_eq!(replies_after.len(), 0);
    }

    #[test]
    fn test_get_last_message_time() {
        let mut ch = setup();
        let t = ch.get_last_message_time().unwrap();
        assert_eq!(t, 0);
        ch.append("user", "user", "msg", "20250601120000", "").unwrap();
        let t2 = ch.get_last_message_time().unwrap();
        assert!(t2 > 0);
    }

    #[test]
    fn test_get_messages_after() {
        let mut ch = setup();
        ch.append("user", "user", "msg1", "20250101120000", "").unwrap();
        ch.append("agent", "bot1", "msg2", "20250101120001", "").unwrap();
        ch.append("user", "user", "msg3", "20250101120002", "").unwrap();
        let msgs = ch.get_messages_after(1, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].3, "msg2");
        assert_eq!(msgs[1].3, "msg3");
    }
}