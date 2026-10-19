//! `crucible-view` fence rendering of `ViewResult`s (WP-7).
//!
//! The fence body is `<view-file>#<View name>`; the view runs in the daemon (`view_run`) with
//! `this` = the embedding note. Results are cached per `(view_id, this_path)` in [`ViewStore`].
//! The fence is wrapped in an iced `sensor`: when it comes into view it asks for its result
//! ([`ViewsMsg::Need`]); the app runs `view_run` off the UI thread and feeds back
//! [`ViewsMsg::Loaded`]. `views_invalidated` marks matching entries stale and bumps their epoch,
//! which is part of the sensor key, so a fence that is on screen fires `Need` again.
//!
//! The result decodes with serde straight into `views::result` (plan tn-review-baseline-remediation
//! WP-R22): an unknown cell tag decodes as `Cell::Unknown` and renders empty; a malformed result
//! becomes an [`State::Error`] entry shown in the fence. Cell behaviour follows the producer-set
//! [`ColumnRole`], never column-key strings. Text cells written `~~x~~` (missing initiative
//! files, `defs/initiatives.toml`) render struck through and dimmed.

use std::collections::HashMap;

use iced::font::Weight;
use iced::widget::{button, column, container, rich_text, row, sensor, span, text};
use iced::{Border, Element, Font, Length, Theme};
use serde::Deserialize;
use serde_json::Value;

use crate::message::{Message, ViewsMsg};
use crate::theme::Tokens;
use crate::widget::virtual_list::{ListWindow, VirtualList};

/// Row height of the result table.
pub const ROW_HEIGHT: f32 = 26.0;
/// More lines than this render through the virtual list in a bounded box.
pub const VIRTUAL_THRESHOLD: usize = 30;
/// Height of the bounded box.
pub const MAX_HEIGHT: f32 = 480.0;

/// `(view_id, this_path)`.
pub type Key = (String, Option<String>);

pub use views::result::{Cell, Column, ColumnRole, Kind, Row};

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

/// Plan round-5 WP-1: the widget id of the virtual result list of view `id` in note `host`.
pub fn list_id(id: &str, host: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("views-list:{host}:{id}"))
}

/// The per-app view cache.
#[derive(Debug, Default)]
pub struct ViewStore {
    pub entries: HashMap<Key, Entry>,
}

impl ViewStore {
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
    }

    /// A tab closed: drop the results it hosted.
    pub fn on_tab_closed(&mut self, path: &str) {
        self.entries.retain(|(_, h), _| h.as_deref() != Some(path));
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
            if e.in_flight || !e.stale {
                return Effect::None;
            }
            e.in_flight = true;
            e.stale = false;
            Effect::Run { id, host }
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
        ViewsMsg::BoolSet(Err(e)) => Effect::Notice(format!("property change failed: {e}")),
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
    Ok(Decoded {
        kind: r.kind,
        columns: r.columns,
        lines,
        notices: r.notices,
        max_height: r.max_height,
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
            result_view(d, id, host, window, t, scale)
        }
    };
    let title = text(id.to_owned())
        .size(px(13.0, scale))
        .font(Font {
            weight: Weight::Bold,
            ..Font::default()
        })
        .color(t.heading.to_color());
    let content = container(column![title, body].spacing(6))
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
    match r.cells.get(i) {
        Some(Cell::Null) | None => Some(None),
        Some(Cell::Text { v }) => Some(v.chars().next().filter(|c| *c != ' ')),
        _ => None,
    }
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
    let checked = matches!(r.cells.get(i), Some(Cell::Bool { v: true }));
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

fn cell_view<'a>(
    c: &Cell,
    bold: bool,
    open: Option<Message>,
    cancelled: bool,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let font = Font {
        weight: if bold { Weight::Bold } else { Weight::Normal },
        ..Font::default()
    };
    let s = format_cell(c);
    let strike = struck(&s)
        .map(str::to_owned)
        .or_else(|| cancelled.then(|| s.clone()));
    if let Some(inner) = strike {
        let sp: iced::widget::text::Span<'a, ()> = span(inner).strikethrough(true);
        let open = open.filter(|_| cancelled);
        let sp = if open.is_some() { sp.link(()) } else { sp };
        let el = rich_text(vec![sp])
            .size(px(13.0, scale))
            .font(font)
            .color(t.text_faint.to_color());
        return match open {
            Some(msg) => {
                let el = el.on_link_click(move |()| msg.clone());
                match cell_menu_target(c) {
                    Some(target) => crate::context_menu::on_right(el, target),
                    None => el.into(),
                }
            }
            None => el.into(),
        };
    }
    match open {
        Some(msg) => {
            let sp: iced::widget::text::Span<'a, ()> =
                span(s).color(t.accent_ui.to_color()).link(());
            let el = rich_text(vec![sp])
                .size(px(13.0, scale))
                .font(font)
                .on_link_click(move |()| msg.clone());
            match cell_menu_target(c) {
                Some(target) => crate::context_menu::on_right(el, target),
                None => el.into(),
            }
        }
        None => text(s)
            .size(px(13.0, scale))
            .font(font)
            .color(t.text.to_color())
            .into(),
    }
}

