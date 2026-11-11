//! Daemon memory instrumentation (plan memory-instrumentation-and-reduction WP-M1): one
//! `memstat::Registry` with the daemon's gauges, a 5-minute `cv::memory` journal line, the
//! editor's latest `memory_report`, and `status.memory`. The journal is the only history; nothing
//! is persisted elsewhere. No gauge or read may panic.
//!
//! It also returns freed heap to the OS (plan memory-release-strategy WP-1; spike record
//! `docs/spikes/memory-release-strategy/FINDINGS.md`) from three triggers, all through
//! [`release_after_burst`]: the end of a job, [`IDLE_RELEASE_AFTER`] of quiet (no request, no
//! running job), and the [`SAMPLE_EVERY`] tick as a backstop for sessions that never go idle.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use cv_protocol::{MemoryReport, MemoryStatus, ReportedMemory};
use memstat::{GaugeKind, Registry};
use tokio::sync::watch;

use super::Shared;
use crate::sync::lock;

/// Journal sampling period.
const SAMPLE_EVERY: Duration = Duration::from_secs(300);
/// An editor report older than this is not journaled.
const EDITOR_FRESH: Duration = Duration::from_secs(600);

/// Quiet time (no request, no running job) after which freed heap is released once.
const IDLE_RELEASE_AFTER: Duration = Duration::from_secs(20);
/// How often the idle task checks; the release need not fire at exactly [`IDLE_RELEASE_AFTER`].
const IDLE_POLL: Duration = Duration::from_secs(5);

/// Free-resident bytes above which an idle or sampler release trims (16 MiB). Once replies stopped
/// building a `Value` tree, about 22 MiB of free memory remains after a burst, under the 32 MiB
/// job threshold, so the idle release never trimmed; 16 MiB stays above the 6-9 MiB that remains
/// after a trim, so the 5-minute sampler tick cannot re-trim a settled daemon (plan
/// memory-release-strategy WP-7).
const QUIET_TRIM_THRESHOLD: u64 = 16 * 1024 * 1024;

/// Why a release was attempted (the debug log label).
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReleaseReason {
    /// A job's steps finished.
    Job,
    /// The daemon has been quiet for [`IDLE_RELEASE_AFTER`].
    Idle,
    /// The [`SAMPLE_EVERY`] tick (sessions that never go idle).
    Sampler,
}

impl ReleaseReason {
    /// The free-resident threshold this trigger trims above: a job's end keeps the shared
    /// default, the quiet-time triggers use [`QUIET_TRIM_THRESHOLD`].
    fn threshold(self) -> u64 {
        match self {
            Self::Job => cv_malloc::TRIM_RETAINED_THRESHOLD,
            Self::Idle | Self::Sampler => QUIET_TRIM_THRESHOLD,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Job => "trim after job",
            Self::Idle => "trim after idle",
            Self::Sampler => "trim on sampler tick",
        }
    }
}

/// Returns freed heap pages to the OS, the one release path for all three triggers (a job's end,
/// [`IDLE_RELEASE_AFTER`] of quiet, the sampler tick; plan memory-release-strategy WP-1): judged
/// on the free-resident figure (anonymous RSS minus live allocator bytes), which falls after a
/// trim, so the threshold alone rate-limits it (per [`ReleaseReason::threshold`]) (`cv_malloc::Figure`). Runs on the blocking pool,
/// never a worker.
pub(crate) fn release_after_burst(reason: ReleaseReason) {
    if let Some(r) = cv_malloc::release_above(cv_malloc::Figure::FreeResident, reason.threshold()) {
        tracing::debug!(
            target: "cv::memory",
            free_resident = r.figure,
            trimmed = r.trimmed,
            "{}",
            reason.label()
        );
    }
}

/// Milliseconds since the first activity mark or read (a process-local monotonic epoch).
fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);
static RUNNING_JOBS: AtomicUsize = AtomicUsize::new(0);

/// Marks request or job activity (one relaxed store; safe on the request path).
pub(crate) fn mark_activity() {
    LAST_ACTIVITY_MS.store(now_ms(), Ordering::Relaxed);
}

/// Counts one job as running while alive; dropping it (also on unwind, so a panicking step is
/// not leaked) decrements the count and marks activity.
pub(crate) struct RunningJob<'a>(&'a AtomicUsize);

impl<'a> RunningJob<'a> {
    fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for RunningJob<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
        mark_activity();
    }
}

/// Guards the running-job count for the daemon (create it inside the blocking closure).
pub(crate) fn running_job() -> RunningJob<'static> {
    mark_activity();
    RunningJob::enter(&RUNNING_JOBS)
}

/// Pure idle decision: release only when quiet for [`IDLE_RELEASE_AFTER`], no job runs, and this
/// idle period has not released yet.
fn idle_release_due(quiet: Duration, running: usize, released: bool) -> bool {
    quiet >= IDLE_RELEASE_AFTER && running == 0 && !released
}

/// Per-idle-period state: re-armed whenever the activity stamp changes.
#[derive(Debug, Default)]
struct IdleGate {
    seen_ms: u64,
    released: bool,
}

