//! The top-level `update` dispatcher, panes, nav, panels, views and the subscription.

use super::connection::notification_stream;
use super::*;

impl App {
    /// The update function.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        self.sync_menu_bar();
        let Some(message) = self.modal_filter(message) else {
            return Task::none();
        };
        let before = self.active_paths();
        let views_before = self.active_views();
        let task = self.update_inner(message);
        self.link_ta_validate();
        if self.dialog.is_some() {
            // The dialog outranks the menu: never leave one open beneath it.
            self.context_menu = None;
        }
        let flush = self.flush_switched_away(&before);
        self.sync_buffers();
        let auto = self.schedule_settings_autosave();
        let restore = self.restore_places(&views_before);
        let persist = self.schedule_persist();
        let extra: Vec<Task<Message>> = [flush, auto, restore, persist]
            .into_iter()
            .filter(|t| t.units() > 0)
            .collect();
        if extra.is_empty() {
            return task;
        }
        Task::batch(std::iter::once(task).chain(extra))
    }

    /// Plan round-10 WP-7: the session menu-bar state follows a changed `menu_bar` pref; an
    /// unchanged pref leaves a toggled state alone.
    pub(super) fn sync_menu_bar(&mut self) {
        if self.prefs.menu_bar_shown != self.menu_bar_pref {
            self.menu_bar_pref = self.prefs.menu_bar_shown;
            self.menu_bar_shown = self.menu_bar_pref;
        }
    }

    pub(super) fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PersistDue => self.write_ui_state(),
            Message::MemoryDue => return self.send_memory_report(),
            Message::TrimTick => return self.maybe_trim(),
            Message::AltKey(i) => {
                if self.alt_tap.feed(i) {
                    self.menu_bar_shown = !self.menu_bar_shown;
                }
            }
            Message::Tabs(m) => return self.update_tabs(m),
            Message::Dialog(m) => return self.update_dialog(m),
            Message::Menu(m) => return self.update_menu(m),
            Message::Find(m) => return self.update_find(m),
            Message::Status(m) => return self.update_status(m),
            Message::Theme(m) => {
                let s = match m {
                    ThemeMsg::Cycle => self.prefs.surround.next(),
                    ThemeMsg::Set(s) => s,
                };
                self.set_surround(s);
            }
            Message::Settings(m) => return self.update_settings(m),
            Message::Log(m) => return self.update_log(m),
            Message::Explorer(m) => return self.update_explorer(m),
            Message::Source(SourceMsg::TakeTheirs(path))
                if buf(&self.docs, &path).is_some_and(|d| d.is_missing()) =>
            {
                // plan editor-data-safety WP-2: the file is gone — discard and close its tabs.
                while let Some(id) = self.tab_at(panes::find(&self.panes, &path)).map(|t| t.id) {
                    self.remove_tab(id);
                }
                self.docs.remove(&path);
            }
            Message::Source(SourceMsg::Viewport { path, lines }) => {
                if let Some(d) = buf_mut(&mut self.docs, &path)
                    && d.on_layout(lines) == crate::source::Relayout::Again
                {
                    return crate::reading::ScrollIntent::SourceRelayout { path, lines }.task();
                }
            }
            Message::Source(SourceMsg::LinkKey(k)) => {
                if let Some(next) = self.link_ta_key(k) {
                    return self.update_inner(next);
                }
            }
            Message::Source(SourceMsg::LinkPick(path)) => self.link_ta_accept(&path),
            Message::Source(SourceMsg::Saved {
                path,
                seq,
                outcome,
                after,
            }) => return self.on_save_landed(&path, seq, outcome, after),
            Message::Source(m) => {
                // plan editor-round-4 WP-2: Shift+click extends the selection.
                let m = crate::source::shift_click(m, self.modifiers.shift());
                let active = self.active_note().map(|t| (t.path.to_owned(), t.id));
                let motion = m.is_user_motion();
                if motion && let Some((p, id)) = &active {
                    // Plan round-6 WP-1: any user scroll, motion, edit or key in the source
                    // editor leaves the edge (Ctrl+PgUp/PgDn goes through `go_to_edge`).
                    let view = self.view_of(p);
                    self.reading.clear_edge(&mut self.docs, p, view);
                    // Plan round-5 WP-8: the Source side is now the most recent selecting side.
                    self.reading.note_source_selection(&mut self.docs, p, *id);
                }
                // Plan round-5 WP-9: an edit / motion re-evaluates the `[[` list after it lands.
                let typed_bracket = matches!(
                    &m,
                    SourceMsg::Action(iced::widget::text_editor::Action::Edit(
                        iced::widget::text_editor::Edit::Insert('[')
                    ))
                );
                let path = match m.target() {
                    SourceTarget::Path(p) => Some(p.to_owned()),
                    SourceTarget::Active => active.map(|(p, _)| p),
                    SourceTarget::App => None,
                };
                // Plan round-9 WP-6 (D26): the autosave point lints the buffer first.
                let lint_cfg = matches!(&m, SourceMsg::AutosaveDue { .. })
                    .then(|| self.auto_lint_cfg())
                    .flatten();
                let Some(buf) = path.and_then(|p| self.docs.get_mut(&p)) else {
                    return Task::none();
                };
                let fx = crate::source::update(
                    &mut buf.buffer,
                    m,
                    self.daemon.as_ref(),
                    lint_cfg.as_ref(),
                );
                let mut tasks = Vec::new();
                for e in fx {
                    match e {
                        crate::source::Effect::Task(t) => tasks.push(t),
                        crate::source::Effect::Error(e) => self.status.set_error(e),
                        // The frontmatter changed: re-render Reading now, not on its next tick.
                        crate::source::Effect::Relinted => {
                            self.reading.recompute(buf, self.daemon.as_ref());
                        }
                    }
                }
                if motion {
                    self.link_ta_sync(typed_bracket);
                }
                return Task::batch(tasks);
            }
            Message::Modifiers(m) => self.modifiers = m,
            Message::TitleRenamed {
                from,
                to,
                seq,
                result,
            } => return self.title_renamed(&from, &to, seq, result),
            Message::OpenUrl(url) => {
                if let Err(e) = crate::open_url::open(&self.prefs.open_url_cmd, &url) {
                    self.status.set_error(e);
                }
            }
            Message::Reading(crate::message::ReadingMsg::ToggleTask { path, line }) => {
                return self.toggle_task(&path, line);
            }
            Message::Reading(crate::message::ReadingMsg::ToggleProp { path, key }) => {
                return self.toggle_prop(&path, &key);
            }
            Message::Reading(crate::message::ReadingMsg::GoToEdge { bottom }) => {
                return self.go_to_edge(bottom);
            }
            Message::Reading(crate::message::ReadingMsg::TitleSubmit { path }) => {
                return self.title_submit(&path);
            }
            Message::Reading(crate::message::ReadingMsg::TitleFocus { path, focused }) => {
                if !focused {
                    return self.title_submit(&path);
                }
            }
            // Plan round-14 WP-2: a link click with Ctrl/Cmd held opens a new tab (decided here,
            // on the click-originated message only; `TabsMsg::Open` keeps its placement).
            Message::Reading(m @ crate::message::ReadingMsg::OpenLink { .. })
                if self.modifiers.command() =>
            {
                let m = crate::reading::new_tab_link(m, true);
                return self.update_inner(Message::Reading(m));
            }
            Message::Reading(m) => {
                let mode_change = matches!(
                    m,
                    crate::message::ReadingMsg::CycleMode
                        | crate::message::ReadingMsg::SetMode { .. }
                );
                // plan editor-ui-round-3 WP-6: the switch's (path, old mode) and the source
                // line at the top before it, carried to the new mode below.
                let carry = if mode_change {
                    self.mode_switch_origin(&m)
                } else {
                    None
                };
                let active = self
                    .panes
                    .get(self.focus)
                    .and_then(PaneState::active_note)
                    .map(|t| t.path);
                let fx = crate::reading::update(
                    &mut self.reading,
                    m,
                    &mut self.docs,
                    active,
                    self.daemon.as_ref(),
                );
                let mut tasks = Vec::new();
                for e in fx {
                    tasks.push(match e {
                        crate::reading::Effect::Task(t) => t,
                        crate::reading::Effect::Notice(severity, text) => {
                            self.status.notify(severity, text);
                            continue;
                        }
                        crate::reading::Effect::MissingLink(target) => {
                            self.missing_link_click(&target)
                        }
                    });
                }
                if mode_change {
                    self.mode_menu = None;
                }
                if let Some((path, old, line)) = carry {
                    tasks.push(self.carry_place(&path, old, line));
                }
                return Task::batch(tasks);
            }
            Message::Switcher(m) => return self.update_switcher(m),
            Message::Palette(m) => return self.update_palette(m),
            Message::Panels(m) => return self.update_panels(m),
            Message::Views(m) => return self.update_views(m),
            Message::Nav(m) => return self.update_nav(m),
            Message::Panes(m) => return self.update_panes(m),
        }
        Task::none()
    }

    fn update_panes(&mut self, m: PanesMsg) -> Task<Message> {
        match m {
            PanesMsg::SplitRight | PanesMsg::SplitDown => {
                let dir = if matches!(m, PanesMsg::SplitRight) {
                    SplitDir::Right
                } else {
                    SplitDir::Down
                };
                if let Some((p, _)) =
                    self.panes
                        .split(panes::axis_of(dir), self.focus, PaneState::default())
                {
                    self.focus = p;
                }
            }
            PanesMsg::Close(pane) => {
                let closed = if self.panes.len() > 1 {
                    match self.panes.close(pane) {
                        Some((state, sibling)) => {
                            if self.focus == pane || self.panes.get(self.focus).is_none() {
                                self.focus = sibling;
                            }
                            state.tabs
                        }
                        None => Vec::new(),
                    }
                } else {
                    self.panes.get_mut(pane).map_or_else(Vec::new, |ps| {
                        ps.active = None;
                        std::mem::take(&mut ps.tabs)
                    })
                };
                for t in &closed {
                    self.on_tab_closed(t);
                }
            }
            PanesMsg::Focus(pane) => {
                if self.focus != pane && self.panes.get(pane).is_some() {
                    self.focus = pane;
                }
            }
            PanesMsg::Hover(pane) => return self.hover_focus(pane),
            PanesMsg::MouseButton(down) => self.mouse_down = down,
            PanesMsg::Resized(ev) => self.panes.resize(ev.split, ev.ratio),
            PanesMsg::Bookmark(b) => return self.update_bookmarks(b),
            PanesMsg::Back(pane) => self.history_step(pane, false),
            PanesMsg::Forward(pane) => self.history_step(pane, true),
            PanesMsg::Page(down) => return self.page(down),
            PanesMsg::BackFocused => self.history_step(self.focus, false),
            PanesMsg::ForwardFocused => self.history_step(self.focus, true),
            PanesMsg::ToggleSide => {
                self.ui.side_collapsed = !self.ui.side_collapsed;
            }
            PanesMsg::ToggleBorders => {
                self.ui.pane_borders = !self.ui.pane_borders;
            }
            PanesMsg::ModeMenu(p) => self.mode_menu = p,
        }
        Task::none()
    }

    fn update_nav(&mut self, m: NavMsg) -> Task<Message> {
        match m {
            NavMsg::Go { pane, target } => match target {
                NavTarget::Path(p) => {
                    match self.open_into(pane, &p, self.nav_place(self.modifiers.command())) {
                        Ok(()) => {}
                        // Plan round-11 WP-7: a link to a missing note creates it (the
                        // configured path is already a full vault path) and `Done` opens it
                        // in the focused pane.
                        Err(DaemonError::NotFound(_)) => {
                            if self.panes.get(pane).is_some() {
                                self.focus = pane;
                            }
                            return self.dispatch_op(FileOp::Create { path: p });
                        }
                        Err(e) => self.status.set_error(open_error(&e)),
                    }
                }
                NavTarget::Period(period, date) => {
                    let params = nav::period_params(period, date);
                    let new_tab = self.modifiers.command();
                    return self.rpc(
                        move |d| {
                            d.run_action(params)
                                .map(|r| r.open_path)
                                .map_err(|e| e.to_string())
                        },
                        move |result| {
                            Message::Nav(NavMsg::Opened {
                                pane,
                                new_tab,
                                result,
                            })
                        },
                    );
                }
            },
            NavMsg::Opened {
                pane,
                new_tab,
                result,
            } => match result {
                Ok(Some(p)) => match self.open_into(pane, &p, self.nav_place(new_tab)) {
                    Ok(()) => {}
                    Err(e) => self.status.set_error(open_error(&e)),
                },
                Ok(None) => self.status.set_error("period.open returned no path".into()),
                Err(e) => self.status.set_error(format!("period.open: {e}")),
            },
        }
        Task::none()
    }

    fn update_panels(&mut self, m: PanelsMsg) -> Task<Message> {
        use crate::panels::Effect;
        match m {
            PanelsMsg::Search
                if self.prefs.search_location
                    == crate::settings::editor_prefs::SearchLocation::Modal =>
            {
                // Decision 9/10 (round-12): one centre modal at a time.
                self.switcher.open = false;
                self.palette.close();
                self.search_modal = true;
                return iced::widget::operation::focus(crate::panels::SEARCH_MODAL_ID);
            }
            PanelsMsg::Search => {
                return self.update_panels(PanelsMsg::Show(crate::panels::PanelKind::Search));
            }
            PanelsMsg::CloseSearchModal => {
                self.search_modal = false;
                return Task::none();
            }
            PanelsMsg::Open(_) => self.search_modal = false,
            _ => {}
        }
        match crate::panels::update(&mut self.panels, m, self.daemon.is_some()) {
            Effect::None => Task::none(),
            Effect::Sync => self.sync_panels(),
            Effect::SyncAndFocusSearch => Task::batch([
                self.sync_panels(),
                iced::widget::operation::focus(crate::panels::SEARCH_ID),
            ]),
            Effect::OutlineJump(line) => {
                if let Some(doc) = self.active_doc_mut() {
                    doc.move_to(iced::widget::text_editor::Cursor {
                        position: iced::widget::text_editor::Position { line, column: 0 },
                        selection: None,
                    });
                }
                Task::none()
            }
            Effect::RunCall(call) => self.run_call(call),
            Effect::Open(p) => {
                self.open(&p);
                Task::none()
            }
            Effect::Error(e) => {
                self.status.set_error(e);
                Task::none()
            }
            Effect::PickTag(tag) => {
                if let Some(d) = self.daemon.clone()
                    && let Err(e) = self.panels.pick_tag(d.as_ref(), &tag)
                {
                    self.status.set_error(format!("tags: {e}"));
                }
                Task::none()
            }
            Effect::SearchDebounce(seq) => {
                let wait = self.prefs.search_debounce();
                Task::perform(async move { tokio::time::sleep(wait).await }, move |()| {
                    Message::Panels(PanelsMsg::SearchDue(seq))
                })
            }
            Effect::SearchRun { seq, q } => {
                let limit = self.prefs.search_limit;
                self.rpc(
                    move |d| crate::panels::run_search(d, &q, limit),
                    move |result| Message::Panels(PanelsMsg::SearchDone { seq, result }),
                )
            }
        }
    }

    /// Brings the panels up to date with the focused pane's active tab (cheap when unchanged).
    fn sync_panels(&mut self) -> Task<Message> {
        if !self.panels.visible {
            return Task::none();
        }
        let t0 = std::time::Instant::now();
        let generation = self.status.generation;
        let d = self.daemon.clone();
        let tab = self
            .panes
            .get(self.focus)
            .and_then(PaneState::active_note)
            .and_then(|t| buf(&self.docs, t.path));
        self.panels.sync_buffer(tab, d.as_deref());
        if let Some(d) = &d
            && let Err(e) = self.panels.sync_tags(d.as_ref(), generation)
        {
            self.status.set_error(format!("tags: {e}"));
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        if ms >= 1.0 {
            tracing::debug!(elapsed_ms = ms, "sync_panels");
        }
        match self.panels.backlinks_due(generation) {
            Some(path) => {
                let p = path.clone();
                self.rpc(
                    move |d| crate::panels::load_backlinks(d, &p),
                    move |result| Message::Panels(PanelsMsg::Backlinks { path, result }),
                )
            }
            None => Task::none(),
        }
    }

    /// WP-7: view fences (`crate::views`); `view_run` runs off the UI thread.
    fn update_views(&mut self, m: crate::message::ViewsMsg) -> Task<Message> {
        match crate::views::update(&mut self.views, m) {
            crate::views::Effect::None => Task::none(),
            crate::views::Effect::Run {
                id,
                host,
                sort,
                select,
            } => {
                let (rid, rhost) = (id.clone(), host.clone());
                self.rpc(
                    move |d| {
                        d.view_run(&rid, rhost.as_deref(), sort.as_ref(), select.as_ref())
                            .map(|r| r.0)
                            .map_err(|e| e.to_string())
                    },
                    move |result| {
                        Message::Views(crate::message::ViewsMsg::Loaded { id, host, result })
                    },
                )
            }
            crate::views::Effect::SetStatus { key, status } => {
                let p = cv_protocol::TrackerItemSetStatusParams {
                    source_kind: key.source_kind,
                    source_key: key.source_key,
                    item_id: key.item_id,
                    status,
                };
                self.rpc(
                    move |d| d.tracker_item_set_status(&p).map_err(|e| e.to_string()),
                    |r| Message::Views(crate::message::ViewsMsg::StatusSet(r)),
                )
            }
            crate::views::Effect::SetBool { path, key, value } => {
                self.set_note_bool(path, key, value)
            }
            crate::views::Effect::Notice(text) => {
                self.status.set_error(text);
                Task::none()
            }
            crate::views::Effect::Open { path, line } => {
                // Plan round-14 WP-2: Ctrl/Cmd+click on a link cell adds a new tab.
                let how = if self.modifiers.command() {
                    Place::NewTab
                } else {
                    Place::Focus
                };
                self.open_with(&path, how);
                let line = line.and_then(|l| usize::try_from(l.saturating_sub(1)).ok());
                if let Some(line) = line
                    && self.active_note().is_some_and(|t| t.path == path)
                    && let Some(doc) = buf_mut(&mut self.docs, &path)
                {
                    doc.move_to(iced::widget::text_editor::Cursor {
                        position: iced::widget::text_editor::Position { line, column: 0 },
                        selection: None,
                    });
                }
                Task::none()
            }
        }
    }

    /// Daemon notifications and global keys as iced subscriptions.
    pub fn subscription(&self) -> Subscription<Message> {
        let captured_esc = if self.switcher.open {
            iced::event::listen_with(switcher::captured_escape)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            Subscription::run_with(self.notifications.clone(), notification_stream),
            captured_esc,
            // WP-4: track held mouse buttons only while hover focus is on (drag guard).
            if self.prefs.focus_follows_mouse {
                iced::event::listen_with(mouse_button_event)
            } else {
                Subscription::none()
            },
            // plan round-10 WP-7: every key/mouse press + Alt release feeds the bare-Alt tracker
            // (captured or not; a bare Alt has no chord, so the palette keymap never sees a tap).
            iced::event::listen_with(super::alt_tap::input_of),
            // plan round-10 WP-8: cursor moves, the left-button release and Esc, only while a
            // tab header is pressed (no per-move churn otherwise).
            if self.tab_drag.armed() {
                iced::event::listen_with(tab_drag_event)
            } else {
                Subscription::none()
            },
            iced::keyboard::listen().filter_map(switcher::key_event),
            // plan round-5 WP-4: Up/Down/Escape (captured or not) for an open form typeahead.
            if self.settings.form.typeahead.is_some() && !self.switcher.open {
                iced::event::listen_with(crate::settings::typeahead_key_event)
            } else {
                Subscription::none()
            },
            // plan round-13 WP-2: Tab / Shift+Tab between Settings inputs, only on the Settings tab.
            if self.settings_traversal_active() {
                iced::keyboard::listen().filter_map(|e| crate::settings::traverse_key_event(&e))
            } else {
                Subscription::none()
            },
            iced::keyboard::listen().filter_map(|e| crate::dialog::key_event(&e)),
            self.reading.subscription(&self.docs),
            // plan memory-instrumentation-and-reduction WP-M2: a snapshot to the daemon every 5 min.
            iced::time::every(crate::memory::REPORT_EVERY).map(|_| Message::MemoryDue),
            // plan memory-instrumentation-and-reduction WP-M4: quiet-period trim, every mode.
            iced::time::every(crate::memory::TRIM_TICK).map(|_| Message::TrimTick),
            // Any input is activity (records a timestamp; emits no message).
            iced::event::listen_with(|_, _, _| {
                crate::memory::touch();
                None
            }),
            crate::palette::subscription(&self.palette, self.panels.visible),
            // plan round-5 WP-3: a 1 s expiry tick while a notice is up (shares PaletteMsg::Tick).
            if self.status.notice.is_some() && self.palette.jobs.toast.is_none() {
                iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Message::Palette(crate::message::PaletteMsg::Tick))
            } else {
                Subscription::none()
            },
            crate::find::subscription(self.panes.iter().any(|(_, ps)| ps.find.is_some())),
            // Window close goes through Quit (dirty buffers flush first).
            iced::window::close_requests().map(|_| Message::Tabs(TabsMsg::Quit)),
            // WP-3: focus loss flushes every dirty note buffer.
            iced::window::events().filter_map(|(_, e)| {
                matches!(e, iced::window::Event::Unfocused)
                    .then_some(Message::Tabs(TabsMsg::FlushAll))
            }),
        ])
    }
}

