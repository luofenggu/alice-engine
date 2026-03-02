//! SignalHub — in-memory signal mechanism for inter-thread communication.
//!
//! Replaces file-based signals (interrupt.signal, switch-model.signal)
//! with memory variables shared between RPC handlers and engine threads.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, atomic::{AtomicBool, Ordering}};
use std::sync::Mutex;

/// Hub for all instance signals, shared between RPC and engine threads.
///
/// RPC handlers set signals; engine/instance threads read and clear them.
#[derive(Clone)]
pub struct SignalHub {
    /// Interrupt signals per instance (true = interrupt requested).
    interrupts: Arc<RwLock<HashMap<String, Arc<AtomicBool>>>>,
    /// Switch-model signals per instance (Some(index) = switch requested).
    switch_models: Arc<RwLock<HashMap<String, Arc<Mutex<Option<usize>>>>>>,
}

impl SignalHub {
    pub fn new() -> Self {
        Self {
            interrupts: Arc::new(RwLock::new(HashMap::new())),
            switch_models: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register an instance and return its signal handles.
    /// Called by engine when creating an Alice instance.
    pub fn register(&self, instance_id: &str) -> InstanceSignals {
        let interrupt = Arc::new(AtomicBool::new(false));
        let switch_model = Arc::new(Mutex::new(None));

        {
            let mut map = self.interrupts.write().unwrap();
            map.insert(instance_id.to_string(), interrupt.clone());
        }
        {
            let mut map = self.switch_models.write().unwrap();
            map.insert(instance_id.to_string(), switch_model.clone());
        }

        InstanceSignals { interrupt, switch_model }
    }

    /// Unregister an instance (cleanup on deletion).
    pub fn unregister(&self, instance_id: &str) {
        self.interrupts.write().unwrap().remove(instance_id);
        self.switch_models.write().unwrap().remove(instance_id);
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
}

/// Signal handles for a single instance, held by Alice struct.
pub struct InstanceSignals {
    /// Interrupt flag: true = interrupt requested.
    pub interrupt: Arc<AtomicBool>,
    /// Switch-model slot: Some(index) = switch requested.
    pub switch_model: Arc<Mutex<Option<usize>>>,
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
}
