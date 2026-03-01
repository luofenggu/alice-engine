#[cfg(test)]
mod tests {
    use crate::*;


    // A simple test model
    #[derive(Debug, Clone, PartialEq)]
    struct Note {
        id: i64,
        title: String,
        content: String,
        priority: i64,
    }

    impl Persist for Note {
        fn collection_name() -> &'static str { "notes" }

        fn id(&self) -> i64 { self.id }

        fn schema() -> Vec<Column> {
            vec![
                Column::id("id"),
                Column::text("title"),
                Column::text("content"),
                Column::integer("priority"),
            ]
        }

        fn to_row(&self) -> Vec<Value> {
            vec![
                Value::from(self.title.clone()),
                Value::from(self.content.clone()),
                Value::from(self.priority),
            ]
        }

        fn from_row(values: &[Value]) -> Result<Self> {
            Ok(Note {
                id: values[0].as_i64().unwrap_or(0),
                title: values[1].as_str().unwrap_or("").to_string(),
                content: values[2].as_str().unwrap_or("").to_string(),
                priority: values[3].as_i64().unwrap_or(0),
            })
        }
    }

    fn temp_backend() -> Backend {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("persist_test_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).ok();
        Backend::Sqlite(dir.join("test.db"))
    }

    #[test]
    fn test_collection_insert_and_query() {
        let backend = temp_backend();
        let mut col: Collection<Note> = Collection::open(&backend).unwrap();

        assert_eq!(col.count(), 0);

        let id1 = col.insert(&Note {
            id: 0,
            title: "First".into(),
            content: "Hello".into(),
            priority: 1,
        }).unwrap();

        let id2 = col.insert(&Note {
            id: 0,
            title: "Second".into(),
            content: "World".into(),
            priority: 2,
        }).unwrap();

        assert_eq!(col.count(), 2);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);

        let all = col.all();
        assert_eq!(all[0].title, "First");
        assert_eq!(all[1].title, "Second");
        assert_eq!(all[0].id, 1);
        assert_eq!(all[1].id, 2);
    }

    #[test]
    fn test_collection_update_where() {
        let backend = temp_backend();
        let mut col: Collection<Note> = Collection::open(&backend).unwrap();

        col.insert(&Note { id: 0, title: "A".into(), content: "aaa".into(), priority: 1 }).unwrap();
        col.insert(&Note { id: 0, title: "B".into(), content: "bbb".into(), priority: 2 }).unwrap();
        col.insert(&Note { id: 0, title: "C".into(), content: "ccc".into(), priority: 1 }).unwrap();

        // Update all priority=1 notes to priority=9
        let updated = col.update_where(
            |n| n.priority == 1,
            |n| n.priority = 9,
        ).unwrap();

        assert_eq!(updated, 2);
        assert_eq!(col.all()[0].priority, 9);
        assert_eq!(col.all()[1].priority, 2);
        assert_eq!(col.all()[2].priority, 9);
    }

    #[test]
    fn test_collection_persistence() {
        let backend = temp_backend();

        // Insert data
        {
            let mut col: Collection<Note> = Collection::open(&backend).unwrap();
            col.insert(&Note { id: 0, title: "Persisted".into(), content: "data".into(), priority: 5 }).unwrap();
            assert_eq!(col.count(), 1);
        }

        // Reopen and verify data survived
        {
            let col: Collection<Note> = Collection::open(&backend).unwrap();
            assert_eq!(col.count(), 1);
            assert_eq!(col.all()[0].title, "Persisted");
            assert_eq!(col.all()[0].priority, 5);
        }
    }

    #[test]
    fn test_kvstore_basic() {
        let backend = temp_backend();
        let mut kv = KvStore::open(&backend, "settings").unwrap();

        assert!(kv.get("foo").is_none());

        kv.set("foo", "bar").unwrap();
        assert_eq!(kv.get("foo").unwrap(), "bar");

        // Overwrite
        kv.set("foo", "baz").unwrap();
        assert_eq!(kv.get("foo").unwrap(), "baz");

        // Remove
        kv.remove("foo").unwrap();
        assert!(kv.get("foo").is_none());
    }