impl IdleGate {
    /// `true` when a release is due now; records it so the period releases once.
    fn poll(&mut self, last_ms: u64, now_ms: u64, running: usize) -> bool {
        if last_ms != self.seen_ms {
            self.seen_ms = last_ms;
            self.released = false;
        }
        let quiet = Duration::from_millis(now_ms.saturating_sub(last_ms));
        let due = idle_release_due(quiet, running, self.released);
        if due {
            self.released = true;
        }
        due
    }
}

/// Releases freed heap once per idle period (see [`IDLE_RELEASE_AFTER`]); the trim runs on the
/// blocking pool.
pub(super) async fn idle_release(mut stop: watch::Receiver<bool>) {
    let mut gate = IdleGate::default();
    let mut tick = tokio::time::interval(IDLE_POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.changed() => return,
        }
        let running = RUNNING_JOBS.load(Ordering::SeqCst);
        if gate.poll(LAST_ACTIVITY_MS.load(Ordering::Relaxed), now_ms(), running)
            && let Err(e) =
                tokio::task::spawn_blocking(|| release_after_burst(ReleaseReason::Idle)).await
        {
            tracing::warn!(target: "cv::memory", error = %e, "idle release task");
        }
    }
}

/// The daemon's registry plus the latest client report.
pub(super) struct Memory {
    registry: Arc<Registry>,
    editor: Mutex<Option<(MemoryReport, Instant)>>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory").finish_non_exhaustive()
    }
}

impl Memory {
    /// Created at startup (the registry's start time stands in for process start).
    pub(super) fn new() -> Self {
        Self {
            registry: Arc::new(Registry::new()),
            editor: Mutex::new(None),
        }
    }

    /// Stores `report` as the editor's latest, replacing the previous one.
    pub(super) fn set_editor(&self, report: MemoryReport) {
        *lock(&self.editor) = Some((report, Instant::now()));
    }

    fn editor(&self) -> Option<(MemoryReport, Duration)> {
        lock(&self.editor)
            .as_ref()
            .map(|(r, at)| (r.clone(), at.elapsed()))
    }
}

/// Registers the daemon gauges; each holds only a `Weak` to the shared state.
pub(super) fn register_gauges(shared: &Arc<Shared>) {
    let reg = &shared.memory.registry;
    let mut errs = Vec::new();
    let mut add =
        |name: &str, kind: GaugeKind, f: Box<dyn Fn() -> memstat::GaugeResult + Send + Sync>| {
            if let Err(e) = reg.register(name, kind, f) {
                errs.push(format!("{name}: {e}"));
            }
        };
    add(
        "malloc_in_use_bytes",
        GaugeKind::Bytes,
        Box::new(|| Ok(cv_malloc::info().in_use)),
    );
    add(
        // Renamed from `malloc_retained_bytes` (`system - in_use`, which never fell after a trim):
        // now anon RSS minus live bytes, so it drops when `malloc_trim` returns pages.
        "malloc_free_resident_bytes",
        GaugeKind::Bytes,
        Box::new(|| {
            cv_malloc::current_free_resident().ok_or_else(|| "status unreadable".to_owned())
        }),
    );
    let w = Arc::downgrade(shared);
    add(
        "search_index_chunks",
        GaugeKind::Count,
        Box::new(move || {
            let s = upgrade(&w)?;
            let runner = s.runner().ok_or("runner not started")?;
            let index = runner.search_db().if_open().ok_or("not open")?;
            let n = match index.try_lock() {
                Ok(i) => i.embedded_count(),
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().embedded_count(),
                Err(std::sync::TryLockError::WouldBlock) => return Err("index busy".into()),
            };
            u64::try_from(n).map_err(|e| e.to_string())
        }),
    );
    let w = Arc::downgrade(shared);
    add(
        "resolver_entries",
        GaugeKind::Count,
        Box::new(move || {
            let writer = upgrade(&w)?.writer().ok_or("writer stopped")?;
            let n = writer
                .call(|v| v.index().resolver().len())
                .map_err(|e| e.to_string())?;
            u64::try_from(n).map_err(|e| e.to_string())
        }),
    );
    let w = Arc::downgrade(shared);
    add(
        "queue_depth",
        GaugeKind::Count,
        Box::new(move || {
            let s = upgrade(&w)?;
            let counts = lock(&s.state).counts().map_err(|e| e.to_string())?;
            Ok(counts
                .iter()
                .filter(|c| !c.status.is_terminal())
                .map(|c| u64::try_from(c.count).unwrap_or(0))
                .sum())
        }),
    );
    for e in errs {
        tracing::warn!(target: "cv::memory", error = %e, "registering gauge");
    }
}

fn upgrade(w: &Weak<Shared>) -> Result<Arc<Shared>, String> {
    w.upgrade().ok_or_else(|| "daemon stopped".to_owned())
}

