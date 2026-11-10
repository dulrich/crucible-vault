//! Read → Done sync (plan round-10-feedback-todos WP-3, decision 3): a note's frontmatter `read`
//! turning `true` ticks every open tracker row linked to it.
//!
//! The index is updated at write time and [`VaultEvent`]s carry no props, so the previous value is
//! not recoverable from the index; [`ReadSync`] keeps its own last-seen `read` per path, seeded
//! from the index at startup. One-way: a row is never un-ticked, and `true → true` never re-ticks
//! (a user-reopened row stays open).
//!
//! Quiet window (plan note-path-continuity D6, `docs/decisions.md` normative): a `read` flip that
//! reverts within the former watcher debounce is not a transition. Daemon events now arrive
//! immediately, so a content change only marks its path pending; the value is read and settled
//! once the path has been quiet for [`READ_QUIET`].

use std::collections::HashMap;
use std::time::Instant;

use super::*;

/// How long a path must be quiet before its `read` value settles (the former debounce).
pub(super) const READ_QUIET: Duration = Duration::from_millis(150);

/// Last-seen `read == true` per markdown path, plus the paths waiting out [`READ_QUIET`]. Owned
/// by [`read_sync_listener`].
#[derive(Debug, Default)]
pub(super) struct ReadSync {
    seen: HashMap<String, bool>,
    pending: HashMap<String, Instant>,
}

impl ReadSync {
    /// A map seeded with the current index state.
    pub(super) fn seeded(seen: HashMap<String, bool>) -> Self {
        Self {
            seen,
            pending: HashMap::new(),
        }
    }

    /// Folds one vault event in at `now`. A content change marks its path pending (settled by
    /// [`Self::settle`] once quiet); a move carries the state (and any pending deadline) to the
    /// destination; a delete forgets both. `read_now` reads the note's current `read` from the
    /// index (`None` = unreadable) and is called only for a move whose source was never seen.
    pub(super) fn observe(
        &mut self,
        path: &str,
        kind: &EventKind,
        now: Instant,
        read_now: impl FnOnce() -> Option<bool>,
    ) {
        match kind {
            EventKind::Deleted { .. } => {
                self.seen.remove(path);
                self.pending.remove(path);
            }
            EventKind::Renamed { from } => {
                // A move carries no transition: the destination inherits the source's state.
                let moved = self
                    .seen
                    .remove(from)
                    .or_else(|| is_md(path).then(read_now).flatten());
                if let Some(v) = moved {
                    self.seen.insert(path.to_owned(), v);
                }
                if let Some(due) = self.pending.remove(from) {
                    self.pending.insert(path.to_owned(), due);
                }
            }
            EventKind::Created | EventKind::Modified => {
                if is_md(path) {
                    self.pending.insert(path.to_owned(), now + READ_QUIET);
                }
            }
        }
    }

    /// The earliest pending deadline.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.pending.values().min().copied()
    }

    /// Settles every path quiet since its deadline: reads its value with `read_now` and returns
    /// the paths that went not-true → `true` (sorted).
    pub(super) fn settle(
        &mut self,
        now: Instant,
        mut read_now: impl FnMut(&str) -> Option<bool>,
    ) -> Vec<String> {
        let mut due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, t)| **t <= now)
            .map(|(p, _)| p.clone())
            .collect();
        due.sort();
        let mut out = Vec::new();
        for p in due {
            self.pending.remove(&p);
            let Some(v) = read_now(&p) else {
                continue;
            };
            let was = self.seen.insert(p.clone(), v).unwrap_or(false);
            if !was && v {
                out.push(p);
            }
        }
        out
    }
}

/// Read-sync's feed: each vault event's `(path, kind)`, sent by the event bridge after the writer
/// updated the index. Unbounded, so no event is ever dropped (the old consumer's guarantee).
pub(super) type Feed = tokio::sync::mpsc::UnboundedSender<(String, EventKind)>;

