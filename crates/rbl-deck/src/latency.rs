//! How long a mixer control takes to be heard: from the moment a setter
//! stores a value to the start of the first audio callback that renders it.
//!
//! Realtime: the callback swaps three atomics and, only when one was set,
//! reads the clock once. No lock, no allocation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Nanoseconds since the first call, never 0 (0 means "nothing pending").
fn now_ns() -> u64 {
    let ns = EPOCH.get_or_init(Instant::now).elapsed().as_nanos();
    u64::try_from(ns).unwrap_or(u64::MAX).max(1)
}

/// The oldest change to a control that no callback has rendered yet.
#[derive(Debug, Default)]
pub(crate) struct Stamp(AtomicU64);

impl Stamp {
    /// After the value is stored: the first unrendered change keeps its time,
    /// so a burst of moves between two callbacks measures the one that waited
    /// longest.
    pub(crate) fn mark(&self) {
        let _ = self.0.compare_exchange(0, now_ns(), Ordering::Release, Ordering::Relaxed);
    }

    /// Before the value is read. Acquire pairs with `mark`'s release, so the
    /// value the callback reads next is at least the one that was stamped.
    pub(crate) fn take(&self) -> u64 {
        self.0.swap(0, Ordering::Acquire)
    }
}

/// What has been measured so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlLatency {
    pub last: Duration,
    pub max: Duration,
    /// Changes measured.
    pub count: u64,
}

#[derive(Debug, Default)]
pub(crate) struct LatencyMeter {
    last: AtomicU64,
    max: AtomicU64,
    count: AtomicU64,
}

impl LatencyMeter {
    /// One writer: the audio callback. `stamps` are what `Stamp::take` gave.
    pub(crate) fn observe(&self, stamps: &[u64]) {
        let Some(oldest) = stamps.iter().copied().filter(|&t| t != 0).min() else { return };
        let took = now_ns().saturating_sub(oldest);
        self.last.store(took, Ordering::Relaxed);
        self.max.fetch_max(took, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> ControlLatency {
        ControlLatency {
            last: Duration::from_nanos(self.last.load(Ordering::Relaxed)),
            max: Duration::from_nanos(self.max.load(Ordering::Relaxed)),
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_is_measured_once_from_its_first_mark() {
        let stamp = Stamp::default();
        let meter = LatencyMeter::default();
        meter.observe(&[stamp.take()]);
        assert_eq!(meter.snapshot().count, 0, "nothing changed, nothing measured");

        stamp.mark();
        std::thread::sleep(Duration::from_millis(5));
        stamp.mark();
        meter.observe(&[stamp.take(), 0]);
        let seen = meter.snapshot();
        assert_eq!(seen.count, 1);
        assert!(seen.last >= Duration::from_millis(5), "the second mark must not reset the first: {seen:?}");
        assert_eq!(seen.max, seen.last);

        meter.observe(&[stamp.take()]);
        assert_eq!(meter.snapshot().count, 1, "a taken stamp is gone");
    }
}
