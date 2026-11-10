//! The command palette, chords, presets and action invocation; job completion follow-up.

use super::*;

impl App {
    pub(super) fn update_palette(&mut self, m: PaletteMsg) -> Task<Message> {
        use crate::palette::prompt::Step;
        match m {
            PaletteMsg::Open => {
                // Decision 9 (round-12): one centre modal at a time.
                self.switcher.open = false;
                self.panels.search_modal = false;
                self.palette.show();
                let fetch = self.fetch_actions();
                return Task::batch([
                    fetch,
                    iced::widget::operation::focus(crate::palette::INPUT_ID),
                ]);
            }
            PaletteMsg::Escape
                if !self.settings.hotkeys.capturing()
                    && self.palette.open
                    && self.palette.prompt.is_none()
                    && !self.palette.query.is_empty() =>
            {
                // plan editor-ui-round-3 WP-5: two-stage Escape (clear, then close).
                self.palette.set_query(String::new());
                // plan editor-round-4 WP-1: iced unfocused the input on Escape.
                return iced::widget::operation::focus(crate::palette::INPUT_ID);
            }
            PaletteMsg::Close | PaletteMsg::Escape if self.settings.hotkeys.capturing() => {
                self.settings.hotkeys.cancel();
            }
            PaletteMsg::Close | PaletteMsg::Escape => self.palette.close(),
            PaletteMsg::Query(q) => self.palette.set_query(q),
            // Plan round-5 WP-1: with no palette open, Up/Down scroll the focused pane one line
            // (the switcher / dialog / menus gate in `page_target`; focused inputs in `step`).
            PaletteMsg::Up | PaletteMsg::Down if !self.palette.open => {
                return self.line_step(matches!(m, PaletteMsg::Down));
            }
            PaletteMsg::Up if self.palette.prompt.is_none() => {
                self.palette.selected = self.palette.selected.saturating_sub(1);
            }
            PaletteMsg::Down if self.palette.prompt.is_none() => {
                if self.palette.selected + 1 < self.palette.results.len() {
                    self.palette.selected += 1;
                }
            }
            PaletteMsg::Up | PaletteMsg::Down => {}
            // plan editor-round-4 WP-1: Enter submits only steps without an `on_submit` input.
            PaletteMsg::Enter => {
                let non_text = self.palette.open
                    && self
                        .palette
                        .prompt
                        .as_ref()
                        .and_then(|p| p.current())
                        .is_some_and(|s| {
                            matches!(
                                s.kind,
                                cv_protocol::ParamKind::Enum | cv_protocol::ParamKind::Bool
                            )
                        });
                if non_text {
                    return self.update_palette(PaletteMsg::Submit);
                }
            }
            PaletteMsg::Submit | PaletteMsg::CtrlEnter => {
                if !self.palette.open {
                    return Task::none();
                }
                let Some(p) = &mut self.palette.prompt else {
                    if matches!(m, PaletteMsg::Submit)
                        && let Some(i) = self.palette.current()
                    {
                        return self.invoke_entry(i);
                    }
                    return Task::none();
                };
                match p.advance() {
                    Err(e) => self.palette.prompt_error = Some(e.to_string()),
                    Ok(Step::More) => {
                        let snapshot = p.clone();
                        self.palette.sync_step(&snapshot);
                    }
                    Ok(Step::Done(params)) => {
                        let a = p.action.clone();
                        self.palette.close();
                        return self.run_call(crate::palette::Call {
                            action_id: a.id,
                            title: a.title,
                            params,
                            needs: a.needs,
                        });
                    }
                }
            }
            PaletteMsg::Pick(i) => return self.invoke_entry(i),
            PaletteMsg::Loaded(r) => {
                self.palette.loading = false;
                match r {
                    Ok(a) => self.palette.set_actions(a),
                    Err(e) => self.status.set_error(format!("actions: {e}")),
                }
                self.sync_hotkeys();
                if let Some(c) = self.palette.pending_chord.take()
                    && self.palette.actions.is_some()
                {
                    return self.on_chord(&c);
                }
            }
            PaletteMsg::Key(c) => return self.on_chord(&c),
            PaletteMsg::Prompt(pm) => {
                use crate::palette::{PromptFx, PromptMsg};
                match self
                    .palette
                    .prompt_input(pm, &self.explorer.paths, &self.ui.recent)
                {
                    PromptFx::None => {}
                    PromptFx::WritePrimary(s) => return iced::clipboard::write_primary(s),
                    PromptFx::ReadPrimary => {
                        return iced::clipboard::read_primary().map(|t| {
                            Message::Palette(PaletteMsg::Prompt(PromptMsg::PastePrimary(t)))
                        });
                    }
                }
            }
            PaletteMsg::Ran {
                title,
                origin,
                result,
            } => match result {
                Ok(r) => {
                    self.palette.jobs.job_titles.insert(r.job_id.clone(), title);
                    let early = self.palette.jobs.early_open.remove(&r.job_id);
                    if let Some(p) = r.open_path.or(early) {
                        self.follow_open(origin, &p);
                    } else if let Some(o) = origin {
                        self.palette.jobs.job_origins.insert(r.job_id, o);
                    }
                }
                Err(e) => self.status.set_error(format!("{title}: {e}")),
            },
            PaletteMsg::Tick => {
                let now = std::time::Instant::now();
                self.palette.jobs.expire_toast(now);
                self.status.expire_notice(now);
            }
            PaletteMsg::RunPreset(p) => return self.run_preset(p),
        }
        Task::none()
    }

