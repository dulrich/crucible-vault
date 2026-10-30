//! The runner's time source: the [`Clock`] trait, the wall clock and a settable test clock.

use std::sync::atomic::{AtomicI64, Ordering};

/// Time source (tests inject [`ManualClock`]).
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
    /// Local UTC offset in ms at `now_ms` (daily schedules).
    fn local_offset_ms(&self, now_ms: i64) -> i64;
}

/// The wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        crate::now_ms()
    }
    fn local_offset_ms(&self, now_ms: i64) -> i64 {
        use chrono::{Local, Offset, TimeZone};
        Local
            .timestamp_millis_opt(now_ms)
            .single()
            .map_or(0, |t| i64::from(t.offset().fix().local_minus_utc()) * 1000)
    }
}

/// A settable clock (UTC).
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    /// Starts at `ms`.
    pub fn new(ms: i64) -> Self {
        Self(AtomicI64::new(ms))
    }
    /// Sets the time.
    pub fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn local_offset_ms(&self, _: i64) -> i64 {
        0
    }
}
