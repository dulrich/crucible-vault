//! Scroll and place: Source/Reading place carry, find in note, edge / line / page motion and focus.

use super::*;

impl App {
    /// plan editor-ui-round-3 WP-6: for a mode-switch message, the tab path, its current mode
    /// and the source line at the top of the view being left — the Source pane's tracked top
    /// line, or the first source line of the Reading pane's probed top block (line 0 when the
    /// reading pane is at the top / never probed).
    pub(super) fn mode_switch_origin(
        &mut self,
        m: &crate::message::ReadingMsg,
    ) -> Option<(String, crate::reading::ViewMode, usize)> {
        let path = match m {
            crate::message::ReadingMsg::SetMode { path, .. } => path.clone(),
            _ => self.active_note()?.path.to_owned(),
        };
        let old = self.reading.mode(&self.docs, &path);
        let view = self.view_of(&path);
        let line = if old.shows_reading() {
            let doc = self.reading.doc(&self.docs, &path);
            // Plan round-8 WP-4: the block under the `scroll_anchor` height (top at 0).
            self.reading
                .anchor_block(&self.docs, &path, view)
                .and_then(|b| doc?.model.block_lines.get(b).map(|r| r.start))
                .unwrap_or(0)
        } else {
            // Plan round-7 WP-4: measured from the shown editor (wrapped lines made the
            // tracked estimate drift).
            let (a, fs) = (self.prefs.anchor(), self.prefs.font_size);
            buf_mut(&mut self.docs, &path)?.measure_anchor_line(a, fs)
        };
        Some((path, old, line))
    }

    /// plan editor-ui-round-3 WP-6 (audit E1, block granularity, never proportional): after a
    /// mode switch from `old`, puts source `line`'s block at the top of the new view — Source
    /// scrolls to the line when it was hidden (Reading → Source/Split); Reading scrolls its
    /// block (`RenderModel::block_for_line`) to the top via a measured layout probe.
    pub(super) fn carry_place(
        &mut self,
        path: &str,
        old: crate::reading::ViewMode,
        line: usize,
    ) -> Task<Message> {
        let new = self.reading.mode(&self.docs, path);
        if new == old {
            return Task::none();
        }
        let view = self.view_of(path);
        // Plan round-6 WP-1: after Ctrl+PgUp/PgDn (not scrolled since) the new mode snaps to
        // that edge instead of carrying a line estimate.
        if let Some(bottom) = self.reading.edge(&self.docs, path, view) {
            if new.shows_source()
                && !old.shows_source()
                && let Some(doc) = buf_mut(&mut self.docs, path)
            {
                doc.scroll_to_edge(bottom);
            }
            if new.shows_reading() {
                let y = if bottom { 1.0 } else { 0.0 };
                return crate::reading::ScrollIntent::SnapTo {
                    path: path.to_owned(),
                    view,
                    y,
                }
                .task()
                .chain(self.reprobe(path));
            }
            return Task::none();
        }
        if new.shows_source()
            && !old.shows_source()
            && let Some(doc) = buf_mut(&mut self.docs, path)
        {
            doc.scroll_to_line_anchored(line, self.prefs.anchor());
        }
        let above_first = self
            .reading
            .doc(&self.docs, path)
            .and_then(|d| d.model.block_lines.first())
            .is_none_or(|r| line < r.start);
        if new.shows_reading() && above_first {
            // Frontmatter / leading lines: the properties panel heads the view.
            self.reading.clear_top_block(&mut self.docs, path, view);
            return crate::reading::ScrollIntent::ScrollTo {
                path: path.to_owned(),
                view,
                y: 0.0,
            }
            .task();
        }
        if new.shows_reading()
            && let Some(doc) = self.reading.doc(&self.docs, path)
            && let Some(b) = doc.model.block_for_line(line)
        {
            let blocks = doc.model.blocks.len();
            return self.reading.carry_to(&mut self.docs, path, view, blocks, b);
        }
        Task::none()
    }

