//! File operations: bookmarks, explorer, Reading-view edits, title sync and rename retargeting, switcher and move note.

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
    pub(super) fn update_bookmarks(&mut self, m: BookmarkMsg) -> Task<Message> {
        match m {
            BookmarkMsg::Open(p) => return self.open_bookmark(&p),
            edit => {
                let active = self.active_note().map(|t| t.path.to_owned());
                let Some(f) = self.settings.file_mut(BOOKMARKS_PATH) else {
                    return Task::none();
                };
                if f.saving() {
                    return Task::none();
                }
                if f.dirty() {
                    self.status.set_error(
                        "bookmarks have unsaved edits on the Settings page (saving or invalid)"
                            .into(),
                    );
                    return Task::none();
                }
                let e = match &edit {
                    BookmarkMsg::SetKeys(path, keys) => f
                        .doc()
                        .and_then(|d| bookmarks::keys_edit(d, path, keys.as_deref())),
                    _ => bookmarks::doc_edit(&edit, &self.bookmarks.items, active.as_deref()),
                };
                let Some(e) = e else {
                    return Task::none();
                };
                if let Err(err) = self.settings.edit(BOOKMARKS_PATH, &e) {
                    self.status.set_error(format!("bookmarks: {err}"));
                    return Task::none();
                }
                // The shared settings write, landing in `SettingsMsg::Saved`.
                if self
                    .settings
                    .file(BOOKMARKS_PATH)
                    .is_some_and(crate::settings::ConfigFile::can_save)
                {
                    let task = self.save_settings_file(BOOKMARKS_PATH.to_owned());
                    self.on_settings_content(BOOKMARKS_PATH);
                    return task;
                }
                self.status.set_error(
                    "bookmarks: the file has form errors; fix them on the Settings page".into(),
                );
                if let (Some(d), Some(f)) =
                    (self.daemon.clone(), self.settings.file_mut(BOOKMARKS_PATH))
                {
                    f.load(d.as_ref());
                }
                self.on_settings_content(BOOKMARKS_PATH);
            }
        }
        Task::none()
    }

    /// Opens a bookmark (panel click, palette command or chord); a bookmark whose path the
    /// index does not hold gives a status notice instead of an error tab (plan
    /// editor-ui-round-3 WP-9).
    pub(super) fn open_bookmark(&mut self, path: &str) -> Task<Message> {
        if self.bookmarks.missing.contains(path) {
            self.status
                .set_error(format!("bookmark target missing: {path}"));
            return Task::none();
        }
        self.open(path);
        Task::none()
    }

    /// WP-9: recomputes which bookmarks the index lacks (load, `FileChanged`,
    /// `IndexGeneration`). Without a daemon nothing is known, so nothing is marked.
    pub(super) fn refresh_bookmark_missing(&mut self) {
        if self.daemon.is_none() {
            self.bookmarks.missing.clear();
            return;
        }
        let held = self.indexed_fn();
        self.bookmarks.refresh_missing(held);
    }

    pub(super) fn dispatch_op(&mut self, op: FileOp) -> Task<Message> {
        // Plan round-18 WP-2 (Decision 2 fast path): a held note (or a folder holding one) is
        // neither renamed nor deleted; the daemon refuses it too.
        if let Some(label) = self.held_by_op(&op) {
            self.notify_held(&label);
            return Task::none();
        }
        // A rename/move/trash re-reads (or closes) the open tab, which would drop unsaved edits.
        if let FileOp::Delete { path } = &op
            && self.doc_dirty(path)
        {
            // WP-2 (round 7): Save / Discard / Cancel, then the delete preview + confirm.
            self.dialog = Some(crate::dialog::Dialog::DeleteUnsaved(path.clone()));
            return Task::none();
        }
        if let FileOp::Rename { from, .. } = &op
            && self.doc_dirty(from)
        {
            self.status
                .set_error("save the open note first (it has unsaved edits)".into());
            return Task::none();
        }
        if let FileOp::Rename { from, to } = &op {
            self.explorer.expect_rename(from, to);
        }
        // Plan note-identity D4: a rename of an open note carries that document's id, so its
        // completion finds it wherever a notification consumed first already moved it.
        let note = match &op {
            FileOp::Rename { from, .. } => self.docs.id_of(from),
            FileOp::Create { .. } | FileOp::Delete { .. } => None,
        };
        let back = op.clone();
        self.rpc(
            move |d| op.run(d),
            move |result| {
                Message::Explorer(ExplorerMsg::Done(FileOpDone { op: back, result }, note))
            },
        )
    }

    pub(super) fn update_explorer(&mut self, m: ExplorerMsg) -> Task<Message> {
        match m {
            ExplorerMsg::Refresh => self.refresh_files(),
            ExplorerMsg::Toggle(f) => {
                if !self.ui.expanded.remove(&f) {
                    self.ui.expanded.insert(f.clone());
                }
                self.explorer.selected = Some(f);
                self.explorer.relayout(&self.ui.expanded);
            }
            ExplorerMsg::Select(p) => self.explorer.selected = Some(p),
            ExplorerMsg::Open(p) => {
                self.explorer.selected = Some(p.clone());
                self.open(&p);
            }
            ExplorerMsg::Scrolled(w) => self.explorer.window = w,
            ExplorerMsg::BeginNew | ExplorerMsg::BeginRename | ExplorerMsg::BeginMove => {
                let kind = match m {
                    ExplorerMsg::BeginNew => EditKind::New,
                    ExplorerMsg::BeginRename => EditKind::Rename,
                    _ => EditKind::Move,
                };
                if !self.explorer.begin(kind) {
                    self.status.set_error("select a file first".into());
                }
            }
            ExplorerMsg::Trash => {
                match self
                    .explorer
                    .selected
                    .clone()
                    .filter(|s| self.explorer.contains(s))
                {
                    // plan editor-ui-round-3 WP-3: always through the delete dialog.
                    Some(path) => return self.begin_delete(path),
                    None => self.status.set_error("select a file first".into()),
                }
            }
            ExplorerMsg::DeleteActive => match self.active_note().map(|t| t.path.to_owned()) {
                Some(path) => return self.begin_delete(path),
                None => self.status.set_error("no active note".into()),
            },
            ExplorerMsg::DeletePreview { path, result } => match result {
                Ok(items) => {
                    let c = crate::dialog::DeleteConfirm::new(path, self.prefs.delete_mode, items);
                    self.dialog = Some(crate::dialog::Dialog::DeleteConfirm(c));
                }
                Err(e) => self.status.set_error(e),
            },
            ExplorerMsg::NoteDeleted(result) => match result {
                Ok(r) => {
                    let indexed = self.indexed_fn();
                    for p in &r.removed {
                        let op = FileOp::Delete { path: p.clone() };
                        self.explorer.apply_op(&op, p, &self.ui.expanded, &indexed);
                        self.close_path(p);
                    }
                    if !r.skipped.is_empty() {
                        self.status.set_error(format!(
                            "kept {} item(s) linked elsewhere: {}",
                            r.skipped.len(),
                            r.skipped.join(", ")
                        ));
                    }
                }
                Err(e) => self.report_error(e),
            },
            ExplorerMsg::Input(s) => {
                if let Some(e) = &mut self.explorer.edit {
                    e.input = s;
                }
            }
            ExplorerMsg::Submit => match self.explorer.submit() {
                Ok(Some(op)) => return self.dispatch_op(op),
                Ok(None) => {}
                Err(e) => self.status.set_error(e),
            },
            ExplorerMsg::Cancel => self.explorer.edit = None,
            ExplorerMsg::Done(FileOpDone { op, result }, note) => match result {
                Ok(final_path) => {
                    let indexed = self.indexed_fn();
                    self.explorer
                        .apply_op(&op, &final_path, &self.ui.expanded, &indexed);
                    match &op {
                        FileOp::Create { .. } => self.open(&final_path),
                        FileOp::Rename { from, .. } => match note {
                            Some(id) => self.follow_doc(id, &final_path, Follow::Daemon),
                            None if !self.docs.contains_key(from) && self.path_open(from) => {
                                // Plan round-15 WP-2: only file tabs show a path without a
                                // document.
                                self.retarget_file(from, &final_path);
                            }
                            None => {}
                        },
                        FileOp::Delete { path } => self.close_path(path),
                    }
                }
                Err(e) => self.report_error(e),
            },
        }
        Task::none()
    }

    /// The one Reading-side buffer edit — applies `f` (one undoable edit; the
    /// toggle's read-only rule) to `path`'s open buffer; when it changed the text, re-renders
    /// now (not on the next debounce tick) and starts the normal autosave. `None` when `path`
    /// has no open buffer (the caller decides: a notice, or the daemon path).
    fn edit_buffer(
        &mut self,
        path: &str,
        f: impl FnOnce(&mut crate::source::SourceBuffer, std::time::Instant) -> bool,
    ) -> Option<Task<Message>> {
        if self.refuse_held_save(path) {
            // Plan round-17 WP-2: a held note takes no Reading / view edit.
            return Some(Task::none());
        }
        let od = self.docs.get_mut(path)?;
        if !f(&mut od.buffer, std::time::Instant::now()) {
            return Some(Task::none());
        }
        let (id, seq) = (od.buffer.note_id(), od.buffer.edit_seq());
        self.reading.recompute(od, self.daemon.as_ref());
        Some(crate::source::autosave_timer(id, seq))
    }

    /// [`Self::edit_buffer`] for a Reading / view control; a note with no open buffer says so
    /// (plan round-6-feedback-todos WP-6: the edit path is the buffer's — never a silent
    /// no-op).
    fn edit_open_buffer(
        &mut self,
        path: &str,
        f: impl FnOnce(&mut crate::source::SourceBuffer, std::time::Instant) -> bool,
    ) -> Task<Message> {
        self.edit_buffer(path, f).unwrap_or_else(|| {
            self.status.set_error(format!("open {path} to change it"));
            Task::none()
        })
    }

    /// plan editor-ui-round-3 WP-4: sets a host-note task's status char in `path`'s buffer.
    pub(super) fn set_task_status(&mut self, path: &str, line: usize, to: char) -> Task<Message> {
        self.edit_open_buffer(path, |d, now| d.set_task_status(line, to, now))
    }

    /// Toggles a host-note task box in `path`'s buffer.
    pub(super) fn toggle_task(&mut self, path: &str, line: usize) -> Task<Message> {
        self.edit_open_buffer(path, |d, now| d.toggle_task(line, now))
    }

    /// plan usability-sprint-captures-links WP-4: flips a frontmatter boolean in `path`'s
    /// buffer — the same edit → autosave → CAS-save path as `toggle_task`.
    pub(super) fn toggle_prop(&mut self, path: &str, key: &str) -> Task<Message> {
        self.edit_open_buffer(path, |d, now| d.set_frontmatter_bool(key, None, now))
    }

    /// Plan round-8-intake-views-anchor WP-3: a view checkbox cell. An open note's buffer gets
    /// the target value as one undoable edit (unsaved edits survive, autosave persists it); a
    /// closed note goes through the daemon's `note_set_bool`. No optimistic view state.
    pub(super) fn set_note_bool(
        &mut self,
        path: String,
        key: String,
        value: bool,
    ) -> Task<Message> {
        if let Some(t) = self.edit_buffer(&path, |d, now| {
            d.set_frontmatter_bool(&key, Some(value), now)
        }) {
            return t;
        }
        let p = cv_protocol::NoteSetBoolParams {
            id: None,
            path,
            key,
            value,
        };
        self.rpc(
            move |d| d.note_set_bool(&p).map_err(|e| crate::daemon::rpc_err(&e)),
            |r| Message::Views(crate::message::ViewsMsg::BoolSet(r)),
        )
    }

    /// Enter / blur in the filename input: rename through the explorer path when changed,
    /// flushing a dirty buffer through the normal save first.
    pub(super) fn title_submit(&mut self, path: &str) -> Task<Message> {
        let Some(input) = self.reading.take_title(&mut self.docs, path) else {
            return Task::none();
        };
        if self.refuse_held_save(path) {
            return Task::none();
        }
        let Some(to) = crate::reading::rename_target(path, &input) else {
            return Task::none();
        };
        if input.contains('/') {
            self.status.set_error("invalid name".into());
            return Task::none();
        }
        let Some(doc) = buf_mut(&mut self.docs, path) else {
            return Task::none();
        };
        if !doc.is_dirty() {
            return self.dispatch_op(FileOp::Rename {
                from: path.to_owned(),
                to,
            });
        }
        self.save_then(path, Some(AfterSave::Rename(to)))
            .unwrap_or_else(|| {
                self.status
                    .set_error("rename not done (the note is saving or has a conflict)".into());
                Task::none()
            })
    }

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

    /// Plan note-identity D4: a `file_changed` carrying note id `id` (now at `path`). Returns the
    /// document to reload (echo / silent reload / conflict bar / deleted state), if any.
    ///
    /// - A move (`from`) of a note no document holds still retargets holds and file tabs (by path).
    /// - `Deleted` whose path is not the document's current path is the source half of a rename
    ///   pair the document already followed (audit E1): the document is live elsewhere, nothing
    ///   happens to it. At its current path, `replaced_by` takes the E2 tab move, and otherwise
    ///   `locate_note` decides: moved → follow; still live → nothing; gone → today's deleted /
    ///   missing / replaced handling.
    /// - Any other kind at a path that is not the document's: a `Renamed` from the document's
    ///   path follows it; anything else is settled by `locate_note`, never by guessing.
    /// - A different note at the path of an open document (path reuse) never touches it.
    pub(super) fn doc_file_changed(
        &mut self,
        id: NoteId,
        path: &str,
        kind: cv_protocol::ChangeKind,
        from: Option<&str>,
        replaced_by: Option<&str>,
    ) -> Option<NoteId> {
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
            let located = self.daemon.clone().map(|d| d.locate_note(id));
            return match located {
                Some(Ok(NoteLocation::Live { path: to })) if to != path => {
                    self.follow_doc(id, &to, Follow::Daemon);
                    None
                }
                Some(Ok(NoteLocation::Live { .. })) => None,
                Some(Ok(NoteLocation::Gone {
                    replaced_by: Some(by),
                })) => match self.located_path(by) {
                    Some(to) if self.follow_replaced(id, &to) => None,
                    _ => {
                        self.doc_gone(id);
                        None
                    }
                },
                Some(Ok(NoteLocation::Gone { .. } | NoteLocation::Unknown)) => {
                    self.doc_gone(id);
                    None
                }
                // No answer (disconnected, an error): re-read the path as before.
                _ => Some(id),
            };
        }
        if cur != path {
            match from {
                Some(f) if f == cur => self.follow_doc(id, path, Follow::Daemon),
                _ => {
                    if let Some(to) = self.located_path(id)
                        && to != cur
                    {
                        self.follow_doc(id, &to, Follow::Daemon);
                    }
                }
            }
        }
        (self.docs.path_of(id).as_deref() == Some(path)).then_some(id)
    }

    /// Today's deleted handling for document `id` whose note is gone (D4: decided from the id,
    /// never by re-reading the path, which may hold another note now): unsaved edits enter the
    /// missing-file state (kept, Keep mine revives the id), a clean document says so.
    fn doc_gone(&mut self, id: NoteId) {
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

    /// Where note `id` lives now, when the daemon says it is live.
    pub(super) fn located_path(&self, id: NoteId) -> Option<String> {
        match self.daemon.as_ref()?.locate_note(id) {
            Ok(NoteLocation::Live { path }) => Some(path),
            _ => None,
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
    /// from disk. A `to` that is already gone (a delayed event of a chain) is settled by
    /// `locate_note`, never by guessing.
    pub(super) fn follow_doc(&mut self, id: NoteId, to: &str, how: Follow) {
        self.follow_doc_at(id, to, how, true);
    }

    fn follow_doc_at(&mut self, id: NoteId, to: &str, how: Follow, locate: bool) {
        let Some(from) = self.docs.path_of(id) else {
            return;
        };
        if from == to {
            return;
        }
        let Some(d) = self.daemon.clone() else { return };
        let (bytes, hash) = match d.read_file(to) {
            Ok(r) => r,
            Err(DaemonError::NotFound(p)) => {
                if locate && let Ok(NoteLocation::Live { path }) = d.locate_note(id) {
                    if path != to {
                        self.follow_doc_at(id, &path, how, false);
                    }
                    return;
                }
                self.status.set_error(open_error(&DaemonError::NotFound(p)));
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
            let mut b = crate::source::SourceBuffer::new(&from, &bytes, hash);
            b.set_note_id(id);
            od.buffer = b;
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
    /// dropped and the absorber's is used (opened and bound when it is not open yet) — never
    /// the retired document under a new path, whose saves would address a retired id. A dirty
    /// retired document is left to today's deleted-on-disk handling (`false`: the caller
    /// reloads; edits are never discarded).
    pub(super) fn follow_replaced(&mut self, id: NoteId, to: &str) -> bool {
        let Some(from) = self.docs.path_of(id) else {
            return false;
        };
        if from == to || self.docs.by_id(id).is_some_and(|d| d.buffer.is_dirty()) {
            return false;
        }
        // `to` is a path the daemon named: the same-path rule picks its open document.
        let open_absorber = self.docs.id_of(to);
        let absorber = if open_absorber.is_some() {
            // Already open (possibly dirty): its document stays; the tabs just show it.
            None
        } else {
            let Some(d) = self.daemon.clone() else {
                return false;
            };
            match super::tabs::bind_read_note(d.as_ref(), to) {
                Ok((aid, bytes, hash)) => {
                    let mut b = crate::source::SourceBuffer::new(to, &bytes, hash);
                    b.set_note_id(aid);
                    Some(OpenDoc {
                        id: aid,
                        buffer: b,
                        close_waiters: Vec::new(),
                        reading: crate::reading::DocReading::default(),
                    })
                }
                Err(e) => {
                    self.status.set_error(open_error(&e));
                    return false;
                }
            }
        };
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

    /// Plan usability-sprint-captures-links WP-5: after a save of note `id` landed, renames the
    /// note when its on-disk `title` changed and the filename was the old title's form. Not while
    /// closing/quitting, or while a follow-up save or another rename
    /// is in flight (the next landed save re-evaluates). A target that exists refuses the rename.
    pub(super) fn title_sync(&mut self, id: NoteId) -> Task<Message> {
        if self
            .docs
            .by_id(id)
            .is_none_or(|d| !d.close_waiters.is_empty())
            || self.quit.is_some()
        {
            return Task::none();
        }
        let Some(doc) = self.docs.by_id(id).map(|d| &d.buffer) else {
            return Task::none();
        };
        if doc.is_saving() || doc.is_renaming() {
            return Task::none();
        }
        let Some(to) = doc.title_rename() else {
            return Task::none();
        };
        let path = doc.path().to_owned();
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        // Case-only renames target the same file on a case-insensitive disk: no collision check.
        if to.to_lowercase() != path.to_lowercase()
            && !matches!(d.read_file(&to), Err(DaemonError::NotFound(_)))
        {
            if let Some(od) = self.docs.by_id_mut(id) {
                od.buffer.settle_title();
            }
            self.status
                .set_error(format!("title changed; not renamed: {to} already exists"));
            return Task::none();
        }
        let Some(doc) = self.docs.by_id_mut(id).map(|d| &mut d.buffer) else {
            return Task::none();
        };
        doc.set_renaming(true);
        let seq = doc.edit_seq();
        self.explorer.expect_rename(&path, &to);
        let target = to.clone();
        // Plan note-identity D4: the rename goes by id (the daemon renames wherever the note is
        // now) and its reply is addressed to the id.
        self.rpc(
            move |d| {
                d.rename_by_id(id, &target)
                    .map(|_| target)
                    .map_err(|e| crate::daemon::rpc_err(&e))
            },
            move |result| Message::TitleRenamed {
                id,
                from: path,
                to,
                seq,
                result,
            },
        )
    }

    /// WP-5: a title rename finished. The tab follows (edits typed while it was in flight are
    /// kept, behind the conflict bar); a failure leaves the tab and relaunches a held save.
    /// Plan note-identity D4: the document is found by `id` whatever path it has by now (a
    /// `Renamed` notification consumed first already moved it: the follow is then a no-op).
    pub(super) fn title_renamed(
        &mut self,
        id: NoteId,
        from: &str,
        to: &str,
        seq: u64,
        result: Result<String, String>,
    ) -> Task<Message> {
        let held = self
            .docs
            .by_id_mut(id)
            .is_some_and(|d| d.buffer.set_renaming(false));
        match result {
            Ok(final_path) => {
                let indexed = self.indexed_fn();
                let op = FileOp::Rename {
                    from: from.to_owned(),
                    to: to.to_owned(),
                };
                self.explorer
                    .apply_op(&op, &final_path, &self.ui.expanded, &indexed);
                let edited = self
                    .docs
                    .by_id(id)
                    .is_some_and(|d| d.buffer.is_dirty() || d.buffer.edit_seq() != seq);
                self.follow_doc(id, &final_path, Follow::Edited(edited));
                Task::none()
            }
            Err(e) => {
                if crate::daemon::held_label(&e).is_some() {
                    self.report_error(e);
                } else {
                    self.status
                        .set_error(format!("title changed; rename failed: {e}"));
                }
                let Some(doc) = self.docs.by_id_mut(id).map(|d| &mut d.buffer) else {
                    return Task::none();
                };
                doc.settle_title();
                if held {
                    crate::source::autosave_timer(Some(id), doc.edit_seq())
                } else {
                    Task::none()
                }
            }
        }
    }

    pub(super) fn update_switcher(&mut self, m: SwitcherMsg) -> Task<Message> {
        let s = &mut self.switcher;
        match m {
            SwitcherMsg::Open => {
                // Decision 9 (round-12): one centre modal at a time.
                self.palette.close();
                self.panels.search_modal = false;
                s.open = true;
                s.move_from = None;
                s.query.clear();
                s.selected = 0;
                s.refresh(&self.explorer.paths, &self.ui.recent);
                return iced::widget::operation::focus(switcher::INPUT_ID);
            }
            SwitcherMsg::OpenMove => return self.open_move_note(),
            SwitcherMsg::Close => s.open = false,
            // plan editor-ui-round-3 WP-5: two-stage Escape.
            SwitcherMsg::Escape if s.open && !s.query.is_empty() => {
                s.query.clear();
                s.selected = 0;
                s.refresh(&self.explorer.paths, &self.ui.recent);
                // plan editor-round-4 WP-1: iced unfocused the input on Escape.
                return iced::widget::operation::focus(switcher::INPUT_ID);
            }
            SwitcherMsg::Escape => s.open = false,
            SwitcherMsg::Query(q) => {
                s.query = q;
                s.selected = 0;
                s.refresh(&self.explorer.paths, &self.ui.recent);
            }
            SwitcherMsg::Up => s.selected = s.selected.saturating_sub(1),
            SwitcherMsg::Down => {
                if s.selected + 1 < s.results.len() {
                    s.selected += 1;
                }
            }
            SwitcherMsg::Submit => {
                if !s.open {
                    return Task::none();
                }
                if s.move_from.is_some() {
                    return match s.current().map(str::to_owned) {
                        Some(f) => self.move_note_to(&f),
                        None => Task::none(),
                    };
                }
                match s.current().map(str::to_owned) {
                    Some(p) if !self.modifiers.shift() => {
                        s.open = false;
                        self.open_with(&p, self.quick_switch_place());
                    }
                    _ => return self.update_switcher(SwitcherMsg::Create),
                }
            }
            SwitcherMsg::Pick(p) if s.move_from.is_some() => return self.move_note_to(&p),
            SwitcherMsg::Pick(p) => {
                s.open = false;
                self.open_with(&p, self.quick_switch_place());
            }
            SwitcherMsg::Create if s.move_from.is_some() => {}
            SwitcherMsg::Create => {
                let Some(path) = switcher::create_path(&self.switcher.query) else {
                    return Task::none();
                };
                return self.rpc(
                    move |d| {
                        d.run_action(switcher::create_params(&path))
                            .map(|r| r.open_path)
                            .map_err(|e| e.to_string())
                    },
                    |r| Message::Switcher(SwitcherMsg::Created(r)),
                );
            }
            SwitcherMsg::Created(r) => match r {
                Ok(Some(p)) => {
                    self.switcher.open = false;
                    self.open(&p);
                }
                Ok(None) => self.status.set_error("note.create returned no path".into()),
                Err(e) => self.status.set_error(format!("note.create: {e}")),
            },
        }
        Task::none()
    }

    /// WP-6 (usability-sprint-captures-links): "Move note…" — the switcher in folder mode for
    /// the active note.
    fn open_move_note(&mut self) -> Task<Message> {
        let Some(from) = self.active_note().map(|t| t.path.to_owned()) else {
            self.status.set_error("move note: no active note".into());
            return Task::none();
        };
        self.palette.close();
        let s = &mut self.switcher;
        s.folders = switcher::folder_candidates(&self.explorer.paths, &from);
        s.move_from = Some(from);
        s.open = true;
        s.query.clear();
        s.selected = 0;
        s.refresh(&self.explorer.paths, &self.ui.recent);
        iced::widget::operation::focus(switcher::INPUT_ID)
    }

    /// WP-6: moves the folder-mode note into `folder` (a [`switcher::folder_candidates`] label)
    /// through [`Self::dispatch_op`]; completion follows via `ExplorerMsg::Done` → `retarget`.
    fn move_note_to(&mut self, folder: &str) -> Task<Message> {
        let Some(from) = self.switcher.move_from.take() else {
            return Task::none();
        };
        self.switcher.open = false;
        let to = switcher::move_target(&from, folder);
        if to == from {
            return Task::none();
        }
        let Some(d) = self.daemon.clone() else {
            self.status.set_error("move note: not connected".into());
            return Task::none();
        };
        match d.read_file(&to) {
            Err(DaemonError::NotFound(_)) => {}
            _ => {
                self.status
                    .set_error(format!("move note: {to} already exists"));
                return Task::none();
            }
        }
        self.dispatch_op(FileOp::Rename { from, to })
    }
}
