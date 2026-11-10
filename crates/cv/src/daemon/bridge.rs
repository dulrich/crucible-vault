//! The notification bridge: vault events → `file_changed`, and notifications → view invalidation.

use super::*;

/// Bridges the writer's std receiver into the broadcast channel on a dedicated thread. Exits when
/// the writer thread does.
pub(super) fn start_event_bridge(
    shared: &Arc<Shared>,
    rx: std::sync::mpsc::Receiver<VaultEvent>,
    read_sync: read_sync::Feed,
) -> Result<()> {
    let weak = Arc::downgrade(shared);
    let events = shared.events.clone();
    std::thread::Builder::new()
        .name("cv-events".into())
        .spawn(move || {
            while let Ok(ev) = rx.recv() {
                forward(&weak, &events, &read_sync, ev);
            }
        })
        .context("spawning event bridge")?;
    Ok(())
}

/// One vault event → `file_changed` notifications (published at once), a non-blocking send to
/// read-sync ([`read_sync::read_sync_listener`]), plus a `.gitignore` rebuild.
fn forward(
    weak: &std::sync::Weak<Shared>,
    events: &broadcast::Sender<Notification>,
    read_sync: &read_sync::Feed,
    ev: VaultEvent,
) {
    // A `.gitignore` create/modify/delete/rename changes index membership: rebuild (plan
    // editor-ui-round-3 WP-1). Checked before anything else, since the file is itself a dot-file
    // the index never holds.
    let ignore_changed = cv_index::is_gitignore_path(&ev.path)
        || matches!(&ev.kind, EventKind::Renamed { from } if cv_index::is_gitignore_path(from));
    // A listener gone at shutdown is not an error.
    let _ = read_sync.send((ev.path.clone(), ev.kind.clone()));
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
