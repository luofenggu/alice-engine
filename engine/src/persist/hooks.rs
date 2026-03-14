//! # Hook Types
//!
//! Data types for cross-instance communication (contacts, relay).
//! The actual extension logic lives in `service::extension::ExtensionHandler`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// API types for hook responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}



/// Format a contacts list into a prompt-friendly string.
/// Pure function, used by ExtensionHandler default implementation.
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
