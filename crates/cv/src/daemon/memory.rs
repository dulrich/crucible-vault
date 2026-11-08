//! Daemon memory instrumentation (plan memory-instrumentation-and-reduction WP-M1): one
//! `memstat::Registry` with the daemon's gauges, a 5-minute `cv::memory` journal line, the
//! editor's latest `memory_report`, and `status.memory`. The journal is the only history; nothing
//! is persisted elsewhere. No gauge or read may panic.

use std::sync::{Arc, Mutex, Weak};
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
