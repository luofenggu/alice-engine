//! # Hooks Configuration
//!
//! Manages hook URLs for external extension points.
//! Hooks allow external services (Product layer) to extend engine behavior:
//! - contacts: inject contact list into agent's prompt
//! - send_msg_relay: route messages to other instances via Product
//! - skills: inject additional skills into agent's prompt

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::Document;

// ---------------------------------------------------------------------------
// HooksConfig — persisted hook URLs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contacts_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_msg_relay_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_url: Option<String>,
}

impl HooksConfig {
    pub fn is_empty(&self) -> bool {
        self.contacts_url.is_none()
            && self.send_msg_relay_url.is_none()
            && self.skills_url.is_none()
    }
}

// ---------------------------------------------------------------------------
// API types for hook responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactsResponse {
    #[serde(default)]
    pub contacts: Vec<ContactInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsResponse {
    #[serde(default)]
    pub skills: Vec<SkillInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRequest {
    pub from_instance: String,
    pub to_instance: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// HooksStore — load/save hooks.json via Document<HooksConfig>
// ---------------------------------------------------------------------------

pub struct HooksStore {
    doc: Document<HooksConfig>,
}

impl HooksStore {
    /// Open hooks.json at the given path. Creates with defaults if missing.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let doc = Document::open(path)?;
        Ok(Self { doc })
    }

    pub fn load(&self) -> Result<HooksConfig> {
        self.doc.load()
    }

    pub fn save(&self, config: &HooksConfig) -> Result<()> {
        self.doc.save(config)
    }

    /// Idempotent update: merge incoming config (only set non-None fields).
    pub fn register(&self, incoming: &HooksConfig) -> Result<HooksConfig> {
        self.doc.update(|current| {
            if incoming.contacts_url.is_some() {
                current.contacts_url = incoming.contacts_url.clone();
            }
            if incoming.send_msg_relay_url.is_some() {
                current.send_msg_relay_url = incoming.send_msg_relay_url.clone();
            }
            if incoming.skills_url.is_some() {
                current.skills_url = incoming.skills_url.clone();
            }
        })?;
        self.doc.load()
    }
}

// ---------------------------------------------------------------------------
// HooksCaller — runtime hook invocation with caching
// ---------------------------------------------------------------------------

const CACHE_TTL: Duration = Duration::from_secs(60);
const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

struct CacheEntry<T> {
    value: T,
    fetched_at: Instant,
}

impl<T> CacheEntry<T> {
    fn is_valid(&self) -> bool {
        self.fetched_at.elapsed() < CACHE_TTL
    }
}

pub struct HooksCaller {
    client: reqwest::blocking::Client,
    config: Mutex<HooksConfig>,
    skills_cache: Mutex<Option<CacheEntry<String>>>,
}

impl HooksCaller {
    pub fn new(config: HooksConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(HOOK_TIMEOUT)
            .build()
            .unwrap_or_default();

        Self {
            client,
            config: Mutex::new(config),
            skills_cache: Mutex::new(None),
        }
    }

    /// Update the hooks config (e.g., after POST /api/hooks registration).
    pub fn update_config(&self, config: HooksConfig) {
        *self.config.lock().unwrap() = config;
        // Invalidate cache on config change
        *self.skills_cache.lock().unwrap() = None;
    }

    /// Get current config snapshot.
    pub fn config(&self) -> HooksConfig {
        self.config.lock().unwrap().clone()
    }

    // -----------------------------------------------------------------------
    // Contacts hook
    // -----------------------------------------------------------------------

    /// Fetch contacts list from hub API (no caching — localhost call is fast enough).
    /// On failure, returns Err with description (caller decides how to handle).
    /// Returns Ok(empty vec) when no contacts_url is configured (normal case).
    pub fn fetch_contacts(&self, instance_id: &str) -> Result<Vec<ContactInfo>, String> {
        let url = {
            let cfg = self.config.lock().unwrap();
            match &cfg.contacts_url {
                Some(u) => u.clone(),
                None => return Ok(Vec::new()),
            }
        };

        // Fetch from hook (URL may contain {instance_id} placeholder)
        let request_url = url.replace("{instance_id}", instance_id);
        let fetch_start = Instant::now();
        tracing::info!("[HOOKS] contacts fetch start: {}", request_url);
        let result = match self.client.get(&request_url).send() {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.json::<ContactsResponse>() {
                        Ok(resp_body) => Ok(resp_body.contacts),
                        Err(e) => {
                            tracing::warn!("[HOOKS] contacts parse error: {}", e);
                            Err(format!("contacts parse error: {}", e))
                        }
                    }
                } else {
                    tracing::warn!("[HOOKS] contacts hook returned {} ({}ms)", status, fetch_start.elapsed().as_millis());
                    Err(format!("contacts hook returned {}", status))
                }
            }
            Err(e) => {
                tracing::warn!("[HOOKS] contacts hook request failed: {}", e);
                Err(format!("contacts request failed: {}", e))
            }
        };
        let elapsed = fetch_start.elapsed();
        match &result {
            Ok(contacts) => tracing::info!("[HOOKS] contacts fetch OK: {} contacts in {:?}", contacts.len(), elapsed),
            Err(e) => tracing::warn!("[HOOKS] contacts fetch FAILED in {:?}: {}", elapsed, e),
        }
        result
    }

    /// Format contacts into a prompt-friendly string.
    pub fn format_contacts_for_prompt(&self, instance_id: &str) -> String {
        let contacts = match self.fetch_contacts(instance_id) {
            Ok(c) => {
                tracing::info!("[HOOKS] format_contacts: fetch OK, {} contacts", c.len());
                c
            }
            Err(e) => {
                tracing::warn!("[HOOKS] format_contacts: fetch FAILED ({}), returning empty", e);
                Vec::new()
            }
        };
        format_contacts_list(&contacts)
    }

    // -----------------------------------------------------------------------
    // Skills hook
    // -----------------------------------------------------------------------

    /// Fetch extra skills. Returns cached result if within TTL.
    /// On failure, logs warning and returns empty string (silent degradation).
    pub fn fetch_skills(&self, instance_id: &str) -> String {
        let url = {
            let cfg = self.config.lock().unwrap();
            match &cfg.skills_url {
                Some(u) => u.clone(),
                None => return String::new(),
            }
        };

        // Check cache
        {
            let cache = self.skills_cache.lock().unwrap();
            if let Some(entry) = cache.as_ref() {
                if entry.is_valid() {
                    return entry.value.clone();
                }
            }
        }

        // Fetch from hook (URL may contain {instance_id} placeholder)
        let request_url = url.replace("{instance_id}", instance_id);
        match self.client.get(&request_url).send() {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.json::<SkillsResponse>() {
                        Ok(skills_resp) => {
                            let skills = skills_resp.skills
                                .iter()
                                .map(|s| format!("### {}\n{}", s.name, s.content))
                                .collect::<Vec<_>>()
                                .join("\n\n");
                            let mut cache = self.skills_cache.lock().unwrap();
                            *cache = Some(CacheEntry {
                                value: skills.clone(),
                                fetched_at: Instant::now(),
                            });
                            skills
                        }
                        Err(e) => {
                            tracing::warn!("[HOOKS] skills parse error: {}", e);
                            String::new()
                        }
                    }
                } else {
                    tracing::warn!("[HOOKS] skills hook returned {}", resp.status());
                    String::new()
                }
            }
            Err(e) => {
                tracing::warn!("[HOOKS] skills hook request failed: {}", e);
                String::new()
            }
        }
    }

    // -----------------------------------------------------------------------
    // Send message relay hook
    // -----------------------------------------------------------------------

    /// Relay a message to another instance via Product.
    /// Returns Ok(RelayResponse) on success, Err on failure.
    /// Caller should handle failure (silent degradation).
    pub fn relay_message(
        &self,
        from_instance: &str,
        to_instance: &str,
        content: &str,
    ) -> Result<RelayResponse> {
        let url = {
            let cfg = self.config.lock().unwrap();
            match &cfg.send_msg_relay_url {
                Some(u) => u.clone(),
                None => anyhow::bail!("send_msg_relay hook not configured"),
            }
        };

        let req = RelayRequest {
            from_instance: from_instance.to_string(),
            to_instance: to_instance.to_string(),
            content: content.to_string(),
        };

        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .map_err(|e| anyhow::anyhow!("relay request failed: {}", e))?;

        if resp.status().is_success() {
            // HTTP 2xx means relay succeeded — don't depend on response body format
            let relay_resp = RelayResponse {
                success: true,
                message: None,
            };
            Ok(relay_resp)
        } else {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("relay hook returned {}: {}", status, body)
        }
    }
}

