//! Text and number fields.

use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Font, Length};

use super::{FIELD_W, Head, Rcx};
use crate::icons::Icon;
use crate::message::Message;
use crate::settings::form::Step;
use crate::settings::{FormMsg, SettingsMsg, pickers};
use crate::theme;
use crate::widget::typeahead;

/// A single-line text field (a typeahead when `picker` is set).
pub(super) fn text_field<'a>(
    h: &Head,
    cx: Rcx<'_>,
    value: &str,
    placeholder: String,
    mono: bool,
    picker: Option<pickers::Picker>,
    required: bool,
) -> Element<'a, Message> {
    let t = cx.t;
    let path = &h.path;
    let shown = match (picker, cx.active_at(path)) {
        (Some(_), Some(a)) => a.draft.clone(),
        (Some(pk), None) => {
            pickers::label_of(cx.settings, pk, value).unwrap_or_else(|| value.to_owned())
        }
        (None, _) => value.to_owned(),
    };
    // WP-6: an empty sources.toml `channel` shows the fetched title (never written).
    let placeholder = if value.is_empty() {
        pickers::source_channel_placeholder(cx.settings, cx.file, path).unwrap_or(placeholder)
    } else {
        placeholder
    };
    let mut input = crate::widget::clip_input::clip_input(&placeholder, &shown)
        .size(14)
        .padding([8, 10])
        .width(Length::Fixed(FIELD_W));
    if mono {
        input = input.font(Font::MONOSPACE);
    }
    if cx.editable {
        let p = path.clone();
        input = input.on_input(cx.form_fn(move |text| FormMsg::Text {
            path: p.clone(),
            text,
            required,
            picker,
        }));
        if picker.is_some() {
            input = input.on_submit(Message::Settings(SettingsMsg::Typeahead(
                typeahead::Key::Enter,
            )));
        }
    }
    let field: Element<'a, Message> = if picker.is_some() {
        let clear = (!value.is_empty())
            .then(|| {
                cx.on(cx.form(FormMsg::Clear {
                    path: path.clone(),
                    required,
                }))
            })
            .flatten();
        let mut r = row![input, typeahead::clear_button(clear, t)]
            .spacing(4)
            .align_y(Alignment::Center);
        if let Some(pk @ (pickers::Picker::Model | pickers::Picker::ModelRef)) = picker {
            // Plan round-6-feedback-todos WP-8: re-fetch the live model list.
            r = r.push(
                button(text("\u{21bb}").size(14))
                    .style(button::text)
                    .padding([2, 6])
                    .on_press_maybe(cx.on(cx.form(FormMsg::RefreshModels {
                        path: path.clone(),
                        picker: pk,
                    }))),
            );
        }
        r.into()
    } else {
        input.into()
    };
    let mut control = column![field].spacing(2);
    let p = path.clone();
    if let Some(l) = cx.ta_list(
        path,
        cx.form_fn(move |value| FormMsg::Pick {
            path: p.clone(),
            value,
        }),
    ) {
        control = control.push(container(l).width(Length::Fixed(FIELD_W)));
    }
    row![h.label_col(t), control]
        .spacing(8)
        .align_y(Alignment::Start)
        .into()
}

/// A number field; a bounded number gets −/+ beside it (Decision 3).
pub(super) fn number<'a>(
    h: &Head,
    cx: Rcx<'_>,
    value: &str,
    placeholder: &str,
    float: bool,
    step: Option<Step>,
) -> Element<'a, Message> {
    let t = cx.t;
    let path = &h.path;
    let mut input = crate::widget::clip_input::clip_input(placeholder, value)
        .size(14)
        .padding([8, 10])
        .width(Length::Fixed(160.0));
    if cx.editable {
        let p = path.clone();
        input = input.on_input(cx.form_fn(move |text| FormMsg::Number {
            path: p.clone(),
            text,
            float,
        }));
    }
    let mut ctl = row![h.label_col(t)].spacing(8).align_y(Alignment::Center);
    match step {
        Some(st) => {
            let m = |v: Option<crate::settings::ScalarValue>| {
                v.and_then(|value| {
                    cx.on(cx.form(FormMsg::Step {
                        path: path.clone(),
                        value,
                    }))
                })
            };
            ctl = ctl
                .push(theme::icon_button(Icon::ChevronDown, m(st.dec), t))
                .push(input)
                .push(theme::icon_button(Icon::ChevronUp, m(st.inc), t));
        }
        None => ctl = ctl.push(input),
    }
    ctl.into()
}