    pub(super) fn update_find(&mut self, m: FindMsg) -> Task<Message> {
        if let FindMsg::Focus(p) = m {
            // `operation::focus` unfocuses every other focusable, the source editor included. A
            // press on the bar's close button closed it first (Close keeps the editor refocus).
            if self.panes.get(p).is_none_or(|ps| ps.find.is_none()) {
                return Task::none();
            }
            return iced::widget::operation::focus(crate::find::input_id(p));
        }
        let pane = self.focus;
        let Some(ps) = self.panes.get_mut(pane) else {
            return Task::none();
        };
        match m {
            FindMsg::Open => {
                if ps.active_note().is_none() {
                    return Task::none();
                }
                ps.find.get_or_insert_with(Default::default);
                return iced::widget::operation::focus(crate::find::input_id(pane));
            }
            FindMsg::Close => {
                if ps.find.take().is_none() {
                    return Task::none();
                }
                // Plan round-7 WP-4: the reading view keeps the place find left it at (the
                // bar's removal re-creates the scrollable, which reports offset 0).
                let restore = self
                    .active_note()
                    .map(|t| t.path.to_owned())
                    .and_then(|p| {
                        let v = self.view_of(&p);
                        self.reading.restore(&mut self.docs, &p, v)
                    })
                    .unwrap_or_else(Task::none);
                return Task::batch([restore, self.focus_editor()]);
            }
            FindMsg::Focus(_) => {}
            FindMsg::Query(q) => {
                let Some((text, seq)) = ps
                    .active_note()
                    .and_then(|t| buf(&self.docs, t.path))
                    .map(|d| (d.text(), d.edit_seq()))
                else {
                    return Task::none();
                };
                let path = ps
                    .active_note()
                    .map(|t| t.path.to_owned())
                    .unwrap_or_default();
                let view = ps.active_tab().map_or(0, |t| t.id);
                let Some(bar) = &mut ps.find else {
                    return Task::none();
                };
                bar.query = q;
                bar.current = None;
                bar.search_in(
                    &text,
                    seq,
                    reading_find_runs(&self.reading, &self.docs, &path),
                );
                if bar.visible.is_some() {
                    // plan editor-round-4 WP-3: Reading starts at the first visible occurrence
                    // at or below the viewport's top block.
                    let top = self.reading.top_block(&self.docs, &path, view).unwrap_or(0);
                    let v = bar.visible.as_deref().unwrap_or_default();
                    // A footnote hit (no block) sits below every block.
                    let below = |o: &editor_model::visible::Occurrence| {
                        o.part.block().is_none_or(|b| b >= top)
                    };
                    bar.current = (!v.is_empty()).then(|| v.iter().position(below).unwrap_or(0));
                    return self.show_match();
                }
                let at = ps
                    .active_note()
                    .and_then(|t| buf(&self.docs, t.path))
                    .map_or(0, |d| {
                        let c = d.cursor().position;
                        editor_model::undo::offset_of(
                            &text,
                            editor_model::undo::Pos::new(c.line, c.column),
                        )
                        .unwrap_or(0)
                    });
                if let Some(bar) = &mut ps.find {
                    bar.current = editor_model::find::first_from(&bar.matches, at);
                }
                return self.show_match();
            }
            FindMsg::Submit | FindMsg::Next | FindMsg::Prev => {
                let back = matches!(m, FindMsg::Prev)
                    || (matches!(m, FindMsg::Submit) && self.modifiers.shift());
                let Some(ps) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let Some((text, seq)) = ps
                    .active_note()
                    .and_then(|t| buf(&self.docs, t.path))
                    .map(|d| (d.text(), d.edit_seq()))
                else {
                    return Task::none();
                };
                let path = ps
                    .active_note()
                    .map(|t| t.path.to_owned())
                    .unwrap_or_default();
                let Some(bar) = &mut ps.find else {
                    return Task::none();
                };
                bar.search_in(
                    &text,
                    seq,
                    reading_find_runs(&self.reading, &self.docs, &path),
                );
                let n = bar.len();
                bar.current = if back {
                    editor_model::find::prev(bar.current, n)
                } else {
                    editor_model::find::next(bar.current, n)
                };
                return self.show_match();
            }
        }
        Task::none()
    }

