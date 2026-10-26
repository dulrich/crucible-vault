//! Tabs and documents: accessors, opening paths into panes, history, tab selection and close.

use super::*;

impl App {
    /// The focused pane's active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.panes.get(self.focus).and_then(PaneState::active_tab)
    }

    /// Plan round-5 WP-1: the tab id showing `path` — the focused pane's active tab when it shows
    /// `path`, else the first pane whose active tab does (0 when none: ids start at 1).
    pub fn view_of(&self, path: &str) -> u64 {
        self.active_note()
            .filter(|t| t.path == path)
            .or_else(|| {
                self.panes
                    .iter()
                    .filter_map(|(_, ps)| ps.active_note())
                    .find(|t| t.path == path)
            })
            .map_or(0, |t| t.id)
    }

    /// Plan round-5 WP-1: every pane's active tab (id and kind).
    pub(super) fn active_views(&self) -> Vec<(u64, TabKind)> {
        self.panes
            .iter()
            .filter_map(|(_, ps)| ps.active_tab())
            .map(|t| (t.id, t.kind.clone()))
            .collect()
    }

    /// Plan round-5 WP-1 (tab blur/resume): iced widget state is positional and only active tab
    /// bodies are in the tree, so every tab that became active in this update gets its stored
    /// place re-applied explicitly — Reading (probe → `scroll_to`), Settings (body offset).
    /// Source re-applies its tracked top line on materialise (`SourceBuffer::shaped`).
    pub(super) fn restore_places(&mut self, before: &[(u64, TabKind)]) -> Task<Message> {
        let mut tasks = Vec::new();
        for (id, kind) in self.active_views() {
            if before.iter().any(|(b, _)| *b == id) {
                continue;
            }
            match kind {
                TabKind::Note(p) => {
                    tasks.extend(self.reading.restore(&mut self.docs, &p, id));
                    if self.reading.mode(&self.docs, &p).shows_reading() {
                        tasks.extend(self.views.restore(&p));
                    }
                }
                TabKind::Settings => tasks.push(self.settings.restore_scroll()),
                // The Log tab has no stored place (was Settings' restore).
                TabKind::Log => {}
            }
        }
        Task::batch(tasks)
    }

    /// The focused pane's active tab when it is a note (never the Settings tab).
    pub fn active_note(&self) -> Option<NoteRef<'_>> {
        self.panes.get(self.focus).and_then(PaneState::active_note)
    }

    /// The document open at `path` (plan editor-feedback-round-2 WP-7).
    pub fn doc(&self, path: &str) -> Option<&crate::source::SourceBuffer> {
        buf(&self.docs, path)
    }

    /// The document open at `path`, mutably.
    pub fn doc_mut(&mut self, path: &str) -> Option<&mut crate::source::SourceBuffer> {
        buf_mut(&mut self.docs, path)
    }

    /// The focused pane's active note's document.
    pub fn active_doc(&self) -> Option<&crate::source::SourceBuffer> {
        self.active_note().and_then(|t| buf(&self.docs, t.path))
    }

    /// The focused pane's active note's document, mutably.
    pub fn active_doc_mut(&mut self) -> Option<&mut crate::source::SourceBuffer> {
        let path = self.active_note()?.path.to_owned();
        buf_mut(&mut self.docs, &path)
    }

    /// Tests: the note saves a quit in progress waits on (started + in flight), per document.
    #[cfg(test)]
    pub(crate) fn quit_waits(&self) -> Option<usize> {
        self.quit
            .as_ref()
            .map(|q| q.pending.len() + q.awaiting.len())
    }

    /// Whether the document at `path` has unsaved edits (false when none is open).
    pub(super) fn doc_dirty(&self, path: &str) -> bool {
        buf(&self.docs, path).is_some_and(|d| d.is_dirty())
    }

    /// Whether any tab shows `path`.
    pub(super) fn path_open(&self, path: &str) -> bool {
        panes::find(&self.panes, path).is_some()
    }

    /// The iced scale factor (`editor.toml` `ui_scale`, clamped 0.5–3.0).
    pub fn scale_factor(&self) -> f32 {
        let (lo, hi) = crate::settings::editor_prefs::ui_scale_range();
        self.prefs.ui_scale.clamp(lo, hi)
    }

    /// Every open note path, pane by pane in layout order.
    pub fn open_paths(&self) -> Vec<String> {
        panes::order(&self.panes)
            .into_iter()
            .filter_map(|p| self.panes.get(p))
            .flat_map(|s| s.tabs.iter().filter_map(Tab::path).map(str::to_owned))
            .collect()
    }

    /// Splits the focused pane to the right and focuses the new pane (unchanged focus when the
    /// split fails).
    fn split_focused(&mut self) {
        if let Some((p, _)) = self.panes.split(
            panes::axis_of(SplitDir::Right),
            self.focus,
            PaneState::default(),
        ) {
            self.focus = p;
        }
    }

    /// "Open in new pane": a second tab of `path` in a new pane right of the focused one.
    pub(super) fn open_in_new_pane(&mut self, path: &str) {
        self.split_focused();
        self.open_with(path, Place::Duplicate);
    }

    /// Opens `path` into the focused pane (focusing an existing tab anywhere). The read is a
    /// local disk read of one note, done inline; RPC calls never run here.
    pub(super) fn open(&mut self, path: &str) {
        self.open_with(path, Place::Focus);
    }

    /// The nav bar's placement (plan round-14 WP-2): Ctrl/Cmd+click always adds a tab, else the
    /// `nav_open` pref.
    pub(super) fn nav_place(&self, new_tab: bool) -> Place {
        use crate::settings::editor_prefs::NavOpen;
        if new_tab {
            return Place::NewTab;
        }
        match self.prefs.nav_open {
            NavOpen::NewTab => Place::Focus,
            NavOpen::Replace => Place::Replace,
        }
    }

    /// [`Self::open`] with an explicit placement (Quick Switch modes).
    pub(super) fn open_with(&mut self, path: &str, how: Place) {
        if let Err(e) = self.open_into(self.focus, path, how) {
            self.status.set_error(open_error(&e));
        }
    }

    /// Focuses tab `i` of `pane` (recording the visit).
    pub(super) fn focus_tab(&mut self, pane: Pane, i: usize, path: &str) {
        self.focus = pane;
        if let Some(ps) = self.panes.get_mut(pane) {
            ps.activate(i);
        }
        self.record_visit(pane, path);
    }

    /// Plan round-10 WP-8: moves the tab at `from` into pane `to` — at index `at` (clamped; for
    /// the tab's own pane, its index once removed) or appended — and focuses it. The `Tab` value
    /// (id, kind) moves, so its shared document (dirty buffer, undo) and id-keyed view state
    /// travel with it. A source pane left empty stays (as after a close). False when `from` or
    /// `to` is missing.
    pub(super) fn move_tab(&mut self, from: (Pane, usize), to: Pane, at: Option<usize>) -> bool {
        if self.panes.get(to).is_none() {
            return false;
        }
        let Some(tab) = self.panes.get_mut(from.0).and_then(|ps| ps.remove(from.1)) else {
            return false;
        };
        let path = tab.path().map(str::to_owned);
        let Some(ps) = self.panes.get_mut(to) else {
            return false;
        };
        let n = at.map_or(ps.tabs.len(), |a| a.min(ps.tabs.len()));
        ps.tabs.insert(n, tab);
        self.focus = to;
        if let Some(ps) = self.panes.get_mut(to) {
            ps.activate(n);
        }
        if let Some(path) = path {
            self.record_visit(to, &path);
        }
        true
    }

    /// Plan round-10 WP-8: the tab-drag messages (see [`tab_drag`]).
    fn update_tab_drag(&mut self, m: TabsMsg) {
        use super::tab_drag::Outcome;
        match m {
            TabsMsg::DragPress(pane, i) => {
                self.select_tab(pane, i);
                self.tab_drag.press((pane, i));
            }
            TabsMsg::DragMove(p) => {
                self.tab_drag.moved(p);
            }
            TabsMsg::DragEnter(t) => self.tab_drag.enter(t),
            TabsMsg::DragLeave(t) => self.tab_drag.leave(t),
            TabsMsg::DragCancel => self.tab_drag.cancel(),
            TabsMsg::HeaderEnter(id) => self.tab_drag.header_enter(id),
            TabsMsg::HeaderLeave(id) => self.tab_drag.header_leave(id),
            TabsMsg::DragRelease => {
                if let Outcome::Drop { from, to, at } = self.tab_drag.release() {
                    self.move_tab(from, to, at);
                }
            }
            _ => {}
        }
    }

    /// Opens `path` in `pane` per `how` (see [`Place`]). A note open nowhere is read into a new
    /// document; a further tab of an open note shares its document (plan
    /// editor-feedback-round-2 WP-7).
    pub(super) fn open_into(
        &mut self,
        pane: Pane,
        path: &str,
        how: Place,
    ) -> Result<(), DaemonError> {
        let pane = if self.panes.get(pane).is_some() {
            pane
        } else {
            self.focus
        };
        if !matches!(how, Place::Restore | Place::NewTab) {
            // In every other mode a tab already in the target pane is just focused.
            let here = self
                .panes
                .get(pane)
                .and_then(|ps| ps.tabs.iter().position(|t| t.shows(path)));
            if let Some(i) = here {
                self.focus_tab(pane, i, path);
                return Ok(());
            }
            match (how, panes::find(&self.panes, path)) {
                (Place::Focus | Place::Replace, Some((p, i))) => {
                    self.focus_tab(p, i, path);
                    return Ok(());
                }
                (Place::Move, Some((p, i))) => {
                    // The tab (and so its shared document: edits, undo) moves pane.
                    self.move_tab((p, i), pane, None);
                    return Ok(());
                }
                _ => {}
            }
        }
        if !self.docs.contains_key(path) {
            let Some(d) = &self.daemon else {
                return Err(DaemonError::Disconnected);
            };
            let (bytes, hash) = d.read_file(path)?;
            let doc = crate::source::SourceBuffer::new(path, &bytes, hash);
            let mode = self.initial_mode(path);
            let mut od = OpenDoc {
                buffer: doc,
                close_waiters: Vec::new(),
                reading: crate::reading::DocReading::default(),
            };
            self.reading
                .register(&mut od, mode, !self.restoring, self.daemon.as_ref());
            self.docs.insert(path.to_owned(), od);
        }
        let tab = Tab::note(path);
        let mut replaced = None;
        if let Some(ps) = self.panes.get_mut(pane) {
            let active = ps.active.filter(|&i| i < ps.tabs.len());
            match active {
                Some(i)
                    if how == Place::Replace
                        && !ps.tabs[i]
                            .path()
                            .and_then(|p| self.docs.get(p))
                            .is_some_and(|d| d.buffer.is_dirty()) =>
                {
                    replaced = Some(std::mem::replace(&mut ps.tabs[i], tab));
                    ps.activate(i);
                }
                _ => {
                    ps.tabs.push(tab);
                    ps.activate(ps.tabs.len() - 1);
                }
            }
        }
        self.focus = pane;
        if let Some(old) = replaced {
            self.on_tab_closed(&old);
        }
        self.record_visit(pane, path);
        self.ui.touch_recent(path);
        Ok(())
    }

    /// Pushes note `path` onto `pane`'s history (not while restoring or stepping
    /// back/forward).
    fn record_visit(&mut self, pane: Pane, path: &str) {
        if self.restoring || self.history_nav {
            return;
        }
        if let Some(ps) = self.panes.get_mut(pane) {
            ps.history.push(path);
        }
    }

    /// Back (`forward = false`) or forward in `pane`'s history: the target path is focused or
    /// reopened in that pane without pushing.
    pub(super) fn history_step(&mut self, pane: Pane, forward: bool) {
        let Some(ps) = self.panes.get_mut(pane) else {
            return;
        };
        let target = if forward {
            ps.history.forward()
        } else {
            ps.history.back()
        };
        let Some(path) = target.map(str::to_owned) else {
            return;
        };
        self.history_nav = true;
        let r = self.open_into(pane, &path, Place::Focus);
        self.history_nav = false;
        if let Err(e) = r {
            self.status.set_error(open_error(&e));
        }
    }

    /// Every tab removal funnels through here (button, pane close, replace, trash). A note's
    /// document and its reading/view state are dropped (one `remove` each) only with the last
    /// tab showing its path (plan editor-feedback-round-2 WP-7).
    pub(super) fn on_tab_closed(&mut self, tab: &Tab) {
        let Some(path) = tab.path() else {
            return;
        };
        self.sink.closed(path);
        if self.path_open(path) {
            return;
        }
        self.docs.remove(path);
        self.reading.closed(path);
        self.views.on_tab_closed(path);
    }

    /// `file_changed`: echo / silent reload / conflict bar (WP-2 `source::file_changed`).
    pub(super) fn reload_tab(&mut self, path: &str, hash: Option<&str>) {
        let daemon = self.daemon.as_ref();
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        if let Some(e) = crate::source::file_changed(&mut doc.buffer, hash, daemon) {
            self.status.set_error(e);
        }
    }

    pub(super) fn update_tabs(&mut self, m: TabsMsg) -> Task<Message> {
        match m {
            TabsMsg::Open(p) => self.open(&p),
            TabsMsg::OpenNew(p) => self.open_with(&p, Place::NewTab),
            TabsMsg::Select(pane, i) => self.select_tab(pane, i),
            TabsMsg::GoTo(n) => {
                let i = usize::from(n.saturating_sub(1));
                if n >= 1
                    && self
                        .panes
                        .get(self.focus)
                        .is_some_and(|ps| i < ps.tabs.len())
                {
                    self.select_tab(self.focus, i);
                }
            }
            TabsMsg::Close(pane, i) => {
                if let Some(id) = self.tab_at(Some((pane, i))).map(|t| t.id) {
                    return self.close_tab(id);
                }
            }
            TabsMsg::CloseActive => {
                if let Some(id) = self.active_tab().map(|t| t.id) {
                    return self.close_tab(id);
                }
            }
            TabsMsg::Quit => return self.quit(),
            TabsMsg::FlushAll => return self.flush_all_notes(),
            m => self.update_tab_drag(m),
        }
        Task::none()
    }

    /// Exits once every quit flush (notes and settings) has landed; anything that could not
    /// be saved opens the unsaved-edits dialog instead (plan editor-data-safety WP-2).
    pub(super) fn quit_progress(&mut self) -> Task<Message> {
        let Some(q) = &self.quit else {
            return Task::none();
        };
        if q.pending.is_empty() && q.settings.is_empty() && q.awaiting.is_empty() {
            let losses = self.quit.take().map(|q| q.losses).unwrap_or_default();
            if losses.is_empty() {
                return self.exit();
            }
            self.dialog = Some(crate::dialog::Dialog::Unsaved {
                losses,
                then: crate::dialog::Discard::Quit,
            });
        }
        Task::none()
    }

    fn select_tab(&mut self, pane: Pane, i: usize) {
        if let Some(ps) = self.panes.get_mut(pane)
            && i < ps.tabs.len()
        {
            ps.activate(i);
            let path = ps.tabs[i].path().map(str::to_owned);
            self.focus = pane;
            if let Some(path) = path {
                self.record_visit(pane, &path);
            }
        }
    }

    /// The tab at `at` (`(pane, index)`, e.g. from [`panes::find`]).
    pub(super) fn tab_at(&self, at: Option<(Pane, usize)>) -> Option<&Tab> {
        let (pane, i) = at?;
        self.panes.get(pane)?.tabs.get(i)
    }

    /// Removes the tab with id `id` now (no save; [`Self::on_tab_closed`] runs). False when
    /// no tab has that id.
    pub(super) fn remove_tab(&mut self, id: u64) -> bool {
        let Some((pane, i)) = panes::find_id(&self.panes, id) else {
            return false;
        };
        match self.panes.get_mut(pane).and_then(|ps| ps.remove(i)) {
            Some(tab) => {
                self.on_tab_closed(&tab);
                true
            }
            None => false,
        }
    }

    /// Close the tab with id `id`; a dirty buffer is flushed through the normal CAS save first
    /// and the close happens only once that save lands with nothing newer typed (Decisions).
    /// Plan editor-feedback-round-2 WP-7 (audit E1): the close is tracked by the tab's id, so
    /// with two tabs of one note the save closes exactly the tab asked for.
    pub(super) fn close_tab(&mut self, id: u64) -> Task<Message> {
        let Some(tab) = self.tab_at(panes::find_id(&self.panes, id)).cloned() else {
            return Task::none();
        };
        let path = match tab.kind {
            // WP-2: closing the Settings tab flushes pending settings writes (the model and
            // its save loops outlive the tab, so the writes land after the close).
            TabKind::Settings => {
                self.remove_tab(id);
                return self.flush_settings().1;
            }
            TabKind::Log => {
                self.remove_tab(id);
                return Task::none();
            }
            TabKind::Note(p) => p,
        };
        let Some(doc) = buf_mut(&mut self.docs, &path) else {
            self.remove_tab(id);
            return Task::none();
        };
        if !doc.is_dirty() {
            self.remove_tab(id);
            return Task::none();
        }
        if doc.conflict().is_some() || !self.status.connected {
            self.close_dialog(&path, vec![id], "not connected");
            return Task::none();
        }
        if let Some(t) = self.save_then(&path, Some(AfterSave::CloseTab(id))) {
            return t;
        }
        // WP-3 rework: a save is in flight (`begin_save` queued the follow-up); the close
        // completes when it lands clean (`note_save_landed`).
        match self.docs.get_mut(&path) {
            Some(d) if d.buffer.is_saving() => {
                if !d.close_waiters.contains(&id) {
                    d.close_waiters.push(id);
                }
            }
            // A dirty buffer whose save cannot start.
            _ => self.close_dialog(&path, vec![id], "the save could not start"),
        }
        Task::none()
    }

    /// The Quick Switch placement from `editor.toml` `quick_switch_open` (plan
    /// editor-feedback-round-2 WP-7).
    pub(super) fn quick_switch_place(&self) -> Place {
        use crate::settings::editor_prefs::QuickSwitchOpen;
        match self.prefs.quick_switch_open {
            QuickSwitchOpen::Switch => Place::Focus,
            QuickSwitchOpen::Move => Place::Move,
            QuickSwitchOpen::Duplicate => Place::Duplicate,
        }
    }

    /// Closes every tab on `path` (trashed files).
    pub(super) fn close_path(&mut self, path: &str) {
        while let Some((pane, i)) = panes::find(&self.panes, path) {
            match self.panes.get_mut(pane).and_then(|ps| ps.remove(i)) {
                Some(t) => self.on_tab_closed(&t),
                None => break,
            }
        }
    }

    /// Opens the singleton tab `kind` (Settings, Log) in `pane`, or focuses it where it is
    /// already open (at most one of each).
    /// Opening the Log resets its unread count; Settings re-syncs its view
    /// file list. A note `kind` opens through [`Self::open_into`].
    pub(super) fn open_singleton(&mut self, kind: TabKind, pane: Pane) {
        match &kind {
            TabKind::Log => self.status.log.unread = 0,
            TabKind::Settings => {}
            TabKind::Note(p) => {
                let p = p.clone();
                if let Err(e) = self.open_into(pane, &p, Place::Focus) {
                    self.status.set_error(open_error(&e));
                }
                return;
            }
        }
        if let Some((p, i)) = panes::find_kind(&self.panes, &kind) {
            self.focus = p;
            if let Some(ps) = self.panes.get_mut(p) {
                ps.activate(i);
            }
            return;
        }
        let pane = if self.panes.get(pane).is_some() {
            pane
        } else {
            self.focus
        };
        let settings = kind == TabKind::Settings;
        if let Some(ps) = self.panes.get_mut(pane) {
            ps.tabs.push(Tab::new(kind));
            ps.activate(ps.tabs.len() - 1);
        }
        self.focus = pane;
        if settings {
            self.sync_view_files();
        }
    }
}
