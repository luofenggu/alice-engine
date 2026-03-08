//! Instance struct — manages all persistent state for a single agent instance.
//!
//! Three generations:
//!   InstanceStore (grandparent) — manages lifecycle of all instances
//!   Instance (parent) — manages all persistence for one instance
//!   Memory (child) — manages memory files (knowledge, history, current, sessions)

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::info;

use super::chat::ChatHistory;
use super::memory::Memory;
use super::Settings;
use crate::persist::Document;
use crate::persist::TextFile;

const SETTINGS_FILE: &str = "settings.json";
const SKILL_FILE: &str = "skill.md";
const KNOWLEDGE_FILE: &str = "knowledge.md";

/// Preset colors for new instances.
const PRESET_COLORS: &[&str] = &[
    "#6c5ce7", "#00b894", "#e17055", "#0984e3", "#fdcb6e", "#e84393", "#00cec9", "#a29bfe",
    "#ff7675", "#55efc4",
];

/// All persistent state for a single agent instance.
pub struct Instance {
    /// Unique instance identifier (6-char hex, also the directory name).
    pub id: String,
    /// Instance root directory (e.g. /root/agents/instances/860021).
    pub instance_dir: PathBuf,
    /// Settings document (JSON file persistence via Document<T>).
    pub settings: Document<Settings>,
    /// Memory subsystem (knowledge, history, current, sessions).
    pub memory: Memory,
    /// Chat history (SQLite connection, shared via Arc for reuse).
    pub chat: Arc<Mutex<ChatHistory>>,
    /// Workspace root path (instance_dir/workspace).
    pub workspace: PathBuf,
    /// Skill file (fixed knowledge, not managed by memory/capture).
    pub skill: TextFile,
}

impl Instance {
    /// Create a new instance atomically — all directories and files are created together.
    ///
    /// This solves the "partial creation" bug where only settings.json existed
    /// but data/, memory/, workspace/ were missing until engine hot-scan.
    pub fn create(
        instances_dir: &Path,
        display_name: Option<&str>,
        knowledge: Option<&str>,
        initial_settings: Option<&Settings>,
    ) -> Result<Self> {
        // Generate 6-char random hex ID
        let id: String = (0..6)
            .map(|_| format!("{:x}", rand::random::<u8>() % 16))
            .collect();
        let instance_dir = instances_dir.join(&id);

        if instance_dir.exists() {
            anyhow::bail!("Instance name collision, please retry");
        }

        // Create all directories atomically
        let memory_dir = instance_dir.join("memory");
        let knowledge_dir = memory_dir.join("knowledge");
        let workspace = instance_dir.join("workspace");
        let data_dir = instance_dir.join("data");

        std::fs::create_dir_all(&instance_dir).with_context(|| {
            format!("Failed to create instance dir: {}", instance_dir.display())
        })?;
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&knowledge_dir).with_context(|| {
            format!(
                "Failed to create knowledge dir: {}",
                knowledge_dir.display()
            )
        })?;
        std::fs::create_dir_all(&workspace)
            .with_context(|| format!("Failed to create workspace dir: {}", workspace.display()))?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;

        // Random color from presets
        let color = PRESET_COLORS[rand::random::<usize>() % PRESET_COLORS.len()];

        // Write settings.json
        let mut settings_obj = Settings {
            color: Some(color.to_string()),
            name: display_name.map(|n| n.to_string()),
            ..Default::default()
        };
        if let Some(update) = initial_settings {
            let mut merged = update.clone();
            merged.merge_fallback(&settings_obj);
            settings_obj = merged;
        }
        let settings_path = instance_dir.join(SETTINGS_FILE);
        let settings_json =
            serde_json::to_string_pretty(&settings_obj).context("Failed to serialize settings")?;
        std::fs::write(&settings_path, &settings_json)
            .with_context(|| format!("Failed to write settings: {}", settings_path.display()))?;

        // Write initial knowledge if provided
        if let Some(k) = knowledge {
            if !k.is_empty() {
                let knowledge_file = memory_dir.join(KNOWLEDGE_FILE);
                crate::util::atomic_write(&knowledge_file, k)?;
            }
        }

        // Open all persistent handles
        let settings =
            Document::open(&settings_path).context("Failed to open settings document")?;
        let memory = Memory::open(&memory_dir).context("Failed to open memory")?;
        let chat_db_path = data_dir.join("chat.db");
        let chat = ChatHistory::open(&chat_db_path).context("Failed to open chat history")?;

