//! Config and persisted state: UI state, bookmarks/settings loads, editor prefs, hotkeys and the Settings tab.

use super::*;

impl App {
    pub(super) fn today(&self) -> NaiveDate {
        self.today
            .unwrap_or_else(|| chrono::Local::now().date_naive())
    }

    /// Index membership for explorer inserts (plan editor-ui-round-3 WP-2, audit B1). Without
    /// a daemon (tests without one) nothing is held.
    pub(super) fn indexed_fn(&self) -> impl Fn(&str) -> bool + use<> {
        let d = self.daemon.clone();
        move |p: &str| d.as_ref().is_some_and(|d| d.index().holds(p))
    }

    pub(super) fn refresh_files(&mut self) {
        let Some(d) = &self.daemon else { return };
        if let Err(e) = self.explorer.build(d.index(), &self.ui.expanded) {
            self.status.set_error(e);
        }
        if self.switcher.open {
            self.switcher.refresh(&self.explorer.paths, &self.ui.recent);
        }
    }

    /// (Re)loads `bookmarks.toml` into its shared settings [`ConfigFile`] (unless the Bookmarks
    /// page holds unsaved edits) and re-derives the panel list.
    pub(super) fn load_bookmarks(&mut self) {
        let Some(d) = self.daemon.clone() else { return };
        if let Some(f) = self.settings.file_mut(BOOKMARKS_PATH) {
            if !f.dirty() {
                f.load(d.as_ref());
            }
            if let crate::settings::LoadState::Malformed(e) | crate::settings::LoadState::Error(e) =
                &f.state()
            {
                self.status.set_error(format!("bookmarks: {e}"));
            }
        }
        self.on_settings_content(BOOKMARKS_PATH);
    }

    /// Plan round-5 WP-6: issues the picker lookups Settings wants (each off the UI thread via
    /// [`Self::rpc`]); geocode first waits out its debounce.
    fn settings_lookups(&mut self) -> Task<Message> {
        use crate::settings::lookups::{GEO_DEBOUNCE_MS, LookupMsg, Want};
        if self.daemon.is_none() {
            return Task::none();
        }
        let lm = |m: LookupMsg| Message::Settings(crate::settings::SettingsMsg::Lookup(m));
        let mut tasks = Vec::new();
        for w in self.settings.lookup_wants() {
            tasks.push(match w {
                Want::Currencies => self.rpc(
                    |d| d.lookup_currencies().map_err(|e| e.to_string()),
                    move |r| lm(LookupMsg::Currencies(r)),
                ),
                Want::Models(p) => {
                    let p2 = p.clone();
                    self.rpc(
                        move |d| d.lookup_models(&p2).map_err(|e| e.to_string()),
                        move |r| lm(LookupMsg::Models(p, r)),
                    )
                }
                Want::Channels(ids) => {
                    let ids2 = ids.clone();
                    self.rpc(
                        move |d| d.lookup_channels(&ids2).map_err(|e| e.to_string()),
                        move |r| lm(LookupMsg::Channels(ids, r)),
                    )
                }
                Want::GeoDebounce(seq) => {
                    if let RpcExec::Queue(q) = &mut self.rpc_exec {
                        // Tests: the debounce elapses at the next drain.
                        q.push_back(lm(LookupMsg::GeoDue(seq)));
                        Task::none()
                    } else {
                        Task::perform(
                            async move {
                                tokio::time::sleep(std::time::Duration::from_millis(
                                    GEO_DEBOUNCE_MS,
                                ))
                                .await;
                            },
                            move |()| lm(LookupMsg::GeoDue(seq)),
                        )
                    }
                }
            });
        }
        Task::batch(tasks)
    }

    /// Folds the live pane grid and view modes into `self.ui`, the snapshot
    /// `editor.json` holds (the WP-1 flat `tabs` / `active_tab` are read-only: never written).
    pub(super) fn sync_ui_state(&mut self) {
        self.ui.panes = Some(panes::snapshot(&self.panes, self.focus));
        self.ui.tabs.clear();
        self.ui.active_tab = None;
        self.ui.modes = self
            .panes
            .iter()
            .flat_map(|(_, ps)| ps.tabs.iter().filter_map(Tab::path))
            .map(|p| (p.to_owned(), self.reading.mode(&self.docs, p)))
            .collect();
    }

