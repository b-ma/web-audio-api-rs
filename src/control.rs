//! User controls for audio playback (play/pause/loop)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Atomic float, only `load` and `store` are supported, no arithmetics
#[derive(Debug)]
struct AtomicF64 {
    inner: AtomicU64,
}

impl AtomicF64 {
    pub fn new(v: f64) -> Self {
        Self {
            inner: AtomicU64::new(u64::from_ne_bytes(v.to_ne_bytes())),
        }
    }
    pub fn load(&self) -> f64 {
        f64::from_ne_bytes(self.inner.load(Ordering::SeqCst).to_ne_bytes())
    }
    pub fn store(&self, v: f64) {
        self.inner
            .store(u64::from_ne_bytes(v.to_ne_bytes()), Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atomic_f64() {
        let f = AtomicF64::new(2.0);
        assert_eq!(f.load(), 2.0);
        f.store(3.0);
        assert_eq!(f.load(), 3.0);
    }
}

/// Helper struct to start and stop audio streams
#[derive(Clone, Debug)]
pub struct Scheduler {
    start: Arc<AtomicF64>,
    stop: Arc<AtomicF64>,
}

impl Scheduler {
    /// Create a new Scheduler. Initial playback state will be: inactive.
    pub fn new() -> Self {
        Self {
            start: Arc::new(AtomicF64::new(f64::MAX)),
            stop: Arc::new(AtomicF64::new(f64::MAX)),
        }
    }

    /// Check if the stream should be active at this timestamp
    pub fn is_active(&self, ts: f64) -> bool {
        ts >= self.start.load() && ts < self.stop.load()
    }

    /// Schedule playback start at this timestamp
    pub fn start_at(&self, start: f64) {
        self.start.store(start)
    }

    /// Stop playback at this timestamp
    pub fn stop_at(&self, stop: f64) {
        self.stop.store(stop)
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper struct to control audio streams
#[derive(Clone, Debug)]
pub struct Controller {
    scheduler: Arc<Scheduler>,
    offset: Arc<AtomicF64>,
    duration: Arc<AtomicF64>,
    loop_: Arc<AtomicBool>,
    loop_start: Arc<AtomicF64>,
    loop_end: Arc<AtomicF64>,
    //playback_rate: Arc<AudioParam>,
}

impl Controller {
    /// Create a new Controller. It will not be active
    pub fn new() -> Self {
        Self {
            scheduler: Arc::new(Scheduler::new()),
            offset: Arc::new(AtomicF64::new(0.)),
            duration: Arc::new(AtomicF64::new(f64::MAX)),
            loop_: Arc::new(AtomicBool::new(false)),
            loop_start: Arc::new(AtomicF64::new(f64::MAX)),
            loop_end: Arc::new(AtomicF64::new(f64::MAX)),
            //playback_rate: ... create audio param pair
        }
    }
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}
