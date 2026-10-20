//! Editor memory reporting (plan memory-instrumentation-and-reduction WP-M2): one
//! `memstat::Registry` whose gauges read atomics, a snapshot built off the UI thread, and a
//! `memory_report` RPC on connect and every 5 minutes. The daemon journals and serves it; the
//! editor shows nothing. No gauge or report path may panic, and any send error is dropped.
//!
//! App-owned counts (tabs, shaped buffers, view-store entries, image cache) cannot be read from
//! a `Send + Sync` gauge closure, so [`Memory::capture`] stores them into [`Counts`] on the UI
//! thread just before each send (a few `len()` calls and one pass over the open buffers, every
//! 5 minutes — never per frame).

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cv_protocol::{GaugeValue, MemoryReport};
use memstat::{GaugeKind, GaugeResult, Registry, Snapshot};

/// The report period.
pub const REPORT_EVERY: Duration = Duration::from_secs(300);

/// Plan memory-instrumentation-and-reduction WP-M4: how long input and image decodes must be
/// absent before a trim may run.
pub const QUIET_PERIOD: Duration = Duration::from_secs(5);
/// The maintenance tick period (it only compares clocks; it does no rendering work).
pub const TRIM_TICK: Duration = Duration::from_secs(5);
/// Retained-but-unused heap bytes above which a quiet-period trim runs (32 MiB).
pub const TRIM_RETAINED_THRESHOLD: u64 = 32 * 1024 * 1024;

static EPOCH: OnceLock<Instant> = OnceLock::new();
/// Milliseconds since [`EPOCH`] of the last user input or completed image decode.
static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);

/// Records activity now (user input or a finished image decode). Cheap and lock-free.
pub fn touch() {
    let epoch = EPOCH.get_or_init(Instant::now);
    let ms = u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
    LAST_ACTIVITY_MS.store(ms, Ordering::Relaxed);
}

/// The instant of the last [`touch`] (the process's first clock read if none yet).
pub fn last_activity() -> Instant {
    let epoch = *EPOCH.get_or_init(Instant::now);
    epoch + Duration::from_millis(LAST_ACTIVITY_MS.load(Ordering::Relaxed))
}

/// Whether to trim now: quiet for [`QUIET_PERIOD`], retained above the threshold, and no trim
/// yet since the last activity (at most once per quiet period).
pub fn should_trim(
    now: Instant,
    last_activity: Instant,
    last_trim: Option<Instant>,
    retained: u64,
) -> bool {
    let quiet = now.saturating_duration_since(last_activity) >= QUIET_PERIOD;
    let fresh = last_trim.is_none_or(|t| t <= last_activity);
    quiet && fresh && retained > TRIM_RETAINED_THRESHOLD
}

/// App-owned counts, refreshed by [`Memory::capture`].
#[derive(Debug, Default)]
struct Counts {
    image_cache_bytes: AtomicU64,
    image_cache_entries: AtomicU64,
    open_tabs: AtomicU64,
    shaped_buffers: AtomicU64,
    view_store_entries: AtomicU64,
}

