//! `crucible-view` fence rendering of `ViewResult`s (WP-7).
//!
//! The fence body is `<view-file>#<View name>`; the view runs in the daemon (`view_run`) with
//! `this` = the embedding note. Results are cached per `(view_id, this_path)` in [`ViewStore`].
//! The fence is wrapped in an iced `sensor`: when it comes into view it asks for its result
//! ([`ViewsMsg::Need`]); the app runs `view_run` off the UI thread and feeds back
//! [`ViewsMsg::Loaded`]. `views_invalidated` marks matching entries stale and bumps their epoch,
//! which is part of the sensor key, so a fence that is on screen fires `Need` again.
//!
//! The result decodes with serde straight into `views::result`:
//! an unknown cell tag decodes as `Cell::Unknown` and renders empty; a malformed result
//! becomes an [`State::Error`] entry shown in the fence. Cell behaviour follows the producer-set
//! [`ColumnRole`], never column-key strings. Text cells written `~~x~~` (missing initiative
//! files, `defs/initiatives.toml`) render struck through and dimmed.

mod layout;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use iced::font::Weight;
use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, sensor, space, text};
use iced::{Border, Element, Font, Length, Theme};
use serde::Deserialize;
use serde_json::Value;

pub use layout::column_lengths;
use layout::line_view;

use crate::message::{Message, ViewsMsg};
use crate::theme::Tokens;
use crate::ui_state::{ViewSortPref, view_sort_key};
use crate::widget::virtual_list::{ListWindow, VirtualList};

/// Row height of the result table.
pub const ROW_HEIGHT: f32 = 26.0;
/// More lines than this render through the virtual list in a bounded box.
pub const VIRTUAL_THRESHOLD: usize = 30;
/// Height of the bounded box.
pub const MAX_HEIGHT: f32 = 480.0;

/// `(view_id, this_path)`.
pub type Key = (String, Option<String>);

pub use views::result::{
    Cell, Column, ColumnRole, Kind, Row, cell_checked, cell_status, has_open_columns, row_is_open,
};

/// A flattened display line: group headers, rows and footers in one list.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Group(Cell),
    Row(Row),
    /// A footer aligned with the columns (`label`, cells; pivot footers carry the label in
    /// cell 0 already).
    Summary {
        label: String,
        cells: Vec<Cell>,
    },
}

/// A decoded `views::ViewResult`, its groups flattened into display [`Line`]s.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub kind: Kind,
    /// The producer's columns (key, label, [`ColumnRole`]).
    pub columns: Vec<Column>,
    pub lines: Vec<Line>,
    pub notices: Vec<String>,
    /// Plan round-8 WP-2: the view's `maxHeight` (`None` = [`MAX_HEIGHT`]).
    pub max_height: Option<views::result::MaxHeight>,
    /// Plan round-9-feedback-todos WP-5: the indices into `lines` kept by "Only open" (see
    /// [`open_only_lines`]); `None` when the result has no Checkbox / Status column (no toggle).
    pub open_lines: Option<Vec<usize>>,
    /// Plan round-13 WP-4: the view's `select_by` property (`None` = not selectable).
    pub select_by: Option<String>,
}

/// Plan round-9-feedback-todos WP-5: the indices into `lines` that stay visible with "Only
/// open" on — every row that is open ([`row_is_open`]) and every summary, minus the group
/// headers left with no row (a [`Line::Group`] not directly followed by a kept [`Line::Row`]).
pub fn open_only_lines(lines: &[Line], cols: &[Column]) -> Vec<usize> {
    let kept: Vec<usize> = (0..lines.len())
        .filter(|&i| match &lines[i] {
            Line::Row(r) => row_is_open(r, cols),
            _ => true,
        })
        .collect();
    kept.iter()
        .enumerate()
        .filter(|&(k, &i)| {
            !matches!(lines[i], Line::Group(_))
                || kept
                    .get(k + 1)
                    .is_some_and(|&n| matches!(lines[n], Line::Row(_)))
        })
        .map(|(_, &i)| i)
        .collect()
}

/// Plan round-8 WP-2: the fixed height of a virtual list for `d`, or `None` when the view is
/// uncapped (`maxHeight = "none"`: all rows render as a plain column inside Reading's scroll).
pub fn list_height(d: &Decoded) -> Option<f32> {
    match d.max_height {
        None => Some(MAX_HEIGHT),
        Some(views::result::MaxHeight::Px(h)) => Some(h),
        Some(views::result::MaxHeight::Uncapped) => None,
    }
}