    /// Puts the focused pane's current match on screen: the source selection (Source/Split) —
    /// whose cursor move scrolls the shaped editor to it — and in Reading/Split a scroll to the
    /// match's block. Painting is derived from the bar at view time: this only
    /// records the current source match's Reading counterpart on the bar. Positions are byte
    /// columns end to end (no char conversion).
    fn show_match(&mut self) -> Task<Message> {
        let pane = self.focus;
        let Some(ps) = self.panes.get_mut(pane) else {
            return Task::none();
        };
        let Some(tab) = ps.active.and_then(|i| ps.tabs.get(i)) else {
            return Task::none();
        };
        let view = tab.id;
        let Some(path) = tab.path().map(str::to_owned) else {
            return Task::none();
        };
        let Some(bar) = ps.find.as_mut() else {
            return Task::none();
        };
        // plan editor-round-4 WP-3: Reading steps through visible occurrences — the active one
        // is drawn strong and its block scrolled to; invisible source matches never count.
        if bar.visible.is_some() {
            bar.counterpart = None;
            let Some(part) = bar.occurrence().map(|o| o.part) else {
                return Task::none();
            };
            let Some(b) = part.block() else {
                // A footnote hit — the footnotes follow the last block.
                return crate::reading::ScrollIntent::SnapTo { path, view, y: 1.0 }.task();
            };
            self.reading.set_top_block(&mut self.docs, &path, view, b);
            let n = self
                .reading
                .doc(&self.docs, &path)
                .map_or(0, |d| d.model.blocks.len());
            return crate::reading::probe(&path, view, n, Some(b));
        }
        let range = bar.range();
        let Some(doc) = buf(&self.docs, &path) else {
            return Task::none();
        };
        let text = doc.text();
        // The rendered counterpart of the source match (none when it is invisible, audit E2).
        bar.counterpart = range.as_ref().and_then(|r| {
            let d = self.reading.doc(&self.docs, &path)?;
            let (b, n) = crate::reading::source_counterpart(&d.model, &text, &bar.query, r)?;
            let occ = editor_model::visible::occurrences(&d.runs, &bar.query);
            editor_model::visible::ordinal(&occ, b, n)
        });
        let Some(r) = range else {
            return Task::none();
        };
        let (l0, c0) = editor_model::find::line_col(&text, r.start);
        let (l1, c1) = editor_model::find::line_col(&text, r.end);
        let mode = self.reading.mode(&self.docs, &path);
        if mode.shows_source()
            && let Some(doc) = buf_mut(&mut self.docs, &path)
        {
            doc.select_range((l0, c0), (l1, c1));
        }
        if mode.shows_reading() {
            // Plan round-7 WP-4: the match's block goes to the top and becomes the stored top
            // (a bare proportional snap left it stale, so closing find / a mode switch
            // restored an old place).
            let hit = self
                .reading
                .doc(&self.docs, &path)
                .and_then(|d| Some((d.model.block_for_line(l0)?, d.model.blocks.len())));
            if let Some((b, n)) = hit {
                self.reading.set_top_block(&mut self.docs, &path, view, b);
                return crate::reading::probe(&path, view, n, Some(b));
            }
            let lines = text.matches('\n').count().max(1);
            #[allow(clippy::cast_precision_loss)]
            let y = (l0 as f32 / lines as f32).clamp(0.0, 1.0);
            return crate::reading::ScrollIntent::SnapTo {
                path: path.clone(),
                view,
                y,
            }
            .task()
            .chain(self.reprobe(&path));
        }
        Task::none()
    }

    /// Plan round-19-yt-cleanup-intake WP-1: a pane whose active tab changed (activated or
    /// opened) scrolls its tab strip so that tab is visible.
    pub(super) fn scroll_tabs_into_view(&self, before: &[(u64, TabKind)]) -> Task<Message> {
        let tasks: Vec<Task<Message>> = self
            .panes
            .iter()
            .filter_map(|(pane, ps)| {
                let i = ps.active.filter(|&i| i < ps.tabs.len())?;
                let tab = &ps.tabs[i];
                if before.iter().any(|(id, _)| *id == tab.id) {
                    return None;
                }
                Some(iced::widget::operation::snap_to(
                    tab_strip_id(*pane),
                    iced::widget::scrollable::RelativeOffset {
                        x: strip_fraction(i, ps.tabs.len()),
                        y: 0.0,
                    },
                ))
            })
            .collect();
        Task::batch(tasks)
    }

