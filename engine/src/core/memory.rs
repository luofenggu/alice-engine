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
//! - commit_summary() — summary transaction: write session → knowledge → clear current
//! - commit_history() — roll transaction: write history → delete old session block

use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use alice_persist::TextFile;

/// Session block file extension
const SESSION_EXT: &str = "jsonl";

/// Memory subsystem for an Alice instance.
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
}

impl Memory {
    /// Open memory from the given memory directory.
    ///
    /// Initializes TextFile handles for knowledge, history, and current.
    /// Creates directories if they don't exist.
    /// 
    /// Note: One-time migrations (e.g. keypoints.md → knowledge.md) should be
    /// performed BEFORE calling this, so TextFile::open reads the migrated content.
    pub fn open(memory_dir: impl Into<PathBuf>) -> Result<Self> {
        let memory_dir = memory_dir.into();
        let sessions_dir = memory_dir.join("sessions");

        // Ensure directories exist
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&sessions_dir)
            .with_context(|| format!("Failed to create sessions dir: {}", sessions_dir.display()))?;

        let knowledge = TextFile::open(memory_dir.join("knowledge.md"))?;
        let history = TextFile::open(sessions_dir.join("history.txt"))?;
        let current = TextFile::open(sessions_dir.join("current.txt"))?;

        Ok(Self {
            memory_dir,
            sessions_dir,
            knowledge,
            history,
            current,
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

    /// Path to the .last_rolled idempotency marker file.
    pub fn last_rolled_path(&self) -> PathBuf {
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
            .with_context(|| format!("Failed to open session block for append: {}", path.display()))?;
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

    /// Normal beat: append to current and flush.
    /// This is a convenience alias for append_current().
    pub fn commit_beat(&self, text: &str) -> Result<()> {
        self.append_current(text)
    }

    /// Summary transaction:
    /// 1. Append session block (new session data)
    /// 2. Write knowledge (updated cognitive framework)
    /// 3. Clear current
    ///
    /// Crash safety: write targets first, clear source last.
    /// Worst case on crash: duplicate session entry + stale current (no data loss).
    pub fn commit_summary(
        &self,
        session_block_name: &str,
        session_lines: &str,
        knowledge_text: &str,
    ) -> Result<()> {
        // Step 1: Write session block
        self.append_session_block(session_block_name, session_lines)?;

        // Step 2: Write knowledge
        self.knowledge.write(knowledge_text)?;

        // Step 3: Clear current
        self.current.clear()?;

        Ok(())
    }

    /// Roll history transaction:
    /// 1. Write compressed history
    /// 2. Delete oldest session block
    ///
    /// Crash safety: write target first, delete source last.
    /// Worst case on crash: history written but old session not deleted (re-roll is safe).
    pub fn commit_history(
        &self,
        new_history: &str,
        oldest_block_name: &str,
    ) -> Result<()> {
        // Step 1: Write history (atomic: tmp + rename)
        self.history.write(new_history)?;

        // Step 2: Delete oldest session block
        self.delete_session_block(oldest_block_name)?;

        // Step 3: Update .last_rolled marker
        let last_rolled_path = self.sessions_dir.join(".last_rolled");
        std::fs::write(&last_rolled_path, oldest_block_name)
            .with_context(|| "Failed to write .last_rolled marker")?;

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
        let memory = Memory::open(&memory_dir).unwrap();
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

        let memory = Memory::open(&memory_dir).unwrap();
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
        memory.write_session_block("20260301120000", "line1\nline2\n").unwrap();
        assert_eq!(memory.list_session_blocks().unwrap(), vec!["20260301120000"]);

        // Read it back
        let content = memory.read_session_block("20260301120000").unwrap();
        assert_eq!(content, "line1\nline2\n");

        // Append to it
        memory.append_session_block("20260301120000", "line3\n").unwrap();
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
        assert_eq!(blocks, vec!["20260301120000", "20260301150000", "20260301180000"]);
    }

    #[test]
    fn test_commit_summary() {
        let (_tmp, mut memory) = setup();

        // Setup: some current content
        memory.append_current("thinking about stuff\n").unwrap();

        // Commit summary
        memory.commit_summary(
            "20260301120000",
            "{\"summary\": \"session data\"}\n",
            "# Updated Knowledge\nnew insights",
        ).unwrap();

        // Verify: session block written
        let session = memory.read_session_block("20260301120000").unwrap();
        assert_eq!(session, "{\"summary\": \"session data\"}\n");

        // Verify: knowledge updated
        assert_eq!(memory.knowledge.read().unwrap(), "# Updated Knowledge\nnew insights");

        // Verify: current cleared
        assert_eq!(memory.current.read().unwrap(), "");

        assert_eq!(memory.knowledge.read().unwrap(), "# Updated Knowledge\nnew insights");
        assert_eq!(memory.current.read().unwrap(), "");
    }

    #[test]
    fn test_commit_history() {
        let (_tmp, mut memory) = setup();

        // Setup: write a session block and initial history
        memory.write_session_block("20260301120000", "old session data\n").unwrap();
        memory.history.write("old history").unwrap();

        // Commit history (roll)
        memory.commit_history(
            "compressed new history",
            "20260301120000",
        ).unwrap();

        // Verify: history updated
        assert_eq!(memory.history.read().unwrap(), "compressed new history");

        // Verify: old session block deleted
        assert!(memory.read_session_block("20260301120000").unwrap().is_empty());

        // Verify: .last_rolled marker
        let marker = std::fs::read_to_string(memory.sessions_dir.join(".last_rolled")).unwrap();
        assert_eq!(marker, "20260301120000");

        // Verify persistence
        assert_eq!(memory.history.read().unwrap(), "compressed new history");
    }

    #[test]
    fn test_commit_summary_appends_to_existing_session() {
        let (_tmp, mut memory) = setup();

        // First summary
        memory.commit_summary("20260301120000", "line1\n", "knowledge v1").unwrap();

        // Second summary appends to same block
        memory.append_current("more thinking\n").unwrap();
        memory.commit_summary("20260301120000", "line2\n", "knowledge v2").unwrap();

        // Session block has both lines
        let session = memory.read_session_block("20260301120000").unwrap();
        assert_eq!(session, "line1\nline2\n");

        // Knowledge is latest version
        assert_eq!(memory.knowledge.read().unwrap(), "knowledge v2");
    }

    #[test]
    fn test_memory_dir_accessor() {
        let (tmp, memory) = setup();
        assert_eq!(memory.memory_dir(), tmp.path().join("memory"));
        assert_eq!(memory.sessions_dir(), tmp.path().join("memory/sessions"));
    }
}

