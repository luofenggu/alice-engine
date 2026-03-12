//! SignalHub — in-memory signal mechanism for inter-thread communication.
//!
//! Replaces file-based signals (interrupt.signal, switch-model.signal)
//! with memory variables shared between RPC handlers and engine threads.
//! Also holds per-instance engine status for RPC observe queries.

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, RwLock,
};
use std::time::Instant;

use crate::external::llm::LlmConfig;

/// Runtime status of an engine instance, updated by engine thread, read by RPC.
pub struct EngineStatus {
    pub inferring: bool,
    pub born: bool,
    pub last_beat: Instant,
    pub log_path: Option<String>,
    pub idle_timeout_secs: Option<u64>,
    pub idle_since: Option<u64>,
}

impl Default for EngineStatus {
    fn default() -> Self {
        Self {
            inferring: false,
            born: false,
            last_beat: Instant::now(),
            log_path: None,
            idle_timeout_secs: None,
            idle_since: None,
        }
    }
}

/// Hub for all instance signals, shared between RPC and engine threads.
///
/// RPC handlers set signals; engine/instance threads read and clear them.
#[derive(Clone)]
pub struct SignalHub {
    /// Interrupt signals per instance (true = interrupt requested).
    interrupts: Arc<RwLock<HashMap<String, Arc<AtomicBool>>>>,
    /// Engine status per instance, updated by engine thread, read by RPC.
    statuses: Arc<RwLock<HashMap<String, Arc<RwLock<EngineStatus>>>>>,
    /// Per-instance channel states, shared with API layer.
    channels: Arc<RwLock<HashMap<String, ChannelState>>>,
}

impl SignalHub {
    pub fn new() -> Self {
        Self {
            interrupts: Arc::new(RwLock::new(HashMap::new())),
            statuses: Arc::new(RwLock::new(HashMap::new())),
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an instance and return its signal handles.
    /// Called by engine when creating an Alice instance.
    pub fn register(&self, instance_id: &str, channel_configs: Vec<LlmConfig>) -> InstanceSignals {
        let interrupt = Arc::new(AtomicBool::new(false));
        let status = Arc::new(RwLock::new(EngineStatus::default()));
        let configs = Arc::new(RwLock::new(channel_configs));
        let index = Arc::new(AtomicU64::new(0));
        let channel_state = ChannelState {
            configs: configs.clone(),
            index: index.clone(),
        };

        {
            let mut map = self.interrupts.write().unwrap();
            map.insert(instance_id.to_string(), interrupt.clone());
        }
        {
            let mut map = self.statuses.write().unwrap();
            map.insert(instance_id.to_string(), status.clone());
        }
        {
            let mut map = self.channels.write().unwrap();
            map.insert(instance_id.to_string(), channel_state);
        }

        InstanceSignals {
            interrupt,
            status,
            channels: ChannelState { configs, index },
        }
    }

    /// Unregister an instance (cleanup on deletion).
    pub fn unregister(&self, instance_id: &str) {
        self.interrupts.write().unwrap().remove(instance_id);
        self.statuses.write().unwrap().remove(instance_id);
        self.channels.write().unwrap().remove(instance_id);
    }

    /// Set interrupt signal for an instance (called by RPC handler).
    pub fn set_interrupt(&self, instance_id: &str) -> bool {
        let map = self.interrupts.read().unwrap();
        if let Some(flag) = map.get(instance_id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Get per-instance channel state (called by API channels handler).
    /// Returns cloned Arc references to configs and index.
    pub fn get_channel_state(&self, instance_id: &str) -> Option<ChannelState> {
        let map = self.channels.read().unwrap();
        map.get(instance_id).map(|cs| ChannelState {
            configs: cs.configs.clone(),
            index: cs.index.clone(),
        })
    }

    /// Read engine status for an instance (called by RPC observe).
    /// Returns a snapshot of the current status.
    pub fn get_status(&self, instance_id: &str) -> Option<EngineStatus> {
        let map = self.statuses.read().unwrap();
        map.get(instance_id).map(|s| {
            let guard = s.read().unwrap();
            EngineStatus {
                inferring: guard.inferring,
                born: guard.born,
                last_beat: guard.last_beat,
                log_path: guard.log_path.clone(),
                idle_timeout_secs: guard.idle_timeout_secs,
                idle_since: guard.idle_since,
            }
        })
    }
}

/// Per-instance channel state, shared between Alice and API layer.
pub struct ChannelState {
    pub configs: Arc<RwLock<Vec<LlmConfig>>>,
    pub index: Arc<AtomicU64>,
}

/// Signal handles for a single instance, held by Alice struct.
pub struct InstanceSignals {
    /// Interrupt flag: true = interrupt requested.
    pub interrupt: Arc<AtomicBool>,
    /// Engine status, updated by engine thread.
    pub status: Arc<RwLock<EngineStatus>>,
    /// Channel state for per-instance LLM channel management.
    pub channels: ChannelState,
}

/// Human-readable name for a channel index (e.g. "primary", "extra1").
pub fn channel_display_name(idx: usize) -> String {
    if idx == 0 {
        "primary".to_string()
    } else {
        format!("extra{}", idx)
    }
}

impl InstanceSignals {
    /// Check and clear interrupt signal. Returns true if interrupt was requested.
    pub fn check_interrupt(&self) -> bool {
        self.interrupt.swap(false, Ordering::Relaxed)
    }

    /// Update engine status (called by engine thread).
    pub fn update_status(&self, f: impl FnOnce(&mut EngineStatus)) {
        let mut guard = self.status.write().unwrap();
        f(&mut guard);
    }
}
