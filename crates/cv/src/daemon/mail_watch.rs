//! Maildir watch (plan `intake-followups-mail-youtube` WP-3, readiness reworked by plan round-16
//! WP-6): enqueue `track-mail` as soon as a dropped message is fully written, instead of waiting
//! for the 15-minute schedule (which stays as the backstop, untouched).
//!
//! Pattern-matched on the vault watcher (`crates/cv-vault/src/watcher.rs`): raw `notify` events
//! are only *hints*. A message path (`<maildir>/*.eml`, or any non-dot file in `<maildir>/new/`) is
//! **ready** on a writer-finished event — rename into place (`Modify(Name(To|Both))`: a browser
//! `.part` → `.eml`, mbsync `tmp/` → `new/`) or `Access(Close(Write))` — **and** when the pure
//! completeness check [`cv_ingest::mail::is_complete`] passes on its bytes (headers terminated,
//! every MIME multipart closed). Ready fires one enqueue with no added wait; an incomplete
//! message waits for its next event. There is no quiet/size-stable timer. The `track-mail` scan
//! applies the same predicate to every candidate it handles, so a complete message B never gets an
//! incomplete message A consumed. The enqueue uses the scheduler's own request
//! ([`RunRequest::schedule`]), so its dedupe key (`track-mail::schedule`) collapses a burst — and a
//! pending scheduled run — into one queued job.
//!
//! The watch is non-recursive on `<maildir>` and `<maildir>/new` (each only if it exists; a
//! missing directory is one `warn!` per (re)build, never retried until the config changes). The
//! loop re-reads `[mail]` every check, so a config reload rebuilds the watcher; daemon stop ends
//! the task with the other background tasks. It only reads candidate bytes for the completeness
//! check and never writes. No Crucible equivalent exists (Crucible had no mail intake); nothing
//! Obsidian-induced is carried.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cv_actions::pipelines::MailPipeline;
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{mpsc, watch};

use crate::runner::{RunRequest, Runner};

/// The action a settled drop enqueues.
pub(crate) const ACTION: &str = "track-mail";

/// Watch-loop timing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timings {
    /// Period of the config re-read (events are handled as they arrive, not on this tick).
    pub check: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            check: Duration::from_secs(1),
        }
    }
}

/// The maildir to watch under `cfg`, or `None` when the watch is off.
pub(crate) fn wanted(cfg: &MailPipeline) -> Option<PathBuf> {
    (cfg.enabled && cfg.watch && !cfg.maildir.trim().is_empty())
        .then(|| PathBuf::from(cfg.maildir.trim()))
}

/// Whether `p` is a message path under `root` (by name only; no read).
fn is_message(root: &Path, p: &Path) -> bool {
    let (Some(parent), Some(name)) = (p.parent(), p.file_name().and_then(|n| n.to_str())) else {
        return false;
    };
    if name.starts_with('.') {
        return false;
    }
    if parent == root {
        return Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("eml"));
    }
    parent == root.join("new")
}

/// The path an event says a writer just finished with, if any: rename into place
/// (`Name(To)`; `Name(Both)` carries `[from, to]`) or close-after-write.
fn finished_paths(ev: &notify::Event) -> Vec<&Path> {
    use notify::event::{AccessKind, AccessMode, ModifyKind, RenameMode};
    match &ev.kind {
        EventKind::Modify(ModifyKind::Name(RenameMode::To))
        | EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
            ev.paths.iter().map(PathBuf::as_path).collect()
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            ev.paths.last().map(PathBuf::as_path).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

/// Whether `ev` makes some message under `root` ready: a writer-finished event on a message path
/// whose bytes pass [`cv_ingest::mail::is_complete`].
fn ready(root: &Path, ev: &notify::Event) -> bool {
    finished_paths(ev).into_iter().any(|p| {
        is_message(root, p)
            && std::fs::read(p).is_ok_and(|bytes| cv_ingest::mail::is_complete(&bytes))
    })
}

type Rx = mpsc::UnboundedReceiver<notify::Result<notify::Event>>;

/// Builds the watcher on `root` and `root/new` (whichever exist).
fn build(root: &Path) -> Option<(notify::RecommendedWatcher, Rx)> {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut w = match notify::recommended_watcher(move |ev| {
        let _ = tx.send(ev);
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "mail watch: watcher unavailable");
            return None;
        }
    };
    let mut any = false;
    for dir in [root.to_path_buf(), root.join("new")] {
        if !dir.is_dir() {
            tracing::warn!(dir = %dir.display(), "mail watch: directory missing, not watched");
            continue;
        }
        match w.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => any = true,
            Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "mail watch: watch failed"),
        }
    }
    any.then_some((w, rx))
}

/// Awaits the next watcher event (never resolves while no watcher is active).
async fn next_event(
    active: &mut Option<(notify::RecommendedWatcher, Rx)>,
) -> Option<notify::Result<notify::Event>> {
    match active {
        Some((_, rx)) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// The watch loop: `config` yields the maildir to watch (re-read every check), `enqueue` fires
/// once per batch of events that made a message ready.
pub(crate) async fn run(
    config: impl Fn() -> Option<PathBuf> + Send + 'static,
    enqueue: impl Fn() + Send + 'static,
    timings: Timings,
    mut stop: watch::Receiver<bool>,
) {
    let mut current: Option<PathBuf> = None;
    let mut active: Option<(notify::RecommendedWatcher, Rx)> = None;
    let mut tick = tokio::time::interval(timings.check);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let first = tokio::select! {
            _ = stop.changed() => return,
            _ = tick.tick() => None,
            ev = next_event(&mut active) => Some(ev),
        };
        let want = config();
        if want != current {
            active = None;
            if let Some(root) = &want {
                active = build(root);
            }
            current = want;
            continue; // events from a replaced watcher are dropped with it
        }
        let Some(first) = first else { continue };
        let (Some(root), Some((_, rx))) = (&current, active.as_mut()) else {
            continue;
        };
        // One batch: the event that woke us plus whatever already queued behind it.
        let mut fire = false;
        let mut closed = false;
        let mut next = Some(first);
        while let Some(ev) = next {
            match ev {
                Some(Ok(ev)) => fire |= ready(root, &ev),
                Some(Err(_)) => {}
                None => {
                    closed = true;
                    break;
                }
            }
            next = rx.try_recv().ok().map(Some);
        }
        if closed {
            active = None; // channel closed: rebuild on the next config check
            current = None;
        }
        if fire {
            enqueue();
        }
    }
}

/// Spawns the daemon's watch over `runner`'s live `[mail]` config.
pub(crate) fn spawn(runner: Runner, stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    let cfg_runner = runner.clone();
    tokio::spawn(run(
        move || wanted(&cfg_runner.config().pipelines().mail),
        move || enqueue_track_mail(&runner),
        Timings::default(),
        stop,
    ))
}

/// Enqueues `track-mail` through the scheduler's request (dedupe `track-mail::schedule`).
fn enqueue_track_mail(runner: &Runner) {
    let config = runner.config();
    let Some(action) = config.armed(ACTION) else {
        tracing::warn!(
            action = ACTION,
            "mail watch: action not armed, drop left to the schedule"
        );
        return;
    };
    let now = runner.clock().now_ms();
    match runner.enqueue(action, RunRequest::schedule(action, now)) {
        Ok(job) => tracing::info!(job = %job, "mail watch: track-mail enqueued"),
        Err(e) => tracing::warn!(error = %e, "mail watch: enqueue failed"),
    }
}

#[cfg(test)]
#[path = "mail_watch_tests.rs"]
mod tests;