    #[test]
    fn test_kvstore_persistence() {
        let backend = temp_backend();

        {
            let mut kv = KvStore::open(&backend, "config").unwrap();
            kv.set("key1", "value1").unwrap();
            kv.set("key2", "value2").unwrap();
        }

        {
            let kv = KvStore::open(&backend, "config").unwrap();
            assert_eq!(kv.get("key1").unwrap(), "value1");
            assert_eq!(kv.get("key2").unwrap(), "value2");
        }
    }

    #[test]
    fn test_collection_reload() {
        let backend = temp_backend();
        let mut col: Collection<Note> = Collection::open(&backend).unwrap();

        col.insert(&Note { id: 0, title: "Before".into(), content: "x".into(), priority: 1 }).unwrap();
        assert_eq!(col.count(), 1);

        col.reload().unwrap();
        assert_eq!(col.count(), 1);
        assert_eq!(col.all()[0].title, "Before");
    }

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
        let mut tf = TextFile::open(&path).unwrap();
        assert_eq!(tf.get(), "");
        assert!(!tf.is_dirty());

        // Set content
        tf.set("hello world");
        assert_eq!(tf.get(), "hello world");
        assert!(tf.is_dirty());

        // Flush
        tf.flush().unwrap();
        assert!(!tf.is_dirty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");

        // Reload
        std::fs::write(&path, "modified externally").unwrap();
        tf.reload().unwrap();
        assert_eq!(tf.get(), "modified externally");
        assert!(!tf.is_dirty());
    }

    #[test]
    fn test_textfile_append() {
        let path = temp_text_path("append.txt");
        let mut tf = TextFile::open(&path).unwrap();

        tf.set("line1\n");
        tf.append("line2\n");
        tf.append("line3\n");
        assert_eq!(tf.get(), "line1\nline2\nline3\n");

        tf.flush().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "line1\nline2\nline3\n");
    }

    #[test]
    fn test_textfile_clear() {
        let path = temp_text_path("clear.txt");
        let mut tf = TextFile::open(&path).unwrap();

        tf.set("some content");
        tf.flush().unwrap();

        tf.clear();
        assert_eq!(tf.get(), "");
        assert!(tf.is_dirty());

        tf.flush().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn test_textfile_persistence() {
        let path = temp_text_path("persist.txt");

        // Write and flush
        {
            let mut tf = TextFile::open(&path).unwrap();
            tf.set("persisted data");
            tf.flush().unwrap();
        }

        // Reopen and verify
        {
            let tf = TextFile::open(&path).unwrap();
            assert_eq!(tf.get(), "persisted data");
        }
    }

    #[test]
    fn test_textfile_no_tmp_after_flush() {
        let path = temp_text_path("atomic.txt");
        let tmp_path = path.with_extension("tmp");

        let mut tf = TextFile::open(&path).unwrap();
        tf.set("atomic write test");
        tf.flush().unwrap();

        // .tmp file should not exist after successful rename
        assert!(!tmp_path.exists());
        assert!(path.exists());
    }

    #[test]
    fn test_textfile_flush_noop_when_clean() {
        let path = temp_text_path("noop.txt");
        let mut tf = TextFile::open(&path).unwrap();

        // Not dirty, flush should be no-op (file shouldn't be created)
        tf.flush().unwrap();
        assert!(!path.exists());

        // Now set and flush
        tf.set("content");
        tf.flush().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn test_textfile_open_existing() {
        let path = temp_text_path("existing.txt");

        // Create file first
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "pre-existing content").unwrap();

        // Open should load existing content
        let tf = TextFile::open(&path).unwrap();
        assert_eq!(tf.get(), "pre-existing content");
        assert!(!tf.is_dirty());
    }
}
