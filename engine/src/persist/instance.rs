//! Instance struct — manages all persistent state for a single agent instance.
//!
//! Three generations:
//!   InstanceStore (grandparent) — manages lifecycle of all instances
//!   Instance (parent) — manages all persistence for one instance
//!   Memory (child) — manages memory files (knowledge, history, current, sessions)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use tracing::info;

use crate::persist::Document;
use super::chat::ChatHistory;
use super::memory::Memory;

const SETTINGS_FILE: &str = "settings.json";

/// Preset colors for new instances.
const PRESET_COLORS: &[&str] = &[
    "#6c5ce7", "#00b894", "#e17055", "#0984e3", "#fdcb6e",
    "#e84393", "#00cec9", "#a29bfe", "#ff7675", "#55efc4",
];

/// All persistent state for a single agent instance.
pub struct Instance {
    /// Unique instance identifier (6-char hex, also the directory name).
    pub id: String,
    /// Instance root directory (e.g. /root/agents/instances/860021).
    pub instance_dir: PathBuf,
    /// Settings document (JSON file persistence via Document<T>).
    pub settings: Document<InstanceSettings>,
    /// Memory subsystem (knowledge, history, current, sessions).
    pub memory: Memory,
    /// Chat history (SQLite in data/chat.db).
    pub chat: ChatHistory,
    /// Workspace root path (instance_dir/workspace).
    pub workspace: PathBuf,
}

impl Instance {
    /// Create a new instance atomically — all directories and files are created together.
    ///
    /// This solves the "partial creation" bug where only settings.json existed
    /// but data/, memory/, workspace/ were missing until engine hot-scan.
    pub fn create(
        instances_dir: &Path,
        user_id: &str,
        display_name: Option<&str>,
        knowledge: Option<&str>,
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

        std::fs::create_dir_all(&instance_dir)
            .with_context(|| format!("Failed to create instance dir: {}", instance_dir.display()))?;
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&knowledge_dir)
            .with_context(|| format!("Failed to create knowledge dir: {}", knowledge_dir.display()))?;
        std::fs::create_dir_all(&workspace)
            .with_context(|| format!("Failed to create workspace dir: {}", workspace.display()))?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;

        // Random color from presets
        let color = PRESET_COLORS[rand::random::<usize>() % PRESET_COLORS.len()];

        // Write settings.json
        let settings_obj = InstanceSettings {
            user_id: user_id.to_string(),
            color: Some(color.to_string()),
            name: display_name.map(|n| n.to_string()),
            ..Default::default()
        };
        let settings_path = instance_dir.join(SETTINGS_FILE);
        let settings_json = serde_json::to_string_pretty(&settings_obj)
            .context("Failed to serialize settings")?;
        std::fs::write(&settings_path, &settings_json)
            .with_context(|| format!("Failed to write settings: {}", settings_path.display()))?;

        // Write initial knowledge if provided
        if let Some(k) = knowledge {
            if !k.is_empty() {
                let knowledge_file = memory_dir.join(crate::inference::beat::KNOWLEDGE_FILE);
                crate::atomic_write(&knowledge_file, k)?;
            }
        }

        // Open all persistent handles
        let settings = Document::open(&settings_path)
            .context("Failed to open settings document")?;
        let memory = Memory::open(&memory_dir)
            .context("Failed to open memory")?;
        let chat_db_path = data_dir.join("chat.db");
        let chat = ChatHistory::open(&chat_db_path)
            .context("Failed to open chat history")?;

        info!("[INSTANCE-{}] Created for user {} at {}", id, user_id, instance_dir.display());

