//! Decoding of a `ViewResult` into display [`Line`]s and the pure row/cell helpers that map
//! cells to click targets (split out of `views/mod.rs`, plan tn-review-2026-10-26 WP-R4).

use serde::Deserialize;
use serde_json::Value;

use super::MAX_HEIGHT;
use super::{Cell, Column, ColumnRole, Kind, Row, cell_checked, cell_status};
use super::{has_open_columns, row_is_open};
use crate::message::{Message, ViewsMsg};

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

/// A malformed `ViewResult`.
#[derive(Debug, thiserror::Error)]
#[error("malformed view result: {0}")]
pub struct DecodeError(#[from] serde_json::Error);

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

/// The role of column `i`.
pub(super) fn role(cols: &[Column], i: usize) -> Option<&ColumnRole> {
    cols.get(i).and_then(|c| c.role.as_ref())
}

/// The index of the first column with `want` role.
pub(super) fn role_index(cols: &[Column], want: &ColumnRole) -> Option<usize> {
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