/// A cache entry's state.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Loading,
    Ready(Decoded),
    Error(String),
}

/// One cached result.
#[derive(Debug, Clone)]
pub struct Entry {
    pub state: State,
    /// A `view_run` is outstanding (dedupe).
    pub in_flight: bool,
    /// Invalidated since the last result: the next `Need` re-requests.
    pub stale: bool,
    /// Bumped on invalidation; part of the fence sensor key.
    pub epoch: u64,
    pub window: ListWindow,
}

impl Entry {
    /// WP-R3 (review E10): a loaded view with a `select_by` property (the one predicate).
    fn is_select_ready(&self) -> bool {
        matches!(&self.state, State::Ready(d) if d.select_by.is_some())
    }
}

/// Plan round-5 WP-1: the widget id of the virtual result list of view `id` in note `host`.
pub fn list_id(id: &str, host: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("views-list:{host}:{id}"))
}

/// The per-app view cache.
#[derive(Debug, Default)]
pub struct ViewStore {
    pub entries: HashMap<Key, Entry>,
    /// Plan round-9-feedback-todos WP-5: ids of the views whose "Only open" toggle is on
    /// (persisted as `UiState.view_filters`).
    pub filters: BTreeSet<String>,
    /// Plan round-11 WP-5: column-header sorts keyed by [`view_sort_key`]
    /// (persisted as `UiState.view_sorts`).
    pub sorts: BTreeMap<String, ViewSortPref>,
    /// Plan round-13 WP-4: the in-place selection of each host note (the clicked serial row's
    /// name). In memory only; dropped with the host's tab.
    pub selections: BTreeMap<String, String>,
}

/// The next header-click state of a column: none/other → ASC, ASC → DESC, DESC → cleared.
pub fn next_sort(current: Option<&ViewSortPref>, column: &str) -> Option<ViewSortPref> {
    match current {
        Some(c) if c.column == column && c.desc => None,
        Some(c) if c.column == column => Some(ViewSortPref {
            column: column.to_owned(),
            desc: true,
        }),
        _ => Some(ViewSortPref {
            column: column.to_owned(),
            desc: false,
        }),
    }
}

impl ViewStore {
    /// The active header sort of `(id, host)`.
    pub fn sort_of(&self, id: &str, host: Option<&str>) -> Option<&ViewSortPref> {
        self.sorts.get(&view_sort_key(id, host))
    }

    /// [`Self::sort_of`] as the wire type.
    fn wire_sort(&self, id: &str, host: Option<&str>) -> Option<cv_protocol::ViewSort> {
        self.sort_of(id, host).map(|s| cv_protocol::ViewSort {
            property: s.column.clone(),
            direction: if s.desc {
                cv_protocol::ViewSortDir::Desc
            } else {
                cv_protocol::ViewSortDir::Asc
            },
        })
    }

    /// Plan round-13 WP-4: the `select_by` property of the loaded view `(id, host)`.
    fn select_by(&self, id: &str, host: Option<&str>) -> Option<&str> {
        match self.get(id, host).map(|e| &e.state) {
            Some(State::Ready(d)) => d.select_by.as_deref(),
            _ => None,
        }
    }

    /// Plan round-13 WP-4: the selection to send with `view_run(id, host)` — the host's selected
    /// value under the view's `select_by` property; `None` for any other view.
    fn wire_select(&self, id: &str, host: Option<&str>) -> Option<cv_protocol::ViewSelect> {
        let property = self.select_by(id, host)?;
        let value = self.selections.get(host?)?;
        Some(cv_protocol::ViewSelect {
            property: property.to_owned(),
            value: value.clone(),
        })
    }

    /// Plan round-13 WP-4: whether note `host` embeds a loaded `select_by` view (its sources
    /// sections' serial rows are then clickable).
    pub fn host_selects(&self, host: &str) -> bool {
        self.entries
            .iter()
            .any(|((_, h), e)| h.as_deref() == Some(host) && e.is_select_ready())
    }

