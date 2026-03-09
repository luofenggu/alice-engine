//! Settings — unified configuration struct for both global and instance levels.
//!
//! This is a pure data object (DO). All fields are Optional for merge-update semantics.
//! Used by both GlobalSettingsStore and Instance's settings Document.

use serde::{Deserialize, Serialize};

/// An inference channel: provider+model bound to an API key.
///
/// Used for extra channels in round-robin failover.
/// The `model` field uses the same `provider@model_id` format as the main model setting.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Channel {
    pub api_key: String,
    pub model: String,
}

/// Unified settings struct — used at both global and instance level.
///
/// All fields are Option for merge semantics:
/// - env ∪ engine.toml → seed Settings
/// - seed ∪ global_settings.json → global Settings
/// - global ∪ instance settings.json → final instance Settings
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privileged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_beats: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_blocks_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_block_kb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_kb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_max_consecutive_beats: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_cooldown_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_env: Option<String>,
    /// Extra inference channels for round-robin failover.
    /// Each channel has its own api_key and model (provider@model_id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_channels: Option<Vec<Channel>>,
}

impl Settings {
    /// Fill None fields from fallback. Self takes priority.
    pub fn merge_fallback(&mut self, fallback: &Settings) {
        if self.api_key.is_none() {
            self.api_key = fallback.api_key.clone();
        }
        if self.model.is_none() {
            self.model = fallback.model.clone();
        }

        if self.privileged.is_none() {
            self.privileged = fallback.privileged;
        }
        if self.max_beats.is_none() {
            self.max_beats = fallback.max_beats;
        }
        if self.session_blocks_limit.is_none() {
            self.session_blocks_limit = fallback.session_blocks_limit;
        }
        if self.session_block_kb.is_none() {
            self.session_block_kb = fallback.session_block_kb;
        }
        if self.history_kb.is_none() {
            self.history_kb = fallback.history_kb;
        }
        if self.safety_max_consecutive_beats.is_none() {
            self.safety_max_consecutive_beats = fallback.safety_max_consecutive_beats;
        }
        if self.safety_cooldown_secs.is_none() {
            self.safety_cooldown_secs = fallback.safety_cooldown_secs;
        }
        if self.name.is_none() {
            self.name = fallback.name.clone();
        }
        if self.color.is_none() {
            self.color = fallback.color.clone();
        }
        if self.avatar.is_none() {
            self.avatar = fallback.avatar.clone();
        }
        if self.temperature.is_none() {
            self.temperature = fallback.temperature;
        }
        if self.max_tokens.is_none() {
            self.max_tokens = fallback.max_tokens;
        }
        if self.host.is_none() {
            self.host = fallback.host.clone();
        }
        if self.shell_env.is_none() {
            self.shell_env = fallback.shell_env.clone();
        }
        if self.extra_channels.as_ref().map_or(true, |v| v.is_empty()) {
            self.extra_channels = fallback.extra_channels.clone();
        }
    }

    /// Build seed settings from environment variables and engine.toml defaults.
    pub fn from_env_and_defaults(env: &crate::policy::EnvConfig) -> Self {
        let llm = &crate::policy::EngineConfig::get().llm;
        let mem = &crate::policy::EngineConfig::get().memory;
        Self {
            api_key: if env.default_api_key.is_empty() {
                None
            } else {
                Some(env.default_api_key.clone())
            },
            model: env
                .default_model
                .clone()
                .or_else(|| Some(llm.default_model.clone())),
            user_id: None,
            privileged: None,
            max_beats: None,
            session_blocks_limit: Some(mem.session_blocks_limit),
            session_block_kb: Some(mem.session_block_kb),
            history_kb: Some(mem.history_kb),
            safety_max_consecutive_beats: Some(mem.safety_max_consecutive_beats),
            safety_cooldown_secs: Some(mem.safety_cooldown_secs),
            name: None,
            color: None,
            avatar: None,
            temperature: Some(llm.temperature),
            max_tokens: Some(llm.max_tokens),
            host: env.host.clone(),
            shell_env: if env.shell_env.is_empty() {
                None
            } else {
                Some(env.shell_env.clone())
            },
            extra_channels: None,
        }
    }

