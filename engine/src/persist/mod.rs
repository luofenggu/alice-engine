//! # Persistence Layer
//!
//! All persist structs live here. Guardian exempts this directory.
//!
//! ## Primitives
//!
//! - `Document<T>` — single struct persisted to a JSON file
//! - `TextFile` — plain text file with atomic writes
//!
//! ## Config
//!

pub mod chat;
pub mod hooks;
pub mod instance;
pub mod memory;
pub mod settings;

/// Global settings filename — private, not exposed outside persist.
const GLOBAL_SETTINGS_FILE: &str = "global_settings.json";

use anyhow::{Context, Result};

// ─── Document ────────────────────────────────────────────────────

/// A single struct persisted to a JSON file.
///
/// Stateless — every read loads from disk, every write goes to disk.
pub struct Document<T: serde::Serialize + serde::de::DeserializeOwned> {
    path: std::path::PathBuf,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> Clone for Document<T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T> Document<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    /// Create a handle to a JSON document at the given path.
    /// If the file does not exist, creates it with `T::default()`.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.exists() {
            let data = T::default();
            let content = serde_json::to_string_pretty(&data)
                .context("failed to serialize default document")?;
            std::fs::write(&path, content)
                .with_context(|| format!("failed to write document: {}", path.display()))?;
        }
        Ok(Self {
            path,
            _phantom: std::marker::PhantomData,
        })
    }

    /// Load the document from disk (read + deserialize).
    pub fn load(&self) -> Result<T> {
        let content = std::fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read document: {}", self.path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse document: {}", self.path.display()))
    }

    /// Save data to disk (serialize + write).
    pub fn save(&self, data: &T) -> Result<()> {
        let content = serde_json::to_string_pretty(data).context("failed to serialize document")?;
        std::fs::write(&self.path, content)
            .with_context(|| format!("failed to write document: {}", self.path.display()))?;
        Ok(())
    }

    /// Load, modify, and save in one step.
    pub fn update(&self, f: impl FnOnce(&mut T)) -> Result<()> {
        let mut data = self.load()?;
        f(&mut data);
        self.save(&data)
    }

    /// Get the file path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

// ─── TextFile: plain text file storage ───

/// Stateless text file handle — reads and writes directly to disk, no caching.
#[derive(Clone)]
pub struct TextFile {
    path: std::path::PathBuf,
}

impl TextFile {
    /// Create a handle to a text file at the given path.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self> {
        Ok(Self { path: path.into() })
    }

    /// Read the entire file content from disk.
    /// Returns empty string if the file does not exist.
    pub fn read(&self) -> Result<String> {
        if self.path.exists() {
            std::fs::read_to_string(&self.path)
                .with_context(|| format!("failed to read text file: {}", self.path.display()))
        } else {
            Ok(String::new())
        }
    }

    /// Write the entire content to disk (atomic: write tmp + rename).
    pub fn write(&self, content: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory: {}", parent.display()))?;
            }
        }
        // Use random suffix to avoid tmp file collision from concurrent writes
        let random_suffix: u32 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let tmp_path = self.path.with_extension(format!("tmp.{}", random_suffix));
        std::fs::write(&tmp_path, content)
            .with_context(|| format!("failed to write tmp file: {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.path)
            .with_context(|| format!("failed to rename tmp to target: {}", self.path.display()))?;
        Ok(())
    }

    /// Append text to the file. Creates the file if it doesn't exist.
    pub fn append(&self, text: &str) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory: {}", parent.display()))?;
            }
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open file for append: {}", self.path.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("failed to append to file: {}", self.path.display()))?;
        Ok(())
    }

    /// Clear the file (write empty content).
    pub fn clear(&self) -> Result<()> {
        self.write("")
    }

    /// Get the file path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_text_path(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir()
            .join(format!("persist_textfile_{}_{}", std::process::id(), n))
            .join(name)
    }

    #[test]
    fn test_textfile_basic() {
        let path = temp_text_path("basic.txt");
        let tf = TextFile::open(&path).unwrap();
        assert_eq!(tf.read().unwrap(), "");
        tf.write("hello world").unwrap();
        assert_eq!(tf.read().unwrap(), "hello world");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
        std::fs::write(&path, "modified externally").unwrap();
        assert_eq!(tf.read().unwrap(), "modified externally");
    }

    #[test]
    fn test_textfile_append() {
        let path = temp_text_path("append.txt");
        let tf = TextFile::open(&path).unwrap();
        tf.write("line1\n").unwrap();
        tf.append("line2\n").unwrap();
        tf.append("line3\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nline2\nline3\n"
        );
    }

    #[test]
    fn test_textfile_clear() {
        let path = temp_text_path("clear.txt");
        let tf = TextFile::open(&path).unwrap();
        tf.write("some content").unwrap();
        tf.clear().unwrap();
        assert_eq!(tf.read().unwrap(), "");
    }

    #[test]
    fn test_textfile_persistence() {
        let path = temp_text_path("persist.txt");
        {
            let tf = TextFile::open(&path).unwrap();
            tf.write("persisted data").unwrap();
        }
        {
            let tf = TextFile::open(&path).unwrap();
            assert_eq!(tf.read().unwrap(), "persisted data");
        }
    }

    #[test]
    fn test_textfile_no_tmp_after_write() {
        let path = temp_text_path("atomic.txt");
        let tmp_path = path.with_extension("tmp");
        let tf = TextFile::open(&path).unwrap();
        tf.write("atomic write test").unwrap();
        assert!(!tmp_path.exists());
        assert!(path.exists());
    }

    #[test]
    fn test_textfile_open_existing() {
        let path = temp_text_path("existing.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "pre-existing content").unwrap();
        let tf = TextFile::open(&path).unwrap();
        assert_eq!(tf.read().unwrap(), "pre-existing content");
    }
}
pub use memory::SessionBlockEntry;
pub use settings::{GlobalSettingsStore, Settings};