    /// WP-R3 (review E10): marks the entry at `key` stale and bumps its epoch (a shown fence asks
    /// again). `Loaded` re-bumps the epoch of a stale entry, so an in-flight run is covered too.
    fn invalidate_entry(&mut self, key: &Key) {
        if let Some(e) = self.entries.get_mut(key) {
            e.stale = true;
            e.epoch = e.epoch.wrapping_add(1);
        }
    }

    /// WP-R3 (review E10): claims the idle entry at `key` for a run (`false` while one is
    /// outstanding or the entry is absent).
    fn claim(&mut self, key: &Key) -> bool {
        match self.entries.get_mut(key) {
            Some(e) if !e.in_flight => {
                e.in_flight = true;
                e.stale = false;
                true
            }
            _ => false,
        }
    }

    /// WP-R3 (review E10): the run request for the claimed view `id` in `host`.
    fn run_effect(&self, id: String, host: Option<String>) -> Effect {
        let sort = self.wire_sort(&id, host.as_deref());
        let select = self.wire_select(&id, host.as_deref());
        Effect::Run {
            id,
            host,
            sort,
            select,
        }
    }

    /// Plan round-13 WP-4: toggles `host`'s selection to `value` (the same value clears it) and
    /// re-runs its `select_by` views: the first idle one now, the rest through their bumped
    /// epoch (a shown fence asks again).
    fn select(&mut self, host: String, value: String) -> Effect {
        if self.selections.get(&host) == Some(&value) {
            self.selections.remove(&host);
        } else {
            self.selections.insert(host.clone(), value);
        }
        let mut ids: Vec<String> = self
            .entries
            .iter()
            .filter(|((_, h), e)| h.as_deref() == Some(host.as_str()) && e.is_select_ready())
            .map(|((id, _), _)| id.clone())
            .collect();
        ids.sort();
        let mut run = None;
        for id in ids {
            let key = (id.clone(), Some(host.clone()));
            self.invalidate_entry(&key);
            if run.is_none() && self.claim(&key) {
                run = Some(id);
            }
        }
        match run {
            Some(id) => self.run_effect(id, Some(host)),
            None => Effect::None,
        }
    }

    /// Plan round-5 WP-1: re-applies the stored scroll window of every virtual list hosted in
    /// note `host` (its scrollable state is positional and lost while the tab was hidden).
    pub fn restore<T: Send + 'static>(&self, host: &str) -> Vec<iced::Task<T>> {
        self.entries
            .iter()
            .filter(|((_, h), e)| h.as_deref() == Some(host) && e.window.offset > 0.0)
            .map(|((id, _), e)| {
                iced::widget::operation::scroll_to(
                    list_id(id, host),
                    iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(e.window.offset),
                    },
                )
            })
            .collect()
    }
}

/// What the app must do after [`update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    /// Run `view_run(id, host)` off the UI thread.
    Run {
        id: String,
        host: Option<String>,
        /// The column-header sort to send with the request.
        sort: Option<cv_protocol::ViewSort>,
        /// Plan round-13 WP-4: the host's selection, for a `select_by` view.
        select: Option<cv_protocol::ViewSelect>,
    },
    /// Open `path` in the focused pane, cursor at 1-based `line` when set.
    Open {
        path: String,
        line: Option<i64>,
    },
    /// Plan round-6-feedback-todos WP-6: run `tracker_item_set_status` off the UI thread.
    SetStatus {
        key: views::result::TrackerKey,
        status: Option<char>,
    },
    /// Plan round-8-intake-views-anchor WP-3: set frontmatter boolean `key` of `path`.
    SetBool {
        path: String,
        key: String,
        value: bool,
    },
    /// A failure notice for the status bar.
    Notice(String),
}

