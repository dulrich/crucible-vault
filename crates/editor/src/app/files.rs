//! File operations: bookmarks, explorer, Reading-view edits, title sync and rename retargeting, switcher and move note.

use super::*;

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
        let back = op.clone();
        self.rpc(
            move |d| op.run(d),
            move |result| Message::Explorer(ExplorerMsg::Done(FileOpDone { op: back, result })),
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
                Err(e) => self.status.set_error(e),
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
            ExplorerMsg::Done(FileOpDone { op, result }) => match result {
                Ok(final_path) => {
                    let indexed = self.indexed_fn();
                    self.explorer
                        .apply_op(&op, &final_path, &self.ui.expanded, &indexed);
                    match &op {
                        FileOp::Create { .. } => self.open(&final_path),
                        FileOp::Rename { from, to } => self.retarget(from, to),
                        FileOp::Delete { path } => self.close_path(path),
                    }
                }
                Err(e) => self.status.set_error(e),
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
        let od = self.docs.get_mut(path)?;
        if !f(&mut od.buffer, std::time::Instant::now()) {
            return Some(Task::none());
        }
        let seq = od.buffer.edit_seq();
        self.reading.recompute(od, self.daemon.as_ref());
        Some(crate::source::autosave_timer(path.to_owned(), seq))
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
        let p = cv_protocol::NoteSetBoolParams { path, key, value };
        self.rpc(
            move |d| d.note_set_bool(&p).map_err(|e| e.to_string()),
            |r| Message::Views(crate::message::ViewsMsg::BoolSet(r)),
        )
    }

    /// Enter / blur in the filename input: rename through the explorer path when changed,
    /// flushing a dirty buffer through the normal save first.
    pub(super) fn title_submit(&mut self, path: &str) -> Task<Message> {
        let Some(input) = self.reading.take_title(&mut self.docs, path) else {
            return Task::none();
        };
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

    /// Round 9 WP-3 (Decision 4): a daemon-side move of `from` to `to` (the `Renamed { from }`
    /// notification) retargets every open tab. Bytes unchanged → re-keyed in place, dirty text
    /// and all, with no deleted / missing state; bytes changed under unsaved edits → the
    /// edited-move path (text kept behind the conflict bar).
    pub(super) fn follow_daemon_move(&mut self, from: &str, to: &str) {
        if from == to || !self.path_open(from) {
            return;
        }
        let base = buf(&self.docs, from)
            .filter(|b| b.is_dirty())
            .map(|b| b.base_hash().clone());
        let edited_elsewhere = base.is_some_and(|base| {
            self.daemon
                .as_ref()
                .and_then(|d| d.read_file(to).ok())
                .is_some_and(|(_, h)| h != base)
        });
        self.follow_moved(from, to, edited_elsewhere);
    }

    /// Round 11 WP-1 (Decision 1): `from` was deleted because `to` absorbed it (the
    /// `file_changed { deleted, replaced_by }` of auto-file's trash). Unless `from` has unsaved
    /// edits (left to the deleted-on-disk conflict path), a tab of `from` in a pane already
    /// showing `to` is closed and every other tab of `from` retargets to `to`; an open `to`
    /// document is kept as is (see [`Self::move_path_state`]).
    pub(super) fn follow_replaced(&mut self, from: &str, to: &str) {
        if from == to
            || !self.path_open(from)
            || buf(&self.docs, from).is_some_and(|b| b.is_dirty())
        {
            return;
        }
        let dup: Vec<u64> = self
            .panes
            .iter()
            .filter(|(_, ps)| ps.tabs.iter().any(|t| t.shows(to)))
            .flat_map(|(_, ps)| ps.tabs.iter().filter(|t| t.shows(from)).map(|t| t.id))
            .collect();
        for id in dup {
            self.remove_tab(id);
        }
        self.retarget(from, to);
    }

    /// A renamed file's open tabs follow it (re-read at the new path). Plan
    /// editor-feedback-round-2 WP-7: the document key and every tab of `from` move to `to`
    /// (tab ids kept); `from`'s reading / title / image-pin state is torn down. Plan
    /// editor-feedback-round-2 WP-8: the explorer rename, the breadcrumb rename and an action's
    /// `move` (via [`Self::follow_open`]) all end here.
    pub(super) fn retarget(&mut self, from: &str, to: &str) {
        if !self.path_open(from) {
            return;
        }
        if !self.docs.contains_key(from) {
            // Plan round-15 WP-2: only image tabs show a path without a document.
            self.retarget_image(from, to);
            return;
        }
        let Some(d) = &self.daemon else { return };
        match d.read_file(to) {
            Ok((bytes, hash)) => {
                // WP-2 (round 7): an unedited buffer whose bytes the move kept is re-keyed in
                // place (cursor, selection, undo, top_line survive); otherwise rebuild from disk.
                let doc = match self.docs.remove(from) {
                    Some(mut doc) if *doc.buffer.base_hash() == hash => {
                        doc.buffer.rename_clean(to);
                        doc
                    }
                    old => OpenDoc {
                        buffer: crate::source::SourceBuffer::new(to, &bytes, hash),
                        close_waiters: old
                            .as_ref()
                            .map(|d| d.close_waiters.clone())
                            .unwrap_or_default(),
                        // The Reading state moves with the rename even when the buffer is rebuilt.
                        reading: old.map(|d| d.reading).unwrap_or_default(),
                    },
                };
                self.move_path_state(from, to, doc);
            }
            Err(e) => self.status.set_error(open_error(&e)),
        }
    }

    /// The shared tail of every rename follow: every tab of `from` (and its history) moves to
    /// `to`, `doc` is installed under `to`, per-path state is re-keyed. Hosted view results move
    /// with the host and go stale (re-run against `to` while the old result stays shown — no
    /// flash), and every other view goes stale too, since a rename changes link targets they
    /// may list (WP-8).
    pub(super) fn move_path_state(&mut self, from: &str, to: &str, mut doc: OpenDoc) {
        for (_, ps) in self.panes.iter_mut() {
            for t in ps.tabs.iter_mut().filter(|t| t.shows(from)) {
                t.retarget(to);
            }
            ps.history.retarget(from, to);
        }
        // The document (close waiters, Reading state) moves as one; its pending filename edit
        // is dropped, the `from` image pins are released, and the new text renders now under
        // `to`.
        doc.reading.clear_title();
        self.reading.renamed(from);
        self.views.rehost(from, to);
        self.views.invalidate(&[]);
        if self.docs.contains_key(to) {
            // Round 11 WP-1 (audit E2): `to` is already open (possibly dirty, e.g. the note that
            // absorbed `from`): its document stays; `from`'s tabs now just show it.
            self.ui.touch_recent(to);
            return;
        }
        if doc.reading.mode.shows_reading() {
            self.reading.recompute(&mut doc, self.daemon.as_ref());
        }
        self.docs.insert(to.to_owned(), doc);
        self.ui.touch_recent(to);
    }

    /// Plan usability-sprint-captures-links WP-5: after a save of `path` landed, renames the note
    /// when its on-disk `title` changed and the filename was the old title's form. Not while
    /// closing/quitting, or while a follow-up save or another rename
    /// is in flight (the next landed save re-evaluates). A target that exists refuses the rename.
    pub(super) fn title_sync(&mut self, path: &str) -> Task<Message> {
        if self
            .docs
            .get(path)
            .is_none_or(|d| !d.close_waiters.is_empty())
            || self.quit.is_some()
        {
            return Task::none();
        }
        let Some(doc) = buf(&self.docs, path) else {
            return Task::none();
        };
        if doc.is_saving() || doc.is_renaming() {
            return Task::none();
        }
        let Some(to) = doc.title_rename() else {
            return Task::none();
        };
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        // Case-only renames target the same file on a case-insensitive disk: no collision check.
        if to.to_lowercase() != path.to_lowercase()
            && !matches!(d.read_file(&to), Err(DaemonError::NotFound(_)))
        {
            if let Some(doc) = buf_mut(&mut self.docs, path) {
                doc.settle_title();
            }
            self.status
                .set_error(format!("title changed; not renamed: {to} already exists"));
            return Task::none();
        }
        let Some(doc) = buf_mut(&mut self.docs, path) else {
            return Task::none();
        };
        doc.set_renaming(true);
        let seq = doc.edit_seq();
        self.explorer.expect_rename(path, &to);
        let op = FileOp::Rename {
            from: path.to_owned(),
            to: to.clone(),
        };
        let from = path.to_owned();
        self.rpc(
            move |d| op.run(d),
            move |result| Message::TitleRenamed {
                from,
                to,
                seq,
                result,
            },
        )
    }

    /// WP-5: a title rename finished. The tab follows (edits typed while it was in flight are
    /// kept, behind the conflict bar); a failure leaves the tab and relaunches a held save.
    pub(super) fn title_renamed(
        &mut self,
        from: &str,
        to: &str,
        seq: u64,
        result: Result<String, String>,
    ) -> Task<Message> {
        let held = self
            .docs
            .get_mut(from)
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
                    .get(from)
                    .is_some_and(|d| d.buffer.is_dirty() || d.buffer.edit_seq() != seq);
                self.follow_moved(from, to, edited);
                Task::none()
            }
            Err(e) => {
                self.status
                    .set_error(format!("title changed; rename failed: {e}"));
                let Some(doc) = buf_mut(&mut self.docs, from) else {
                    return Task::none();
                };
                doc.settle_title();
                if held {
                    crate::source::autosave_timer(from.to_owned(), doc.edit_seq())
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
                self.search_modal = false;
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
