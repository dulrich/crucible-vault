//! Maildir watch (plan `intake-followups-mail-youtube` WP-3): enqueue `track-mail` as soon as a
//! dropped message settles, instead of waiting for the 15-minute schedule (which stays as the
//! backstop, untouched).
//!
//! Pattern-matched on the vault watcher (`crates/cv-vault/src/watcher.rs`): raw `notify` events
//! are only *hints*. A hint on a message path (`<maildir>/*.eml`, or any non-dot file in
//! `<maildir>/new/`) restarts that path's quiet timer; once every pending path has been quiet for
//! [`Timings::quiet`] (~5 s) **and** its size is unchanged across two consecutive checks
//! [`Timings::check`] (~1 s) apart, one enqueue fires (audit E3). A file still being written is
//! therefore not enqueued until it settles. The enqueue uses the scheduler's own request
//! ([`RunRequest::schedule`]), so its dedupe key (`track-mail::schedule`) collapses a burst — and a
//! pending scheduled run — into one queued job.
//!
//! The watch is non-recursive on `<maildir>` and `<maildir>/new` (each only if it exists; a
//! missing directory is one `warn!` per (re)build, never retried until the config changes). The
//! loop re-reads `[mail]` every check, so a config reload rebuilds the watcher; daemon stop ends
//! the task with the other background tasks. It never reads message contents and never writes.
//! No Crucible equivalent exists (Crucible had no mail intake); nothing Obsidian-induced is carried.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cv_actions::pipelines::MailPipeline;
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::{mpsc, watch};

use crate::runner::{RunRequest, Runner};

/// The action a settled drop enqueues.
pub(crate) const ACTION: &str = "track-mail";

/// Readiness timings.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timings {
    /// A path is examined once no event touched it for this long.
    pub quiet: Duration,
    /// Loop period; also the spacing of the two size checks.
    pub check: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            quiet: Duration::from_secs(5),
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

/// Whether `kind` hints at a (possibly new) message: create, rename-into, write, close-write.
fn is_hint(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(_)
            | EventKind::Access(notify::event::AccessKind::Close(_))
    )
}

#[derive(Debug)]
struct Pending {
    last_event: Instant,
    size: Option<u64>,
}

/// The per-path debounce state.
#[derive(Debug, Default)]
struct Settle {
    pending: HashMap<PathBuf, Pending>,
}

impl Settle {
    fn hint(&mut self, p: PathBuf, now: Instant) {
        self.pending.insert(
            p,
            Pending {
                last_event: now,
                size: None,
            },
        );
    }

    /// One check: `true` when every pending path is quiet and size-stable (then clears).
    fn ready(&mut self, now: Instant, quiet: Duration) -> bool {
        if self.pending.is_empty()
            || self
                .pending
                .values()
                .any(|e| now.duration_since(e.last_event) < quiet)
        {
            return false;
        }
        let mut stable = true;
        self.pending.retain(|p, e| {
            let Ok(md) = std::fs::metadata(p) else {
                return false; // moved away or deleted: nothing to wait for
            };
            if !md.is_file() {
                return false;
            }
            if e.size != Some(md.len()) {
                e.size = Some(md.len());
                stable = false;
            }
            true
        });
        if stable && !self.pending.is_empty() {
            self.pending.clear();
            return true;
        }
        false
    }
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

/// The watch loop: `config` yields the maildir to watch (re-read every check), `enqueue` fires
/// once per settled drop.
pub(crate) async fn run(
    config: impl Fn() -> Option<PathBuf> + Send + 'static,
    enqueue: impl Fn() + Send + 'static,
    timings: Timings,
    mut stop: watch::Receiver<bool>,
) {
    let mut current: Option<PathBuf> = None;
    let mut active: Option<(notify::RecommendedWatcher, Rx)> = None;
    let mut settle = Settle::default();
    let mut tick = tokio::time::interval(timings.check);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = tick.tick() => {}
        }
        let want = config();
        if want != current {
            active = None;
            settle = Settle::default();
            if let Some(root) = &want {
                active = build(root);
            }
            current = want;
        }
        let (Some(root), Some((_, rx))) = (&current, active.as_mut()) else {
            continue;
        };
        let now = Instant::now();
        while let Ok(ev) = rx.try_recv() {
            let Ok(ev) = ev else { continue };
            if !is_hint(&ev.kind) {
                continue;
            }
            for p in ev.paths {
                if is_message(root, &p) {
                    settle.hint(p, now);
                }
            }
        }
        if settle.ready(now, timings.quiet) {
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
