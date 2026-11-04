//! A signal that ends a running platform operation early.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A cheap, cloneable flag that running work polls to stop early.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