        info!(
            "[INSTANCE-{}] Created at {}",
            id,
            instance_dir.display()
        );

        let skill =
            TextFile::open(instance_dir.join(SKILL_FILE)).context("Failed to open skill file")?;
        Ok(Self {
            id,
            instance_dir,
            settings,
            memory,
            chat: Arc::new(Mutex::new(chat)),
            workspace,
            skill,
        })
    }

    /// Open an existing instance from its directory.
    ///
    /// Ensures all subdirectories exist (creates them if missing for backward compatibility).
    /// Handles one-time migration of keypoints.md + knowledge/*.md → knowledge.md.
    /// Apply security permissions for sandbox isolation (紧箍咒).
    ///
    /// Protects sensitive directories (memory, data, settings) from sandbox user.
    /// Workspace is accessible to sandbox user for script execution.
    ///
    /// NOTE: This is asset-protection, not whitelist isolation. Sandbox user can
    /// still read system directories (755). Future: chroot/namespace isolation.
    pub fn apply_security_permissions(&self) {
        use std::os::unix::fs::PermissionsExt;
        let set_perm = |path: &std::path::Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).ok();
        };
        // instance_dir: traverse only (sandbox can cd through but not list)
        set_perm(&self.instance_dir, 0o711);
        // memory: root only (记忆=最高机密)
        set_perm(self.memory.memory_dir(), 0o700);
        // data: root only (数据库)
        set_perm(&self.instance_dir.join("data"), 0o700);
        // workspace: sandbox user can work here
        set_perm(&self.workspace, 0o750);
        // settings: root only
        set_perm(self.settings.path(), 0o600);
    }

    pub fn open(instance_dir: &Path) -> Result<Self> {
        let id = instance_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Invalid instance directory: {}", instance_dir.display())
            })?
            .to_string();

        let memory_dir = instance_dir.join("memory");
        let knowledge_dir = memory_dir.join("knowledge");
        let workspace = instance_dir.join("workspace");
        let data_dir = instance_dir.join("data");

        // Ensure directories exist (backward compatibility)
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&knowledge_dir).with_context(|| {
            format!(
                "Failed to create knowledge dir: {}",
                knowledge_dir.display()
            )
        })?;
        std::fs::create_dir_all(&workspace)
            .with_context(|| format!("Failed to create workspace dir: {}", workspace.display()))?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;

        // One-time migration: keypoints.md + knowledge/*.md → knowledge.md
        let knowledge_file = memory_dir.join(KNOWLEDGE_FILE);
        if !knowledge_file.exists() {
            Self::migrate_knowledge(&knowledge_dir, &knowledge_file, &id)?;
        }

        // Open settings
        let settings_path = instance_dir.join(SETTINGS_FILE);
        let settings = Document::open(&settings_path)
            .with_context(|| format!("Failed to open settings: {}", settings_path.display()))?;

        // Open memory (after migration so TextFile reads migrated content)
        let memory = Memory::open(&memory_dir).context("Failed to open memory")?;

        // Open chat history
        let chat_db_path = data_dir.join("chat.db");
        let chat = ChatHistory::open(&chat_db_path).context("Failed to open chat history")?;

        info!("[INSTANCE-{}] Opened at {}", id, instance_dir.display());

        let skill =
            TextFile::open(instance_dir.join(SKILL_FILE)).context("Failed to open skill file")?;
        Ok(Self {
            id,
            instance_dir: instance_dir.to_path_buf(),
            settings,
            memory,
            chat: Arc::new(Mutex::new(chat)),
            workspace,
            skill,
        })
    }

    /// Open with an injected (cached) chat connection.
    pub fn open_with_chat(instance_dir: &Path, chat: Arc<Mutex<ChatHistory>>) -> Result<Self> {
        let id = instance_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("Invalid instance directory: {}", instance_dir.display())
            })?
            .to_string();

        let memory_dir = instance_dir.join("memory");
        let knowledge_dir = memory_dir.join("knowledge");
        let workspace = instance_dir.join("workspace");
        let data_dir = instance_dir.join("data");

        // Ensure directories exist (backward compatibility)
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&knowledge_dir).with_context(|| {
            format!(
                "Failed to create knowledge dir: {}",
                knowledge_dir.display()
            )
        })?;
        std::fs::create_dir_all(&workspace)
            .with_context(|| format!("Failed to create workspace dir: {}", workspace.display()))?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;

        // One-time migration: keypoints.md + knowledge/*.md → knowledge.md
        let knowledge_file = memory_dir.join(KNOWLEDGE_FILE);
        if !knowledge_file.exists() {
            Self::migrate_knowledge(&knowledge_dir, &knowledge_file, &id)?;
        }

        let settings_path = instance_dir.join(SETTINGS_FILE);
        let settings = Document::open(&settings_path)
            .with_context(|| format!("Failed to open settings: {}", settings_path.display()))?;

        let memory = Memory::open(&memory_dir).context("Failed to open memory")?;

        info!("[INSTANCE-{}] Opened at {}", id, instance_dir.display());

        let skill =
            TextFile::open(instance_dir.join(SKILL_FILE)).context("Failed to open skill file")?;

        Ok(Self {
            id,
            instance_dir: instance_dir.to_path_buf(),
            settings,
            memory,
            chat,
            workspace,
            skill,
        })
    }


    /// One-time migration: keypoints.md + knowledge/*.md → knowledge.md
    fn migrate_knowledge(
        knowledge_dir: &Path,
        knowledge_file: &Path,
        instance_id: &str,
    ) -> Result<()> {
        let keypoints_path = knowledge_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid knowledge dir"))?
            .join("keypoints.md");

        if !keypoints_path.exists() {
            return Ok(());
        }

        let mut merged = String::new();
        if let Ok(kp) = std::fs::read_to_string(&keypoints_path) {
            if !kp.trim().is_empty() {
                merged.push_str(&kp);
            }
        }

        // Read knowledge/*.md files sorted by name
        if let Ok(entries) = std::fs::read_dir(knowledge_dir) {
            let mut files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "md"))
                .collect();
            files.sort_by_key(|e| e.file_name());
            for entry in files {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    if !content.trim().is_empty() {
                        if !merged.is_empty() {
                            merged.push_str("\n\n");
                        }
                        merged.push_str(&content);
                    }
                }
            }
        }

        if !merged.is_empty() {
            crate::util::atomic_write(knowledge_file, &merged)?;
            info!(
                "[INSTANCE-{}] Migrated keypoints.md + knowledge/*.md → knowledge.md ({} bytes)",
                instance_id,
                merged.len()
            );
        }

        Ok(())
    }
}