    pub(super) fn fetch_actions(&mut self) -> Task<Message> {
        if self.palette.actions.is_some() || self.palette.loading || self.daemon.is_none() {
            return Task::none();
        }
        self.palette.loading = true;
        self.rpc(
            |d| d.actions_list().map_err(|e| e.to_string()),
            |r| Message::Palette(PaletteMsg::Loaded(r)),
        )
    }

    /// A chord from the palette key listener or a focused source editor, decided from the
    /// effective keymap (plan editor-settings-tab WP-4, audit E1). While the Hotkeys page is
    /// recording, the chord goes to the capture instead.
    fn on_chord(&mut self, c: &crate::palette::chord::Chord) -> Task<Message> {
        use crate::palette::Bound;
        if self.settings.hotkeys.capturing() {
            return self.capture_chord(c);
        }
        match self.palette.keymap.get(c).cloned() {
            Some(Bound::Client(cmd)) => return self.update_inner(cmd.message(self.focus)),
            Some(Bound::Preset(preset)) => {
                if self.palette.actions.is_none() {
                    self.palette.pending_chord = Some(c.clone());
                    return self.fetch_actions();
                }
                return self.run_preset(preset);
            }
            Some(Bound::Action(id)) => return self.invoke_action(&id),
            Some(Bound::Bookmark(p)) => return self.open_bookmark(&p),
            None => {}
        }
        // Plan round-5 WP-7: unbound Ctrl+A / Ctrl+C go to the focused Reading view (a text
        // input or the source editor would have captured them before the key listener).
        if let Some(t) = self.reading_chord(c) {
            return t;
        }
        if crate::palette::chord::text_editing(c) || self.palette.actions.is_some() {
            return Task::none();
        }
        self.palette.pending_chord = Some(c.clone());
        self.fetch_actions()
    }

    /// Plan round-5 WP-7: Ctrl+A selects the focused Reading view's document, Ctrl+C copies its
    /// selection to the clipboard. `None` = not a Reading chord here (the focused tab is not
    /// showing Reading, or a modal is open).
    fn reading_chord(&mut self, c: &crate::palette::chord::Chord) -> Option<Task<Message>> {
        let plain_ctrl = c.command && !c.shift && !c.alt;
        if !plain_ctrl || !matches!(c.key.as_str(), "a" | "c") {
            return None;
        }
        if self.palette.open || self.switcher.open || self.dialog.is_some() {
            return None;
        }
        let tab = self.active_note()?;
        let (path, view) = (tab.path.to_owned(), tab.id);
        if !self.reading.mode(&self.docs, &path).shows_reading() {
            return None;
        }
        if c.key == "a" {
            self.reading.select_all(&mut self.docs, &path, view);
            return Some(Task::none());
        }
        Some(
            self.reading
                .selection_text(&self.docs, &path, view)
                .map_or_else(Task::none, iced::clipboard::write),
        )
    }

