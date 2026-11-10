//! The notification bridge: vault events → `file_changed`, and notifications → view invalidation.

use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;

use super::read_sync::ReadSync;
use super::*;

/// Bridges the writer's std receiver into the broadcast channel on a dedicated thread. Exits when
/// the writer thread does.
pub(super) fn start_event_bridge(
    shared: &Arc<Shared>,
    rx: std::sync::mpsc::Receiver<VaultEvent>,
) -> Result<()> {
    let weak = Arc::downgrade(shared);
    let events = shared.events.clone();
    let mut read_sync = seed_read_sync(shared);
    std::thread::Builder::new()
        .name("cv-events".into())
        .spawn(move || {
            loop {
                // Wait for the next event, or until the earliest read-sync deadline (D6).
                let next = match read_sync.next_due() {
                    Some(t) => match rx.recv_timeout(t.saturating_duration_since(Instant::now())) {
                        Ok(ev) => Some(ev),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    None => match rx.recv() {
                        Ok(ev) => Some(ev),
                        Err(_) => break,
                    },
                };
                if let Some(ev) = next {
                    forward(&weak, &events, &mut read_sync, ev);
                }
                if let Some(s) = weak.upgrade() {
                    settle_read(&s, &mut read_sync);
                }
            }
        })
        .context("spawning event bridge")?;
    Ok(())
}

/// One vault event → `file_changed` notifications (published at once), plus the read-sync fold
/// and a `.gitignore` rebuild.
fn forward(
    weak: &std::sync::Weak<Shared>,
    events: &broadcast::Sender<Notification>,
    read_sync: &mut ReadSync,
    ev: VaultEvent,
) {
    // A `.gitignore` create/modify/delete/rename changes index membership: rebuild (plan
    // editor-ui-round-3 WP-1). Checked before anything else, since the file is itself a dot-file
    // the index never holds.
    let ignore_changed = cv_index::is_gitignore_path(&ev.path)
        || matches!(&ev.kind, EventKind::Renamed { from } if cv_index::is_gitignore_path(from));
    if let Some(s) = weak.upgrade() {
        sync_read(&s, read_sync, &ev.path, &ev.kind);
    }
    for n in cv_protocol::file_changed(ev.into()) {
        let _ = events.send(n);
    }
    if ignore_changed && let Some(w) = weak.upgrade().and_then(|s| s.writer()) {
        match w.call(|vw| vw.rebuild()) {
            Ok(Ok(n)) => {
                let _ = events.send(Notification::IndexGeneration { n });
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "index rebuild after .gitignore change failed");
            }
            Err(e) => tracing::warn!(error = %e, "writer gone during .gitignore rebuild"),
        }
    }
}

/// The last-seen `read` map, seeded from the index (an unreadable index logs and starts empty).
fn seed_read_sync(shared: &Arc<Shared>) -> ReadSync {
    let seeded = shared
        .writer()
        .ok_or_else(|| anyhow!("writer gone"))
        .and_then(|w| {
            w.call(|vw| read_sync::seed_map(&vw.index().query()))
                .map_err(Into::into)
                .and_then(|r| r)
        });
    match seeded {
        Ok(m) => ReadSync::seeded(m),
        Err(e) => {
            tracing::warn!(error = %e, "read-sync seed failed; starting empty");
            ReadSync::default()
        }
    }
}

/// Folds one vault event into the read-sync map and ticks the note's open tracker rows on a
/// not-true → `true` transition. Never fails the bridge: errors are logged. The tick raises the
/// `tracker_items` change signal on the state db (`set_tracker_item_status`), which is also what
/// the Intake UI tick relies on to refresh views.
fn sync_read(shared: &Arc<Shared>, rs: &mut ReadSync, path: &str, kind: &EventKind) {
    rs.observe(path, kind, Instant::now(), || read_index(shared, path));
}

/// Settles every read-sync path quiet for [`read_sync::READ_QUIET`] (D6) and ticks the open
/// tracker rows of each not-true → `true` transition.
fn settle_read(shared: &Arc<Shared>, rs: &mut ReadSync) {
    for path in rs.settle(Instant::now(), |p| read_index(shared, p)) {
        match read_sync::tick_open_rows(&shared.state, &path) {
            Ok(0) => {}
            Ok(n) => tracing::info!(rows = n, "read-sync: ticked open tracker rows"),
            Err(e) => tracing::warn!(error = %e, "read-sync: tick failed"),
        }
    }
}

/// The note's current `read` from the index, through the writer (`None` = unreadable).
fn read_index(shared: &Arc<Shared>, path: &str) -> Option<bool> {
    let p = path.to_owned();
    let w = shared.writer()?;
    match w.call(move |vw| read_sync::read_of(&vw.index().query(), &p)) {
        Ok(Ok(v)) => Some(v),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "read-sync: index read failed");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "read-sync: writer gone");
            None
        }
    }
}

/// Coalescing window for incremental note changes → one `views_invalidated` (plan
/// editor-ui-round-3 WP-2).
pub(crate) const VIEWS_COALESCE: Duration = Duration::from_millis(150);

/// Forwards `index_generation` to the view registry, and coalesces `file_changed` (daemon- and
/// external-origin; the index is already updated when it is sent) into one [`ViewRegistry::notify_notes_changed`] per [`VIEWS_COALESCE`] window.
pub(super) async fn generation_listener(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut rx = shared.subscribe();
    let mut due: Option<tokio::time::Instant> = None;
    loop {
        let flush = async {
            match due {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = stop.changed() => return,
            () = flush => {
                due = None;
                shared.views.notify_notes_changed();
            }
            r = rx.recv() => match r {
                Ok(Notification::IndexGeneration { n }) => {
                    // A generation invalidates everything itself; a pending batch is subsumed.
                    due = None;
                    shared.views.notify_generation(n);
                }
                Ok(Notification::FileChanged { .. }) => {
                    due.get_or_insert_with(|| tokio::time::Instant::now() + VIEWS_COALESCE);
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(k)) => {
                    tracing::warn!(skipped = k, "generation listener lagged");
                    due.get_or_insert_with(|| tokio::time::Instant::now() + VIEWS_COALESCE);
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}