/// Format a contacts list into a prompt-friendly string.
/// Pure function, extracted for testability.
pub fn format_contacts_list(contacts: &[ContactInfo]) -> String {
    if contacts.is_empty() {
        return String::new();
    }

    let entries: Vec<String> = contacts
        .iter()
        .map(|c| match &c.name {
            Some(name) if !name.is_empty() => format!("{}({})", name, c.id),
            _ => c.id.clone(),
        })
        .collect();

    format!("可联系的其他实例：{}", entries.join(", "))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_hooks_config_default_is_empty() {
        let config = HooksConfig::default();
        assert!(config.is_empty());
    }

    #[test]
    fn test_hooks_config_not_empty() {
        let config = HooksConfig {
            contacts_url: Some("http://localhost:3000/contacts".to_string()),
            ..Default::default()
        };
        assert!(!config.is_empty());
    }

    #[test]
    fn test_hooks_store_open_and_load() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        let store = HooksStore::open(&path).unwrap();
        let config = store.load().unwrap();
        assert!(config.is_empty());
    }

    #[test]
    fn test_hooks_store_register_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        let store = HooksStore::open(&path).unwrap();

        // First registration
        let incoming1 = HooksConfig {
            contacts_url: Some("http://localhost:3000/contacts".to_string()),
            ..Default::default()
        };
        let result1 = store.register(&incoming1).unwrap();
        assert_eq!(
            result1.contacts_url,
            Some("http://localhost:3000/contacts".to_string())
        );
        assert!(result1.send_msg_relay_url.is_none());

        // Second registration (adds relay, keeps contacts)
        let incoming2 = HooksConfig {
            send_msg_relay_url: Some("http://localhost:3000/relay".to_string()),
            ..Default::default()
        };
        let result2 = store.register(&incoming2).unwrap();
        assert_eq!(
            result2.contacts_url,
            Some("http://localhost:3000/contacts".to_string())
        );
        assert_eq!(
            result2.send_msg_relay_url,
            Some("http://localhost:3000/relay".to_string())
        );
    }

    #[test]
    fn test_hooks_caller_no_config_returns_empty() {
        let caller = HooksCaller::new(HooksConfig::default());
        let contacts = caller.fetch_contacts("test-instance").unwrap();
        assert!(contacts.is_empty());

        let skills = caller.fetch_skills("test-instance");
        assert!(skills.is_empty());

        let prompt = caller.format_contacts_for_prompt("test-instance");
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_format_contacts_list() {
        let contacts = vec![
            ContactInfo {
                id: "abc123".to_string(),
                name: Some("进化之王".to_string()),
            },
            ContactInfo {
                id: "def456".to_string(),
                name: None,
            },
            ContactInfo {
                id: "ghi789".to_string(),
                name: Some("".to_string()),
            },
        ];
        let result = format_contacts_list(&contacts);
        assert_eq!(result, "可联系的其他实例：进化之王(abc123), def456, ghi789");
    }

    #[test]
    fn test_format_contacts_list_empty() {
        let result = format_contacts_list(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_relay_message_no_config() {
        let caller = HooksCaller::new(HooksConfig::default());
        let result = caller.relay_message("from", "to", "hello");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not configured"));
    }
}