    /// After every update, a UI-state snapshot that differs from the last one
    /// written schedules one debounced write ([`PERSIST_DEBOUNCE`]; one pending at a time).
    /// Tests drive the debounce with [`Message::PersistDue`].
    pub(super) fn schedule_persist(&mut self) -> Task<Message> {
        self.sync_ui_state();
        if self.persist_pending || self.persisted.as_ref() == Some(&self.ui) {
            return Task::none();
        }
        self.persist_pending = true;
        if matches!(self.rpc_exec, RpcExec::Queue(_)) {
            return Task::none();
        }
        Task::perform(async { tokio::time::sleep(PERSIST_DEBOUNCE).await }, |()| {
            Message::PersistDue
        })
    }

    /// Writes the UI-state snapshot now (synchronously, on the UI thread) when it differs from
    /// the last one written: the debounce tick and exit.
    pub(super) fn write_ui_state(&mut self) {
        self.persist_pending = false;
        self.sync_ui_state();
        if self.persisted.as_ref() == Some(&self.ui) {
            return;
        }
        let Some(path) = &self.state_path else { return };
        self.ui_writes += 1;
        let vault = self.daemon.as_ref().map(|d| d.vault_root().to_path_buf());
        match ui_state::save(path, &self.ui, vault.as_deref()) {
            Ok(()) => self.persisted = Some(self.ui.clone()),
            Err(e) => {
                tracing::warn!(error = %e, "saving editor.json failed");
                self.status
                    .notify(Severity::Warning, format!("saving editor.json failed: {e}"));
            }
        }
    }

    /// Loads `editor.toml` (boot): seeds `surround` from the old `UiState` once, applies prefs.
    pub(super) fn load_editor_prefs(&mut self) {
        use crate::settings::editor_prefs::{self, EDITOR_PATH};
        // WP-4 (audit E2): `captures.toml` is loaded at boot too — navigation reads its period
        // folders, not only the Captures page.
        if let (Some(d), Some(f)) = (&self.daemon, self.settings.files.get_mut(CAPTURES_PATH)) {
            f.load(d.as_ref());
        }
        if let (Some(d), Some(f)) = (&self.daemon, self.settings.files.get_mut(EDITOR_PATH)) {
            f.load(d.as_ref());
            if editor_prefs::seed_surround(f, self.ui.surround) {
                self.settings.editor_note = Some(
                    "surround taken from the local editor state; it is written to editor.toml \
                     with the next Save"
                        .into(),
                );
            }
        }
        self.on_settings_content(CAPTURES_PATH);
        self.on_settings_content(EDITOR_PATH);
    }

    /// Recomputes the effective prefs from the `editor.toml` doc and applies them live.
    fn apply_prefs(&mut self) {
        use crate::settings::editor_prefs::{self, EDITOR_PATH};
        let prefs = match self.settings.file(EDITOR_PATH) {
            Some(f) => editor_prefs::effective(f, self.ui.surround),
            None => editor_prefs::Prefs {
                surround: self.ui.surround,
                ..editor_prefs::Prefs::default()
            },
        };
        self.tokens = tokens_for(prefs.surround);
        self.palette.set_hidden(prefs.palette_hidden.clone());
        // WP-4: `[keys]` overrides rebuild the effective keymap live (no restart).
        self.palette.set_keys(prefs.keys.clone());
        self.reading.set_anchor(&mut self.docs, prefs.anchor());
        self.prefs = prefs;
        self.sync_hotkeys();
    }

    /// The one fan-out after a settings file's doc changed (load, reload, edit,
    /// save landed, conflict reload, external change) — re-derives what `path` feeds:
    /// `editor.toml` → the live prefs and the local `UiState` surround mirror; `bookmarks.toml`
    /// → the bookmarks panel / commands; `captures.toml` → the period folders; `actions.toml`
    /// → the Hotkeys page snapshots.
    pub(super) fn on_settings_content(&mut self, path: &str) {
        use crate::settings::editor_prefs::EDITOR_PATH;
        use crate::settings::pages::hotkeys::ACTIONS_PATH;
        match path {
            EDITOR_PATH => {
                self.apply_prefs();
                self.ui.surround = self.prefs.surround;
            }
            BOOKMARKS_PATH => self.sync_bookmarks(),
            CAPTURES_PATH => {
                self.period_folders = period_folders(self.settings.file(CAPTURES_PATH));
            }
            ACTIONS_PATH => self.sync_hotkeys(),
            _ => {}
        }
    }