/// A malformed `ViewResult`.
#[derive(Debug, thiserror::Error)]
#[error("malformed view result: {0}")]
pub struct DecodeError(#[from] serde_json::Error);

impl ViewStore {
    /// The entry for `(id, host)`.
    pub fn get(&self, id: &str, host: Option<&str>) -> Option<&Entry> {
        self.entries.get(&(id.to_owned(), host.map(str::to_owned)))
    }

    /// `views_invalidated`: entries whose id is in `ids` (all when empty) go stale and bump
    /// their epoch, so shown fences request again.
    pub fn invalidate(&mut self, ids: &[String]) {
        for ((id, _), e) in &mut self.entries {
            if ids.is_empty() || ids.contains(id) {
                e.stale = true;
                e.epoch = e.epoch.wrapping_add(1);
            }
        }
    }

    /// The host note `from` was renamed/moved to `to` (plan editor-feedback-round-2 WP-8): its
    /// hosted entries move to `to` keeping their shown state and scroll window (no flash), go
    /// stale and bump their epoch so the fence re-runs against `to`. A run in flight for `from`
    /// is abandoned (its `Loaded` finds no `from` entry).
    pub fn rehost(&mut self, from: &str, to: &str) {
        let keys: Vec<Key> = self
            .entries
            .keys()
            .filter(|(_, h)| h.as_deref() == Some(from))
            .cloned()
            .collect();
        for k in keys {
            if let Some(mut e) = self.entries.remove(&k) {
                e.in_flight = false;
                e.stale = true;
                e.epoch = e.epoch.wrapping_add(1);
                self.entries.insert((k.0, Some(to.to_owned())), e);
            }
        }
        if let Some(v) = self.selections.remove(from) {
            self.selections.insert(to.to_owned(), v);
        }
    }

    /// A tab closed: drop the results it hosted.
    pub fn on_tab_closed(&mut self, path: &str) {
        self.entries.retain(|(_, h), _| h.as_deref() != Some(path));
        self.selections.remove(path);
    }
}

/// Applies `m`; the caller performs the returned [`Effect`].
pub fn update(store: &mut ViewStore, m: ViewsMsg) -> Effect {
    match m {
        ViewsMsg::Need { id, host } => {
            let e = store
                .entries
                .entry((id.clone(), host.clone()))
                .or_insert_with(|| Entry {
                    state: State::Loading,
                    in_flight: false,
                    stale: true,
                    epoch: 0,
                    window: ListWindow::default(),
                });
            if !e.stale || !store.claim(&(id.clone(), host.clone())) {
                return Effect::None;
            }
            store.run_effect(id, host)
        }
        ViewsMsg::SortBy { id, host, column } => {
            let key = view_sort_key(&id, host.as_deref());
            match next_sort(store.sorts.get(&key), &column) {
                Some(s) => store.sorts.insert(key, s),
                None => store.sorts.remove(&key),
            };
            let key = (id.clone(), host.clone());
            store.invalidate_entry(&key);
            if store.claim(&key) {
                store.run_effect(id, host)
            } else {
                Effect::None
            }
        }
        ViewsMsg::Loaded { id, host, result } => {
            if let Some(e) = store.entries.get_mut(&(id, host)) {
                e.in_flight = false;
                if e.stale {
                    // Invalidated while in flight (WP-8): the fence's `Need` for the bumped
                    // epoch was refused then, so bump again to make a shown fence re-request.
                    e.epoch = e.epoch.wrapping_add(1);
                }
                e.state = match result.and_then(|v| decode(&v).map_err(|e| e.to_string())) {
                    Ok(d) => State::Ready(d),
                    Err(err) => State::Error(err),
                };
            }
            Effect::None
        }
        ViewsMsg::Scrolled { id, host, window } => {
            if let Some(e) = store.entries.get_mut(&(id, host)) {
                e.window = window;
            }
            Effect::None
        }
        ViewsMsg::Open { path, line } => Effect::Open { path, line },
        ViewsMsg::SetStatus { key, status } => match views::result::TrackerKey::decode(&key) {
            Some(key) => Effect::SetStatus { key, status },
            None => Effect::Notice("status toggle: malformed row key".into()),
        },
        // Success needs nothing: the refresh arrives as `views_invalidated`.
        ViewsMsg::StatusSet(Ok(())) => Effect::None,
        ViewsMsg::StatusSet(Err(e)) => Effect::Notice(format!("status change failed: {e}")),
        ViewsMsg::SetBool { path, key, value } => Effect::SetBool { path, key, value },
        // Success needs nothing: the refresh arrives with the file change.
        ViewsMsg::BoolSet(Ok(())) => Effect::None,
        ViewsMsg::ToggleOpenOnly { id } => {
            if !store.filters.remove(&id) {
                store.filters.insert(id);
            }
            Effect::None
        }
        ViewsMsg::BoolSet(Err(e)) => Effect::Notice(format!("property change failed: {e}")),
        ViewsMsg::Select { host, value } => store.select(host, value),
    }
}

/// Decodes a `ViewResult` JSON value (serde, `views::result`) and flattens its groups: group
/// header, rows, group footers, then the result footers.
pub fn decode(v: &Value) -> Result<Decoded, DecodeError> {
    let r = views::ViewResult::deserialize(v)?;
    let mut lines = Vec::new();
    let summary = |s: views::result::Summary| Line::Summary {
        label: s.label,
        cells: s.cells,
    };
    for g in r.groups {
        if let Some(k) = g.key {
            lines.push(Line::Group(k));
        }
        lines.extend(g.rows.into_iter().map(Line::Row));
        lines.extend(g.summaries.into_iter().map(summary));
    }
    lines.extend(r.summaries.into_iter().map(summary));
    let open_lines = has_open_columns(&r.columns).then(|| open_only_lines(&lines, &r.columns));
    Ok(Decoded {
        kind: r.kind,
        columns: r.columns,
        lines,
        notices: r.notices,
        max_height: r.max_height,
        open_lines,
        select_by: r.select_by,
    })
}

/// Display text of a cell.
pub fn format_cell(c: &Cell) -> String {
    match c {
        Cell::Null | Cell::Unknown => String::new(),
        Cell::Bool { v } => if *v { "✓" } else { "✗" }.to_owned(),
        Cell::Number { v: n } => {
            let s = format!("{n:.2}");
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        Cell::Text { v } => v.clone(),
        Cell::Date { v: d } => match d.split_once('T') {
            Some((day, time)) => format!("{day} {}", time.get(..5).unwrap_or(time)),
            None => d.clone(),
        },
        // An absent label shows the target.
        Cell::Link { label, path: t } | Cell::Url { label, url: t } => {
            if label.is_empty() { t } else { label }.clone()
        }
        Cell::List { v: l } => l.iter().map(format_cell).collect::<Vec<_>>().join(", "),
    }
}

/// The view id of a fence body `<file>#<name>`: the file's stem (directories and a `.toml` /
/// `.base` extension dropped) + `#` + the view name.
pub fn view_id(file: &str, name: &str) -> String {
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = base
        .strip_suffix(".toml")
        .or_else(|| base.strip_suffix(".base"))
        .unwrap_or(base);
    format!("{stem}#{name}")
}

/// `~~x~~` → `Some(x)`.
pub fn struck(s: &str) -> Option<&str> {
    s.strip_prefix("~~")
        .and_then(|r| r.strip_suffix("~~"))
        .filter(|r| !r.is_empty())
}

/// The fence widget for `id` embedded in `host`.
/// Plan round-7-feedback-decisions WP-5: a view design size (px; Reading paragraph = 16) at the
/// Reading text scale, so view text tracks `editor.toml` `font_size` like Reading text.
pub fn px(design: f32, scale: f32) -> f32 {
    crate::reading::view::scaled(design, scale)
}

/// Plan round-7-feedback-decisions WP-5 (audit E4): the virtual-list row height at `scale` —
/// [`ROW_HEIGHT`] scaled with the text so large fonts neither clip nor overlap rows.
pub fn row_height(scale: f32) -> f32 {
    ROW_HEIGHT * scale
}

pub fn fence<'a>(
    store: &'a ViewStore,
    id: &str,
    host: &str,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let entry = store.get(id, Some(host));
    let body: Element<'a, Message> = match entry.map(|e| &e.state) {
        None | Some(State::Loading) => text("Loading…")
            .size(px(13.0, scale))
            .color(t.text_faint.to_color())
            .into(),
        Some(State::Error(e)) => text(format!("view error: {e}"))
            .size(px(13.0, scale))
            .color(t.error_text.to_color())
            .into(),
        Some(State::Ready(d)) => {
            let window = entry.map(|e| e.window).unwrap_or_default();
            result_view(d, id, host, window, store, t, scale)
        }
    };
    let title = text(id.to_owned())
        .size(px(13.0, scale))
        .font(Font {
            weight: Weight::Bold,
            ..Font::default()
        })
        .color(t.heading.to_color());
    let toggle = match entry.map(|e| &e.state) {
        Some(State::Ready(d)) if d.open_lines.is_some() => {
            Some(open_toggle(id, store.filters.contains(id), t, scale))
        }
        _ => None,
    };
    let chip = selection_chip(store, id, host).map(|value| {
        let msg = Message::Views(ViewsMsg::Select {
            host: host.to_owned(),
            value: value.clone(),
        });
        let clear = iced::widget::button(text("\u{d7}").size(px(13.0, scale)))
            .padding([0, 6])
            .style(iced::widget::button::text)
            .on_press(msg);
        let name = text(value)
            .size(px(12.0, scale))
            .color(t.accent_ui.to_color());
        Element::from(
            row![name, clear]
                .spacing(2)
                .align_y(iced::Alignment::Center),
        )
    });
    let head = row![title]
        .push(chip)
        .push(space::horizontal())
        .push(toggle)
        .spacing(8)
        .align_y(iced::Alignment::Center);
    let content = container(column![head, body].spacing(6))
        .padding(8)
        .width(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(t.inset.to_color().into()),
            border: Border {
                color: t.line.to_color(),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        });
    let epoch = entry.map_or(0, |e| e.epoch);
    let (nid, nhost) = (id.to_owned(), host.to_owned());
    sensor(content)
        .key((id.to_owned(), host.to_owned(), epoch))
        .on_show(move |_| {
            Message::Views(ViewsMsg::Need {
                id: nid.clone(),
                host: Some(nhost.clone()),
            })
        })
        .into()
}