    /// Runs a preset action with its fixed params; a no-op when the action is not loaded.
    fn run_preset(&mut self, p: crate::palette::Preset) -> Task<Message> {
        let Some(info) = self.palette.action(p.action).cloned() else {
            return Task::none();
        };
        let mut params = serde_json::Map::new();
        if let Some(period) = p.period {
            params.insert("period".into(), period.as_str().into());
            params.insert(
                "date".into(),
                editor_model::period::iso(self.today()).into(),
            );
        }
        self.palette.close();
        self.run_call(crate::palette::Call {
            action_id: info.id,
            title: info.title,
            params,
            needs: info.needs,
        })
    }

    fn invoke_entry(&mut self, i: usize) -> Task<Message> {
        let Some(e) = self.palette.entries.get(i).cloned() else {
            return Task::none();
        };
        match e.id {
            crate::palette::EntryId::Client(c) => {
                self.palette.close();
                self.update_inner(c.message(self.focus))
            }
            crate::palette::EntryId::Action(id) => self.invoke_action(&id),
            crate::palette::EntryId::Bookmark(p) => {
                self.palette.close();
                self.open_bookmark(&p)
            }
        }
    }

    /// Invokes an action exactly as the palette does: prompt its params, then run.
    fn invoke_action(&mut self, id: &str) -> Task<Message> {
        let Some(info) = self.palette.action(id).cloned() else {
            self.status.set_error(format!("unknown action {id}"));
            return Task::none();
        };
        if !info.params.is_empty() {
            let today = self.today();
            self.palette.begin_prompt(info, today);
            return iced::widget::operation::focus(crate::palette::INPUT_ID);
        }
        self.palette.close();
        self.run_call(crate::palette::Call {
            action_id: info.id,
            title: info.title,
            params: serde_json::Map::new(),
            needs: info.needs,
        })
    }

    /// Save-first (dirty active buffer), then `run_action`.
    pub(crate) fn run_call(&mut self, call: crate::palette::Call) -> Task<Message> {
        let Some(ps) = self.panes.get_mut(self.focus) else {
            return self.dispatch_call(call);
        };
        let Some(note) = ps.active_note(&self.docs).map(|t| t.note) else {
            if call.needs.contains(&cv_protocol::ContextNeed::ActivePath) {
                self.status
                    .set_error(format!("{}: open a note first", call.title));
                return Task::none();
            }
            return self.dispatch_call(call);
        };
        let Some(note) = note.filter(|n| self.docs.by_id(*n).is_some_and(|d| d.buffer.is_dirty()))
        else {
            return self.dispatch_call(call);
        };
        let title = call.title.clone();
        self.save_then(note, Some(AfterSave::RunCall(call)))
            .unwrap_or_else(|| {
                self.status.set_error(format!(
                    "{title}: not run (the note is saving or has a conflict)"
                ));
                Task::none()
            })
    }

    fn dispatch_call(&mut self, call: crate::palette::Call) -> Task<Message> {
        let reading_sel = self.reading_selection();
        let ctx = crate::palette::context(self.active_doc(), reading_sel);
        self.dispatch_with(call, ctx)
    }

    /// Plan round-5 WP-8: the focused note's Reading selection text when its tab shows Reading
    /// — in Split, only when the Reading side selected more recently than the Source side.
    pub(super) fn reading_selection(&self) -> Option<String> {
        let tab = self.active_note()?;
        let mode = self.reading.mode(&self.docs, tab.path);
        if !mode.shows_reading() {
            return None;
        }
        if mode.shows_source()
            && self
                .reading
                .source_selected_last(&self.docs, tab.path, tab.id)
        {
            return None;
        }
        self.reading.selection_text(&self.docs, tab.path, tab.id)
    }

