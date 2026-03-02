//! SignalHub — in-memory signal mechanism for inter-thread communication.
//!
//! Replaces file-based signals (interrupt.signal, switch-model.signal)
//! with memory variables shared between RPC handlers and engine threads.
//! Also holds per-instance engine status for RPC observe queries.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, atomic::{AtomicBool, Ordering}};
use std::sync::Mutex;
use std::time::Instant;

/// Runtime status of an engine instance, updated by engine thread, read by RPC.
pub struct EngineStatus {
    pub inferring: bool,
    pub born: bool,
    pub last_beat: Instant,
    pub log_path: Option<String>,
    pub active_model: usize,
    pub model_count: usize,
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
            active_model: 0,
            model_count: 1,
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
    /// Switch-model signals per instance (Some(index) = switch requested).
    switch_models: Arc<RwLock<HashMap<String, Arc<Mutex<Option<usize>>>>>>,
    /// Engine status per instance, updated by engine thread, read by RPC.
    statuses: Arc<RwLock<HashMap<String, Arc<RwLock<EngineStatus>>>>>,
}

impl SignalHub {
    pub fn new() -> Self {
        Self {
            interrupts: Arc::new(RwLock::new(HashMap::new())),
            switch_models: Arc::new(RwLock::new(HashMap::new())),
            statuses: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an instance and return its signal handles.
    /// Called by engine when creating an Alice instance.
    pub fn register(&self, instance_id: &str) -> InstanceSignals {
        let interrupt = Arc::new(AtomicBool::new(false));
        let switch_model = Arc::new(Mutex::new(None));
        let status = Arc::new(RwLock::new(EngineStatus::default()));

        {
            let mut map = self.interrupts.write().unwrap();
            map.insert(instance_id.to_string(), interrupt.clone());
        }
        {
            let mut map = self.switch_models.write().unwrap();
            map.insert(instance_id.to_string(), switch_model.clone());
        }
        {
            let mut map = self.statuses.write().unwrap();
            map.insert(instance_id.to_string(), status.clone());
        }

        InstanceSignals { interrupt, switch_model, status }
    }

    /// Unregister an instance (cleanup on deletion).
    pub fn unregister(&self, instance_id: &str) {
        self.interrupts.write().unwrap().remove(instance_id);
        self.switch_models.write().unwrap().remove(instance_id);
        self.statuses.write().unwrap().remove(instance_id);
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

    /// Set switch-model signal for an instance (called by RPC handler).
    pub fn set_switch_model(&self, instance_id: &str, model_index: usize) -> bool {
        let map = self.switch_models.read().unwrap();
        if let Some(slot) = map.get(instance_id) {
            let mut guard = slot.lock().unwrap();
            *guard = Some(model_index);
            true
        } else {
            false
        }
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
                active_model: guard.active_model,
                model_count: guard.model_count,
                idle_timeout_secs: guard.idle_timeout_secs,
                idle_since: guard.idle_since,
            }
        })
    }
}

/// Signal handles for a single instance, held by Alice struct.
pub struct InstanceSignals {
    /// Interrupt flag: true = interrupt requested.
    pub interrupt: Arc<AtomicBool>,
    /// Switch-model slot: Some(index) = switch requested.
    pub switch_model: Arc<Mutex<Option<usize>>>,
    /// Engine status, updated by engine thread.
    pub status: Arc<RwLock<EngineStatus>>,
}

impl InstanceSignals {
    /// Check and clear interrupt signal. Returns true if interrupt was requested.
    pub fn check_interrupt(&self) -> bool {
        self.interrupt.swap(false, Ordering::Relaxed)
    }

    /// Check and take switch-model signal. Returns Some(index) if switch was requested.
    pub fn take_switch_model(&self) -> Option<usize> {
        let mut guard = self.switch_model.lock().unwrap();
        guard.take()
    }

    /// Update engine status (called by engine thread).
    pub fn update_status(&self, f: impl FnOnce(&mut EngineStatus)) {
        let mut guard = self.status.write().unwrap();
        f(&mut guard);
    }
}
