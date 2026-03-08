//! Hub module — master mode for multi-engine management.
//!
//! When ALICE_HUB=true, this engine becomes a hub (master) that:
//! - Aggregates instances from multiple slave engines
//! - Proxies API requests to the correct engine based on instance_id
//! - Provides hook callbacks for contacts and message relay
//! - Registers hooks on slave engines at startup
//!
//! Design: hub is an optional layer on top of the engine. When disabled,
//! the engine operates normally as a standalone instance.

pub mod config;
pub mod hooks;
pub mod proxy;

use std::collections::HashMap;
use std::sync::RwLock;

use config::{EngineEntry, HubConfig, HubConfigStore};
use crate::api::types::InstanceInfo;

/// Routing entry: maps an instance_id to its engine.
#[derive(Debug, Clone)]
pub struct InstanceRoute {
    /// Engine endpoint (e.g., "http://localhost:8080")
    pub endpoint: String,
    /// Auth token for this engine
    pub auth_token: String,
    /// Engine label
    pub label: String,
}

/// Hub state — shared across the HTTP server.
pub struct HubState {
    /// Hub configuration store.
    pub config_store: HubConfigStore,
    /// Engine entries from config.
    pub engines: Vec<EngineEntry>,
    /// Instance routing table: instance_id -> InstanceRoute.
    /// Updated periodically by refresh.
    routing_table: RwLock<HashMap<String, InstanceRoute>>,
    /// Cached instance list from all engines (for aggregated GET /api/instances).
    cached_instances: RwLock<Vec<(InstanceInfo, String)>>, // (info, engine_label)
    /// HTTP client for outbound requests to slave engines.
    pub client: reqwest::Client,
    /// This hub's own port (for constructing hook callback URLs).
    pub self_port: u16,
}

impl HubState {
    /// Create a new HubState from config.
    pub fn new(config: HubConfig, config_store: HubConfigStore, self_port: u16) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            engines: config.engines.clone(),
            config_store,
            routing_table: RwLock::new(HashMap::new()),
            cached_instances: RwLock::new(Vec::new()),
            client,
            self_port,
        }
    }

    /// Look up which engine owns an instance_id.
    pub fn route(&self, instance_id: &str) -> Option<InstanceRoute> {
        self.routing_table.read().unwrap().get(instance_id).cloned()
    }

    /// Get all cached instances (from all engines).
    pub fn all_instances(&self) -> Vec<InstanceInfo> {
        self.cached_instances
            .read()
            .unwrap()
            .iter()
            .map(|(info, _)| info.clone())
            .collect()
    }

    /// Refresh the routing table and instance cache by querying all slave engines.
    pub async fn refresh_instances(&self) {
        let mut new_routing = HashMap::new();
        let mut new_instances = Vec::new();

        for engine in &self.engines {
            match self.fetch_engine_instances(engine).await {
                Ok(instances) => {
                    for inst in instances {
                        new_routing.insert(
                            inst.id.clone(),
                            InstanceRoute {
                                endpoint: engine.endpoint.clone(),
                                auth_token: engine.auth_token.clone(),
                                label: engine.label.clone(),
                            },
                        );
                        new_instances.push((inst, engine.label.clone()));
                    }
                    tracing::info!(
                        "[HUB] Refreshed instances from {} ({})",
                        engine.label,
                        engine.endpoint
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "[HUB] Failed to fetch instances from {} ({}): {}",
                        engine.label,
                        engine.endpoint,
                        e
                    );
                }
            }
        }

        *self.routing_table.write().unwrap() = new_routing;
        *self.cached_instances.write().unwrap() = new_instances;
    }

    /// Fetch instances from a single engine.
    async fn fetch_engine_instances(
        &self,
        engine: &EngineEntry,
    ) -> Result<Vec<InstanceInfo>, String> {
        let url = format!("{}/api/instances", engine.endpoint);

        // Build auth cookie for the slave engine
        let session_token = Self::compute_session_token(&engine.auth_token);
        let cookie_name = crate::api::http_protocol::build_session_cookie_name(
            &Self::extract_port(&engine.endpoint),
        );
        let cookie_value = format!("{}={}", cookie_name, session_token);

        let resp = self
            .client
            .get(&url)
            .header("Cookie", cookie_value)
            .send()
            .await
            .map_err(|e| format!("request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        resp.json::<Vec<InstanceInfo>>()
            .await
            .map_err(|e| format!("parse error: {}", e))
    }

    /// Compute session token (SHA256 of auth_token) — same logic as EngineState.
    fn compute_session_token(auth_secret: &str) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(auth_secret.as_bytes());
        hex::encode(hash)
    }

    /// Extract port from endpoint URL (e.g., "http://localhost:8080" -> "8080").
    fn extract_port(endpoint: &str) -> String {
        endpoint
            .rsplit(':')
            .next()
            .unwrap_or("8080")
            .trim_end_matches('/')
            .to_string()
    }

    /// Build the auth cookie header value for a slave engine.
    pub fn build_auth_cookie(&self, route: &InstanceRoute) -> String {
        let session_token = Self::compute_session_token(&route.auth_token);
        let port = Self::extract_port(&route.endpoint);
        let cookie_name = crate::api::http_protocol::build_session_cookie_name(&port);
        format!("{}={}", cookie_name, session_token)
    }
}