    /// Refreshes the Hotkeys page's keymap / action snapshots from the palette.
    pub(super) fn sync_hotkeys(&mut self) {
        let h = &mut self.settings.hotkeys;
        if !std::sync::Arc::ptr_eq(&h.keymap, &self.palette.keymap) {
            h.keymap = self.palette.keymap.clone();
        }
        // Plan editor-feedback-round-2 WP-1: an invalidated (refetching) action cache is not
        // an empty action list — keep the last snapshot until the refetch lands, else every
        // action row vanishes from the page.
        let Some(actions) = self.palette.actions.as_deref() else {
            return;
        };
        if h.actions != actions {
            h.actions = actions.to_vec();
        }
    }

    /// A chord captured while the Hotkeys page records: becomes the row's chord (in the doc;
    /// auto-saved, plan editor-feedback-round-2 WP-2), or a note when it cannot be bound.
    pub(super) fn capture_chord(&mut self, c: &crate::palette::chord::Chord) -> Task<Message> {
        use crate::palette::chord;
        use crate::settings::pages::hotkeys::{self, ACTIONS_PATH, Target};
        let Some(target) = self.settings.hotkeys.recording.take() else {
            return Task::none();
        };
        let actions_doc = self.settings.file(ACTIONS_PATH).and_then(|f| f.doc());
        if !chord::text_editing(c)
            && let Some(other) = hotkeys::owner(&self.settings.hotkeys, actions_doc, &target, c)
        {
            // Plan editor-feedback-round-2 WP-1: the existing binding wins; nothing is edited.
            self.settings.hotkeys.note = Some(format!("{c} is already assigned to {other}"));
            return Task::none();
        }
        if let Target::Bookmark(path) = target {
            // WP-9: the chord goes on the bookmark record, written at once (shared CAS write).
            return match hotkeys::check(c) {
                Ok(()) => {
                    self.update_bookmarks(BookmarkMsg::SetKeys(path, Some(vec![c.to_string()])))
                }
                Err(n) => {
                    self.settings.hotkeys.note = Some(n);
                    Task::none()
                }
            };
        }
        match hotkeys::record_edit(&target, c, actions_doc) {
            Ok((file, edit)) => self.hotkey_edit(&file, &edit),
            Err(n) => self.settings.hotkeys.note = Some(n),
        }
        Task::none()
    }

    /// Applies a Hotkeys document edit and re-derives prefs / keymap.
    fn hotkey_edit(&mut self, file: &str, edit: &crate::settings::DocEdit) {
        if let Err(e) = self.settings.edit(file, edit) {
            self.settings.hotkeys.note = Some(e);
        }
        self.on_settings_content(file);
    }

    fn update_hotkeys(&mut self, m: crate::settings::pages::hotkeys::HotkeyMsg) -> Task<Message> {
        use crate::settings::pages::hotkeys::{self, ACTIONS_PATH, HotkeyMsg, Target};
        let doc = |s: &crate::settings::Settings, p: &str| s.file(p).and_then(|f| f.doc().cloned());
        match m {
            HotkeyMsg::Clear(Target::Bookmark(path)) => {
                return self.update_bookmarks(BookmarkMsg::SetKeys(path, None));
            }
            HotkeyMsg::Record(t) => self.settings.hotkeys.record(t),
            HotkeyMsg::Cancel => self.settings.hotkeys.cancel(),
            HotkeyMsg::Clear(t) => {
                let ad = doc(&self.settings, ACTIONS_PATH);
                match hotkeys::clear_edit(&t, ad.as_ref()) {
                    Ok(Some((f, e))) => self.hotkey_edit(&f, &e),
                    Ok(None) => {}
                    Err(n) => self.settings.hotkeys.note = Some(n),
                }
            }
            HotkeyMsg::Reset(id) => {
                let ed = doc(&self.settings, crate::settings::editor_prefs::EDITOR_PATH);
                if let Some((f, e)) = hotkeys::reset_edit(&id, ed.as_ref()) {
                    self.hotkey_edit(&f, &e);
                }
            }
        }
        Task::none()
    }