// ─── InstanceStore (grandparent) ─────────────────────────────────

/// Manages the lifecycle of all instances under a directory.
///
/// Three generations:
///   InstanceStore (grandparent) — create / delete / list / open
///   Instance (parent) — manages all persistence for one instance
///   Memory (child) — manages memory files
pub struct InstanceStore {
    instances_dir: PathBuf,
    /// SQLite connections — the only resource that needs reuse.
    connections: Arc<std::sync::RwLock<HashMap<String, Arc<Mutex<ChatHistory>>>>>,
}

impl Clone for InstanceStore {
    fn clone(&self) -> Self {
        Self {
            instances_dir: self.instances_dir.clone(),
            connections: self.connections.clone(),
        }
    }
}

impl InstanceStore {
    /// Create a new store rooted at the given instances directory.
    pub fn new(instances_dir: PathBuf) -> Self {
        Self {
            instances_dir,
            connections: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// The root directory containing all instances.
    pub fn instances_dir(&self) -> &Path {
        &self.instances_dir
    }

    /// The workspace directory for a given instance.
    pub fn workspace_dir(&self, id: &str) -> PathBuf {
        self.instances_dir.join(id).join("workspace")
    }

    /// Get or create a cached SQLite connection for an instance.
    fn get_connection(&self, id: &str) -> Result<Arc<Mutex<ChatHistory>>> {
        // Fast path: read lock
        {
            let cache = self.connections.read().unwrap();
            if let Some(ch) = cache.get(id) {
                return Ok(ch.clone());
            }
        }
        // Slow path: write lock, open connection
        let mut cache = self.connections.write().unwrap();
        // Double-check after acquiring write lock
        if let Some(ch) = cache.get(id) {
            return Ok(ch.clone());
        }
        let instance_dir = self.instances_dir.join(id);
        let chat_db_path = instance_dir.join("data").join("chat.db");
        let ch = ChatHistory::open(&chat_db_path)?;
        let arc = Arc::new(Mutex::new(ch));
        cache.insert(id.to_string(), arc.clone());
        Ok(arc)
    }

    /// Create a new instance atomically.
    pub fn create(
        &self,
        display_name: Option<&str>,
        knowledge: Option<&str>,
        initial_settings: Option<&Settings>,
    ) -> Result<Instance> {
        let instance = Instance::create(
            &self.instances_dir,
            display_name,
            knowledge,
            initial_settings,
        )?;
        // Cache the connection from the newly created instance
        let mut cache = self.connections.write().unwrap();
        cache.insert(instance.id.clone(), instance.chat.clone());
        Ok(instance)
    }

    /// Open an existing instance by ID (with cached SQLite connection).
    pub fn open(&self, id: &str) -> Result<Instance> {
        let instance_dir = self.instances_dir.join(id);
        if !instance_dir.exists() {
            anyhow::bail!("Instance not found: {}", id);
        }
        let chat = self.get_connection(id)?;
        let instance = Instance::open_with_chat(&instance_dir, chat)?;
        Ok(instance)
    }

    /// Delete an instance by moving it to .trash directory.
    ///
    /// Returns the trash path name for logging.
    pub fn delete(&self, id: &str) -> Result<String> {
        // Safety: refuse suspicious names
        if id.contains('/') || id.contains("..") || id.is_empty() {
            anyhow::bail!("Invalid instance id: {}", id);
        }

        let instance_dir = self.instances_dir.join(id);
        if !instance_dir.exists() {
            anyhow::bail!("Instance not found: {}", id);
        }

        let trash_dir = self.instances_dir.join(".trash");
        std::fs::create_dir_all(&trash_dir)
            .with_context(|| format!("Failed to create trash dir: {}", trash_dir.display()))?;

        let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
        let trash_name = format!("{}_{}", id, timestamp);
        let trash_path = trash_dir.join(&trash_name);

        std::fs::rename(&instance_dir, &trash_path)
            .with_context(|| format!("Failed to move {} to trash", id))?;

        // Clear cached connection
        if let Ok(mut cache) = self.connections.write() {
            cache.remove(id);
        }

        info!(
            "[INSTANCE-STORE] Deleted instance: {} -> .trash/{}",
            id, trash_name
        );
        Ok(trash_name)
    }

    /// List all valid instance IDs (directories with settings.json, excluding hidden dirs).
    pub fn list_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();

        let entries = std::fs::read_dir(&self.instances_dir).with_context(|| {
            format!(
                "Failed to read instances dir: {}",
                self.instances_dir.display()
            )
        })?;

        for entry in entries {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden directories (e.g. .trash)
            if name.starts_with('.') {
                continue;
            }
            // Only include directories with settings.json
            if entry.path().join("settings.json").exists() {
                ids.push(name);
            }
        }

        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_instance() {
        let tmp = TempDir::new().unwrap();
        let instances_dir = tmp.path();

        let instance =
            Instance::create(instances_dir, Some("TestBot"), None, None).unwrap();

        assert_eq!(instance.id.len(), 6);
        assert!(instance.instance_dir.exists());
        assert!(instance.workspace.exists());
        assert!(instance.instance_dir.join("data").exists());
        assert!(instance.instance_dir.join("memory").exists());

        let settings = instance.settings.load().unwrap();
        assert_eq!(settings.name, Some("TestBot".to_string()));
        assert!(settings.color.is_some());
    }

    #[test]
    fn test_create_instance_with_knowledge() {
        let tmp = TempDir::new().unwrap();
        let instances_dir = tmp.path();

        let instance = Instance::create(
            instances_dir,
            Some("TestBot"),
            Some("# Test Knowledge\nHello world"),
            None,
        )
        .unwrap();

        let knowledge = instance.memory.knowledge.read().unwrap();
        assert!(knowledge.contains("Test Knowledge"));
        assert!(knowledge.contains("Hello world"));
    }

    #[test]
    fn test_open_instance() {
        let tmp = TempDir::new().unwrap();
        let instances_dir = tmp.path();

        // Create first
        let created =
            Instance::create(instances_dir, Some("TestBot"), None, None).unwrap();
        let id = created.id.clone();
        let dir = created.instance_dir.clone();
        drop(created);

        // Open
        let opened = Instance::open(&dir).unwrap();
        assert_eq!(opened.id, id);
    }

    #[test]
    fn test_create_all_dirs_exist() {
        let tmp = TempDir::new().unwrap();
        let instances_dir = tmp.path();

        let instance = Instance::create(instances_dir, None, None, None).unwrap();

        // All subdirectories should exist immediately after create
        assert!(instance.instance_dir.join("memory").exists());
        assert!(instance
            .instance_dir
            .join("memory")
            .join("knowledge")
            .exists());
        assert!(instance.instance_dir.join("workspace").exists());
        assert!(instance.instance_dir.join("data").exists());
        assert!(instance.instance_dir.join(SETTINGS_FILE).exists());
    }

    #[test]
    fn test_open_creates_missing_dirs() {
        let tmp = TempDir::new().unwrap();
        let instance_dir = tmp.path().join("test01");
        std::fs::create_dir_all(&instance_dir).unwrap();

        // Write minimal settings.json
        let settings = Settings {
            user_id: Some("user1".to_string()),
            ..Default::default()
        };
        let settings_json = serde_json::to_string_pretty(&settings).unwrap();
        std::fs::write(instance_dir.join(SETTINGS_FILE), &settings_json).unwrap();

        // Open should create missing subdirectories
        let instance = Instance::open(&instance_dir).unwrap();
        assert!(instance.instance_dir.join("memory").exists());
        assert!(instance.instance_dir.join("workspace").exists());
        assert!(instance.instance_dir.join("data").exists());
    }

    #[test]
    fn test_knowledge_migration() {
        let tmp = TempDir::new().unwrap();
        let instance_dir = tmp.path().join("migr01");
        std::fs::create_dir_all(&instance_dir).unwrap();

        // Setup: keypoints.md in memory/
        let memory_dir = instance_dir.join("memory");
        let knowledge_dir = memory_dir.join("knowledge");
        std::fs::create_dir_all(&knowledge_dir).unwrap();
        std::fs::write(
            memory_dir.join("keypoints.md"),
            "# Keypoints\nImportant stuff",
        )
        .unwrap();
        std::fs::write(knowledge_dir.join("01_basics.md"), "# Basics\nBasic info").unwrap();

        // Write settings
        let settings = Settings {
            user_id: Some("user1".to_string()),
            ..Default::default()
        };
        std::fs::write(
            instance_dir.join(SETTINGS_FILE),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        // Open triggers migration
        let instance = Instance::open(&instance_dir).unwrap();
        let knowledge = instance.memory.knowledge.read().unwrap();
        assert!(knowledge.contains("Keypoints"));
        assert!(knowledge.contains("Basics"));
    }

    // ─── InstanceStore tests ─────────────────────────────────

    #[test]
    fn test_store_create_and_list() {
        let tmp = TempDir::new().unwrap();
        let store = InstanceStore::new(tmp.path().to_path_buf());

        // Empty store
        let ids = store.list_ids().unwrap();
        assert!(ids.is_empty());

        // Create an instance
        let instance = store
            .create(Some("Test Agent"), None, None)
            .unwrap();
        assert_eq!(instance.id.len(), 6);

        // List should return it
        let ids = store.list_ids().unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], instance.id);
    }

    #[test]
    fn test_store_open() {
        let tmp = TempDir::new().unwrap();
        let store = InstanceStore::new(tmp.path().to_path_buf());

        let created = store
            .create(None, Some("test knowledge"), None)
            .unwrap();
        let id = created.id.clone();
        drop(created);

        let opened = store.open(&id).unwrap();
        assert_eq!(opened.id, id);
        assert!(opened
            .memory
            .knowledge
            .read()
            .unwrap()
            .contains("test knowledge"));
    }

    #[test]
    fn test_store_delete() {
        let tmp = TempDir::new().unwrap();
        let store = InstanceStore::new(tmp.path().to_path_buf());

        let instance = store.create(Some("Doomed"), None, None).unwrap();
        let id = instance.id.clone();
        drop(instance);

        // Delete moves to .trash
        let trash_name = store.delete(&id).unwrap();
        assert!(trash_name.starts_with(&id));

        // No longer listed
        let ids = store.list_ids().unwrap();
        assert!(ids.is_empty());

        // .trash directory exists with the moved instance
        assert!(tmp.path().join(".trash").join(&trash_name).exists());
    }

    #[test]
    fn test_store_delete_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let store = InstanceStore::new(tmp.path().to_path_buf());

        let result = store.delete("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_store_delete_invalid_id() {
        let tmp = TempDir::new().unwrap();
        let store = InstanceStore::new(tmp.path().to_path_buf());

        assert!(store.delete("../escape").is_err());
        assert!(store.delete("path/traversal").is_err());
        assert!(store.delete("").is_err());
    }
}
