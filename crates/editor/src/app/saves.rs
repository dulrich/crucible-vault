//! Saving: save-first flows, the unsaved-edits / delete dialogs, flushes, settings auto-save and quit.

use super::*;

impl App {
    /// The dialog's OK / Cancel.
    pub(super) fn update_dialog(&mut self, m: crate::dialog::DialogMsg) -> Task<Message> {
        use crate::dialog::{Dialog, DialogMsg, Discard};
        if let DialogMsg::Toggle(n) = m {
            if let Some(Dialog::DeleteConfirm(c)) = self.dialog.as_mut() {
                c.toggle(n);
            }
            return Task::none();
        }
        let Some(d) = self.dialog.take() else {
            return Task::none();
        };
        match (m, d) {
            (DialogMsg::Alt, Dialog::DeleteUnsaved(path)) => self.discard_then_delete(path),
            // Enter (Default) is Cancel in every dialog: all are destructive.
            (DialogMsg::Ok, Dialog::Unsaved { then, .. }) => match then {
                Discard::Quit => self.exit(),
                Discard::CloseTabs(ids) => {
                    for id in ids {
                        self.remove_tab(id);
                    }
                    Task::none()
                }
            },
            (DialogMsg::Ok, Dialog::DeleteConfirm(c)) => {
                let ticked = c.ticked();
                self.run_delete_note(c.path, ticked, c.mode)
            }
            (DialogMsg::Ok, Dialog::DeleteUnsaved(path)) => self.save_then_delete(path),
            _ => Task::none(),
        }
    }

    /// Unsaved-edits delete dialog, "Save then delete": save first ([`AfterSave::Delete`]);
    /// the delete preview follows once it lands clean.
    fn save_then_delete(&mut self, path: String) -> Task<Message> {
        if !buf(&self.docs, &path).is_some_and(|d| d.is_dirty()) {
            return self.begin_delete(path);
        }
        let Some(id) = self.docs.id_of(&path) else {
            return self.begin_delete(path);
        };
        self.save_then(id, Some(AfterSave::Delete))
            .unwrap_or_else(|| {
                self.status
                    .set_error("delete not done (the note is saving or has a conflict)".into());
                Task::none()
            })
    }