    /// The surround switch: edits `editor.toml`'s `surround` (auto-saved after the debounce,
    /// plan editor-feedback-round-2 WP-2) and keeps the local
    /// `UiState` copy in step.
    pub(super) fn set_surround(&mut self, s: crate::theme::Surround) {
        use crate::settings::editor_prefs::{EDITOR_PATH, surround_key};
        use crate::settings::{DocEdit, ScalarValue};
        let edit = DocEdit::Set(
            cv_schema::Path::root().key("surround"),
            ScalarValue::Str(surround_key(s).to_owned()),
        );
        let edited = self
            .settings
            .file_mut(EDITOR_PATH)
            .is_some_and(|f| f.edit(|doc| edit.apply(doc)).is_ok());
        if !edited {
            tracing::debug!("surround: editor.toml not editable; local state only");
        }
        self.ui.surround = s;
        self.on_settings_content(EDITOR_PATH);
    }

    /// Registers the `views/*.toml` files the explorer knows and loads every unloaded page.
    pub(super) fn sync_view_files(&mut self) {
        self.settings.sync_views(&self.explorer.paths);
        if let Some(d) = &self.daemon {
            self.settings.load_unloaded(d.as_ref());
        }
    }

    /// Re-derives the bookmarks panel list from the shared `bookmarks.toml` [`ConfigFile`].
    fn sync_bookmarks(&mut self) {
        if let Some(f) = self.settings.file(BOOKMARKS_PATH) {
            self.bookmarks.sync(f);
        }
        // WP-9: the bookmark commands / chords follow the list.
        self.palette.set_bookmarks(self.bookmarks.items.clone());
        self.settings.hotkeys.bookmarks = self.bookmarks.items.clone();
        self.refresh_bookmark_missing();
        self.sync_hotkeys();
    }

    /// A `file_changed` for one of the settings files: reload when clean (external edits).
    pub(super) fn settings_file_changed(&mut self, path: &str, hash: Option<&str>) {
        let Some(d) = self.daemon.clone() else {
            return;
        };
        let reloaded = self
            .settings
            .file_mut(path)
            .is_some_and(|f| f.file_changed(hash, d.as_ref()));
        if reloaded {
            self.settings.form.clear_drafts(path);
            self.on_settings_content(path);
        }
    }

    /// Fetches `status` off the UI thread when the Settings tab shows the Status page.
    pub(super) fn refresh_status_if_open(&mut self) -> Task<Message> {
        let open = panes::find_kind(&self.panes, &TabKind::Settings).is_some();
        if open && self.settings.section == crate::settings::Section::Status {
            return self.fetch_status();
        }
        Task::none()
    }

    fn fetch_status(&mut self) -> Task<Message> {
        use crate::settings::SettingsMsg;
        if self.daemon.is_none() {
            self.settings.status.error = Some("not connected".into());
            return Task::none();
        }
        self.settings.status.loading = true;
        self.rpc(
            |d| d.status().map_err(|e| e.to_string()),
            |r| Message::Settings(SettingsMsg::StatusLoaded(r)),
        )
    }

