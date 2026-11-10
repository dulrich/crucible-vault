//! The identity-follow machine (plan note-identity D4, E2): how an open document follows its
//! note by id through daemon `file_changed` notifications, moves, deletions and absorptions —
//! `doc_file_changed` (and its `id: None` fallback `path_file_changed`), `doc_gone`,
//! `follow_daemon_move`, `follow_doc`, `move_doc_path`, `follow_replaced`, `absorb`,
//! `move_path_state`, `retarget_note_tabs` and the [`Follow`] mode. Locating a note off the UI
//! thread (settle, open, insert) stays in `settle.rs`; bookmarks, explorer ops, Reading-side
//! edits and title sync stay in `files.rs`.

use super::*;

/// How a followed move treats a document's buffer (plan note-identity D4, [`App::follow_doc`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Follow {
    /// A daemon-side move: unsaved edits are kept behind the conflict bar only when the moved
    /// bytes are not ones the buffer holds (its base or its in-flight save).
    Daemon,
    /// An editor-initiated move: `true` = edited since it was issued (conflict bar).
    Edited(bool),
}

impl App {
    /// Plan note-identity D4: a daemon-side move of `from` to `to` known only by its paths (an
    /// `id: None` notification, see `update_status`): the document open at `from`, if any,
    /// follows; otherwise only file tabs show the path.
    pub(super) fn follow_daemon_move(&mut self, from: &str, to: &str) {
        self.retarget_holds(from, to);
        if from == to || !self.path_open(from) {
            return;
        }
        match self.docs.id_of(from) {
            Some(id) => self.follow_doc(id, to, Follow::Daemon),
            // Plan round-15 WP-2: only file tabs show a path without a document.
            None => self.retarget_file(from, to),
        }
    }

    /// Plan note-identity D4: a `file_changed` carrying note id `id` (now at `path`, content
    /// `hash`). Returns the document to reload now (echo / silent reload / conflict bar /
    /// deleted state), if any; a settle's answer reloads later ([`super::settle::AfterSettle`]).
    ///
    /// - A move (`from`) of a note no document holds still retargets holds and file tabs (by path).
    /// - `Deleted` whose path is not the document's current path is the source half of a rename
    ///   pair the document already followed (audit E1): the document is live elsewhere, nothing
    ///   happens to it. At its current path, `replaced_by` takes the E2 tab move, and otherwise
    ///   the settle decides ([`Self::apply_settled`]); no answer re-reads the path.
    /// - Any other kind at a path that is not the document's: a `Renamed` from the document's
    ///   path follows it; anything else is settled, never guessed, and reloads once the
    ///   document is at `path`.
    /// - A different note at the path of an open document (path reuse) never touches it.
    pub(super) fn doc_file_changed(
        &mut self,
        id: NoteId,
        path: &str,
        hash: Option<&str>,
        kind: cv_protocol::ChangeKind,
        from: Option<&str>,
        replaced_by: Option<&str>,
    ) -> Option<NoteId> {
        use super::settle::AfterSettle;
        if let Some(f) = from {
            self.retarget_holds(f, path);
            if self.docs.by_id(id).is_none() && !self.docs.contains_key(f) && self.path_open(f) {
                // Plan round-15 WP-2: only file tabs show a path without a document.
                self.retarget_file(f, path);
            }
        }
        let cur = self.docs.path_of(id)?;
        if kind == cv_protocol::ChangeKind::Deleted {
            if cur != path {
                // Audit E1: the rename pair's source half; the document is live at `cur`.
                return None;
            }
            if let Some(by) = replaced_by {
                return (!self.follow_replaced(id, by)).then_some(id);
            }
            if self.daemon.is_none() {
                return Some(id);
            }
            let hash = hash.map(str::to_owned);
            self.settle_doc(id, AfterSettle::Deleted { hash });
            return None;
        }
        if cur != path {
            if from == Some(cur.as_str()) {
                self.follow_doc(id, path, Follow::Daemon);
            } else {
                let (path, hash) = (path.to_owned(), hash.map(str::to_owned));
                self.settle_doc(id, AfterSettle::Changed { path, hash });
                return None;
            }
        }
        (self.docs.path_of(id).as_deref() == Some(path)).then_some(id)
    }

