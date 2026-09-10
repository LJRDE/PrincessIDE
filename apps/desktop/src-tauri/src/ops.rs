//! Operation registry — the shared cancellation entry point behind
//! `princess:op:cancel` (contract §3, §6.3).
//!
//! Every long-running operation registers itself here, so cancellation is one
//! mechanism instead of one per feature.  P3 registers the doctor run (a real
//! child process); P2's build/run backends plug into the same registry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A registered operation.  Dropping the handle does not unregister it — call
/// [`OpRegistry::finish`] so a late `op:cancel` reports "not found" instead of
/// pretending to cancel something that already exited.
#[derive(Clone)]
pub struct OpHandle {
    pub op_id: String,
    pub kind: String,
    cancel: Arc<AtomicBool>,
}

impl OpHandle {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Request cancellation; returns true when this call flipped the flag.
    pub fn cancel(&self) -> bool {
        !self.cancel.swap(true, Ordering::SeqCst)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// A live operation was asked to stop.
    Cancelled,
    /// The operation exists but was already cancelled earlier.
    AlreadyCancelled,
    /// No live operation under that id.
    NotFound,
}

#[derive(Default)]
pub struct OpRegistry {
    inner: Mutex<HashMap<String, OpHandle>>,
    counter: AtomicU64,
}

impl OpRegistry {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()), counter: AtomicU64::new(0) }
    }

    /// Register an operation and mint an id shaped like the contract example (`op-7f3a`).
    pub fn register(&self, kind: &str) -> OpHandle {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let op_id = format!("op-{:04x}", n);
        self.register_with_id(&op_id, kind)
    }

    /// Register under an explicit id (the UI may pass its own correlation id).
    pub fn register_with_id(&self, op_id: &str, kind: &str) -> OpHandle {
        let handle = OpHandle {
            op_id: op_id.to_string(),
            kind: kind.to_string(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.inner
            .lock()
            .expect("op registry poisoned")
            .insert(op_id.to_string(), handle.clone());
        handle
    }

    pub fn finish(&self, op_id: &str) {
        self.inner.lock().expect("op registry poisoned").remove(op_id);
    }

    pub fn cancel(&self, op_id: &str) -> CancelOutcome {
        let guard = self.inner.lock().expect("op registry poisoned");
        match guard.get(op_id) {
            None => CancelOutcome::NotFound,
            Some(handle) => {
                if handle.cancel() {
                    CancelOutcome::Cancelled
                } else {
                    CancelOutcome::AlreadyCancelled
                }
            }
        }
    }

    pub fn live(&self) -> Vec<String> {
        let guard = self.inner.lock().expect("op registry poisoned");
        let mut ids: Vec<String> = guard.keys().cloned().collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_cancel_and_finish() {
        let reg = OpRegistry::new();
        let op = reg.register("tools:detect");
        assert_eq!(op.op_id, "op-0001");
        assert_eq!(op.kind, "tools:detect");
        assert!(!op.is_cancelled());
        assert_eq!(reg.live(), vec!["op-0001".to_string()]);

        assert_eq!(reg.cancel("op-0001"), CancelOutcome::Cancelled);
        assert!(op.is_cancelled());
        assert_eq!(reg.cancel("op-0001"), CancelOutcome::AlreadyCancelled);

        reg.finish("op-0001");
        assert_eq!(reg.cancel("op-0001"), CancelOutcome::NotFound);
        assert!(reg.live().is_empty());
    }

    #[test]
    fn unknown_ids_report_not_found() {
        let reg = OpRegistry::new();
        assert_eq!(reg.cancel("op-dead"), CancelOutcome::NotFound);
    }
}