    /// Check that required fields are present.
    pub fn validate(&self) -> anyhow::Result<()> {
        let key = self.api_key.as_deref().unwrap_or_default();
        if key.is_empty() {
            anyhow::bail!("Missing api_key: set in settings.json or ALICE_DEFAULT_API_KEY env var");
        }
        Ok(())
    }

    /// Get api_key or empty string.
    pub fn api_key_or_default(&self) -> String {
        self.api_key.clone().unwrap_or_default()
    }

    /// Get model or empty string.
    pub fn model_or_default(&self) -> String {
        self.model.clone().unwrap_or_default()
    }

    /// Get user_id or empty string.
    pub fn user_id_or_default(&self) -> String {
        self.user_id.clone().unwrap_or_default()
    }

    /// Get privileged or false.
    pub fn privileged_or_default(&self) -> bool {
        self.privileged.unwrap_or(false)
    }
}

// ─── GlobalSettingsStore ─────────────────────────────────────────

/// Store for global_settings.json — initialized once with base_dir.
#[derive(Clone)]
pub struct GlobalSettingsStore {
    doc: super::Document<Settings>,
}

impl GlobalSettingsStore {
    /// Initialize global settings store.
    ///
    /// Performs three-layer merge: env ∪ engine.toml → seed, seed ∪ persisted → global.
    /// Writes merged result back to disk.
    /// Returns (merged_settings, store).
    pub fn init(
        base_dir: &std::path::Path,
        env: &crate::policy::EnvConfig,
    ) -> anyhow::Result<(Settings, Self)> {
        let path = base_dir.join(super::GLOBAL_SETTINGS_FILE);
        let seed = Settings::from_env_and_defaults(env);

        let mut gs = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str::<Settings>(&c).ok())
                .unwrap_or_default()
        } else {
            Settings::default()
        };
        gs.merge_fallback(&seed);

        let content = serde_json::to_string_pretty(&gs)
            .map_err(|e| anyhow::anyhow!("Failed to serialize global settings: {}", e))?;
        std::fs::write(&path, content)
            .map_err(|e| anyhow::anyhow!("Failed to write global settings: {}", e))?;

        let doc = super::Document::open(path)?;
        Ok((gs, Self { doc }))
    }

    /// Load current global settings from disk.
    pub fn load(&self) -> anyhow::Result<Settings> {
        self.doc.load()
    }

    /// Save global settings to disk.
    pub fn save(&self, settings: &Settings) -> anyhow::Result<()> {
        self.doc.save(settings)
    }

    /// Merge-update: apply update's non-None fields on top of current, then save.
    pub fn merge_update(&self, update: Settings) -> anyhow::Result<()> {
        let mut merged = update;
        let current = self.load()?;
        merged.merge_fallback(&current);
        self.save(&merged)
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::Document;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_merge_fallback() {
        let mut base = Settings {
            model: Some("base-model".into()),
            ..Default::default()
        };
        let fallback = Settings {
            model: Some("fallback-model".into()),
            api_key: Some("fallback-key".into()),
            ..Default::default()
        };
        base.merge_fallback(&fallback);
        assert_eq!(
            base.model.as_deref(),
            Some("base-model"),
            "Self takes priority"
        );
        assert_eq!(
            base.api_key.as_deref(),
            Some("fallback-key"),
            "None filled from fallback"
        );
    }

    #[test]
    fn test_validate_empty_key() {
        let s = Settings::default();
        assert!(
            s.validate().is_err(),
            "Empty api_key should fail validation"
        );
    }

    #[test]
    fn test_validate_with_key() {
        let s = Settings {
            api_key: Some("key".into()),
            ..Default::default()
        };
        assert!(s.validate().is_ok(), "Non-empty api_key should pass");
    }

    #[test]
    fn test_defaults() {
        assert_eq!(Settings::default().privileged_or_default(), false);
        assert_eq!(Settings::default().api_key_or_default(), "");
        assert_eq!(Settings::default().model_or_default(), "");
        assert_eq!(Settings::default().user_id_or_default(), "");
    }

    #[test]
    fn test_three_layer_settings_inheritance() {
        let tmp = TempDir::new().unwrap();

        // === Layer 1: Simulate env-derived global settings ===
        let global = Settings {
            api_key: Some("env-api-key".into()),
            model: Some("env-model".into()),
            user_id: Some("user1".into()),
            temperature: Some(0.7),
            ..Default::default()
        };
        // Persist global settings via GlobalSettingsStore
        let global_path = tmp.path().join("global_settings.json");
        std::fs::write(&global_path, serde_json::to_string_pretty(&global).unwrap()).unwrap();
        let global_doc = Document::<Settings>::open(&global_path).unwrap();
        let store = GlobalSettingsStore { doc: global_doc };

        // === Layer 2: Create instance with blank (留白) settings ===
        let instance_settings_path = tmp.path().join("instance_settings.json");
        let instance_doc = Document::<Settings>::open(&instance_settings_path).unwrap();
        // Instance starts blank
        let instance_settings = instance_doc.load().unwrap();
        assert_eq!(
            instance_settings.api_key, None,
            "Instance should start blank"
        );
        assert_eq!(instance_settings.model, None, "Instance should start blank");

        // === Layer 3: Runtime merge — instance enjoys global (env) values ===
        let mut runtime = instance_settings.clone();
        runtime.merge_fallback(&global);
        assert_eq!(
            runtime.api_key.as_deref(),
            Some("env-api-key"),
            "Should inherit global api_key"
        );
        assert_eq!(
            runtime.model.as_deref(),
            Some("env-model"),
            "Should inherit global model"
        );
        assert_eq!(
            runtime.temperature,
            Some(0.7),
            "Should inherit global temperature"
        );

        // === Update instance setting (only model) — 留白: only store what was explicitly set ===
        let update = Settings {
            model: Some("instance-model".into()),
            ..Default::default()
        };
        let mut new_instance = update.clone();
        new_instance.merge_fallback(&instance_settings); // preserve previously set fields
        instance_doc.save(&new_instance).unwrap();

        // Verify 留白: instance file only has model, not api_key
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&instance_settings_path).unwrap())
                .unwrap();
        assert!(saved.get("model").is_some(), "model should be persisted");
        assert!(
            saved.get("api_key").is_none(),
            "api_key should NOT be in instance file (留白)"
        );

        // Runtime merge — instance model wins, api_key from global
        let instance_settings = instance_doc.load().unwrap();
        let mut runtime = instance_settings.clone();
        runtime.merge_fallback(&global);
        assert_eq!(
            runtime.model.as_deref(),
            Some("instance-model"),
            "Instance model should win"
        );
        assert_eq!(
            runtime.api_key.as_deref(),
            Some("env-api-key"),
            "Should still inherit global api_key"
        );

        // === Update global setting (change api_key) ===
        let global_update = Settings {
            api_key: Some("new-global-key".into()),
            ..Default::default()
        };
        store.merge_update(global_update).unwrap();
        let new_global = store.load().unwrap();
        assert_eq!(
            new_global.api_key.as_deref(),
            Some("new-global-key"),
            "Global api_key should be updated"
        );
        assert_eq!(
            new_global.model.as_deref(),
            Some("env-model"),
            "Global model unchanged"
        );

        // Runtime merge with updated global — instance picks up new global api_key
        let instance_settings = instance_doc.load().unwrap();
        let mut runtime = instance_settings.clone();
        runtime.merge_fallback(&new_global);
        assert_eq!(
            runtime.api_key.as_deref(),
            Some("new-global-key"),
            "Instance should enjoy new global api_key (not overridden)"
        );
        assert_eq!(
            runtime.model.as_deref(),
            Some("instance-model"),
            "Instance model should NOT be affected by global changes"
        );
        assert_eq!(
            runtime.temperature,
            Some(0.7),
            "Temperature inherited from global (unchanged)"
        );
    }

    #[test]
    fn test_hot_reload_channel_merge_instance_priority() {
        // Reproduces bug: hot-reload merge used global as base (priority),
        // instance as fallback. Correct: instance takes priority, global as fallback.

        let global = Settings {
            api_key: Some("global-key".into()),
            model: Some("global-model".into()),
            temperature: Some(0.5),
            extra_channels: Some(vec![
                Channel { api_key: "global-extra-key".into(), model: "global-extra-model".into() },
            ]),
            ..Default::default()
        };

        let instance = Settings {
            model: Some("instance-model".into()),
            extra_channels: Some(vec![
                Channel { api_key: "instance-extra-key".into(), model: "instance-extra-model".into() },
            ]),
            ..Default::default()
        };

        // BUG behavior (global as base, instance as fallback):
        let mut bug_merged = global.clone();
        bug_merged.merge_fallback(&instance);
        // Global model wins — WRONG
        assert_eq!(bug_merged.model.as_deref(), Some("global-model"),
            "Bug: global model takes priority over instance");
        // Global extra_channels wins — WRONG
        assert_eq!(bug_merged.extra_channels.as_ref().unwrap()[0].model, "global-extra-model",
            "Bug: global extra_channels takes priority over instance");

        // CORRECT behavior (instance as base, global as fallback):
        let mut correct_merged = instance.clone();
        correct_merged.merge_fallback(&global);
        // Instance model wins — CORRECT
        assert_eq!(correct_merged.model.as_deref(), Some("instance-model"),
            "Fix: instance model takes priority over global");
        // Instance extra_channels wins — CORRECT
        assert_eq!(correct_merged.extra_channels.as_ref().unwrap()[0].model, "instance-extra-model",
            "Fix: instance extra_channels takes priority over global");
        // api_key falls back to global (instance didn't set it) — CORRECT
        assert_eq!(correct_merged.api_key.as_deref(), Some("global-key"),
            "Fix: api_key falls back to global when instance doesn't set it");
    }
}

