#[cfg(test)]
mod tests {
    use crate::*;

    // ─── TextFile tests ───

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

        // Open non-existent file → empty content
        let tf = TextFile::open(&path).unwrap();
        assert_eq!(tf.read().unwrap(), "");

        // Write content
        tf.write("hello world").unwrap();
        assert_eq!(tf.read().unwrap(), "hello world");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");

        // External modification is immediately visible (no cache)
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
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "line1\nline2\nline3\n");
        assert_eq!(tf.read().unwrap(), "line1\nline2\nline3\n");
    }

    #[test]
    fn test_textfile_clear() {
        let path = temp_text_path("clear.txt");
        let tf = TextFile::open(&path).unwrap();

        tf.write("some content").unwrap();
        assert_eq!(tf.read().unwrap(), "some content");

        tf.clear().unwrap();
        assert_eq!(tf.read().unwrap(), "");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn test_textfile_persistence() {
        let path = temp_text_path("persist.txt");

        // Write
        {
            let tf = TextFile::open(&path).unwrap();
            tf.write("persisted data").unwrap();
        }

        // Reopen and verify
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

        // .tmp file should not exist after successful rename
        assert!(!tmp_path.exists());
        assert!(path.exists());
    }

    #[test]
    fn test_textfile_open_existing() {
        let path = temp_text_path("existing.txt");

        // Create file first
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "pre-existing content").unwrap();

        // Open should read existing content
        let tf = TextFile::open(&path).unwrap();
        assert_eq!(tf.read().unwrap(), "pre-existing content");
    }
}