/// WP-4: mouse press / release → [`PanesMsg::MouseButton`] (captured or not).
fn mouse_button_event(
    e: iced::Event,
    _: iced::event::Status,
    _: iced::window::Id,
) -> Option<Message> {
    match e {
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_)) => {
            Some(Message::Panes(PanesMsg::MouseButton(true)))
        }
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(_)) => {
            Some(Message::Panes(PanesMsg::MouseButton(false)))
        }
        _ => None,
    }
}

/// Plan round-10 WP-8: cursor move / left release / Esc → the tab-drag messages (any status).
fn tab_drag_event(e: iced::Event, _: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    use iced::keyboard::{Event as K, Key, key::Named};
    use iced::mouse::{Button, Event as M};
    match e {
        iced::Event::Mouse(M::CursorMoved { position }) => {
            Some(Message::Tabs(TabsMsg::DragMove(position)))
        }
        iced::Event::Mouse(M::ButtonReleased(Button::Left)) => {
            Some(Message::Tabs(TabsMsg::DragRelease))
        }
        iced::Event::Keyboard(K::KeyPressed { key, .. })
            if key.as_ref() == Key::Named(Named::Escape) =>
        {
            Some(Message::Tabs(TabsMsg::DragCancel))
        }
        _ => None,
    }
}