/// Plan round-13 WP-4: the selected name shown in the header of `select_by` view `(id, host)`
/// (with a clear control), when the host has a selection.
pub(crate) fn selection_chip(store: &ViewStore, id: &str, host: &str) -> Option<String> {
    store.select_by(id, Some(host))?;
    store.selections.get(host).cloned()
}

/// Plan round-9-feedback-todos WP-5: the "Only open" toggle of the fence header; its tooltip
/// says counts and summaries stay the unfiltered totals.
fn open_toggle<'a>(id: &str, on: bool, t: Tokens, scale: f32) -> Element<'a, Message> {
    let nid = id.to_owned();
    let cb = iced::widget::checkbox(on)
        .label("Only open")
        .on_toggle(move |_| Message::Views(ViewsMsg::ToggleOpenOnly { id: nid.clone() }))
        .size(px(13.0, scale))
        .text_size(px(12.0, scale));
    iced::widget::tooltip(
        cb,
        container(
            text(
                "Hides rows that are checked or done. Counts and summaries stay unfiltered totals.",
            )
            .size(px(12.0, scale)),
        )
        .padding(6)
        .style(crate::theme::seg_group_style(t)),
        iced::widget::tooltip::Position::Bottom,
    )
    .delay(std::time::Duration::from_millis(400))
    .into()
}