impl Shared {
    /// Takes the daemon snapshot off the async threads (the resolver gauge waits on the writer).
    async fn daemon_report(&self) -> MemoryReport {
        let reg = Arc::clone(&self.memory.registry);
        match tokio::task::spawn_blocking(move || (reg.snapshot(), cv_malloc::info())).await {
            Ok((snap, malloc)) => cv_malloc::to_report(&snap, malloc),
            Err(e) => {
                tracing::warn!(target: "cv::memory", error = %e, "memory snapshot task");
                MemoryReport::default()
            }
        }
    }

    /// `status.memory`.
    pub(super) async fn memory_status(&self) -> MemoryStatus {
        MemoryStatus {
            daemon: self.daemon_report().await,
            editor: self.memory.editor().map(|(report, age)| ReportedMemory {
                report,
                age_secs: age.as_secs(),
            }),
        }
    }

    /// `memory_report`.
    pub(super) fn memory_report(&self, report: MemoryReport) {
        self.memory.set_editor(report);
    }
}

/// Logs one `cv::memory` line every [`SAMPLE_EVERY`] (first tick skipped), plus an editor line
/// when its report is fresh.
pub(super) async fn memory_logger(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(SAMPLE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    let mut baseline: Option<u64> = None;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.changed() => return,
        }
        if let Err(e) =
            tokio::task::spawn_blocking(|| release_after_burst(ReleaseReason::Sampler)).await
        {
            tracing::warn!(target: "cv::memory", error = %e, "sampler release task");
        }
        let r = shared.daemon_report().await;
        let total = r.rss.unwrap_or(0).saturating_add(r.swap.unwrap_or(0));
        let base = *baseline.get_or_insert(total);
        let delta = i128::from(total) - i128::from(base);
        log_report("daemon", &r, Some(delta), None);
        if let Some((e, age)) = shared.memory.editor()
            && age <= EDITOR_FRESH
        {
            log_report("editor", &e, None, Some(age.as_secs()));
        }
    }
}

fn log_report(process: &str, r: &MemoryReport, delta: Option<i128>, age_secs: Option<u64>) {
    let free_resident = r
        .anon
        .zip(r.malloc_in_use)
        .map(|(a, u)| cv_malloc::free_resident(a, u));
    let gauges: Vec<String> = r
        .gauges
        .iter()
        .map(|g| match (&g.value, &g.error) {
            (Some(v), _) => format!("{}={v}", g.name),
            (None, Some(e)) => format!("{}=err({e})", g.name),
            (None, None) => format!("{}=?", g.name),
        })
        .collect();
    tracing::info!(
        target: "cv::memory",
        process,
        rss = ?r.rss,
        anon = ?r.anon,
        swap = ?r.swap,
        malloc_in_use = ?r.malloc_in_use,
        malloc_free_resident = ?free_resident,
        hwm = ?r.hwm,
        delta_rss_swap_since_first = ?delta,
        age_secs = ?age_secs,
        uptime_secs = r.uptime_secs,
        gauges = %gauges.join(" "),
        "memory"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn each_reason_uses_its_threshold_strictly() {
        let mib = 1024 * 1024;
        assert_eq!(ReleaseReason::Job.threshold(), 32 * mib);
        assert_eq!(ReleaseReason::Idle.threshold(), 16 * mib);
        assert_eq!(ReleaseReason::Sampler.threshold(), 16 * mib);
        for r in [
            ReleaseReason::Job,
            ReleaseReason::Idle,
            ReleaseReason::Sampler,
        ] {
            let t = r.threshold();
            assert!(!cv_malloc::above(t, t), "{r:?} at the boundary");
            assert!(cv_malloc::above(t + 1, t), "{r:?} just above");
        }
        // A settled free-resident figure (6-9 MiB) never re-trims.
        assert!(!cv_malloc::above(9 * mib, QUIET_TRIM_THRESHOLD));
    }

    #[test]
    fn not_yet_quiet_does_not_release() {
        assert!(!idle_release_due(IDLE_RELEASE_AFTER - S, 0, false));
    }

    #[test]
    fn quiet_with_a_running_job_does_not_release() {
        assert!(!idle_release_due(IDLE_RELEASE_AFTER, 1, false));
    }

    #[test]
    fn quiet_and_idle_releases() {
        assert!(idle_release_due(IDLE_RELEASE_AFTER, 0, false));
    }

    #[test]
    fn already_released_this_period_does_not_release() {
        let mut g = IdleGate::default();
        assert!(g.poll(1_000, 1_000 + 20_000, 0));
        assert!(!g.poll(1_000, 1_000 + 25_000, 0));
    }

    #[test]
    fn new_activity_rearms() {
        let mut g = IdleGate::default();
        assert!(g.poll(1_000, 21_000, 0));
        assert!(!g.poll(30_000, 31_000, 0));
        assert!(g.poll(30_000, 50_000, 0));
    }

    #[test]
    fn running_job_count_recovers_after_a_panic() {
        let counter = AtomicUsize::new(0);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = RunningJob::enter(&counter);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
            panic!("step panicked");
        }));
        assert!(r.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
}
