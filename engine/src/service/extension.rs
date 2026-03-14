//! Extension handler trait — engine extension interface for cross-instance communication.
//!
//! Defines the contract for relay messaging and contact discovery.
//! Implemented by hub module for cross-engine communication.

use mad_hatter::tunnel_service;
use crate::persist::hooks::ContactInfo;

/// Extension handler for cross-instance operations.
///
/// All methods are async. The `#[tunnel_service]` macro automatically
/// generates Proxy (for tunnel RPC) and Dispatcher (for local dispatch).
#[tunnel_service]
pub trait ExtensionHandler: Send + Sync {
    /// Relay a message from one instance to another.
    /// Returns Ok(()) on success, Err with description on failure.
    async fn relay_message(
        &self,
        from_instance_id: String,
        to_instance_id: String,
        content: String,
    ) -> Result<(), String>;

    /// Fetch the list of contactable instances for the given instance.
    /// Returns contacts excluding the requesting instance itself.
    async fn fetch_contacts(&self, instance_id: String) -> Result<Vec<ContactInfo>, String>;
}

/// No-op implementation for testing and when no extension is configured.
pub struct NoopExtensionHandler;

#[async_trait::async_trait]
impl ExtensionHandler for NoopExtensionHandler {
    async fn relay_message(
        &self,
        _from_instance_id: String,
        _to_instance_id: String,
        _content: String,
    ) -> Result<(), String> {
        Err("No extension handler configured".to_string())
    }

    async fn fetch_contacts(&self, _instance_id: String) -> Result<Vec<ContactInfo>, String> {
        Ok(Vec::new())
    }
}