    /// Plan round-19-yt-cleanup-intake WP-1: the one mode-aware "go to source line `line`" of
    /// `path` (0-based) — Outline entries and `[[Note#Heading]]` links. Source/Split move the
    /// cursor (the shaped editor scrolls to it); Reading/Split put the line's block at the top,
    /// stored as the tab's top block so a mode switch or re-activation keeps it.
    pub(super) fn jump_to_line(&mut self, path: &str, line: usize) -> Task<Message> {
        let mode = self.reading.mode(&self.docs, path);
        if mode.shows_source()
            && let Some(doc) = buf_mut(&mut self.docs, path)
        {
            doc.move_to(iced::widget::text_editor::Cursor {
                position: iced::widget::text_editor::Position { line, column: 0 },
                selection: None,
            });
        }
        if mode.shows_reading() {
            let view = self.view_of(path);
            let hit = self
                .reading
                .doc(&self.docs, path)
                .and_then(|d| Some((d.model.block_for_line(line)?, d.model.blocks.len())));
            if let Some((b, n)) = hit {
                self.reading.set_top_block(&mut self.docs, path, view, b);
                return crate::reading::probe(path, view, n, Some(b));
            }
        }
        Task::none()
    }

    /// Plan round-19-yt-cleanup-intake WP-1: jumps `path` to its first heading whose text equals
    /// `fragment` (trimmed; the Obsidian rule). No match, a `^block` fragment or an empty one:
    /// stays where it is.
    pub(super) fn jump_to_heading(&mut self, path: &str, fragment: &str) -> Task<Message> {
        let Some(line) = self
            .doc(path)
            .and_then(|d| heading_line(&d.text(), fragment))
        else {
            return Task::none();
        };
        self.jump_to_line(path, line)
    }

    /// plan editor-ui-round-3 WP-5: Go to top / bottom. Source/Split: the cursor moves to the
    /// buffer's start / end (the shaped editor scrolls to its cursor; the selection drops);
    /// Reading/Split: the reading scrollable snaps to its top / bottom.
    pub(super) fn go_to_edge(&mut self, bottom: bool) -> Task<Message> {
        let Some(path) = self.active_note().map(|t| t.path.to_owned()) else {
            return Task::none();
        };
        let mode = self.reading.mode(&self.docs, &path);
        // Plan round-6 WP-1: a later mode switch snaps the other mode to this edge.
        let view = self.view_of(&path);
        self.reading.set_edge(&mut self.docs, &path, view, bottom);
        if mode.shows_source()
            && let Some(doc) = buf_mut(&mut self.docs, &path)
        {
            doc.go_to_edge(bottom);
        }
        if mode.shows_reading() {
            let y = if bottom { 1.0 } else { 0.0 };
            // plan editor-round-4 WP-2: a programmatic snap emits no `on_scroll`, so the stored
            // top block is re-probed after it (else a view switch carries a stale place).
            return crate::reading::ScrollIntent::SnapTo {
                path: path.clone(),
                view,
                y,
            }
            .task()
            .chain(self.reprobe(&path));
        }
        Task::none()
    }

    /// Re-probes `path`'s reading top block (after a programmatic scroll).
    fn reprobe(&self, path: &str) -> Task<Message> {
        self.reading
            .doc(&self.docs, path)
            .map_or_else(Task::none, |d| {
                crate::reading::probe(path, self.view_of(path), d.model.blocks.len(), None)
            })
    }

    /// plan editor-round-4 WP-2: the scrollable plain PageUp/PageDown pages in the focused
    /// pane — Reading (incl. Split's reading side; a focused source editor captures the key
    /// first) or the Settings page; `None` with a modal open or a Source-only note. The second
    /// field is the note to re-probe afterwards.
    pub fn page_target(&self) -> Option<(iced::widget::Id, Option<String>)> {
        if self.palette.open || self.switcher.open || self.dialog.is_some() {
            return None;
        }
        match &self.active_tab()?.kind {
            TabKind::Settings => {
                Some((crate::settings::body_scroll_id(self.settings.section), None))
            }
            TabKind::Log | TabKind::File { .. } => None,
            TabKind::Note(path) => self
                .reading
                .mode(&self.docs, path)
                .shows_reading()
                .then(|| {
                    (
                        crate::find::reading_scroll_id(path, self.view_of(path)),
                        Some(path.to_owned()),
                    )
                }),
        }
    }

