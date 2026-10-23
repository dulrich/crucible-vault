//! Column widths of a `crucible-view` result table (split out of `views/mod.rs`, plan
//! tn-review-2026-10-23-remediation WP-R7).

use iced::font::Weight;
use iced::widget::text::Wrapping;
use iced::widget::{button, container, rich_text, row, span, text};
use iced::{Element, Font, Length};

use super::{
    Cell, ColumnRole, Decoded, Kind, Line, cell_menu_target, checkbox_cell, format_cell, px,
    role_index, row_cancelled, row_height, row_targets, status_cell, status_menu_target, struck,
    task_row_status,
};
use crate::message::{Message, ViewsMsg};
use crate::theme::Tokens;

/// Approximate glyph advance as a fraction of the font size (proportional UI font average).
const CHAR_ADVANCE: f32 = 0.6;
/// Cell font design size (see [`cell_view`]).
const CELL_FONT: f32 = 13.0;
/// Horizontal slack added to a role-sized column's measured text (design px).
const FIXED_PAD: f32 = 8.0;
/// Clamp of a role-sized (number/checkbox/status) column width (design px).
const FIXED_MIN: f32 = 28.0;
const FIXED_MAX: f32 = 120.0;
/// `FillPortion` of the file/link column versus other text columns.
const NAME_PORTION: u16 = 4;
const TEXT_PORTION: u16 = 2;

/// Plan intake-round-2 WP-4: the width of every column of `d` at `scale`, shared by the header,
/// data and summary rows so they align. A `columnSize` column is `Fixed(width * scale)`; an
/// unsized column of numbers, booleans, or a checkbox/status role is `Fixed` from its header and
/// widest formatted cell (about [`CHAR_ADVANCE`] em per char, clamped to
/// [`FIXED_MIN`]..=[`FIXED_MAX`]); every other column is a `FillPortion`, [`NAME_PORTION`] for
/// `file.name` or a link column and [`TEXT_PORTION`] otherwise.
pub fn column_lengths(d: &Decoded, scale: f32) -> Vec<Length> {
    if d.columns.is_empty() {
        return vec![Length::FillPortion(1)];
    }
    let cell_at = |i: usize| {
        d.lines.iter().flat_map(move |l| match l {
            Line::Row(r) => r.cells.get(i).into_iter().collect::<Vec<_>>(),
            Line::Summary { cells, .. } => cells.get(i).into_iter().collect::<Vec<_>>(),
            Line::Group(_) => Vec::new(),
        })
    };
    d.columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if let Some(w) = c.width.filter(|w| *w > 0.0) {
                return Length::Fixed(w * scale);
            }
            let role_fixed = matches!(
                c.role,
                Some(ColumnRole::Checkbox { .. } | ColumnRole::Status)
            );
            let mut any = false;
            let mut compact = true;
            let mut link = c.key == "file.name";
            let mut widest = c.label.chars().count();
            for cell in cell_at(i) {
                match cell {
                    Cell::Null | Cell::Unknown => {}
                    Cell::Number { .. } | Cell::Bool { .. } => any = true,
                    Cell::Link { .. } => {
                        compact = false;
                        link = true;
                    }
                    _ => compact = false,
                }
                widest = widest.max(format_cell(cell).chars().count());
            }
            if role_fixed || (any && compact) {
                #[allow(clippy::cast_precision_loss)]
                let w = (widest as f32 * CELL_FONT * CHAR_ADVANCE + FIXED_PAD)
                    .clamp(FIXED_MIN, FIXED_MAX);
                Length::Fixed(w * scale)
            } else if link {
                Length::FillPortion(NAME_PORTION)
            } else {
                Length::FillPortion(TEXT_PORTION)
            }
        })
        .collect()
}

pub(super) fn cell_view<'a>(
    c: &Cell,
    bold: bool,
    open: Option<Message>,
    cancelled: bool,
    nowrap: bool,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let wrap = if nowrap {
        Wrapping::None
    } else {
        Wrapping::default()
    };
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
            .wrapping(wrap)
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
                .wrapping(wrap)
                .on_link_click(move |()| msg.clone());
            match cell_menu_target(c) {
                Some(target) => crate::context_menu::on_right(el, target),
                None => el.into(),
            }
        }
        None => text(s)
            .size(px(13.0, scale))
            .font(font)
            .wrapping(wrap)
            .color(t.text.to_color())
            .into(),
    }
}

/// Plan intake-round-2 WP-4: one display line. `single` (the virtual-list path, whose row height
/// is fixed) renders every cell on one clipped line so a row never exceeds its height; otherwise
/// rows grow to fit wrapped text.
pub(super) fn line_view<'a>(
    d: &Decoded,
    l: &Line,
    lengths: &[Length],
    single: bool,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let el = line_inner(d, l, lengths, single, t, scale);
    if single {
        container(el)
            .height(Length::Fixed(row_height(scale)))
            .width(Length::Fill)
            .clip(true)
            .into()
    } else {
        el
    }
}

pub(super) fn line_inner<'a>(
    d: &Decoded,
    l: &Line,
    lengths: &[Length],
    single: bool,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let cols = d.columns.len().max(1);
    let len = |i: usize| lengths.get(i).copied().unwrap_or(Length::FillPortion(1));
    let grid = |cs: &[Cell], bold: bool, targets: &[Option<Message>]| -> Element<'a, Message> {
        row((0..cols).map(|i| {
            let c = cs.get(i).unwrap_or(&Cell::Null);
            let open = targets.get(i).cloned().flatten();
            let el = cell_view(c, bold, open, false, single, t, scale);
            container(el).width(len(i)).clip(single).into()
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
                                single,
                                t,
                                scale,
                            )
                        };
                    container(el).width(len(i)).clip(single).into()
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
pub(super) fn status_box<'a>(
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
pub(super) fn row_link_style(t: Tokens, status: button::Status) -> button::Style {
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