        Ok(Self {
            id,
            instance_dir,
            settings,
            memory,
            chat,
            workspace,
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
            .ok_or_else(|| anyhow::anyhow!("Invalid instance directory: {}", instance_dir.display()))?
            .to_string();

        let memory_dir = instance_dir.join("memory");
        let knowledge_dir = memory_dir.join("knowledge");
        let workspace = instance_dir.join("workspace");
        let data_dir = instance_dir.join("data");

        // Ensure directories exist (backward compatibility)
        std::fs::create_dir_all(&memory_dir)
            .with_context(|| format!("Failed to create memory dir: {}", memory_dir.display()))?;
        std::fs::create_dir_all(&knowledge_dir)
            .with_context(|| format!("Failed to create knowledge dir: {}", knowledge_dir.display()))?;
        std::fs::create_dir_all(&workspace)
            .with_context(|| format!("Failed to create workspace dir: {}", workspace.display()))?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;

        // One-time migration: keypoints.md + knowledge/*.md → knowledge.md
        let knowledge_file = memory_dir.join(crate::inference::beat::KNOWLEDGE_FILE);
        if !knowledge_file.exists() {
            Self::migrate_knowledge(&knowledge_dir, &knowledge_file, &id)?;
        }

        // Open settings
        let settings_path = instance_dir.join(SETTINGS_FILE);
        let settings = Document::open(&settings_path)
            .with_context(|| format!("Failed to open settings: {}", settings_path.display()))?;

        // Open memory (after migration so TextFile reads migrated content)
        let memory = Memory::open(&memory_dir)
            .context("Failed to open memory")?;

        // Open chat history
        let chat_db_path = data_dir.join("chat.db");
        let chat = ChatHistory::open(&chat_db_path)
            .context("Failed to open chat history")?;

        info!("[INSTANCE-{}] Opened at {}", id, instance_dir.display());

        Ok(Self {
            id,
            instance_dir: instance_dir.to_path_buf(),
            settings,
            memory,
            chat,
            workspace,
        })
    }

    /// Get user_id from settings.
    pub fn user_id(&self) -> String {
        self.settings.load().map(|s| s.user_id).unwrap_or_default()
    }


    /// One-time migration: keypoints.md + knowledge/*.md → knowledge.md
    fn migrate_knowledge(knowledge_dir: &Path, knowledge_file: &Path, instance_id: &str) -> Result<()> {
        let keypoints_path = knowledge_dir.parent()
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
            crate::atomic_write(knowledge_file, &merged)?;
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
    /// Cached ChatHistory connections per instance (connection reuse).
    chat_cache: std::sync::Arc<std::sync::RwLock<HashMap<String, std::sync::Arc<std::sync::Mutex<ChatHistory>>>>>,
}

impl Clone for InstanceStore {
    fn clone(&self) -> Self {
        Self {
            instances_dir: self.instances_dir.clone(),
            chat_cache: self.chat_cache.clone(),
        }
    }
}

impl InstanceStore {
    /// Create a new store rooted at the given instances directory.
    pub fn new(instances_dir: PathBuf) -> Self {
        Self {
            instances_dir,
            chat_cache: std::sync::Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// The root directory containing all instances.
    pub fn instances_dir(&self) -> &Path {
        &self.instances_dir
    }

    /// Create a new instance atomically.
    pub fn create(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        knowledge: Option<&str>,
    ) -> Result<Instance> {
        Instance::create(&self.instances_dir, user_id, display_name, knowledge)
    }

    /// Open an existing instance by ID.
    pub fn open(&self, id: &str) -> Result<Instance> {
        let instance_dir = self.instances_dir.join(id);
        if !instance_dir.exists() {
            anyhow::bail!("Instance not found: {}", id);
        }
        Instance::open(&instance_dir)
    }

    /// Get or create a cached ChatHistory connection for an instance.
    pub fn get_chat(&self, id: &str) -> Result<std::sync::Arc<std::sync::Mutex<ChatHistory>>> {
        // Fast path: read lock
        {
            let cache = self.chat_cache.read().unwrap();
            if let Some(ch) = cache.get(id) {
                return Ok(ch.clone());
            }
        }
        // Slow path: write lock, open connection
        let mut cache = self.chat_cache.write().unwrap();
        // Double-check after acquiring write lock
        if let Some(ch) = cache.get(id) {
            return Ok(ch.clone());
        }
        let instance_dir = self.instances_dir.join(id);
        let chat_db_path = instance_dir.join("data").join("chat.db");
        let ch = ChatHistory::open(&chat_db_path)?;
        let arc = std::sync::Arc::new(std::sync::Mutex::new(ch));
        cache.insert(id.to_string(), arc.clone());
        Ok(arc)
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
        if let Ok(mut cache) = self.chat_cache.write() {
            cache.remove(id);
        }

        info!("[INSTANCE-STORE] Deleted instance: {} -> .trash/{}", id, trash_name);
        Ok(trash_name)
    }

    /// List all valid instance IDs (directories with settings.json, excluding hidden dirs).
    pub fn list_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();

        let entries = std::fs::read_dir(&self.instances_dir)
            .with_context(|| format!("Failed to read instances dir: {}", self.instances_dir.display()))?;

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

        let instance = Instance::create(instances_dir, "user1", Some("TestBot"), None).unwrap();

        assert_eq!(instance.id.len(), 6);
        assert!(instance.instance_dir.exists());
        assert!(instance.workspace.exists());
        assert!(instance.instance_dir.join("data").exists());
        assert!(instance.instance_dir.join("memory").exists());
        assert_eq!(instance.user_id(), "user1");

        let settings = instance.settings.load().unwrap();
        assert_eq!(settings.user_id, "user1");
        assert_eq!(settings.name, Some("TestBot".to_string()));
        assert!(settings.color.is_some());
    }

    #[test]
    fn test_create_instance_with_knowledge() {
        let tmp = TempDir::new().unwrap();
        let instances_dir = tmp.path();

        let instance = Instance::create(
            instances_dir,
            "user1",
            Some("TestBot"),
            Some("# Test Knowledge\nHello world"),
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
        let created = Instance::create(instances_dir, "user1", Some("TestBot"), None).unwrap();
        let id = created.id.clone();
        let dir = created.instance_dir.clone();
        drop(created);

        // Open
        let opened = Instance::open(&dir).unwrap();
        assert_eq!(opened.id, id);
        assert_eq!(opened.user_id(), "user1");
    }

    #[test]
    fn test_create_all_dirs_exist() {
        let tmp = TempDir::new().unwrap();
        let instances_dir = tmp.path();

        let instance = Instance::create(instances_dir, "user1", None, None).unwrap();

        // All subdirectories should exist immediately after create
        assert!(instance.instance_dir.join("memory").exists());
        assert!(instance.instance_dir.join("memory").join("knowledge").exists());
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
        let settings = InstanceSettings {
            user_id: "user1".to_string(),
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
        std::fs::write(memory_dir.join("keypoints.md"), "# Keypoints\nImportant stuff").unwrap();
        std::fs::write(knowledge_dir.join("01_basics.md"), "# Basics\nBasic info").unwrap();

        // Write settings
        let settings = InstanceSettings {
            user_id: "user1".to_string(),
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
        let instance = store.create("user1", Some("Test Agent"), None).unwrap();
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

        let created = store.create("user1", None, Some("test knowledge")).unwrap();
        let id = created.id.clone();
        drop(created);

        let opened = store.open(&id).unwrap();
        assert_eq!(opened.id, id);
        assert!(opened.memory.knowledge.read().unwrap().contains("test knowledge"));
    }

    #[test]
    fn test_store_delete() {
        let tmp = TempDir::new().unwrap();
        let store = InstanceStore::new(tmp.path().to_path_buf());

        let instance = store.create("user1", Some("Doomed"), None).unwrap();
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

// ---------------------------------------------------------------------------
// InstanceSettings — per-instance configuration (.proto for settings.json)
// ---------------------------------------------------------------------------

// InstanceSettings and ExtraModel are defined in alice-rpc crate (type sharing).
pub use alice_rpc::{InstanceSettings, SettingsUpdate};

/// Extension trait for InstanceSettings — engine-specific logic.
pub trait InstanceSettingsExt {
    fn apply_env_fallbacks(&mut self, env_config: &crate::policy::EnvConfig);
    fn validate(&self) -> anyhow::Result<()>;
}

impl InstanceSettingsExt for InstanceSettings {
    /// Apply environment variable fallbacks for api_key, model, and user_id.
    /// Call this after loading from file to fill in missing values.
    fn apply_env_fallbacks(&mut self, env_config: &crate::policy::EnvConfig) {
        if self.api_key.is_empty() {
            self.api_key = env_config.default_api_key.clone();
        }
        if self.model.is_empty() {
            let llm_config = &crate::policy::EngineConfig::get().llm;
            self.model = env_config.default_model.clone()
                .unwrap_or_else(|| llm_config.default_model.clone());
        }
        if self.user_id.is_empty() {
            self.user_id = env_config.user_id.clone();
        }
    }

    /// Check that required fields are present. Call after apply_env_fallbacks().
    fn validate(&self) -> anyhow::Result<()> {
        if self.api_key.is_empty() {
            anyhow::bail!("Missing api_key: set in settings.json or ALICE_DEFAULT_API_KEY env var");
        }
        Ok(())
    }
}