/// The role of column `i`.
fn role(cols: &[Column], i: usize) -> Option<&ColumnRole> {
    cols.get(i).and_then(|c| c.role.as_ref())
}

/// The index of the first column with `want` role.
fn role_index(cols: &[Column], want: &ColumnRole) -> Option<usize> {
    cols.iter().position(|c| c.role.as_ref() == Some(want))
}

/// Plan round-6-feedback-todos WP-6: the status of cell `i` of `r` when it is a status cell —
/// text or null under a [`ColumnRole::Status`] column of a keyed row (`Some(None)` = open).
pub(crate) fn status_cell(r: &Row, cols: &[Column], i: usize) -> Option<Option<char>> {
    if r.key.is_none() || role(cols, i) != Some(&ColumnRole::Status) {
        return None;
    }
    cell_status(r.cells.get(i))
}

/// Plan round-6-feedback-todos WP-6: the right-click menu of a keyed row's status cell.
pub(crate) fn status_menu_target(r: &Row) -> Option<crate::context_menu::Target> {
    r.key
        .as_ref()
        .map(|key| crate::context_menu::Target::TrackerStatus { key: key.clone() })
}

/// Plan round-6-feedback-todos WP-6: a `Tasks` row's status char (its [`ColumnRole::Status`]
/// cell; `' '` when absent) and its note-edit targets — the click (Reading's `ToggleTask`) and
/// the status menu (`Target::Task`) — when the row carries `path` + 1-based `line`.
pub(crate) fn task_row_status(
    r: &Row,
    cols: &[Column],
) -> (char, Option<(Message, crate::context_menu::Target)>) {
    let c = match role_index(cols, &ColumnRole::Status).and_then(|i| r.cells.get(i)) {
        Some(Cell::Text { v }) => v.chars().next().unwrap_or(' '),
        _ => ' ',
    };
    let targets = match (&r.path, r.line.and_then(|l| usize::try_from(l - 1).ok())) {
        (Some(path), Some(line)) => Some((
            Message::Reading(crate::message::ReadingMsg::ToggleTask {
                path: path.clone(),
                line,
            }),
            crate::context_menu::Target::Task {
                path: path.clone(),
                line,
            },
        )),
        _ => None,
    };
    (c, targets)
}

