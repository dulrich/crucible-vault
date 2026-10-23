//! Read → Done sync (plan round-10-feedback-todos WP-3, decision 3): a note's frontmatter `read`
//! turning `true` ticks every open tracker row linked to it.
//!
//! The index is updated at write time and [`VaultEvent`]s carry no props, so the previous value is
//! not recoverable from the index; [`ReadSync`] keeps its own last-seen `read` per path, seeded
//! from the index at startup. One-way: a row is never un-ticked, and `true → true` never re-ticks
//! (a user-reopened row stays open).

use std::collections::HashMap;

use super::*;

/// Last-seen `read == true` per markdown path. Owned by the event-bridge thread.
#[derive(Debug, Default)]
pub(super) struct ReadSync {
    seen: HashMap<String, bool>,
}

impl ReadSync {
    /// A map seeded with the current index state.
    pub(super) fn seeded(seen: HashMap<String, bool>) -> Self {
        Self { seen }
    }

    /// Folds one vault event in; returns whether `path` just went not-true → `true`. `read_now`
    /// reads the note's current `read` from the index (`None` = unreadable: no change recorded);
    /// it is called only for events that need it.
    pub(super) fn observe(
        &mut self,
        path: &str,
        kind: &EventKind,
        read_now: impl FnOnce() -> Option<bool>,
    ) -> bool {
        match kind {
            EventKind::Deleted => {
                self.seen.remove(path);
                false
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
                false
            }
            EventKind::Created | EventKind::Modified => {
                if !is_md(path) {
                    return false;
                }
                let Some(now) = read_now() else {
                    return false;
                };
                let was = self.seen.insert(path.to_owned(), now).unwrap_or(false);
                !was && now
            }
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
        let open = db
            .tracker_item_status(&item.source_kind, &item.source_key, &item.item_id)?
            .is_some_and(|s| s.is_none());
        if open
            && db.set_tracker_item_status(
                &item.source_kind,
                &item.source_key,
                &item.item_id,
                Some('x'),
            )?
        {
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

    /// Drives one event the way the bridge does.
    fn event(rs: &mut ReadSync, state: &Mutex<StateDb>, path: &str, now: bool) {
        if rs.observe(path, &EventKind::Modified, || Some(now)) {
            tick_open_rows(state, path).unwrap();
        }
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
        assert!(rs.observe("n.md", &EventKind::Modified, || Some(true)));
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
        assert!(!rs.observe("b.md", &ren, || Some(true)));
        assert!(!rs.observe("b.md", &EventKind::Modified, || Some(true)));
        assert!(!rs.observe("b.md", &EventKind::Deleted, || None));
        // Deleted forgot it: a recreate that is read counts as a transition.
        assert!(rs.observe("b.md", &EventKind::Created, || Some(true)));
        // Non-markdown paths are ignored.
        assert!(!rs.observe("x.png", &EventKind::Modified, || Some(true)));
    }
}
