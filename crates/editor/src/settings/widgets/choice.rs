//! Toggles, closed choices and union kind pickers.

use iced::widget::{button, container, pick_list, row, text, toggler};
use iced::{Alignment, Element};

use super::{Head, Rcx};
use crate::message::Message;
use crate::settings::{DocEdit, ScalarValue};
use crate::theme;

pub(super) fn toggle<'a>(h: &Head, cx: Rcx<'_>, value: bool) -> Element<'a, Message> {
    let p = h.path.clone();
    let tg = toggler(value).on_toggle_maybe(
        cx.editable
            .then(|| cx.edit_fn(move |b| DocEdit::Set(p.clone(), ScalarValue::Bool(b)))),
    );
    row![
        h.label_col(cx.t),
        container(tg)
            .height(theme::CONTROL)
            .align_y(Alignment::Center)
    ]
    .spacing(8)
    .into()
}

/// A closed choice: seg-group when ≤4 choices, else a pick list.
pub(super) fn enumeration<'a>(
    h: &Head,
    cx: Rcx<'_>,
    choices: &'static [&'static str],
    value: Option<String>,
    required: bool,
) -> Element<'a, Message> {
    let t = cx.t;
    let path = &h.path;
    let control: Element<'a, Message> = if choices.len() <= 4 {
        let segs = choices.iter().map(|c| {
            let active = value.as_deref() == Some(*c);
            let m = if active && !required {
                DocEdit::Remove(path.clone())
            } else {
                DocEdit::Set(path.clone(), ScalarValue::Str((*c).to_owned()))
            };
            button(theme::control_label(*c, 14.0))
                .height(theme::SEGMENT)
                .padding([0, 12])
                .style(theme::segment_style(t, active))
                .on_press_maybe(cx.on(cx.edit(m)))
                .into()
        });
        container(row(segs).spacing(0))
            .padding(1)
            .style(theme::seg_group_style(t))
            .into()
    } else if cx.editable {
        const UNSET: &str = "(unset)";
        let mut opts: Vec<String> = Vec::new();
        if !required {
            opts.push(UNSET.to_owned());
        }
        opts.extend(choices.iter().map(|c| (*c).to_owned()));
        let p = path.clone();
        pick_list(
            opts,
            value,
            cx.edit_fn(move |c: String| {
                if c == UNSET {
                    DocEdit::Remove(p.clone())
                } else {
                    DocEdit::Set(p.clone(), ScalarValue::Str(c))
                }
            }),
        )
        .text_size(14)
        .padding([8, 10])
        .into()
    } else {
        text(value.unwrap_or_default()).size(14).into()
    };
    row![h.label_col(t), control]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// A tagged union's kind picker.
pub(super) fn variant<'a>(
    h: &Head,
    cx: Rcx<'_>,
    union: &'static cv_schema::Union,
    tag: Option<String>,
) -> Element<'a, Message> {
    let tags: Vec<String> = union.variants.iter().map(|v| v.tag.to_owned()).collect();
    let rec = cv_schema::Path(h.path.0[..h.path.0.len().saturating_sub(1)].to_vec());
    let control: Element<'a, Message> = if cx.editable {
        pick_list(
            tags,
            tag,
            cx.edit_fn(move |new| DocEdit::SwitchVariant(rec.clone(), union, new)),
        )
        .placeholder("choose a kind")
        .text_size(14)
        .padding([8, 10])
        .into()
    } else {
        text(tag.unwrap_or_default()).size(14).into()
    };
    row![h.label_col(cx.t), control]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}