    /// Today's deleted handling for document `id` whose note is gone (D4: decided from the id,
    /// never by re-reading the path, which may hold another note now): unsaved edits enter the
    /// missing-file state (kept, Keep mine revives the id), a clean document says so.
    pub(super) fn doc_gone(&mut self, id: NoteId) {
        let Some(od) = self.docs.by_id_mut(id) else {
            return;
        };
        if od.buffer.is_dirty() {
            if !crate::daemon::is_no_base(od.buffer.base_hash()) {
                od.buffer.mark_missing();
            }
        } else {
            self.status.set_error("file deleted on disk".into());
        }
    }

    /// The `id: None` fallback of [`Self::doc_file_changed`]: today's path matching (no id to settle
    /// anything by). Returns the document to reload.
    pub(super) fn path_file_changed(
        &mut self,
        path: &str,
        kind: cv_protocol::ChangeKind,
        from: Option<&str>,
        replaced_by: Option<&str>,
    ) -> Option<NoteId> {
        if let Some(f) = from {
            // Round 9 WP-3 (Decision 4): the bridge sends this before the source `Deleted`.
            self.follow_daemon_move(f, path);
        }
        let id = self.docs.id_of(path)?;
        if kind == cv_protocol::ChangeKind::Deleted
            && let Some(by) = replaced_by
            && self.follow_replaced(id, by)
        {
            return None;
        }
        Some(id)
    }

    /// Plan note-identity D4: document `id` now lives at `to` (a rename reply, a `Renamed`
    /// notification, a located move). Round 9 WP-3 / round 7 WP-2: bytes the buffer already
    /// holds (its base, or its in-flight save) → the path field changes and nothing else
    /// (cursor, undo, dirty text, the in-flight save all survive); edits the move did not carry
    /// → the text is kept behind the conflict bar (audit M2); otherwise the buffer is rebuilt
    /// from disk. A `to` that is already gone (a delayed event of a chain) is settled
    /// ([`Self::settle_doc`]), never guessed; an answer naming `to` again stops there.
    pub(super) fn follow_doc(&mut self, id: NoteId, to: &str, how: Follow) {
        let Some(from) = self.docs.path_of(id) else {
            return;
        };
        if from == to {
            return;
        }
        let Some(d) = self.daemon.clone() else { return };
        let (bytes, hash) = match d.read_file(to) {
            Ok(r) => r,
            Err(DaemonError::NotFound(_)) => {
                let failed = to.to_owned();
                self.settle_doc(id, super::settle::AfterSettle::Follow { failed });
                return;
            }
            Err(e) => {
                self.status.set_error(open_error(&e));
                return;
            }
        };
        let Some(od) = self.docs.by_id_mut(id) else {
            return;
        };
        let edited = match how {
            Follow::Daemon => od.buffer.is_dirty() && !od.buffer.holds_bytes(&hash),
            Follow::Edited(e) => e,
        };
        if edited {
            od.buffer.follow_move(hash);
        } else if !od.buffer.holds_bytes(&hash) {
            od.buffer = crate::source::SourceBuffer::new(id, &from, &bytes, hash);
        }
        self.move_doc_path(id, to);
        if edited {
            self.status.set_error(format!(
                "{from} moved to {to} while you were editing: keep mine saves your text there"
            ));
        }
    }

    /// Plan note-identity D4: **the one owner of a document's path change.** The document keeps
    /// its key (its id); its path field changes, then everything path-keyed follows from here
    /// ([`Self::move_path_state`]): tabs, history, recents and the per-path satellite stores.
    pub(super) fn move_doc_path(&mut self, id: NoteId, to: &str) {
        let Some(from) = self.docs.path_of(id) else {
            return;
        };
        if from == to {
            return;
        }
        self.docs.set_path(id, to);
        self.retarget_note_tabs(id, id, to);
        self.move_path_state(&from, to);
        // Its pending filename edit is dropped and the text renders now under `to`.
        if let Some(od) = self.docs.by_id_mut(id) {
            od.reading.clear_title();
            if od.reading.mode.shows_reading() {
                self.reading.recompute(od, self.daemon.as_ref());
            }
        }
        self.ui.touch_recent(to);
    }