/// plan editor-round-4 WP-3: what each cell of a non-task row opens on click. Every link cell
/// opens its own path; a row with a `path` but no link cell opens it (and `line`) from its first
/// cell. There is no whole-row click target. Round-5 WP-11: a `url` cell opens in the browser.
/// Round-6 WP-6: a status cell ([`ColumnRole::Status`]) toggles `x` ↔ open.
pub(crate) fn row_targets(r: &Row, cols: &[Column]) -> Vec<Option<Message>> {
    let mut out: Vec<Option<Message>> = r
        .cells
        .iter()
        .enumerate()
        .map(|(i, c)| match c {
            Cell::Link { path, .. } => Some(Message::Views(ViewsMsg::Open {
                path: path.clone(),
                line: None,
            })),
            Cell::Url { url, .. } => Some(Message::OpenUrl(url.clone())),
            _ => status_cell(r, cols, i).map(|st| {
                Message::Views(ViewsMsg::SetStatus {
                    key: r.key.clone().unwrap_or_default(),
                    status: crate::task_status::toggled(st),
                })
            }),
        })
        .collect();
    if let Some(p) = &r.path
        && out.iter().all(Option::is_none)
    {
        if out.is_empty() {
            out.push(None);
        }
        out[0] = Some(Message::Views(ViewsMsg::Open {
            path: p.clone(),
            line: r.line,
        }));
    }
    out
}

/// Plan round-13 WP-4 (Decision 1): the Name cell of a selectable row of a section in note
/// `host` that embeds a `select_by` view — `(column index, click)`. Only rows the producer marks
/// with [`Row::select`] (a `sources` `[[serial]]` row) qualify; the Name cell is the column with
/// [`ColumnRole::Name`], else the first. The click toggles the host's selection to the row's name.
pub(crate) fn select_cell(r: &Row, cols: &[Column], host: &str) -> Option<(usize, Message)> {
    let value = r.select.clone()?;
    let i = cols
        .iter()
        .position(|c| c.role == Some(ColumnRole::Name))
        .unwrap_or(0);
    Some((
        i,
        Message::Views(ViewsMsg::Select {
            host: host.to_owned(),
            value,
        }),
    ))
}

/// One cell. `open` = its click message: drawn like a Reading-mode note link (accent-ui, no
/// permanent underline; iced's rich-text link underlines it and shows a pointer while hovered).
/// Plan round-6 WP-5: the right-click menu of a link / URL cell. A `Cell::Link` carries the
/// vault path, which resolves to itself (so `src` = `target` = the path); `view` 0 is inert
/// (`selection: false` — there is no Reading selection here).
pub(crate) fn cell_menu_target(c: &Cell) -> Option<crate::context_menu::Target> {
    let (target, external) = match c {
        Cell::Link { path, .. } => (path.clone(), false),
        Cell::Url { url, .. } => (url.clone(), true),
        _ => return None,
    };
    Some(crate::context_menu::Target::Link {
        src: if external {
            String::new()
        } else {
            target.clone()
        },
        target,
        external,
        view: 0,
        selection: false,
        missing: false,
    })
}

/// Plan round-8-intake-views-anchor WP-3: cell `i` of `r` as a frontmatter checkbox — `(checked,
/// click)` when column `i` is a [`ColumnRole::Checkbox`] and the row has a `path`. Only
/// `Bool(true)` is checked (false / null / missing draw unchecked); the click sets the opposite
/// value on the role's frontmatter key.
pub(crate) fn checkbox_cell(r: &Row, cols: &[Column], i: usize) -> Option<(bool, Message)> {
    let Some(ColumnRole::Checkbox { key }) = role(cols, i) else {
        return None;
    };
    let path = r.path.clone()?;
    let key = key.clone();
    let checked = cell_checked(r.cells.get(i));
    Some((
        checked,
        Message::Views(ViewsMsg::SetBool {
            path,
            key,
            value: !checked,
        }),
    ))
}