/// The read-sync listener (review 2026-11-10 D7), beside `generation_listener`: folds each vault
/// event from its own [`Feed`] into [`ReadSync`] and settles quiet paths, ticking the open tracker
/// rows of each not-true → `true` transition. Index reads and ticks run on the blocking pool,
/// never on the event-bridge thread. Errors are logged.
pub(super) async fn read_sync_listener(
    shared: Arc<Shared>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<(String, EventKind)>,
    mut stop: watch::Receiver<bool>,
) {
    let s = Arc::clone(&shared);
    let Ok(mut rs) = tokio::task::spawn_blocking(move || seed(&s)).await else {
        return;
    };
    loop {
        let due = rs.next_due();
        let flush = async {
            match due {
                Some(t) => tokio::time::sleep_until(t.into()).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = stop.changed() => return,
            () = flush => {
                let s = Arc::clone(&shared);
                let settled = tokio::task::spawn_blocking(move || {
                    settle_read(&s, &mut rs);
                    rs
                });
                match settled.await {
                    Ok(r) => rs = r,
                    Err(_) => return,
                }
            }
            ev = rx.recv() => {
                let Some((path, kind)) = ev else { return };
                // Only a move from a never-seen source reads the index.
                let unseen =
                    matches!(&kind, EventKind::Renamed { from } if !rs.seen.contains_key(from));
                let now = if unseen && is_md(&path) {
                    let (s, p) = (Arc::clone(&shared), path.clone());
                    tokio::task::spawn_blocking(move || read_index(&s, &p))
                        .await
                        .ok()
                        .flatten()
                } else {
                    None
                };
                rs.observe(&path, &kind, Instant::now(), || now);
            }
        }
    }
}

/// The last-seen `read` map, seeded from the index (an unreadable index logs and starts empty).
fn seed(shared: &Arc<Shared>) -> ReadSync {
    let seeded = shared
        .writer()
        .ok_or_else(|| anyhow!("writer gone"))
        .and_then(|w| {
            w.call(|vw| seed_map(&vw.index().query()))
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

/// Settles every path quiet for [`READ_QUIET`] (D6) and ticks the open tracker rows of each
/// not-true → `true` transition.
fn settle_read(shared: &Arc<Shared>, rs: &mut ReadSync) {
    for path in rs.settle(Instant::now(), |p| read_index(shared, p)) {
        match tick_open_rows(&shared.state, &path) {
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
    match w.call(move |vw| read_of(&vw.index().query(), &p)) {
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

fn is_md(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        && path.contains('.')
}

/// Ticks (`x`) every open (NULL-status) row linked to `path`; returns how many. Cancelled and
/// already-done rows are left alone.
pub(super) fn tick_open_rows(state: &Mutex<StateDb>, path: &str) -> Result<usize, StateError> {
    let mut db = lock(state);
    let mut ticked = 0;
    for item in db.tracker_items_for_note(path)? {
        // The op's NULL-status guard leaves cancelled and done rows alone.
        if db.finalize_tracker_item(
            &item.source_kind,
            &item.source_key,
            &item.item_id,
            None,
            true,
        )? {
            ticked += 1;
        }
    }
    Ok(ticked)
}

/// The note's current `read` from an index query (`true` only for a boolean `true`).
pub(super) fn read_of(q: &cv_index::Query<'_>, path: &str) -> Result<bool> {
    Ok(q.props(path)?
        .iter()
        .any(|p| p.key == "read" && p.value == Value::Bool(true)))
}

/// Every markdown note's `read` state, for seeding.
pub(super) fn seed_map(q: &cv_index::Query<'_>) -> Result<HashMap<String, bool>> {
    Ok(q.md_props_with_key("read")?
        .into_iter()
        .map(|p| (p.path, p.value == Value::Bool(true)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cv_protocol::TrackerOutcome;
    use cv_state::TrackerItem;

    fn db() -> (tempfile::TempDir, Mutex<StateDb>) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
        (dir, Mutex::new(db))
    }

    fn add(state: &Mutex<StateDb>, kind: &str, id: &str, path: Option<&str>) {
        lock(state)
            .insert_tracker_item(&TrackerItem {
                source_kind: kind.into(),
                source_key: "k".into(),
                item_id: id.into(),
                title: "T".into(),
                url: format!("https://example.test/{id}"),
                first_seen_ms: 1,
                day: "2026-10-22".into(),
                outcome: TrackerOutcome::Note,
                score: None,
                full_body: None,
                note_path: path.map(str::to_owned),
                source_name: None,
                duration_s: None,
                word_count: None,
                access: None,
                complete: None,
                full_word_count: None,
            })
            .unwrap();
    }

    fn status(state: &Mutex<StateDb>, kind: &str, id: &str) -> Option<char> {
        lock(state)
            .tracker_item_status(kind, "k", id)
            .unwrap()
            .unwrap()
    }

    /// One event, then a settle once the quiet window has passed; whether `path` transitioned.
    fn step(rs: &mut ReadSync, path: &str, kind: &EventKind, read: Option<bool>) -> bool {
        let t0 = Instant::now();
        rs.observe(path, kind, t0, || read);
        let got = rs.settle(t0 + READ_QUIET, |_| read);
        assert!(got.iter().all(|p| p == path));
        !got.is_empty()
    }

    /// Drives one event the way the bridge does.
    fn event(rs: &mut ReadSync, state: &Mutex<StateDb>, path: &str, now: bool) {
        if step(rs, path, &EventKind::Modified, Some(now)) {
            tick_open_rows(state, path).unwrap();
        }
    }

    #[test]
    fn read_flip_reverted_inside_the_quiet_window_ticks_nothing() {
        let (_d, state) = db();
        add(&state, "blog", "b1", Some("n.md"));
        let mut rs = ReadSync::seeded(HashMap::from([("n.md".to_owned(), true)]));
        let t0 = Instant::now();
        // true → false → true, each edit inside the window: the value settles once, unchanged.
        rs.observe("n.md", &EventKind::Modified, t0, || None);
        let t1 = t0 + Duration::from_millis(50);
        assert!(rs.settle(t1, |_| Some(false)).is_empty(), "not quiet yet");
        rs.observe("n.md", &EventKind::Modified, t1, || None);
        assert!(
            rs.settle(t0 + READ_QUIET, |_| Some(false)).is_empty(),
            "window restarted"
        );
        assert_eq!(rs.next_due(), Some(t1 + READ_QUIET));
        assert!(rs.settle(t1 + READ_QUIET, |_| Some(true)).is_empty());
        assert_eq!(rs.next_due(), None);
        assert_eq!(status(&state, "blog", "b1"), None, "nothing ticked");
        // A settled false → true still ticks.
        rs.observe("n.md", &EventKind::Modified, t1, || None);
        assert!(rs.settle(t1 + READ_QUIET, |_| Some(false)).is_empty());
        // false → true → false inside the window (the brief flip): settles false, ticks nothing.
        rs.observe("n.md", &EventKind::Modified, t1, || None);
        let t2 = t1 + Duration::from_millis(40);
        rs.observe("n.md", &EventKind::Modified, t2, || None);
        assert!(rs.settle(t2 + READ_QUIET, |_| Some(false)).is_empty());
        assert_eq!(status(&state, "blog", "b1"), None, "flip ticked nothing");
        rs.observe("n.md", &EventKind::Modified, t1, || None);
        assert_eq!(
            rs.settle(t1 + READ_QUIET, |_| Some(true)),
            vec!["n.md".to_owned()]
        );
    }

    #[test]
    fn transition_ticks_every_open_row_of_any_kind() {
        let (_d, state) = db();
        add(&state, "mail", "m1", Some("n.md"));
        add(&state, "unknown-sender", "u1", Some("n.md"));
        add(&state, "blog", "b1", Some("n.md"));
        add(&state, "blog", "other", Some("o.md"));
        lock(&state)
            .set_tracker_item_status("blog", "k", "other", Some('-'))
            .unwrap();
        let mut rs = ReadSync::default();
        event(&mut rs, &state, "n.md", true);
        assert_eq!(status(&state, "mail", "m1"), Some('x'));
        assert_eq!(status(&state, "unknown-sender", "u1"), Some('x'));
        assert_eq!(status(&state, "blog", "b1"), Some('x'));
        assert_eq!(status(&state, "blog", "other"), Some('-'));
        // A cancelled row on the transitioning note stays cancelled.
        add(&state, "blog", "c1", Some("p.md"));
        lock(&state)
            .set_tracker_item_status("blog", "k", "c1", Some('-'))
            .unwrap();
        event(&mut rs, &state, "p.md", true);
        assert_eq!(status(&state, "blog", "c1"), Some('-'));
    }

    #[test]
    fn true_to_true_never_reticks_a_reopened_row() {
        let (_d, state) = db();
        add(&state, "blog", "b1", Some("n.md"));
        let mut rs = ReadSync::default();
        event(&mut rs, &state, "n.md", true);
        lock(&state)
            .set_tracker_item_status("blog", "k", "b1", None)
            .unwrap();
        event(&mut rs, &state, "n.md", true);
        assert_eq!(status(&state, "blog", "b1"), None);
    }

    #[test]
    fn true_to_false_leaves_rows_untouched() {
        let (_d, state) = db();
        add(&state, "blog", "b1", Some("n.md"));
        let mut rs = ReadSync::default();
        event(&mut rs, &state, "n.md", true);
        event(&mut rs, &state, "n.md", false);
        assert_eq!(status(&state, "blog", "b1"), Some('x'));
        add(&state, "blog", "b2", Some("m.md"));
        event(&mut rs, &state, "m.md", false);
        assert_eq!(status(&state, "blog", "b2"), None);
    }

    #[test]
    fn note_without_rows_is_a_no_op() {
        let (_d, state) = db();
        let mut rs = ReadSync::default();
        assert!(step(&mut rs, "n.md", &EventKind::Modified, Some(true)));
        assert_eq!(tick_open_rows(&state, "n.md").unwrap(), 0);
    }

    #[test]
    fn seeded_read_note_does_not_tick_on_unrelated_edit() {
        let (_d, state) = db();
        add(&state, "blog", "b1", Some("n.md"));
        let mut rs = ReadSync::seeded(HashMap::from([("n.md".to_owned(), true)]));
        event(&mut rs, &state, "n.md", true);
        assert_eq!(status(&state, "blog", "b1"), None);
    }

    #[test]
    fn rename_and_delete_move_state_without_transition() {
        let mut rs = ReadSync::seeded(HashMap::from([("a.md".to_owned(), true)]));
        let ren = EventKind::Renamed {
            from: "a.md".into(),
        };
        assert!(!step(&mut rs, "b.md", &ren, Some(true)));
        assert!(!step(&mut rs, "b.md", &EventKind::Modified, Some(true)));
        assert!(!step(
            &mut rs,
            "b.md",
            &EventKind::Deleted { replaced_by: None },
            None
        ));
        // Deleted forgot it: a recreate that is read counts as a transition.
        assert!(step(&mut rs, "b.md", &EventKind::Created, Some(true)));
        // Non-markdown paths are ignored.
        assert!(!step(&mut rs, "x.png", &EventKind::Modified, Some(true)));
    }
}
