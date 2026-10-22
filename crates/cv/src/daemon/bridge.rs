//! The notification bridge: vault events → `file_changed`, and notifications → view invalidation.

use super::*;

/// Bridges the writer's std receiver into the broadcast channel on a dedicated thread. Exits when
/// the writer thread does.
pub(super) fn start_event_bridge(
    shared: &Arc<Shared>,
    rx: std::sync::mpsc::Receiver<VaultEvent>,
) -> Result<()> {
    let weak = Arc::downgrade(shared);
    let events = shared.events.clone();
    std::thread::Builder::new()
        .name("cv-events".into())
        .spawn(move || {
            while let Ok(ev) = rx.recv() {
                // A `.gitignore` create/modify/delete/rename changes index membership: rebuild
                // (plan editor-ui-round-3 WP-1). Checked before anything else, since the file is
                // itself a dot-file the index never holds.
                let ignore_changed = cv_index::is_gitignore_path(&ev.path)
                    || matches!(&ev.kind, EventKind::Renamed { from } if cv_index::is_gitignore_path(from));
                let (kind, want_hash) = match ev.kind {
                    EventKind::Created => (ChangeKind::Created, true),
                    EventKind::Modified => (ChangeKind::Modified, true),
                    EventKind::Deleted => (ChangeKind::Deleted, false),
                    EventKind::Renamed { .. } => (ChangeKind::Renamed, false),
                };
                let hash = if want_hash {
                    weak.upgrade().and_then(|s| s.writer()).and_then(|w| {
                        let p = ev.path.clone();
                        w.call(move |vw| vw.fs().hash(&p))
                            .ok()
                            .and_then(Result::ok)
                            .flatten()
                            .map(|h| h.as_str().to_owned())
                    })
                } else {
                    None
                };
                let origin = match ev.origin {
                    cv_vault::Origin::Daemon => Origin::Daemon,
                    cv_vault::Origin::External => Origin::External,
                };
                for n in file_changes(ev.path, &ev.kind, kind, hash, origin) {
                    let _ = events.send(n);
                }
                if ignore_changed
                    && let Some(w) = weak.upgrade().and_then(|s| s.writer())
                {
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
        })
        .context("spawning event bridge")?;
    Ok(())
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

/// One vault event's `file_changed` notifications. A rename (daemon or external) reports the
/// destination as `Renamed { from }` first, then its source as `Deleted`: a client that follows
/// moves (the editor's tabs) re-keys on the first and finds nothing open for the second, while
/// clients without a rename record (no starter lists `deleted`) still drop the old path (plan
/// intake-round-2-clean-dedupe-reading WP-5a proposal 1; plan round-9-feedback-todos WP-3).
fn file_changes(
    path: String,
    ev_kind: &EventKind,
    kind: ChangeKind,
    hash: Option<String>,
    origin: Origin,
) -> Vec<Notification> {
    let from = match ev_kind {
        EventKind::Renamed { from } => Some(from.clone()),
        _ => None,
    };
    let mut out = Vec::with_capacity(2);
    out.push(Notification::FileChanged {
        path,
        kind,
        hash,
        origin,
        from: from.clone(),
    });
    if let Some(from) = from {
        out.push(Notification::FileChanged {
            path: from,
            kind: ChangeKind::Deleted,
            hash: None,
            origin,
            from: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_reports_destination_with_from_then_source_deleted() {
        let ev = EventKind::Renamed {
            from: "a.md".into(),
        };
        let got = file_changes(
            "b.md".into(),
            &ev,
            ChangeKind::Renamed,
            None,
            Origin::Daemon,
        );
        assert_eq!(
            got,
            vec![
                Notification::FileChanged {
                    path: "b.md".into(),
                    kind: ChangeKind::Renamed,
                    hash: None,
                    origin: Origin::Daemon,
                    from: Some("a.md".into()),
                },
                Notification::FileChanged {
                    path: "a.md".into(),
                    kind: ChangeKind::Deleted,
                    hash: None,
                    origin: Origin::Daemon,
                    from: None,
                },
            ]
        );
        let one = file_changes(
            "c.md".into(),
            &EventKind::Created,
            ChangeKind::Created,
            None,
            Origin::External,
        );
        assert_eq!(one.len(), 1);
    }
}
