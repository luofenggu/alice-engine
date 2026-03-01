//! # Chat Module
//!
//! Message storage and channel backed by persist layer.
//! All queries happen in memory; mutations write through to storage.
//!
//! @TRACE: MSG — All message I/O operations.

use anyhow::{Result, Context};
use alice_persist::{Collection, KvStore, Backend};
use tracing::info;

use crate::model::{Message, ChatMessage, InboxMessage, RangeMessage, QueryResult};

// ---------------------------------------------------------------------------
// ChatHistory — backed by persist layer
// ---------------------------------------------------------------------------

/// @TRACE: MSG
///
/// Chat history and message channel.
/// All data lives in memory; persist layer handles durability.
pub struct ChatHistory {
    messages: Collection<Message>,
    status: KvStore,
}

impl ChatHistory {
    /// Open (or create) the chat database at the given path.
    ///
    /// @TRACE: MSG — `[MSG] ChatHistory initialized: {path}`
    pub fn open(db_path: &std::path::Path) -> Result<Self> {
        let backend = Backend::Sqlite(db_path.to_path_buf());

        let messages = Collection::<Message>::open(&backend)
            .with_context(|| format!("failed to open messages collection: {:?}", db_path))?;

        let status = KvStore::open(&backend, "engine_status")
            .with_context(|| format!("failed to open engine_status store: {:?}", db_path))?;

        info!("[MSG] ChatHistory initialized: {:?} ({} messages loaded)", db_path, messages.count());
        Ok(Self { messages, status })
    }

    // ==================== Chat History ====================

    /// Append a message to history (marked as read, for display).
    ///
    /// @TRACE: MSG
    pub fn append(&mut self, sender: &str, role: &str, content: &str, timestamp: &str) -> Result<()> {
        let msg = Message {
            id: 0,
            sender: sender.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            timestamp: timestamp.to_string(),
            read_status: Message::STATUS_READ.to_string(),
            msg_type: Message::TYPE_CHAT.to_string(),
        };
        self.messages.insert(&msg)?;
        Ok(())
    }

    /// Query messages with pagination.
    ///
    /// @TRACE: MSG
    pub fn query(&self, limit: i64, before: i64) -> Result<QueryResult> {
        let all = self.messages.all();
        let total = all.len() as i64;

        let filtered: Vec<&Message> = if before <= 0 {
            // Last N messages
            let skip = if total > limit { (total - limit) as usize } else { 0 };
            all.iter().skip(skip).collect()
        } else {
            // Messages with id < before, last N
            let matching: Vec<&Message> = all.iter().filter(|m| m.id < before).collect();
            let skip = if matching.len() as i64 > limit { matching.len() - limit as usize } else { 0 };
            matching.into_iter().skip(skip).collect()
        };

        let messages: Vec<ChatMessage> = filtered.iter().map(|m| ChatMessage::from(*m)).collect();
        let start_id = messages.first().map(|m| m.id).unwrap_or(0);
        let has_more = start_id > 1;

        Ok(QueryResult { messages, total, start_id, has_more })
    }

    /// Get the timestamp of the last message (as epoch millis, 0 if empty).
    pub fn get_last_message_time(&self) -> Result<i64> {
        let all = self.messages.all();
        match all.last() {
            Some(msg) => Ok(parse_timestamp_to_millis(&msg.timestamp).unwrap_or(0)),
            None => Ok(0),
        }
    }

    // ==================== Message Channel (Inbox/Outbox) ====================

    /// Write a user message (unread, for engine to pick up).
    ///
    /// @TRACE: MSG
    pub fn write_user_message(
        &mut self, sender: &str, content: &str, timestamp: &str, msg_type: &str,
    ) -> Result<i64> {
        let msg = Message {
            id: 0,
            sender: sender.to_string(),
            role: Message::ROLE_USER.to_string(),
            content: content.to_string(),
            timestamp: timestamp.to_string(),
            read_status: Message::STATUS_UNREAD.to_string(),
            msg_type: msg_type.to_string(),
        };
        let id = self.messages.insert(&msg)?;
        info!("[MSG] User message written: id={}, sender={}, type={}", id, sender, msg_type);
        Ok(id)
    }

    /// Read all unread user messages (inbox) and mark them as read.
    ///
    /// @TRACE: MSG
    pub fn read_unread_user_messages(&mut self) -> Result<Vec<InboxMessage>> {
        // Collect unread user messages
        let inbox: Vec<InboxMessage> = self.messages.all().iter()
            .filter(|m| m.role == Message::ROLE_USER && m.read_status == Message::STATUS_UNREAD)
            .map(InboxMessage::from)
            .collect();

        if !inbox.is_empty() {
            // Mark as read
            self.messages.update_where(
                |m| m.role == Message::ROLE_USER && m.read_status == Message::STATUS_UNREAD,
                |m| m.read_status = Message::STATUS_READ.to_string(),
            )?;
            info!("[MSG] Read {} unread user messages", inbox.len());
        }

        Ok(inbox)
    }