    pub(super) fn update_settings(&mut self, m: crate::settings::SettingsMsg) -> Task<Message> {
        use crate::settings::{Section, SettingsMsg};
        match m {
            SettingsMsg::Open => {
                self.open_singleton(TabKind::Settings, self.focus);
                if self.settings.section == Section::Hotkeys {
                    return self.fetch_actions();
                }
                return self.refresh_status_if_open();
            }
            SettingsMsg::Hotkey(m) => return self.update_hotkeys(m),
            SettingsMsg::Section(s) => {
                self.settings.section = s;
                self.settings.hotkeys.cancel();
                // Plan round-5 WP-1: the section's own body offset, re-applied explicitly.
                let restore = self.settings.restore_scroll();
                if s == Section::Views {
                    self.sync_view_files();
                }
                if s == Section::Hotkeys {
                    self.sync_view_files();
                    return Task::batch([restore, self.fetch_actions()]);
                }
                if s == Section::Status {
                    return Task::batch([restore, self.fetch_status()]);
                }
                return Task::batch([restore, self.settings_lookups()]);
            }
            SettingsMsg::BodyScrolled(s, y) => {
                self.settings.body_offsets.insert(s, y);
            }
            SettingsMsg::Edit(path, edit) => {
                let _ = self.settings.edit(&path, &edit);
                self.on_settings_content(&path);
            }
            SettingsMsg::Typeahead(k) => {
                let file = self
                    .settings
                    .form
                    .typeahead
                    .as_ref()
                    .map(|a| a.file.clone());
                self.settings.typeahead_key(k, &self.explorer.paths);
                if let Some(file) = file {
                    self.on_settings_content(&file);
                }
                return self.settings_lookups();
            }
            SettingsMsg::Form(path, m) => {
                self.settings.form_msg_paths(&path, m, &self.explorer.paths);
                self.on_settings_content(&path);
                return self.settings_lookups();
            }
            SettingsMsg::Lookup(crate::settings::lookups::LookupMsg::GeoDue(seq)) => {
                if let Some(q) = self.settings.lookups.geo_due(seq) {
                    let q2 = q.clone();
                    return self.rpc(
                        move |d| d.lookup_geocode(&q2).map_err(|e| e.to_string()),
                        move |r| {
                            Message::Settings(SettingsMsg::Lookup(
                                crate::settings::lookups::LookupMsg::Geo(q, r),
                            ))
                        },
                    );
                }
            }
            SettingsMsg::Lookup(m) => self.settings.lookups.apply(m),
            SettingsMsg::ViewSelect(p) => self.settings.view_selected = Some(p),
            SettingsMsg::NewViewName(n) => self.settings.new_view = n,
            SettingsMsg::NewViewCreate => {
                let name = std::mem::take(&mut self.settings.new_view);
                if let Err(e) = self.settings.new_view_file(&name) {
                    self.settings.new_view = name;
                    self.status.set_error(e);
                }
            }
            SettingsMsg::AutoSaveDue(path, seq) => {
                // Coalescing: only the timer of the latest edit writes.
                if self
                    .settings
                    .file(&path)
                    .is_some_and(|f| f.edit_seq() == seq)
                {
                    return self.save_settings_file(path);
                }
            }
            SettingsMsg::Saved(path, r) => return self.settings_saved(path, r),
            SettingsMsg::StartOver(path) => {
                if let Some(f) = self.settings.file_mut(&path) {
                    f.start_over();
                }
                self.on_settings_content(&path);
            }
            SettingsMsg::RefreshStatus => return self.fetch_status(),
            SettingsMsg::StatusLoaded(r) => self.settings.status.loaded(r),
        }
        Task::none()
    }

    /// A settings write landed: notices, the requeued follow-up ([`FinishOutcome`]), quit
    /// progress and the derived-state fan-out.
    ///
    /// [`FinishOutcome`]: crate::settings::FinishOutcome
    fn settings_saved(
        &mut self,
        path: String,
        r: Result<crate::settings::WriteOutcome, String>,
    ) -> Task<Message> {
        use crate::settings::editor_prefs::EDITOR_PATH;
        let d = self.daemon.clone();
        let Some(f) = self.settings.file_mut(&path) else {
            return self.quit_progress();
        };
        // Plan round-7 WP-3: a failed save reaches the bar (Error); a CAS conflict (reloaded
        // from disk) is a Warning. Bookmarks (panel edits): a conflict is an Error too, and a
        // failed write reloads the shared file (the panel shows what is on disk).
        let bookmarks = path == BOOKMARKS_PATH;
        let severity = if r.is_err() || bookmarks {
            Severity::Error
        } else {
            Severity::Warning
        };
        let out = f.finish_save(r, d.as_deref(), bookmarks);
        if out.failed
            && let Some(n) = f.notice()
        {
            self.status
                .notify_from(severity, n.to_owned(), path.clone());
        }
        if let Some(q) = &mut self.quit
            && q.settings.contains(&path)
            && !out.requeue
        {
            if out.failed {
                q.losses.setting(&path);
            }
            q.settings.remove(&path);
        }
        let task = if out.requeue {
            self.save_settings_file(path.clone())
        } else {
            self.quit_progress()
        };
        if path == EDITOR_PATH {
            self.settings.editor_note = None;
        }
        self.on_settings_content(&path);
        task
    }
}