    /// Unsaved-edits delete dialog, "Discard and delete": the buffer reloads from disk (edits
    /// dropped), then the delete preview + confirm as usual.
    fn discard_then_delete(&mut self, path: String) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            self.status.set_error("daemon disconnected".into());
            return Task::none();
        };
        match d.read_file(&path) {
            Ok((bytes, hash)) => {
                if let Some(doc) = buf_mut(&mut self.docs, &path) {
                    doc.reload(&bytes, hash);
                }
                self.begin_delete(path)
            }
            Err(e) => {
                self.status.set_error(open_error(&e));
                Task::none()
            }
        }
    }

    /// plan editor-ui-round-3 WP-3: "Delete note…" / explorer Trash. Fetches the preview, then
    /// opens the always-confirm dialog. A note with unsaved edits is refused with a notice
    /// (save or close it first), so the delete never drops edits silently.
    pub(super) fn begin_delete(&mut self, path: String) -> Task<Message> {
        if self.doc_dirty(&path) {
            self.dialog = Some(crate::dialog::Dialog::DeleteUnsaved(path));
            return Task::none();
        }
        // An open document is addressed by its id: the preview names its current path.
        let id = self.docs.id_of(&path);
        self.rpc(
            move |d| match d.delete_preview(id, &path) {
                Ok(r) => Ok((r.path.unwrap_or(path), Ok(r.items))),
                Err(e) => Ok((path, Err(e.to_string()))),
            },
            |r| {
                let (path, result) = r.unwrap_or_else(|e| (String::new(), Err(e)));
                Message::Explorer(ExplorerMsg::DeletePreview { path, result })
            },
        )
    }

    /// Dialog OK: `delete_note` with the ticked items in the `editor.toml` `delete_mode`.
    fn run_delete_note(
        &mut self,
        path: String,
        attachments: Vec<String>,
        mode: crate::daemon::DeleteMode,
    ) -> Task<Message> {
        if self.doc_dirty(&path) {
            // Edited between the confirm opening and OK: back through the unsaved-edits dialog.
            self.dialog = Some(crate::dialog::Dialog::DeleteUnsaved(path));
            return Task::none();
        }
        let id = self.docs.id_of(&path);
        self.rpc(
            move |d| {
                d.delete_note(id, &path, &attachments, mode)
                    .map_err(|e| crate::daemon::rpc_err(&e))
            },
            |r| Message::Explorer(ExplorerMsg::NoteDeleted(r)),
        )
    }

    /// Why document `id`'s edits could not be saved (`fallback` when no conflict / outage
    /// explains it).
    pub(super) fn loss_reason(&self, id: NoteId, fallback: &str) -> crate::dialog::LossReason {
        use crate::dialog::LossReason;
        match self.docs.by_id(id).map(|d| &d.buffer) {
            Some(d) if d.is_missing() => LossReason::Missing,
            Some(d) if d.conflict().is_some() => LossReason::Conflict,
            _ if !self.status.connected => LossReason::Disconnected,
            _ => LossReason::SaveFailed(fallback.to_owned()),
        }
    }

    /// Opens the unsaved-edits dialog for the tab close of document `id`.
    pub(super) fn close_dialog(&mut self, id: NoteId, ids: Vec<u64>, fallback: &str) {
        let Some(path) = self.docs.path_of(id) else {
            return;
        };
        let mut losses = crate::dialog::UnsavedEdits::default();
        losses.note(&path, self.loss_reason(id, fallback));
        self.dialog = Some(crate::dialog::Dialog::Unsaved {
            losses,
            then: crate::dialog::Discard::CloseTabs(ids),
        });
    }

    /// Each pane's active note path (WP-3 tab-switch flush snapshot).
    pub(super) fn active_paths(&self) -> Vec<(Pane, String, Option<NoteId>)> {
        self.panes
            .iter()
            .filter_map(|(p, ps)| ps.active_note().map(|t| (*p, t.path.to_owned(), t.note)))
            .collect()
    }

    /// Plan editor-feedback-round-2 WP-3: every path that changes a pane's active tab (select,
    /// Alt+N, open/Quick Switch, history, closing a neighbour) runs through `update`, so one
    /// before/after comparison here flushes the note a pane switched away from — no per-path
    /// save calls. The tab must still exist (a closed tab already went through `close_tab`).
    pub(super) fn flush_switched_away(
        &mut self,
        before: &[(Pane, String, Option<NoteId>)],
    ) -> Task<Message> {
        // Plan note-identity, WP-4: the document a pane switched away from, by its tab's id.
        let left: Vec<Option<NoteId>> = before
            .iter()
            .filter(|(p, path, note)| {
                self.panes
                    .get(*p)
                    .and_then(|ps| ps.active_tab())
                    .is_none_or(|t| !t.shows(path) || t.note != *note)
            })
            .map(|(_, _, note)| *note)
            .collect();
        Task::batch(
            left.into_iter()
                .map(|note| match note.and_then(|i| self.docs.by_id_mut(i)) {
                    Some(od) => crate::source::flush(&mut od.buffer, self.daemon.as_ref()),
                    None => Task::none(),
                }),
        )
    }

    /// Flushes the note tab at `path` now (no-op when clean, saving — re-queued — conflicted,
    /// or not open).
    fn flush_note(&mut self, path: &str) -> Task<Message> {
        match buf_mut(&mut self.docs, path) {
            Some(doc) => crate::source::flush(doc, self.daemon.as_ref()),
            None => Task::none(),
        }
    }

    /// WP-3: window focus loss flushes every dirty note buffer.
    pub(super) fn flush_all_notes(&mut self) -> Task<Message> {
        let dirty: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, d)| d.buffer.is_dirty())
            .map(|(p, _)| p.to_owned())
            .collect();
        Task::batch(dirty.iter().map(|path| self.flush_note(path)))
    }

    /// Plan editor-feedback-round-2 WP-2: arms one debounce timer per settings file whose
    /// latest edit left it valid (every edit path — form, prefs, hotkeys, theme cycle — ends
    /// here). A missing daemon arms nothing (the file stays dirty).
    pub(super) fn schedule_settings_autosave(&mut self) -> Task<Message> {
        if self.daemon.is_none() {
            return Task::none();
        }
        let due = self.settings.due_autosaves();
        Task::batch(due.into_iter().map(|(path, seq)| {
            Task::perform(
                // Built lazily inside the async block (it needs the executor's reactor).
                async { tokio::time::sleep(crate::settings::AUTOSAVE_DELAY).await },
                move |()| {
                    Message::Settings(crate::settings::SettingsMsg::AutoSaveDue(path.clone(), seq))
                },
            )
        }))
    }

    /// Writes settings file `path` now when it is valid and dirty; behind an in-flight write it
    /// is queued (re-run when that write lands).
    pub(super) fn save_settings_file(&mut self, path: String) -> Task<Message> {
        use crate::settings::SettingsMsg;
        let Some(f) = self.settings.file_mut(&path) else {
            return Task::none();
        };
        let Some(job) = f.request_save() else {
            return Task::none();
        };
        self.rpc(
            move |d| job.run(d),
            move |r| Message::Settings(SettingsMsg::Saved(path, r)),
        )
    }

    /// Flushes every pending settings write (Settings tab close, quit). Returns the paths now
    /// writing (started or queued) and the task.
    pub(super) fn flush_settings(&mut self) -> (Vec<String>, Task<Message>) {
        let paths: Vec<String> = self
            .settings
            .files
            .values()
            .filter(|f| f.wants_save() || f.saving())
            .map(|f| f.path().to_owned())
            .collect();
        let tasks: Vec<Task<Message>> = paths
            .iter()
            .map(|p| self.save_settings_file(p.clone()))
            .collect();
        (paths, Task::batch(tasks))
    }

    /// The one save path. Starts document `id`'s save; `after` (a save-first continuation)
    /// runs when it lands, `None` is a plain save. `None` back when no save can start (clean,
    /// in flight, renaming or conflicted).
    pub(super) fn save_then(
        &mut self,
        id: NoteId,
        after: Option<AfterSave>,
    ) -> Option<Task<Message>> {
        let path = self.docs.path_of(id)?;
        if self.refuse_held_save(&path) {
            return None;
        }
        let job = self.docs.by_id_mut(id)?.buffer.begin_save()?;
        let Some(after) = after else {
            return Some(crate::source::spawn_save(self.daemon.as_ref(), Some(job)));
        };
        let (id, seq) = (job.id, job.seq);
        Some(self.rpc(
            move |d| Ok(job.run(d)),
            move |r| {
                let (outcome, at) =
                    r.unwrap_or_else(|e| (crate::source::SaveOutcome::Failed(e), None));
                Message::Source(SourceMsg::Saved {
                    id,
                    seq,
                    outcome,
                    at,
                    after: Some(after),
                })
            },
        ))
    }

    /// Every save completion (`SourceMsg::Saved`). Applies it, starts a queued
    /// follow-up (the continuation moves onto it), runs a deferred reconnect resync, the
    /// title sync (plain saves only — a continuation renames, deletes or acts on the note
    /// itself) and the close/quit waiters, then resumes the continuation.
    pub(super) fn on_save_landed(
        &mut self,
        id: NoteId,
        seq: u64,
        (outcome, at): crate::source::Landing,
        after: Option<AfterSave>,
    ) -> Task<Message> {
        // Plan note-identity D4: the completion is addressed to the note id; the path is read
        // from the document now (a move since the save started changed only that field). A
        // document closed meanwhile is `Landed::Gone`.
        // The reply says where the daemon acted: the document takes that path (the one move
        // function), then the outcome applies as a plain one.
        if let Some(to) = at {
            self.move_doc_path(id, &to);
        }
        let ok = matches!(outcome, crate::source::SaveOutcome::Saved(_));
        let result = self
            .docs
            .by_id_mut(id)
            .map(|d| d.buffer.finish_save(seq, outcome));
        // Plan round-18 WP-2: a daemon `HELD` refusal keeps the buffer dirty and posts the one
        // hold notice (not a generic failure); later text sees a plain message.
        let held = match &result {
            Some(Err(e)) => crate::daemon::held_label(e).map(str::to_owned),
            _ => None,
        };
        let result = match (&held, result) {
            (Some(label), Some(Err(_))) => Some(Err(format!("{label} is updating this note"))),
            (_, r) => r,
        };
        if let Some(label) = &held {
            if after.is_none() {
                self.notify_held(label);
            }
        } else if after.is_none()
            && let Some(Err(e)) = &result
        {
            self.status.set_error(format!("save failed: {e}"));
        }
        let follow = match &result {
            Some(Ok(true)) => self.save_then(id, after.clone()),
            _ => None,
        };
        let dirty = self.docs.by_id(id).is_some_and(|d| d.buffer.is_dirty());
        let landed = match result {
            None => Landed::Gone,
            Some(Err(e)) => Landed::Failed(e),
            Some(Ok(_)) if !ok => Landed::Conflict,
            Some(Ok(_)) if dirty => Landed::Changed,
            Some(Ok(_)) => Landed::Clean,
        };
        // plan editor-data-safety WP-1 (audit E1): a reconnect that arrived while this save was
        // in flight resyncs now (a follow-up save already started runs on the current client).
        let resync = if self
            .docs
            .by_id_mut(id)
            .is_some_and(|d| d.buffer.take_resync_pending() && !d.buffer.is_saving())
        {
            self.resync_doc(id)
        } else {
            Task::none()
        };
        // Plan usability-sprint-captures-links WP-5: a landed save that changed `title`.
        let sync = if ok && after.is_none() {
            self.title_sync(id)
        } else {
            Task::none()
        };
        // A save for a document no longer open resolves any wait as landed.
        let waiters = self.note_save_landed(id, ok || matches!(landed, Landed::Gone));
        let resume = match after {
            Some(a) if follow.is_none() => self.resume_after(id, a, &landed),
            _ => Task::none(),
        };
        Task::batch([
            follow.unwrap_or_else(Task::none),
            resync,
            sync,
            waiters,
            resume,
        ])
    }

    /// A save-first continuation after its save landed. Only `Clean` (or a closed
    /// document) proceeds; a failed save never closes, quits, deletes, renames or runs — except
    /// a run, which today proceeds over a newer unsaved edit. Every continuation that needs the
    /// note's path has an open document (`Landed::Gone` is the only closed case).
    fn resume_after(&mut self, note: NoteId, after: AfterSave, landed: &Landed) -> Task<Message> {
        let Some(path) = self.docs.path_of(note) else {
            return self.resume_gone(note, after);
        };
        let path = path.as_str();
        let clean = matches!(landed, Landed::Clean | Landed::Gone);
        let (why, refusal) = match landed {
            Landed::Failed(e) => (e.clone(), format!("save failed: {e}")),
            Landed::Conflict => (
                "save conflict".to_owned(),
                "save conflict: the file changed on disk".to_owned(),
            ),
            _ => (
                "the note changed while saving".to_owned(),
                "the note changed while saving".to_owned(),
            ),
        };
        match after {
            // Audit E1: the close removes exactly the tab it was asked for (by id).
            AfterSave::CloseTab(id) => {
                if panes::find_id(&self.panes, id).is_some() && !(clean && self.remove_tab(id)) {
                    self.close_dialog(note, vec![id], &why);
                }
            }
            AfterSave::Quit => {
                let reason = (!clean).then(|| self.loss_reason(note, &why));
                let Some(q) = &mut self.quit else {
                    return Task::none();
                };
                q.pending.remove(&note);
                if let Some(reason) = reason {
                    q.losses.note(path, reason);
                }
                return self.quit_progress();
            }
            AfterSave::Delete if clean => return self.begin_delete(path.to_owned()),
            AfterSave::Rename(to) if clean => {
                return self.dispatch_op(FileOp::Rename {
                    from: path.to_owned(),
                    to,
                });
            }
            AfterSave::Delete => self
                .status
                .set_error(format!("delete not done ({refusal})")),
            AfterSave::Rename(_) => self
                .status
                .set_error(format!("rename not done ({refusal})")),
            AfterSave::RunCall(call) => match landed {
                Landed::Gone => return self.resume_gone(note, AfterSave::RunCall(call)),
                Landed::Clean | Landed::Changed => {
                    let reading_sel = self
                        .active_note()
                        .is_some_and(|t| t.path == path)
                        .then(|| self.reading_selection())
                        .flatten();
                    let ctx = crate::palette::context(buf(&self.docs, path), reading_sel);
                    return self.dispatch_with(call, ctx);
                }
                Landed::Failed(_) | Landed::Conflict => self
                    .status
                    .set_error(format!("{}: not run ({refusal})", call.title)),
            },
        }
        Task::none()
    }

    /// [`Self::resume_after`] for a document closed before its save landed: a close or quit
    /// proceeds, a delete or rename is dropped, a run is refused.
    fn resume_gone(&mut self, note: NoteId, after: AfterSave) -> Task<Message> {
        match after {
            AfterSave::CloseTab(id) => {
                self.remove_tab(id);
            }
            AfterSave::Quit => {
                let Some(q) = &mut self.quit else {
                    return Task::none();
                };
                q.pending.remove(&note);
                return self.quit_progress();
            }
            AfterSave::Delete | AfterSave::Rename(_) => {}
            AfterSave::RunCall(call) => self
                .status
                .set_error(format!("{}: not run (note closed)", call.title)),
        }
        Task::none()
    }

    /// WP-3 rework: a normal note save (`SourceMsg::Saved`, already applied to the buffer)
    /// landed; resolves a close or quit waiting on it. Clean → done; saved but a queued
    /// follow-up is in flight → keep waiting; conflict, failure or a newer unsaved edit →
    /// cancel with a status message. Every await ends here, so the wait is bounded.
    pub(super) fn note_save_landed(&mut self, id: NoteId, ok: bool) -> Task<Message> {
        let waiting_quit = self.quit.as_ref().is_some_and(|q| q.awaiting.contains(&id));
        let waiting_close = self
            .docs
            .by_id(id)
            .is_some_and(|d| !d.close_waiters.is_empty());
        if !waiting_quit && !waiting_close {
            return Task::none();
        }
        let (saving, dirty) = self.docs.by_id(id).map_or((false, false), |d| {
            (d.buffer.is_saving(), d.buffer.is_dirty())
        });
        if ok && saving {
            return Task::none();
        }
        let done = ok && !dirty;
        let why = if !ok {
            "save failed"
        } else {
            "the note changed while saving"
        };
        if waiting_close {
            let ids = self
                .docs
                .by_id_mut(id)
                .map(|d| std::mem::take(&mut d.close_waiters))
                .unwrap_or_default();
            if done {
                for id in ids {
                    self.remove_tab(id);
                }
            } else {
                let ids: Vec<u64> = ids
                    .into_iter()
                    .filter(|id| panes::find_id(&self.panes, *id).is_some())
                    .collect();
                if !ids.is_empty() && self.quit.is_none() {
                    self.close_dialog(id, ids, why);
                }
            }
        }
        if waiting_quit {
            let reason = (!done).then(|| self.loss_reason(id, why));
            let path = self.docs.path_of(id);
            if let Some(q) = &mut self.quit {
                q.awaiting.remove(&id);
                if let (Some(reason), Some(path)) = (reason, path) {
                    q.losses.note(&path, reason);
                }
            }
            return self.quit_progress();
        }
        Task::none()
    }

    /// Quit: flush every dirty document (once per document, however many tabs show it — plan
    /// editor-feedback-round-2 WP-7), exit once all saves land; any failure cancels the quit.
    pub(super) fn quit(&mut self) -> Task<Message> {
        if self.dialog.is_some() {
            return Task::none();
        }
        if self.quit.is_some() {
            return self.quit_anyway_dialog();
        }
        let dirty: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, d)| d.buffer.is_dirty())
            .map(|(p, _)| p.to_owned())
            .collect();
        let settings_pending = self
            .settings
            .files
            .values()
            .any(|f| f.wants_save() || f.saving());
        // plan editor-data-safety WP-2 (audit M1): dirty settings files the flush cannot
        // write (validation errors, malformed / unloaded) are losses, never written (D20).
        let mut losses = crate::dialog::UnsavedEdits::default();
        for f in self.settings.files.values() {
            if f.dirty() && !f.wants_save() && !f.saving() {
                losses.setting(f.path());
            }
        }
        if dirty.is_empty() && losses.is_empty() && !settings_pending {
            return self.exit();
        }
        let connected = self.status.connected && self.daemon.is_some();
        let (mut pending, mut saves) = (std::collections::HashSet::new(), Vec::new());
        let mut awaiting = std::collections::HashSet::new();
        let mut held = std::collections::HashSet::new();
        for path in dirty {
            if !connected {
                losses.note(&path, crate::dialog::LossReason::Disconnected);
                continue;
            }
            // Plan round-18 WP-2: a held note's save starts when the hold releases.
            if self.held(&path).is_some() {
                held.insert(path);
                continue;
            }
            let Some(note) = self.docs.id_of(&path) else {
                continue;
            };
            let reason = self.loss_reason(note, "the save could not start");
            if let Some(t) = self.save_then(note, Some(AfterSave::Quit)) {
                saves.push(t);
                pending.insert(note);
            } else if buf(&self.docs, &path).is_some_and(|d| d.is_saving()) {
                // WP-3 rework: in flight — `begin_save` queued the follow-up; await it.
                awaiting.insert(note);
            } else {
                // Conflict / missing file (or a save that cannot start).
                losses.note(&path, reason);
            }
        }
        if !connected {
            for f in self.settings.files.values() {
                if f.wants_save() || f.saving() {
                    losses.setting(f.path());
                }
            }
        }
        self.quit = Some(QuitState {
            pending,
            settings: std::collections::HashSet::new(),
            awaiting,
            held,
            losses,
        });
        if !connected {
            return self.quit_progress();
        }
        // WP-2: settings writes are flushed and awaited like note saves.
        // Await exactly the paths the flush started or queued.
        let (flushing, settings_task) = self.flush_settings();
        if let Some(q) = &mut self.quit {
            q.settings.extend(flushing);
        }
        if self.quit.is_none() {
            // Every write landed inline and the quit already exited.
            return settings_task;
        }
        let mut tasks: Vec<Task<Message>> = vec![settings_task, self.quit_progress()];
        tasks.extend(saves);
        Task::batch(tasks)
    }

    pub(super) fn exit(&mut self) -> Task<Message> {
        self.write_ui_state();
        self.exiting = true;
        iced::exit()
    }
}

/// How a save-first save landed, for [`App::resume_after`].
enum Landed {
    /// Saved and the buffer is clean.
    Clean,
    /// The document is no longer open.
    Gone,
    Failed(String),
    Conflict,
    /// Saved, but an edit typed meanwhile left it dirty (no follow-up was queued).
    Changed,
}