    /// Count unread user messages.
    pub fn count_unread_user_messages(&self) -> Result<i64> {
        let count = self.messages.all().iter()
            .filter(|m| m.role == Message::ROLE_USER && m.read_status == Message::STATUS_UNREAD)
            .count();
        Ok(count as i64)
    }

    /// Write an agent reply (unread, for Web to pick up and display).
    ///
    /// @TRACE: MSG
    pub fn write_agent_reply(&mut self, sender: &str, content: &str, timestamp: &str) -> Result<()> {
        let msg = Message {
            id: 0,
            sender: sender.to_string(),
            role: Message::ROLE_AGENT.to_string(),
            content: content.to_string(),
            timestamp: timestamp.to_string(),
            read_status: Message::STATUS_UNREAD.to_string(),
            msg_type: Message::TYPE_CHAT.to_string(),
        };
        self.messages.insert(&msg)?;
        info!("[MSG] Agent reply written: sender={}", sender);
        Ok(())
    }

    /// Read all unread agent replies and mark them as read.
    /// Returns Vec<(id, content, timestamp)> for deduplication on frontend.
    pub fn read_unread_agent_replies(&mut self) -> Result<Vec<(i64, String, String)>> {
        let replies: Vec<(i64, String, String)> = self.messages.all().iter()
            .filter(|m| m.role == Message::ROLE_AGENT && m.read_status == Message::STATUS_UNREAD)
            .map(|m| (m.id, m.content.clone(), m.timestamp.clone()))
            .collect();

        if !replies.is_empty() {
            self.messages.update_where(
                |m| m.role == Message::ROLE_AGENT && m.read_status == Message::STATUS_UNREAD,
                |m| m.read_status = Message::STATUS_READ.to_string(),
            )?;
        }

        Ok(replies)
    }

    /// Get agent replies with id > after_id (for polling without read_status).
    /// Returns Vec<(id, content, timestamp)>.
    pub fn get_agent_replies_after(&self, after_id: i64) -> Result<Vec<(i64, String, String)>> {
        let replies: Vec<(i64, String, String)> = self.messages.all().iter()
            .filter(|m| m.role == Message::ROLE_AGENT && m.id > after_id)
            .map(|m| (m.id, m.content.clone(), m.timestamp.clone()))
            .collect();
        Ok(replies)
    }

    /// Get all messages (any role) after a given id, for multi-client sync
    pub fn get_messages_after(&self, after_id: i64) -> Result<Vec<(i64, String, String, String)>> {
        let rows: Vec<(i64, String, String, String)> = self.messages.all().iter()
            .filter(|m| m.id > after_id)
            .map(|m| (m.id, m.role.clone(), m.content.clone(), m.timestamp.clone()))
            .collect();
        Ok(rows)
    }

    // ==================== Time Range Query ====================

    /// Read all messages within a timestamp range (inclusive).
    /// Timestamps are in "yyyyMMddHHmmss" format.
    pub fn read_messages_in_range(&self, start: &str, end: &str) -> Result<Vec<RangeMessage>> {
        let messages: Vec<RangeMessage> = self.messages.all().iter()
            .filter(|m| m.timestamp.as_str() >= start && m.timestamp.as_str() <= end)
            .take(50)
            .map(RangeMessage::from)
            .collect();
        Ok(messages)
    }

    // ==================== Engine Status ====================

    /// Update engine status (JSON string stored under "status" key).
    ///
    /// @TRACE: MSG
    pub fn update_status(&mut self, status_json: &str) -> Result<()> {
        self.status.set("status", status_json)
    }

    /// Read engine status JSON.
    pub fn read_status(&self) -> Result<Option<String>> {
        Ok(self.status.get("status").map(|s| s.to_string()))
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
        let mut ch = setup();
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
        let mut ch = setup();

        ch.write_user_message("24007", "hello agent", "20260220120000", "chat").unwrap();
        ch.write_user_message("24007", "are you there?", "20260220120001", "chat").unwrap();

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

        ch.write_agent_reply("alice", "hello user!", "20260220120000").unwrap();

        let replies = ch.read_unread_agent_replies().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].1, "hello user!");

        let replies2 = ch.read_unread_agent_replies().unwrap();
        assert!(replies2.is_empty());
    }

    #[test]
    fn test_engine_status() {
        let mut ch = setup();

        assert!(ch.read_status().unwrap().is_none());

        ch.update_status(r#"{"inferring": true, "log": "/tmp/out.log"}"#).unwrap();
        let status = ch.read_status().unwrap().unwrap();
        assert!(status.contains("inferring"));

        ch.update_status(r#"{"inferring": false}"#).unwrap();
        let status2 = ch.read_status().unwrap().unwrap();
        assert!(status2.contains("false"));
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
        ch.append("agent", "agent", "msg2", "20260220120001").unwrap();
        ch.append("user", "user", "msg3", "20260220120002").unwrap();

        let after = ch.get_messages_after(1).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].2, "msg2");
        assert_eq!(after[1].2, "msg3");
    }
}