/// The editor's registry and the counts its gauges read.
pub struct Memory {
    registry: Registry,
    counts: Arc<Counts>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory").finish_non_exhaustive()
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

fn to_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl Memory {
    /// Builds the registry with the editor's gauges (a registration failure is logged, never fatal).
    pub fn new() -> Self {
        let counts = Arc::new(Counts::default());
        let registry = Registry::new();
        let add = |name: &str, kind: GaugeKind, f: Box<dyn Fn() -> GaugeResult + Send + Sync>| {
            if let Err(e) = registry.register(name, kind, f) {
                tracing::warn!(target: "cv::memory", error = %e, gauge = name, "registering gauge");
            }
        };
        add(
            "malloc_in_use_bytes",
            GaugeKind::Bytes,
            Box::new(|| Ok(cv_malloc::info().in_use)),
        );
        add(
            "malloc_retained_bytes",
            GaugeKind::Bytes,
            Box::new(|| Ok(cv_malloc::info().retained())),
        );
        let atomic = |pick: fn(&Counts) -> &AtomicU64| {
            let c = Arc::clone(&counts);
            Box::new(move || -> GaugeResult { Ok(pick(&c).load(Ordering::Relaxed)) })
                as Box<dyn Fn() -> GaugeResult + Send + Sync>
        };
        add(
            "image_cache_bytes",
            GaugeKind::Bytes,
            atomic(|c| &c.image_cache_bytes),
        );
        add(
            "image_cache_entries",
            GaugeKind::Count,
            atomic(|c| &c.image_cache_entries),
        );
        add("open_tabs", GaugeKind::Count, atomic(|c| &c.open_tabs));
        add(
            "shaped_buffers",
            GaugeKind::Count,
            atomic(|c| &c.shaped_buffers),
        );
        add(
            "view_store_entries",
            GaugeKind::Count,
            atomic(|c| &c.view_store_entries),
        );
        Self { registry, counts }
    }

    /// Stores the app-owned counts (UI thread, just before a send).
    pub fn capture(&self, app: &crate::app::App) {
        let c = &self.counts;
        let tabs: usize = app.panes.iter().map(|(_, ps)| ps.tabs.len()).sum();
        let shaped = app.docs.values().filter(|d| d.buffer.is_shaped()).count();
        let put = |a: &AtomicU64, n: usize| a.store(to_u64(n), Ordering::Relaxed);
        put(&c.image_cache_bytes, app.reading.images.used_bytes());
        put(&c.image_cache_entries, app.reading.images.len());
        put(&c.open_tabs, tabs);
        put(&c.shaped_buffers, shaped);
        put(&c.view_store_entries, app.views.entries.len());
    }

    /// Builds the report (blocking: reads `/proc`; call off the UI thread).
    pub fn report(&self) -> MemoryReport {
        to_report(&self.registry.snapshot(), cv_malloc::info())
    }
}

/// Maps a `memstat` snapshot (Serialize-only) plus `mallinfo2` onto the wire type (the same
/// mapping as the daemon's `to_report`; `cv-protocol` must not depend on `memstat`).
pub fn to_report(snap: &Snapshot, malloc: cv_malloc::MallocInfo) -> MemoryReport {
    let status = snap.status.as_ref();
    let mut gauges: Vec<GaugeValue> = snap
        .gauges
        .iter()
        .map(|g| GaugeValue {
            name: g.name.clone(),
            value: g.value,
            error: g.error.clone(),
        })
        .collect();
    if let Some(e) = &snap.registry_error {
        gauges.push(GaugeValue {
            name: "registry".into(),
            value: None,
            error: Some(e.clone()),
        });
    }
    let nonzero = |v: u64| (v > 0).then_some(v);
    MemoryReport {
        uptime_secs: if snap.uptime_secs.is_finite() && snap.uptime_secs > 0.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                snap.uptime_secs as u64
            }
        } else {
            0
        },
        rss: status.and_then(|s| s.vm_rss),
        anon: snap.smaps_rollup.as_ref().and_then(|r| r.pss_anon),
        swap: status.and_then(|s| s.vm_swap),
        hwm: status.and_then(|s| s.vm_hwm),
        malloc_in_use: nonzero(malloc.in_use),
        malloc_system: nonzero(malloc.system),
        gauges,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn trim_only_when_quiet_over_threshold_and_once() {
        let t0 = Instant::now();
        let big = TRIM_RETAINED_THRESHOLD + 1;
        let quiet = t0 + QUIET_PERIOD;
        assert!(should_trim(quiet, t0, None, big));
        assert!(!should_trim(
            quiet - Duration::from_millis(1),
            t0,
            None,
            big
        ));
        assert!(!should_trim(quiet, t0, None, TRIM_RETAINED_THRESHOLD));
        // Already trimmed this quiet period; new activity re-arms it.
        assert!(!should_trim(
            quiet,
            t0,
            Some(t0 + Duration::from_secs(6)),
            big
        ));
        let t1 = t0 + Duration::from_secs(20);
        assert!(should_trim(
            t1 + QUIET_PERIOD,
            t1,
            Some(t0 + Duration::from_secs(6)),
            big
        ));
    }

    use super::*;

    #[test]
    fn report_carries_every_gauge_and_survives_a_failing_one() {
        let m = Memory::new();
        m.registry
            .register("broken", GaugeKind::Count, || Err("boom".into()))
            .expect("register");
        let r = m.report();
        let get = |n: &str| r.gauges.iter().find(|g| g.name == n);
        for n in [
            "malloc_in_use_bytes",
            "malloc_retained_bytes",
            "image_cache_bytes",
            "image_cache_entries",
            "open_tabs",
            "shaped_buffers",
            "view_store_entries",
        ] {
            assert!(get(n).is_some_and(|g| g.value.is_some()), "{n}");
        }
        assert_eq!(get("broken").and_then(|g| g.error.as_deref()), Some("boom"));
    }
}
