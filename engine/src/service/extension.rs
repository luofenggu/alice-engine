//! Extension handler trait — engine extension interface for cross-instance communication.
//!
//! Defines the contract for relay messaging and contact discovery.
//! Implemented by hub module for cross-engine communication.

use tracing::info;
use crate::persist::hooks::ContactInfo;

/// Extension handler for cross-instance operations.
///
/// All methods are synchronous. Implementations that need async
/// should use `tokio::task::block_in_place` + `Handle::current().block_on()`.
pub trait ExtensionHandler: Send + Sync {
    /// Relay a message from one instance to another.
    /// Returns Ok(()) on success, Err with description on failure.
    fn relay_message(
        &self,
        from_instance_id: &str,
        to_instance_id: &str,
        content: &str,
    ) -> Result<(), String>;

    /// Fetch the list of contactable instances for the given instance.
    /// Returns contacts excluding the requesting instance itself.
    fn fetch_contacts(&self, instance_id: &str) -> Vec<ContactInfo>;

    /// Format contacts into a prompt-friendly string.
    fn format_contacts_for_prompt(&self, instance_id: &str) -> String {
        let contacts = self.fetch_contacts(instance_id);
        info!("[EXT] format_contacts: fetch OK, {} contacts", contacts.len());
        crate::persist::hooks::format_contacts_list(&contacts)
    }
}
/// No-op implementation for testing and when no extension is configured.
pub struct NoopExtensionHandler;

impl ExtensionHandler for NoopExtensionHandler {
    fn relay_message(
        &self,
        _from_instance_id: &str,
        _to_instance_id: &str,
        _content: &str,
    ) -> Result<(), String> {
        Err("No extension handler configured".to_string())
    }

    fn fetch_contacts(&self, _instance_id: &str) -> Vec<ContactInfo> {
        Vec::new()
    }
}
