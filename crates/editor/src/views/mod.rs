//! `crucible-view` fence rendering of `ViewResult`s (WP-7).
//!
//! The fence body is `<view-file>#<View name>`; the view runs in the daemon (`view_run`) with
//! `this` = the embedding note. Results are cached per `(view_id, this_path)` in [`ViewStore`].
//! The fence is wrapped in an iced `sensor`: when it comes into view it asks for its result
//! ([`ViewsMsg::Need`]); the app runs `view_run` off the UI thread and feeds back
//! [`ViewsMsg::Loaded`]. `views_invalidated` marks matching entries stale and bumps their epoch,
//! which is part of the sensor key, so a fence that is on screen fires `Need` again.
//!
//! The decoder is tolerant: an unknown cell tag decodes as text `?`; a malformed result becomes
//! an [`State::Error`] entry shown in the fence. Text cells written `~~x~~` (missing initiative
//! files, `defs/initiatives.toml`) render struck through and dimmed.

use std::collections::HashMap;

use iced::font::Weight;
use iced::widget::{button, column, container, rich_text, row, sensor, span, text};
use iced::{Border, Element, Font, Length, Theme};
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

/// The view kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Table,
    Pivot,
    Tasks,
}

/// One displayed value (`views::result::Cell`, decoded tolerantly).
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Date(String),
    Link { path: String, label: String },
    List(Vec<Cell>),
}

/// A result row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: Option<String>,
    pub line: Option<i64>,
    pub cells: Vec<Cell>,
}

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

/// A decoded `ViewResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub kind: Kind,
    pub columns: Vec<String>,
    pub lines: Vec<Line>,
    pub notices: Vec<String>,
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

/// The per-app view cache.
#[derive(Debug, Default)]
pub struct ViewStore {
    pub entries: HashMap<Key, Entry>,
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
}

/// A malformed `ViewResult`.
#[derive(Debug, thiserror::Error)]
#[error("malformed view result: {0}")]
pub struct DecodeError(&'static str);

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
    }
}

/// Decodes one cell; unknown tags become text `?`.
pub fn decode_cell(v: &Value) -> Cell {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    match v.get("t").and_then(Value::as_str) {
        Some("null") => Cell::Null,
        Some("bool") => v
            .get("v")
            .and_then(Value::as_bool)
            .map_or(Cell::Null, Cell::Bool),
        Some("number") => v
            .get("v")
            .and_then(Value::as_f64)
            .map_or(Cell::Null, Cell::Number),
        Some("text") => s("v").map_or(Cell::Null, Cell::Text),
        Some("date") => s("v").map_or(Cell::Null, Cell::Date),
        Some("link") => match s("path") {
            Some(path) => Cell::Link {
                label: s("label").unwrap_or_else(|| path.clone()),
                path,
            },
            None => Cell::Text("?".into()),
        },
        Some("list") => Cell::List(
            v.get("v")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(decode_cell).collect())
                .unwrap_or_default(),
        ),
        _ => Cell::Text("?".into()),
    }
}

fn cells(v: Option<&Value>) -> Vec<Cell> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(decode_cell).collect())
        .unwrap_or_default()
}

fn summaries(v: Option<&Value>, out: &mut Vec<Line>) {
    for s in v.and_then(Value::as_array).into_iter().flatten() {
        out.push(Line::Summary {
            label: s
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            cells: cells(s.get("cells")),
        });
    }
}