fn line_view<'a>(d: &Decoded, l: &Line, t: Tokens, scale: f32) -> Element<'a, Message> {
    let cols = d.columns.len().max(1);
    let grid = |cs: &[Cell], bold: bool, targets: &[Option<Message>]| -> Element<'a, Message> {
        row((0..cols).map(|i| {
            let c = cs.get(i).unwrap_or(&Cell::Null);
            let open = targets.get(i).cloned().flatten();
            let el = cell_view(c, bold, open, false, t, scale);
            container(el).width(Length::FillPortion(1)).into()
        }))
        .spacing(8)
        .into()
    };
    match l {
        Line::Group(k) => text(format_cell(k))
            .size(px(14.0, scale))
            .font(Font {
                weight: Weight::Bold,
                ..Font::default()
            })
            .color(t.heading.to_color())
            .into(),
        Line::Summary { label, cells } => {
            if d.kind == Kind::Pivot {
                grid(cells, true, &[])
            } else {
                let mut cs = cells.clone();
                // Ungrouped tables carry the summary name only in `label`: show it in an empty
                // first cell.
                if matches!(cs.first(), Some(Cell::Null) | None) {
                    if cs.is_empty() {
                        cs.push(Cell::Null);
                    }
                    cs[0] = Cell::Text { v: label.clone() };
                }
                grid(&cs, true, &[])
            }
        }
        Line::Row(r) => {
            if d.kind != Kind::Tasks {
                // A status cell draws as the shared task checkbox.
                let targets = row_targets(r, &d.columns);
                let cancelled = row_cancelled(r, &d.columns);
                return row((0..cols).map(|i| {
                    let open = targets.get(i).cloned().flatten();
                    let el: Element<'a, Message> =
                        if let Some((checked, msg)) = checkbox_cell(r, &d.columns, i) {
                            status_box(if checked { 'x' } else { ' ' }, Some(msg), None, t, scale)
                        } else if let Some(st) = status_cell(r, &d.columns, i) {
                            status_box(st.unwrap_or(' '), open, status_menu_target(r), t, scale)
                        } else {
                            cell_view(
                                r.cells.get(i).unwrap_or(&Cell::Null),
                                false,
                                open,
                                cancelled,
                                t,
                                scale,
                            )
                        };
                    container(el).width(Length::FillPortion(1)).into()
                }))
                .spacing(8)
                .into();
            }
            let cell = |want: ColumnRole| {
                role_index(&d.columns, &want)
                    .and_then(|i| r.cells.get(i))
                    .map(format_cell)
                    .unwrap_or_default()
            };
            let task = cell(ColumnRole::TaskText);
            let file = cell(ColumnRole::TaskFile);
            let (c, targets) = task_row_status(r, &d.columns);
            let (press, menu) = targets.map_or((None, None), |(m, tg)| (Some(m), Some(tg)));
            let label: Element<'a, Message> = row![
                text(task).size(px(13.0, scale)).color(t.text.to_color()),
                text(format!("— {file}"))
                    .size(px(13.0, scale))
                    .color(t.text_dim.to_color()),
            ]
            .spacing(6)
            .into();
            let label = match &r.path {
                // Tasks rows keep their whole-row open button (plan editor-round-4 WP-3).
                Some(p) => button(label)
                    .padding(0)
                    .width(Length::Fill)
                    .style(move |_, status| row_link_style(t, status))
                    .on_press(Message::Views(ViewsMsg::Open {
                        path: p.clone(),
                        line: r.line,
                    }))
                    .into(),
                None => label,
            };
            row![status_box(c, press, menu, t, scale), label]
                .spacing(6)
                .align_y(iced::Alignment::Center)
                .into()
        }
    }
}

/// Plan round-6-feedback-todos WP-6: a status checkbox (the shared glyph map) — a button when
/// `press` is set, with the status menu on right-click when `menu` is set.
fn status_box<'a>(
    c: char,
    press: Option<Message>,
    menu: Option<crate::context_menu::Target>,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let mark = crate::task_status::task_box(c, px(14.0, scale), t);
    let Some(msg) = press else {
        return mark;
    };
    let b = button(mark).padding(0).style(button::text).on_press(msg);
    match menu {
        Some(target) => crate::context_menu::on_right(b, target),
        None => b.into(),
    }
}

/// A Tasks row-target button: accent-ui text, a `sel_tint` wash while hovered or pressed.
fn row_link_style(t: Tokens, status: button::Status) -> button::Style {
    let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hot.then(|| t.sel_tint.to_color().into()),
        text_color: if hot { t.accent_ui_hover } else { t.accent_ui }.to_color(),
        border: iced::Border {
            radius: 4.0.into(),
            ..iced::Border::default()
        },
        ..button::Style::default()
    }
}

fn result_view<'a>(
    d: &'a Decoded,
    id: &str,
    host: &str,
    window: ListWindow,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
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
            row(d.columns.iter().map(|c| {
                container(
                    text(c.label.clone())
                        .size(px(13.0, scale))
                        .color(t.text_dim.to_color()),
                )
                .width(Length::FillPortion(1))
                .into()
            }))
            .spacing(8)
            .into(),
        );
    }
    if d.lines.is_empty() {
        items.push(
            text("No results")
                .size(px(13.0, scale))
                .color(t.text_faint.to_color())
                .into(),
        );
    } else if let Some(cap) = list_height(d).filter(|_| d.lines.len() > VIRTUAL_THRESHOLD) {
        let (sid, shost) = (id.to_owned(), host.to_owned());
        let list = VirtualList::new(&d.lines, row_height(scale), window, move |w| {
            Message::Views(ViewsMsg::Scrolled {
                id: sid.clone(),
                host: Some(shost.clone()),
                window: w,
            })
        })
        .id(list_id(id, host))
        .view(move |_, l| line_view(d, l, t, scale));
        items.push(
            container(list)
                .height(Length::Fixed(cap))
                .width(Length::Fill)
                .into(),
        );
    } else {
        items.push(
            column(d.lines.iter().map(|l| line_view(d, l, t, scale)))
                .spacing(4)
                .into(),
        );
    }
    column(items).spacing(4).into()
}

#[cfg(test)]
mod tests;
