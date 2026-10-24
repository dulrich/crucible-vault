//! String-list chips: a chip strip + adder, or (Channel picker) an add/drop list.

use iced::widget::{Space, button, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element, Length};

use super::{FIELD_W, Head, Rcx};
use crate::icons::Icon;
use crate::message::Message;
use crate::settings::{FormMsg, SettingsMsg, pickers};
use crate::theme;
use crate::widget::typeahead;

pub(super) fn chips<'a>(
    h: &Head,
    cx: Rcx<'_>,
    items: &[String],
    choices: &'static [&'static str],
    draft: &str,
    picker: Option<pickers::Picker>,
) -> Element<'a, Message> {
    let t = cx.t;
    let path = &h.path;
    let channel = picker == Some(pickers::Picker::Channel);
    let chans = if channel {
        pickers::channels(cx.settings)
    } else {
        Vec::new()
    };
    let adder = adder(cx, path, items, choices, draft, picker);
    let remove = |i: usize| {
        cx.on(cx.form(FormMsg::ChipRemove {
            path: path.clone(),
            index: i,
        }))
    };
    if channel {
        // An add/drop list like Daily Brief's record lists: one channel per row with a
        // remove control, then the adder (plan round-7-feedback-decisions WP-5). The rows
        // sit in their own column so the adder keeps one tree position as the list grows.
        let mut rows = column![].spacing(2);
        for (i, it) in items.iter().enumerate() {
            rows = rows.push(
                row![
                    container(text(pickers::chip_label(cx.settings, &chans, it)).size(14))
                        .width(Length::Fixed(FIELD_W - theme::CONTROL))
                        .height(theme::CONTROL)
                        .align_y(Alignment::Center),
                    theme::icon_button(Icon::X, remove(i), t),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            );
        }
        return row![h.label_col(t), column![rows, adder].spacing(4)]
            .spacing(8)
            .into();
    }
    let mut strip = row![].spacing(4).align_y(Alignment::Center);
    for (i, it) in items.iter().enumerate() {
        strip = strip.push(
            container(
                row![
                    text(pickers::channel_label(&chans, it)).size(13),
                    button(
                        // `icon_text` is Fill x Fill: inside a chip it made the whole field row
                        // Fill-height, which the settings column lays out at zero height
                        // (user report 2026-10-24: the prefix adder vanished once a chip existed).
                        theme::icon_text(Icon::X)
                            .size(12)
                            .width(Length::Fixed(14.0))
                            .height(Length::Fixed(14.0)),
                    )
                    .style(button::text)
                    .padding(2)
                    .on_press_maybe(remove(i)),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            )
            .padding([2, 8])
            .style(theme::seg_group_style(t)),
        );
    }
    // The chips wrap within the field width and the adder sits on its own line below them: it
    // keeps one tree position as chips are added or removed (a growing strip shifted it, dropping
    // its focus; plan round-12-feedback-todos WP-2), and long chips can never push it off the
    // right edge, where it was laid out at zero height (user report 2026-10-24).
    let body = column![strip.wrap().vertical_spacing(4), adder];
    row![
        h.label_col(t),
        body.spacing(4).width(Length::Fixed(FIELD_W))
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

/// The adder: a (typeahead) text input, or a pick list over the choices left.
fn adder<'a>(
    cx: Rcx<'_>,
    path: &cv_schema::Path,
    items: &[String],
    choices: &'static [&'static str],
    draft: &str,
    picker: Option<pickers::Picker>,
) -> Element<'a, Message> {
    if !choices.is_empty() {
        let left: Vec<String> = choices
            .iter()
            .filter(|c| !items.iter().any(|i| i == *c))
            .map(|c| (*c).to_owned())
            .collect();
        if !cx.editable || left.is_empty() {
            return Space::new().into();
        }
        let p = path.clone();
        return pick_list(
            left,
            None::<String>,
            cx.form_fn(move |c| FormMsg::ChipAdd {
                path: p.clone(),
                value: Some(c),
            }),
        )
        .placeholder("add…")
        .text_size(14)
        .padding([8, 10])
        .into();
    }
    let mut input = text_input("add…", draft)
        .size(14)
        .padding([8, 10])
        .width(Length::Fixed(160.0));
    if cx.editable {
        let p = path.clone();
        input = input
            .on_input(cx.form_fn(move |text| FormMsg::ChipDraft {
                path: p.clone(),
                text,
                picker,
            }))
            .on_submit(if picker.is_some() {
                Message::Settings(SettingsMsg::Typeahead(typeahead::Key::Enter))
            } else {
                cx.form(FormMsg::ChipAdd {
                    path: path.clone(),
                    value: None,
                })
            });
    }
    let p = path.clone();
    match cx.ta_list(
        path,
        cx.form_fn(move |v| FormMsg::ChipAdd {
            path: p.clone(),
            value: Some(v),
        }),
    ) {
        // One tree shape open or closed: swapping a bare input for a column moved
        // the input in the widget tree, dropping its focus and the press on a
        // candidate (plan round-6-feedback-todos WP-8).
        Some(l) => column![input, container(l).width(Length::Fixed(FIELD_W))]
            .spacing(2)
            .into(),
        None => column![input, container(Space::new())].spacing(0).into(),
    }
}