    /// Round 11 WP-1 (Decision 1) + plan note-identity audit E2: document `id` was retired
    /// because the note at `to` absorbed it. Its tabs move to the absorbing document: a tab in a
    /// pane already showing `to` closes, every other tab retargets, the retired document is
    /// dropped and the absorber's is used (opened and bound when it is not open yet: plan
    /// WP-R4, off the UI thread, the move completing in [`Self::absorb`]) — never the retired
    /// document under a new path, whose saves would address a retired id. A dirty retired
    /// document is left to today's deleted-on-disk handling (`false`: the caller reloads; edits
    /// are never discarded).
    pub(super) fn follow_replaced(&mut self, id: NoteId, to: &str) -> bool {
        let Some(from) = self.docs.path_of(id) else {
            return false;
        };
        if from == to || self.docs.by_id(id).is_some_and(|d| d.buffer.is_dirty()) {
            return false;
        }
        // `to` is a path the daemon named: the same-path rule picks its open document.
        if self.docs.id_of(to).is_none() {
            if self.daemon.is_none() {
                return false;
            }
            self.start_open(to, super::settle::OpenFor::Absorb(id));
            return true;
        }
        self.absorb(id, to, None)
    }

    /// The E2 tab move of retired document `id` onto the note at `to`: its open document, or
    /// `absorber` (just opened, not yet in the store).
    pub(super) fn absorb(&mut self, id: NoteId, to: &str, absorber: Option<OpenDoc>) -> bool {
        let Some(from) = self.docs.path_of(id) else {
            return false;
        };
        let open_absorber = self.docs.id_of(to);
        let dup: Vec<u64> = self
            .panes
            .iter()
            .filter(|(_, ps)| ps.tabs.iter().any(|t| t.shows(to)))
            .flat_map(|(_, ps)| ps.tabs.iter().filter(|t| t.note == Some(id)).map(|t| t.id))
            .collect();
        for t in dup {
            self.remove_tab(t);
        }
        let retired = self.docs.remove_id(id);
        let Some(aid) = open_absorber.or(absorber.as_ref().map(|a| a.id)) else {
            return false;
        };
        if let Some(mut a) = absorber {
            // The retired document's close waiters and Reading state carry over to the note
            // that absorbed it (as the rebuilt buffer did before ids).
            if let Some(r) = retired {
                a.close_waiters = r.close_waiters;
                a.reading = r.reading;
            }
            a.reading.clear_title();
            self.docs.insert(a);
        }
        self.retarget_note_tabs(id, aid, to);
        self.move_path_state(&from, to);
        if let Some(od) = self.docs.by_id_mut(aid)
            && od.reading.mode.shows_reading()
        {
            self.reading.recompute(od, self.daemon.as_ref());
        }
        self.ui.touch_recent(to);
        true
    }

    /// The path-keyed tail of every move follow ([`Self::move_doc_path`], the E2 tab move): every
    /// tab of `from` (and its history) moves to `to` and the per-path satellite stores are
    /// re-keyed. Hosted view results move with the host and go stale (re-run against `to` while
    /// the old result stays shown — no flash), and every other view goes stale too, since a
    /// rename changes link targets they may list (WP-8). Plan note-identity WP-5 re-keys these
    /// stores by id and removes this tail.
    pub(super) fn move_path_state(&mut self, from: &str, to: &str) {
        for (_, ps) in self.panes.iter_mut() {
            // Note tabs bound to a document moved by id ([`Self::retarget_note_tabs`]); this
            // retargets file tabs (and unbound note tabs) by path.
            for t in ps
                .tabs
                .iter_mut()
                .filter(|t| t.note.is_none() && t.shows(from))
            {
                t.retarget(to);
            }
            ps.history.retarget(from, to);
        }
        // The `from` image pins move to `to` (plan round-16 WP-3: the tab's image sensors never
        // re-fire for an unchanged key).
        self.reading.renamed(from, to);
        self.pdf_note_closed(from);
        self.views.rehost(from, to);
        self.views.invalidate(&[]);
    }

    /// Plan note-identity, WP-4: every tab of document `id` now shows document `new` at `to`
    /// (`new == id` for a move; the absorber's id in the E2 tab move — the one place a tab's
    /// note id changes). Its `TabKind::Note` path follows, so navigation, history and
    /// persistence see `to`.
    fn retarget_note_tabs(&mut self, id: NoteId, new: NoteId, to: &str) {
        for (_, ps) in self.panes.iter_mut() {
            for t in ps.tabs.iter_mut().filter(|t| t.note == Some(id)) {
                t.note = Some(new);
                t.retarget(to);
            }
        }
    }
}
