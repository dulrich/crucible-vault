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
//! [`ColumnRole`], never column-key strings. Text cells written `~~x~~` (missing linked
//! files) render struck through and dimmed.

mod decode;
mod layout;
mod store;

use iced::font::Weight;
use iced::widget::{column, container, row, sensor, space, text};
use iced::{Border, Element, Font, Length, Theme};

pub use layout::column_lengths;
use layout::line_view;

use crate::message::{Message, ViewsMsg};
use crate::theme::Tokens;
use crate::widget::chrome_text::chrome_text;
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

pub use decode::*;
pub use store::*;

/// Plan round-5 WP-1: the widget id of the virtual result list of view `id` in note `host`.
pub fn list_id(id: &str, host: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("views-list:{host}:{id}"))
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

/// The message a sortable column header's click publishes.
pub(crate) fn sort_msg(id: &str, host: &str, column: &str) -> Message {
    Message::Views(ViewsMsg::SortBy {
        id: id.to_owned(),
        host: Some(host.to_owned()),
        column: column.to_owned(),
    })
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
    // plan round-24 WP-2: header text is selectable (drag + Ctrl+C), see `widget::chrome_text`.
    let mut sel = t.accent_ui.to_color();
    sel.a = 0.35;
    let title = chrome_text::<Message>(id.to_owned(), px(13.0, scale), t.heading.to_color(), sel)
        .font(Font {
            weight: Weight::Bold,
            ..Font::default()
        });
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
        let name = chrome_text::<Message>(value, px(12.0, scale), t.accent_ui.to_color(), sel);
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
                let mut sel = t.accent_ui.to_color();
                sel.a = 0.35;
                let mut label = chrome_text::<Message>(
                    format!("{}{mark}", c.label),
                    px(13.0, scale),
                    t.text_dim.to_color(),
                    sel,
                );
                if d.kind == Kind::Table {
                    // A plain click sorts; a drag past the threshold selects instead.
                    label = label.on_click(sort_msg(id, host, &c.key));
                }
                let header: Element<'a, Message> = label.into();
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