    /// Plan round-5 WP-1: Up/Down — one Reading line in the page target (never with a context
    /// or mode menu open, nor while a widget holds keyboard focus).
    pub(super) fn line_step(&self, down: bool) -> Task<Message> {
        if self.context_menu.is_some() || self.mode_menu.is_some() {
            return Task::none();
        }
        let Some((id, note)) = self.page_target() else {
            return Task::none();
        };
        let dy = crate::page::line_step(self.prefs.font_size, down);
        match note {
            Some(p) => crate::reading::ScrollIntent::Step {
                path: p.clone(),
                view: self.view_of(&p),
                dy,
            }
            .task()
            .chain(self.reprobe(&p)),
            None => crate::page::step(id, dy),
        }
    }

    pub(super) fn page(&self, down: bool) -> Task<Message> {
        let Some((id, note)) = self.page_target() else {
            return Task::none();
        };
        match note {
            Some(p) => crate::reading::ScrollIntent::Page {
                path: p.clone(),
                view: self.view_of(&p),
                down,
            }
            .task()
            .chain(self.reprobe(&p)),
            None => crate::page::page(id, down),
        }
    }

    /// Refocuses the focused pane's source editor (when it shows one).
    fn focus_editor(&self) -> Task<Message> {
        match self.active_note() {
            Some(t) if self.reading.mode(&self.docs, t.path).shows_source() => {
                iced::widget::operation::focus(crate::source::editor_id(t.path))
            }
            _ => Task::none(),
        }
    }

    /// plan editor-round-4 WP-4: `focus_follows_mouse` — the pointer entered `pane`. Ignored
    /// while any modal / menu is open or a mouse button is held (drag, selection). Besides
    /// `self.focus` (which routes source actions via `active_note`), native widget focus moves
    /// too (audit E3): the entered pane's source editor is focused, or — when it shows none —
    /// every focusable is unfocused (`operation::focus` on an id no widget carries), so keys
    /// never land in the previous pane's editor.
    pub(super) fn hover_focus(&mut self, pane: Pane) -> Task<Message> {
        let modal = self.palette.open
            || self.switcher.open
            || self.dialog.is_some()
            || self.context_menu.is_some()
            || self.mode_menu.is_some();
        if !self.prefs.focus_follows_mouse
            || modal
            || self.mouse_down
            || self.focus == pane
            || self.panes.get(pane).is_none()
        {
            return Task::none();
        }
        self.focus = pane;
        match self.active_note() {
            Some(t) if self.reading.mode(&self.docs, t.path).shows_source() => self.focus_editor(),
            _ => iced::widget::operation::focus(iced::widget::Id::new("hover-focus-none")),
        }
    }
}

/// plan editor-round-4 WP-3: the rendered model the find bar counts visible occurrences in —
/// `path` in Reading mode with a render; `None` (source matches drive the bar) otherwise.
fn reading_find_runs<'a>(
    reading: &'a crate::reading::ReadingStore,
    docs: &'a super::Docs,
    path: &str,
) -> Option<&'a [editor_model::visible::VisRun]> {
    (reading.mode(docs, path) == crate::reading::ViewMode::Reading)
        .then(|| reading.doc(docs, path).map(|d| d.runs.as_slice()))
        .flatten()
}

/// Plan round-19-yt-cleanup-intake WP-1: the 0-based line of the first heading of `source`
/// whose text equals `fragment` after trimming (the Outline's heading list; `^block` ids and
/// empty fragments never match).
pub(crate) fn heading_line(source: &str, fragment: &str) -> Option<usize> {
    let want = fragment.trim();
    if want.is_empty() || want.starts_with('^') {
        return None;
    }
    editor_model::outline::outline(source)
        .into_iter()
        .find(|h| h.text.trim() == want)
        .map(|h| h.line)
}

/// Plan round-19-yt-cleanup-intake WP-1: the tab strip scrollable's widget id for `pane`.
pub(crate) fn tab_strip_id(pane: Pane) -> iced::widget::Id {
    iced::widget::Id::from(format!("tab-strip:{pane:?}"))
}

/// Plan round-19-yt-cleanup-intake WP-1: the relative horizontal offset that shows tab `i` of
/// `n` fully — `i / (n - 1)` puts the tab's left edge at `i * (viewport - width) / (n - 1)`
/// into the viewport, so a fixed-width tab is always inside it.
pub(crate) fn strip_fraction(i: usize, n: usize) -> f32 {
    if n < 2 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let f = i as f32 / (n - 1) as f32;
    f.clamp(0.0, 1.0)
}