/// Plan round-8-intake-views-anchor WP-1: whether `r` is a cancelled row — a status cell
/// (see [`status_cell`]) holding `-`. Its text cells draw struck-through and faint.
pub(crate) fn row_cancelled(r: &Row, cols: &[Column]) -> bool {
    (0..r.cells.len().max(cols.len())).any(|i| status_cell(r, cols, i) == Some(Some('-')))
}

fn result_view<'a>(
    d: &'a Decoded,
    id: &str,
    host: &str,
    window: ListWindow,
    store: &ViewStore,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let only_open = store.filters.contains(id);
    // Plan round-13 WP-4: serial rows select only on a host that embeds a `select_by` view.
    let select_host: Option<&str> = store.host_selects(host).then_some(host);
    let sort = store.sort_of(id, Some(host));
    // Plan tn-review-2026-10-23-remediation WP-R7: "Only open" walks the index list into
    // `d.lines` — no cloned lines.
    let open: Option<&'a [usize]> = match &d.open_lines {
        Some(open) if only_open => Some(open),
        _ => None,
    };
    let count = open.map_or(d.lines.len(), <[usize]>::len);
    let lengths = column_lengths(d, scale);
    let mut items: Vec<Element<'a, Message>> = d
        .notices
        .iter()
        .map(|n| {
            text(n.clone())
                .size(px(12.0, scale))
                .color(t.text_faint.to_color())
                .into()
        })
        .collect();
    if d.kind != Kind::Tasks && !d.columns.is_empty() {
        items.push(
            row(d.columns.iter().enumerate().map(|(i, c)| {
                let mark = match sort {
                    Some(s) if s.column == c.key => {
                        if s.desc {
                            " \u{25bc}"
                        } else {
                            " \u{25b2}"
                        }
                    }
                    _ => "",
                };
                let label = text(format!("{}{mark}", c.label))
                    .size(px(13.0, scale))
                    .wrapping(Wrapping::None)
                    .color(t.text_dim.to_color());
                let header: Element<'a, Message> = if d.kind == Kind::Table {
                    iced::widget::button(label)
                        .padding(0)
                        .style(|_, _| iced::widget::button::Style::default())
                        .on_press(Message::Views(ViewsMsg::SortBy {
                            id: id.to_owned(),
                            host: Some(host.to_owned()),
                            column: c.key.clone(),
                        }))
                        .into()
                } else {
                    label.into()
                };
                container(header)
                    .width(lengths.get(i).copied().unwrap_or(Length::FillPortion(1)))
                    .clip(true)
                    .into()
            }))
            .spacing(8)
            .into(),
        );
    }
    if count == 0 {
        items.push(
            text("No results")
                .size(px(13.0, scale))
                .color(t.text_faint.to_color())
                .into(),
        );
    } else if let Some(cap) = list_height(d).filter(|_| count > VIRTUAL_THRESHOLD) {
        let (sid, shost) = (id.to_owned(), host.to_owned());
        let on_scroll = move |w| {
            Message::Views(ViewsMsg::Scrolled {
                id: sid.clone(),
                host: Some(shost.clone()),
                window: w,
            })
        };
        let list = match open {
            Some(idx) => VirtualList::new(idx, row_height(scale), window, on_scroll)
                .id(list_id(id, host))
                .view(move |_, &i| {
                    line_view(d, &d.lines[i], &lengths, true, select_host, t, scale)
                }),
            None => VirtualList::new(&d.lines, row_height(scale), window, on_scroll)
                .id(list_id(id, host))
                .view(move |_, l| line_view(d, l, &lengths, true, select_host, t, scale)),
        };
        items.push(
            container(list)
                .height(Length::Fixed(cap))
                .width(Length::Fill)
                .into(),
        );
    } else {
        let lines: Box<dyn Iterator<Item = &'a Line> + 'a> = match open {
            Some(idx) => Box::new(idx.iter().map(|&i| &d.lines[i])),
            None => Box::new(d.lines.iter()),
        };
        items.push(
            column(lines.map(|l| line_view(d, l, &lengths, false, select_host, t, scale)))
                .spacing(4)
                .into(),
        );
    }
    column(items).spacing(4).into()
}

#[cfg(test)]
mod tests;
