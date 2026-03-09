pub mod tunnel;
pub mod host;
pub mod slave;
pub mod hooks;

use std::sync::Arc;
use tokio::sync::RwLock;
use sha2::{Sha256, Digest};
use tracing::info;

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
    pub async fn enable_host(&self, join_token: String, local_port: u16, auth_secret: String) -> Result<(), String> {
        let mut mode = self.mode.write().await;
        match &*mode {
            HubMode::Off => {
                let host = Arc::new(HostState::new(join_token, local_port, auth_secret));
                info!("[HUB] Host mode enabled");
                *mode = HubMode::Host(host);
                Ok(())
            }
            HubMode::Host(_) => Err("Already in host mode".to_string()),
            HubMode::Joined(_) => Err("Currently joined to a host. Leave first.".to_string()),
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
            _ => Err("Not in host mode".to_string()),
        }
    }

    /// Join a host as a slave
    pub async fn join_host(
        &self,
        host_url: String,
        join_token: String,
        instances: Vec<TunnelInstanceInfo>,
        engine_id: &str,
        local_port: u16,
        auth_token: String,
    ) -> Result<(), String> {
        let mut mode = self.mode.write().await;
        match &*mode {
            HubMode::Off => {
                let slave = Arc::new(SlaveState::new(
                    host_url.clone(),
                    local_port,
                    auth_token,
                ));
                let slave_clone = slave.clone();
                let instances_clone = instances.clone();
                let engine_id_owned = engine_id.to_string();
                let join_token_clone = join_token.clone();
                // Spawn connection task
                tokio::spawn(async move {
                    if let Err(e) = slave_clone.connect(instances_clone, &engine_id_owned, &join_token_clone).await {
                        tracing::error!("[HUB] Slave connection failed: {}", e);
                    }
                });
                *mode = HubMode::Joined(slave);
                info!("[HUB] Joined host at {}", host_url);
                Ok(())
            }
            HubMode::Host(_) => Err("Currently in host mode. Disable first.".to_string()),
            HubMode::Joined(_) => Err("Already joined to a host. Leave first.".to_string()),
        }
    }

    /// Leave the current host
    pub async fn leave_host(&self) -> Result<(), String> {
        let mut mode = self.mode.write().await;
        match &*mode {
            HubMode::Joined(_) => {
                info!("[HUB] Left host");
                *mode = HubMode::Off;
                // SlaveState will be dropped, closing the WebSocket
                Ok(())
            }
            _ => Err("Not joined to any host".to_string()),
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
                    detail: Some(format!("{} slaves connected", count)),
                }
            }
            HubMode::Joined(slave) => HubStatus {
                mode: "joined".to_string(),
                detail: Some(format!("connected to {}", slave.host_url)),
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

