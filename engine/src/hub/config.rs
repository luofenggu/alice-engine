//! Hub configuration — engine registry and persistence.
//!
//! Hub manages a list of slave engines. Each engine has an endpoint and auth token.
//! Configuration is persisted to hub.json.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::persist::Document;

/// A registered slave engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineEntry {
    /// Display label for this engine (e.g., "production", "testing").
    pub label: String,
    /// Base URL of the engine (e.g., "http://localhost:8080").
    pub endpoint: String,
    /// Auth token for this engine (= ALICE_AUTH_SECRET on the slave side).
    pub auth_token: String,
}

/// Hub configuration file format.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HubConfig {
    /// List of slave engines managed by this hub.
    #[serde(default)]
    pub engines: Vec<EngineEntry>,
}

/// Persistent hub configuration store.
pub struct HubConfigStore {
    doc: Document<HubConfig>,
}

impl HubConfigStore {
    /// Open hub.json at the given path. Creates with defaults if missing.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let doc = Document::open(path)?;
        Ok(Self { doc })
    }

    pub fn load(&self) -> Result<HubConfig> {
        self.doc.load()
    }

    pub fn save(&self, config: &HubConfig) -> Result<()> {
        self.doc.save(config)
    }
}