#[cfg(test)]
mod tests_empty_array_bug {
    use super::*;

    #[test]
    fn test_merge_fallback_empty_extra_channels_inherits_from_fallback() {
        // Bug: instance has extra_channels: Some(vec![]) (empty array from JSON),
        // merge_fallback only checked is_none(), so empty array blocked inheritance.
        let global = Settings {
            extra_channels: Some(vec![
                Channel { api_key: "global-key".into(), model: "global-model".into() },
            ]),
            ..Default::default()
        };

        // Case 1: extra_channels is None — should inherit
        let mut none_instance = Settings {
            extra_channels: None,
            ..Default::default()
        };
        none_instance.merge_fallback(&global);
        assert_eq!(none_instance.extra_channels.as_ref().unwrap().len(), 1,
            "None extra_channels should inherit from fallback");

        // Case 2: extra_channels is Some(vec![]) — should also inherit (the bug)
        let mut empty_instance = Settings {
            extra_channels: Some(vec![]),
            ..Default::default()
        };
        empty_instance.merge_fallback(&global);
        assert_eq!(empty_instance.extra_channels.as_ref().unwrap().len(), 1,
            "Empty extra_channels should inherit from fallback");
        assert_eq!(empty_instance.extra_channels.as_ref().unwrap()[0].model, "global-model",
            "Inherited channel should be from global");

        // Case 3: extra_channels has values — should NOT inherit
        let mut set_instance = Settings {
            extra_channels: Some(vec![
                Channel { api_key: "inst-key".into(), model: "inst-model".into() },
            ]),
            ..Default::default()
        };
        set_instance.merge_fallback(&global);
        assert_eq!(set_instance.extra_channels.as_ref().unwrap().len(), 1,
            "Non-empty extra_channels should keep its own value");
        assert_eq!(set_instance.extra_channels.as_ref().unwrap()[0].model, "inst-model",
            "Non-empty extra_channels should not be overwritten by fallback");
    }
}
