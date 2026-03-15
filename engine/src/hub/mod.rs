pub mod tunnel;
pub mod host;
pub mod slave;
pub mod extension_impl;

use std::sync::Arc;
use tokio::sync::RwLock;
use sha2::{Sha256, Digest};
use tracing::info;
use crate::bindings::i18n;
use crate::persist::instance::InstanceStore;

use crate::hub::host::HostState;
use crate::hub::slave::SlaveState;
use crate::hub::tunnel::TunnelInstanceInfo;

/// Hub operating mode
pub enum HubMode {
    /// Hub is off - normal standalone engine
    Off,
    /// This engine is a host - accepts slave connections
    Host(Arc<HostState>),
    /// This engine is a slave - connected to a host
    Joined(Arc<SlaveState>),
}

impl std::fmt::Debug for HubMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HubMode::Off => write!(f, "Off"),
            HubMode::Host(_) => write!(f, "Host"),
            HubMode::Joined(_) => write!(f, "Joined"),
        }
    }
}

/// Hub state: manages the current mode and transitions
pub struct HubState {
    mode: RwLock<HubMode>,
}

impl std::fmt::Debug for HubState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HubState")
    }
}

impl HubState {
    pub fn new() -> Self {
        Self {
            mode: RwLock::new(HubMode::Off),
        }
    }

    /// Enable host mode with a join token (room password)
    pub async fn enable_host(&self, join_token: String, instance_store: crate::persist::instance::InstanceStore) -> Result<(), String> {
        if join_token.is_empty() {
            return Err(i18n::hub_join_token_empty());
        }
        let mut mode = self.mode.write().await;
        match &*mode {
            HubMode::Off => {
                let host = Arc::new(HostState::new(join_token, instance_store));
                info!("[HUB] Host mode enabled");
                *mode = HubMode::Host(host);
                Ok(())
            }
            HubMode::Host(_) => Err(i18n::hub_already_host()),
            HubMode::Joined(_) => Err(i18n::hub_joined_leave_first()),
        }
    }

    /// Disable host mode
    pub async fn disable_host(&self) -> Result<(), String> {
        let mut mode = self.mode.write().await;
        match &*mode {
            HubMode::Host(_) => {
                info!("[HUB] Host mode disabled");
                *mode = HubMode::Off;
                Ok(())
            }
            _ => Err(i18n::hub_not_host()),
        }
    }

    /// Join a host as a slave
    /// Now synchronously waits for connection + token verification before returning
    pub async fn join_host(
        &self,
        host_url: String,
        join_token: String,
        instances: Vec<TunnelInstanceInfo>,
        engine_id: &str,
        local_port: u16,
        auth_token: String,
        instance_store: InstanceStore,
    ) -> Result<(), String> {
        // 1. Check current mode (read lock, released immediately)
        {
            let mode = self.mode.read().await;
            match &*mode {
                HubMode::Off => {},
                HubMode::Host(_) => return Err(i18n::hub_host_disable_first()),
                HubMode::Joined(_) => return Err(i18n::hub_already_joined()),
            }
        }

        // 2. Create slave and attempt connection (no lock held — may take seconds)
        let slave = Arc::new(SlaveState::new(
            host_url.clone(),
            local_port,
            auth_token,
            join_token.clone(),
            engine_id.to_string(),
            instance_store,
        ));
        slave.connect(instances, engine_id, &join_token).await?;

        // 3. Connection succeeded — set mode (write lock, re-check for concurrent changes)
        let mut mode = self.mode.write().await;
        match &*mode {
            HubMode::Off => {
                *mode = HubMode::Joined(slave);
                info!("[HUB] Joined host at {}", host_url);
                Ok(())
            }
            _ => Err(i18n::hub_mode_changed()),
        }
    }

    /// Leave the current host (sends Leave message before disconnecting)
    pub async fn leave_host(&self) -> Result<(), String> {
        let slave = {
            let mut mode = self.mode.write().await;
            match std::mem::replace(&mut *mode, HubMode::Off) {
                HubMode::Joined(slave) => {
                    slave.stop_reconnect();
                    info!("[HUB] Left host");
                    slave
                }
                other => {
                    *mode = other;
                    return Err(i18n::hub_not_joined());
                }
            }
        };
        // Send Leave message gracefully (lock released, won't block other operations)
        slave.disconnect().await;
        Ok(())
    }

    /// Notify host of instance list changes (called after create/delete instance)
    pub async fn notify_instances_changed(&self, instances: Vec<TunnelInstanceInfo>) {
        let mode = self.mode.read().await;
        if let HubMode::Joined(slave) = &*mode {
            slave.send_instances_update(instances).await;
        }
    }

    /// Get current mode status
    pub async fn status(&self) -> HubStatus {
        let mode = self.mode.read().await;
        match &*mode {
            HubMode::Off => HubStatus {
                mode: "off".to_string(),
                detail: None,
            },
            HubMode::Host(host) => {
                let count = host.slave_count().await;
                HubStatus {
                    mode: "host".to_string(),
                    detail: Some(i18n::hub_slaves_connected(count)),
                }
            }
            HubMode::Joined(slave) => HubStatus {
                mode: "joined".to_string(),
                detail: Some(i18n::hub_connected_to(&slave.host_url)),
            },
        }
    }

    /// Get host state if in host mode
    pub async fn as_host(&self) -> Option<Arc<HostState>> {
        let mode = self.mode.read().await;
        match &*mode {
            HubMode::Host(host) => Some(host.clone()),
            _ => None,
        }
    }

    /// Get slave state if in joined mode
    pub async fn as_slave(&self) -> Option<Arc<SlaveState>> {
        let mode = self.mode.read().await;
        match &*mode {
            HubMode::Joined(slave) => Some(slave.clone()),
            _ => None,
        }
    }
}

#[derive(serde::Serialize)]
pub struct HubStatus {
    pub mode: String,
    pub detail: Option<String>,
}

/// Compute session token from auth secret (SHA256 hex)
pub fn compute_session_token(auth_secret: &str) -> String {
    hex::encode(Sha256::digest(auth_secret.as_bytes()))
}