/// Decodes a `ViewResult` JSON value.
pub fn decode(v: &Value) -> Result<Decoded, DecodeError> {
    let kind = match v.get("kind").and_then(Value::as_str) {
        Some("table") => Kind::Table,
        Some("pivot") => Kind::Pivot,
        Some("tasks") => Kind::Tasks,
        _ => return Err(DecodeError("kind")),
    };
    let columns = v
        .get("columns")
        .and_then(Value::as_array)
        .ok_or(DecodeError("columns"))?
        .iter()
        .map(|c| {
            c.get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let groups = v
        .get("groups")
        .and_then(Value::as_array)
        .ok_or(DecodeError("groups"))?;
    let mut lines = Vec::new();
    for g in groups {
        if let Some(k) = g.get("key") {
            lines.push(Line::Group(decode_cell(k)));
        }
        let rows = g
            .get("rows")
            .and_then(Value::as_array)
            .ok_or(DecodeError("rows"))?;
        for r in rows {
            lines.push(Line::Row(Row {
                path: r.get("path").and_then(Value::as_str).map(str::to_owned),
                line: r.get("line").and_then(Value::as_i64),
                cells: cells(r.get("cells")),
            }));
        }
        summaries(g.get("summaries"), &mut lines);
    }
    summaries(v.get("summaries"), &mut lines);
    let notices = v
        .get("notices")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(Decoded {
        kind,
        columns,
        lines,
        notices,
    })
}

/// Display text of a cell.
pub fn format_cell(c: &Cell) -> String {
    match c {
        Cell::Null => String::new(),
        Cell::Bool(b) => if *b { "✓" } else { "✗" }.to_owned(),
        Cell::Number(n) => {
            let s = format!("{n:.2}");
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        Cell::Text(s) => s.clone(),
        Cell::Date(d) => match d.split_once('T') {
            Some((day, time)) => format!("{day} {}", time.get(..5).unwrap_or(time)),
            None => d.clone(),
        },
        Cell::Link { label, .. } => label.clone(),
        Cell::List(l) => l.iter().map(format_cell).collect::<Vec<_>>().join(", "),
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
pub fn fence<'a>(store: &'a ViewStore, id: &str, host: &str, t: Tokens) -> Element<'a, Message> {
    let entry = store.get(id, Some(host));
    let body: Element<'a, Message> = match entry.map(|e| &e.state) {
        None | Some(State::Loading) => text("Loading…")
            .size(13)
            .color(t.text_faint.to_color())
            .into(),
        Some(State::Error(e)) => text(format!("view error: {e}"))
            .size(13)
            .color(t.error_text.to_color())
            .into(),
        Some(State::Ready(d)) => {
            let window = entry.map(|e| e.window).unwrap_or_default();
            result_view(d, id, host, window, t)
        }
    };
    let title = text(id.to_owned())
        .size(13)
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

/// How a [`Cell::Link`] renders: `Plain` (summary labels), `Styled` (looks like a link; its row is
/// the click target) or `Clickable` (looks like a link and opens its own path).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkCell {
    Plain,
    Styled,
    Clickable,
}

fn cell_view<'a>(c: &Cell, bold: bool, link: LinkCell, t: Tokens) -> Element<'a, Message> {
    let font = Font {
        weight: if bold { Weight::Bold } else { Weight::Normal },
        ..Font::default()
    };
    let s = format_cell(c);
    if let Some(inner) = struck(&s) {
        let sp: iced::widget::text::Span<'a, ()> = span(inner.to_owned()).strikethrough(true);
        return rich_text(vec![sp])
            .size(13)
            .font(font)
            .color(t.text_faint.to_color())
            .into();
    }
    match c {
        // Styled like Reading-mode links (`reading/view.rs` `Inline::WikiLink`): accent-ui, no
        // permanent underline. `Clickable` makes the span an iced rich-text link, which
        // underlines it and shows a pointer while hovered.
        // `Styled`: no explicit colour, so the text inherits the row button's `text_color`
        // (`accent_ui`, `accent_ui_hover` while the row is hovered; plan editor-feedback-round-2
        // WP-5).
        Cell::Link { .. } if link == LinkCell::Styled => text(s).size(13).font(font).into(),
        Cell::Link { path, .. } if link != LinkCell::Plain => {
            let clickable = link == LinkCell::Clickable;
            let mut sp: iced::widget::text::Span<'a, ()> = span(s).color(t.accent_ui.to_color());
            if clickable {
                sp = sp.link(());
            }
            let rt = rich_text(vec![sp]).size(13).font(font);
            if clickable {
                let path = path.clone();
                rt.on_link_click(move |()| {
                    Message::Views(ViewsMsg::Open {
                        path: path.clone(),
                        line: None,
                    })
                })
                .into()
            } else {
                rt.into()
            }
        }
        _ => text(s).size(13).font(font).color(t.text.to_color()).into(),
    }
}

fn line_view<'a>(d: &Decoded, l: &Line, t: Tokens) -> Element<'a, Message> {
    let cols = d.columns.len().max(1);
    let grid = |cs: &[Cell], bold: bool, links: LinkCell| -> Element<'a, Message> {
        row((0..cols).map(|i| {
            let c = cs.get(i).unwrap_or(&Cell::Null);
            let el = cell_view(c, bold, links, t);
            container(el).width(Length::FillPortion(1)).into()
        }))
        .spacing(8)
        .into()
    };
    match l {
        Line::Group(k) => text(format_cell(k))
            .size(14)
            .font(Font {
                weight: Weight::Bold,
                ..Font::default()
            })
            .color(t.heading.to_color())
            .into(),
        Line::Summary { label, cells } => {
            if d.kind == Kind::Pivot {
                grid(cells, true, LinkCell::Plain)
            } else {
                let mut cs = cells.clone();
                // Ungrouped tables carry the summary name only in `label`: show it in an empty
                // first cell.
                if matches!(cs.first(), Some(Cell::Null) | None) {
                    if cs.is_empty() {
                        cs.push(Cell::Null);
                    }
                    cs[0] = Cell::Text(label.clone());
                }
                grid(&cs, true, LinkCell::Plain)
            }
        }
        Line::Row(r) => {
            let el: Element<'a, Message> = if d.kind == Kind::Tasks {
                let task = r.cells.first().map(format_cell).unwrap_or_default();
                let file = r.cells.get(1).map(format_cell).unwrap_or_default();
                row![
                    text(format!("☐ {task}")).size(13).color(t.text.to_color()),
                    text(format!("— {file}"))
                        .size(13)
                        .color(t.text_dim.to_color()),
                ]
                .spacing(6)
                .into()
            } else {
                // A row with its own target is clickable as a whole; otherwise link cells are.
                let links = if r.path.is_some() {
                    LinkCell::Styled
                } else {
                    LinkCell::Clickable
                };
                grid(&r.cells, false, links)
            };
            match &r.path {
                // A button (not a `mouse_area`) so the hover state comes from the widget's own
                // status: no hover message, no stored state, no extra redraw traffic.
                Some(p) => button(el)
                    .padding(0)
                    .width(Length::Fill)
                    .style(move |_, status| row_link_style(t, status))
                    .on_press(Message::Views(ViewsMsg::Open {
                        path: p.clone(),
                        line: r.line,
                    }))
                    .into(),
                None => el,
            }
        }
    }
}

