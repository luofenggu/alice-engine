//! # Streaming Infrastructure
//!
//! Provides the consumer-side handle for streaming inference results.
//! The producer (inference thread) sends items through a channel;
//! the consumer (React loop) calls `next()` to receive them.
//!
//! @TRACE: STREAM
//!
//! ## Design
//!
//! Java used `StreamHandler<T>` abstract class with `BlockingQueue`.
//! Rust uses `mpsc::channel` which is naturally a blocking queue.
//! The `InferenceStream` struct wraps the receiver end.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crate::action::Action;
use super::UsageInfo;

// ---------------------------------------------------------------------------
// Stream Items
// ---------------------------------------------------------------------------

/// Items flowing through the inference stream channel.
///
/// @TRACE: STREAM
#[derive(Debug)]
pub enum StreamItem {
    /// A parsed action ready for execution
    Action(Action),
    /// Inference completed successfully, contains full output text and optional usage info
    Done(String, Option<UsageInfo>),
    /// Inference encountered an error
    Error(String),
}

// ---------------------------------------------------------------------------
// Receive Result (for timed receive)
// ---------------------------------------------------------------------------

/// Result of a timed receive attempt.
pub enum RecvResult {
    /// Received an item
    Item(StreamItem),
    /// Timed out waiting, channel still open
    Timeout,
    /// Channel disconnected (sender dropped)
    Disconnected,
}

// ---------------------------------------------------------------------------
// Inference Stream (consumer handle)
// ---------------------------------------------------------------------------

/// Consumer handle for streaming inference results.
///
/// Created by `LlmClient::infer_async()`. The React loop calls `next()`
/// repeatedly to receive actions as they are parsed from the SSE stream.
///
/// @TRACE: STREAM — `[STREAM-{id}] Received action/done/error`
pub struct InferenceStream {
    receiver: mpsc::Receiver<StreamItem>,
    /// Path to the inference output log file
    pub log_path: PathBuf,
}

impl InferenceStream {
    pub fn new(receiver: mpsc::Receiver<StreamItem>, log_path: PathBuf) -> Self {
        Self { receiver, log_path }
    }

    /// Receive the next stream item, blocking until available.
    ///
    /// Returns `None` if the channel is closed (inference thread exited).
    pub fn next(&self) -> Option<StreamItem> {
        self.receiver.recv().ok()
    }

    /// Receive with timeout, distinguishing timeout from disconnect.
    pub fn next_or_timeout(&self, timeout: Duration) -> RecvResult {
        match self.receiver.recv_timeout(timeout) {
            Ok(item) => RecvResult::Item(item),
            Err(mpsc::RecvTimeoutError::Timeout) => RecvResult::Timeout,
            Err(mpsc::RecvTimeoutError::Disconnected) => RecvResult::Disconnected,
        }
    }

    /// Create a mock stream for testing (sends predefined items).
    /// Not behind #[cfg(test)] because Alice.beat() uses it for scripted testing.
    pub fn mock(items: Vec<StreamItem>) -> Self {
        let (tx, rx) = mpsc::channel();
        for item in items {
            tx.send(item).unwrap();
        }
        drop(tx); // Close sender so receiver knows when done
        Self {
            receiver: rx,
            log_path: PathBuf::from("/tmp/mock_inference.log"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_item_action() {
        let item = StreamItem::Action(Action::Idle { timeout_secs: None });
        match item {
            StreamItem::Action(Action::Idle { timeout_secs: None }) => {}
            _ => panic!("Expected Action(Idle)"),
        }
    }

    #[test]
    fn test_stream_item_done() {
        let item = StreamItem::Done("full output".to_string(), None);
        match item {
            StreamItem::Done(text, _) => assert_eq!(text, "full output"),
            _ => panic!("Expected Done"),
        }
    }

    #[test]
    fn test_stream_item_error() {
        let item = StreamItem::Error("timeout".to_string());
        match item {
            StreamItem::Error(msg) => assert_eq!(msg, "timeout"),
            _ => panic!("Expected Error"),
        }
    }

    #[test]
    fn test_mock_stream() {
        let stream = InferenceStream::mock(vec![
            StreamItem::Action(Action::Idle { timeout_secs: None }),
            StreamItem::Action(Action::Thinking { content: "hmm".to_string() }),
            StreamItem::Done("all done".to_string(), None),
        ]);

        // First item
        match stream.next().unwrap() {
            StreamItem::Action(Action::Idle { timeout_secs: None }) => {}
            _ => panic!("Expected Idle"),
        }

        // Second item
        match stream.next().unwrap() {
            StreamItem::Action(Action::Thinking { content }) => assert_eq!(content, "hmm"),
            _ => panic!("Expected Thinking"),
        }

        // Third item
        match stream.next().unwrap() {
            StreamItem::Done(text, _) => assert_eq!(text, "all done"),
            _ => panic!("Expected Done"),
        }

        // Channel closed
        assert!(stream.next().is_none());
    }

    #[test]
    fn test_mock_stream_disconnected() {
        let stream = InferenceStream::mock(vec![]);
        // Channel already closed, should return Disconnected
        match stream.next_or_timeout(Duration::from_millis(10)) {
            RecvResult::Disconnected => {}
            _ => panic!("Expected Disconnected"),
        }
    }
}