    pub(super) fn dispatch_with(
        &mut self,
        call: crate::palette::Call,
        context: cv_protocol::ActionContext,
    ) -> Task<Message> {
        let params = cv_protocol::RunActionParams {
            action_id: call.action_id,
            params: call.params,
            context,
        };
        let title = call.title;
        let origin = params.context.active_path.as_ref().and_then(|p| {
            let d = self.docs.get(p)?;
            Some((d.id, d.buffer.edit_seq()))
        });
        self.rpc(
            move |d| d.run_action(params).map_err(|e| e.to_string()),
            move |result| {
                Message::Palette(PaletteMsg::Ran {
                    title,
                    origin,
                    result,
                })
            },
        )
    }

    /// A terminal `JobUpdated`: follow its `open_path` (WP-5). A job whose `run_action` reply has
    /// not arrived yet is remembered for [`PaletteMsg::Ran`].
    pub(super) fn job_finished(&mut self, j: &cv_protocol::JobStatus) {
        // WP-6 (usability-sprint-captures-links): `Pending`/`Running` are not terminal — the
        // daemon publishes `Running` when the runner starts a durable job, usually after the
        // `run_action` reply; dropping the origin there left the tab at the vanished path.
        if !crate::palette::terminal(j.status) {
            return;
        }
        if j.status != cv_protocol::JobState::Done {
            self.palette.jobs.job_origins.remove(&j.job_id);
            if j.status != cv_protocol::JobState::Deferred {
                let title = self
                    .palette
                    .jobs
                    .job_titles
                    .get(&j.job_id)
                    .cloned()
                    .unwrap_or_else(|| j.job_id.clone());
                let state = crate::palette::state_label(j.status);
                let reason = j.message.as_deref().unwrap_or("no reason given");
                self.status.notify_from(
                    Severity::Error,
                    format!("{title} {state}: {reason}"),
                    format!("job {}", j.job_id),
                );
            }
            return;
        }
        let Some(open) = j.open_path.clone() else {
            self.palette.jobs.job_origins.remove(&j.job_id);
            return;
        };
        if self.palette.jobs.job_titles.contains_key(&j.job_id) {
            if let Some(origin) = self.palette.jobs.job_origins.remove(&j.job_id) {
                self.follow_open(Some(origin), &open);
            }
            // A non-durable run already followed its reply's `open_path`; a durable run started
            // without a tab has no origin and does not open anything unasked.
        } else {
            if self.palette.jobs.early_open.len() >= 64 {
                self.palette.jobs.early_open.clear();
            }
            self.palette.jobs.early_open.insert(j.job_id.clone(), open);
        }
    }

    /// An action asks the client to open `to`. If the invoking note's document is still open,
    /// not already at `to`, and its file no longer exists at its path, the document follows the
    /// note (as explorer renames do); edits made during the run are kept and surfaced as a
    /// conflict instead of being replaced (audit M2). Otherwise `to` opens as a new tab. Plan
    /// note-identity D4: the origin is the note id, so a move followed meanwhile cannot lose it.
    pub(super) fn follow_open(&mut self, origin: Option<(NoteId, u64)>, to: &str) {
        let Some((id, seq)) = origin else {
            self.open(to);
            return;
        };
        let Some(from) = self.docs.path_of(id).filter(|p| p != to) else {
            self.open(to);
            return;
        };
        if !self.path_open(&from) {
            self.open(to);
            return;
        }
        let Some(d) = self.daemon.clone() else { return };
        match d.read_file(&from) {
            Err(DaemonError::NotFound(_)) => {}
            _ => {
                self.open(to);
                return;
            }
        }
        let edited = self
            .docs
            .by_id(id)
            .is_some_and(|d| d.buffer.edit_seq() != seq);
        self.follow_doc(id, to, super::identity::Follow::Edited(edited));
    }
}