/// A row-target button: link cells take `text_color` (accent-ui, brighter on hover) and the row
/// gets a `sel_tint` wash while hovered or pressed.
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
) -> Element<'a, Message> {
    let mut items: Vec<Element<'a, Message>> = d
        .notices
        .iter()
        .map(|n| {
            text(n.clone())
                .size(12)
                .color(t.text_faint.to_color())
                .into()
        })
        .collect();
    if d.kind != Kind::Tasks && !d.columns.is_empty() {
        items.push(
            row(d.columns.iter().map(|c| {
                container(text(c.clone()).size(13).color(t.text_dim.to_color()))
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
                .size(13)
                .color(t.text_faint.to_color())
                .into(),
        );
    } else if d.lines.len() > VIRTUAL_THRESHOLD {
        let (sid, shost) = (id.to_owned(), host.to_owned());
        let list = VirtualList::new(&d.lines, ROW_HEIGHT, window, move |w| {
            Message::Views(ViewsMsg::Scrolled {
                id: sid.clone(),
                host: Some(shost.clone()),
                window: w,
            })
        })
        .view(move |_, l| line_view(d, l, t));
        items.push(
            container(list)
                .height(Length::Fixed(MAX_HEIGHT))
                .width(Length::Fill)
                .into(),
        );
    } else {
        items.push(
            column(d.lines.iter().map(|l| line_view(d, l, t)))
                .spacing(4)
                .into(),
        );
    }
    column(items).spacing(4).into()
}

#[cfg(test)]
mod tests;
