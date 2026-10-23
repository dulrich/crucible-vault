//! Structural rows: headings, read-only values, list items and map keys.

use iced::widget::{Space, button, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element, Font, Length};

use super::{FIELD_W, Head, LABEL_W, Rcx, small, text_button};
use crate::icons::Icon;
use crate::message::Message;
use crate::settings::{DocEdit, FormMsg, ScalarValue};
use crate::theme::{self, Tokens};

pub(super) fn heading<'a>(h: &Head, t: Tokens) -> Element<'a, Message> {
    let mut c = column![text(h.label.clone()).size(15).color(t.heading.to_color())].spacing(2);
    if !h.help.is_empty() {
        c = c.push(small(h.help, t));
    }
    c.into()
}

pub(super) fn read_only<'a>(h: &Head, t: Tokens, toml: String) -> Element<'a, Message> {
    row![
        h.label_col(t),
        column![
            container(text(toml).size(13).font(Font::MONOSPACE))
                .padding([6, 10])
                .width(Length::Fixed(FIELD_W))
                .style(theme::seg_group_style(t)),
            small("read-only here (edit the file directly)", t),
        ]
        .spacing(2),
    ]
    .spacing(8)
    .into()
}

fn chevron(expanded: bool) -> Icon {
    if expanded {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    }
}

/// One list item's summary row (`reorder`: show the up/down controls).
#[allow(clippy::too_many_arguments)]
pub(super) fn item<'a>(
    h: &Head,
    cx: Rcx<'_>,
    list: &cv_schema::Path,
    index: usize,
    len: usize,
    summary: String,
    expanded: bool,
    reorder: bool,
) -> Element<'a, Message> {
    let (t, path) = (cx.t, &h.path);
    let mv = |to: usize| cx.on(cx.edit(DocEdit::Move(list.clone(), index, to)));
    let mut r = row![
        theme::icon_button(
            chevron(expanded),
            Some(cx.form(FormMsg::Toggle(path.clone()))),
            t
        ),
        button(theme::control_label(summary, 14.0))
            .style(button::text)
            .height(theme::CONTROL)
            .width(Length::Fill)
            .on_press(cx.form(FormMsg::Toggle(path.clone()))),
    ];
    if reorder {
        r = r
            .push(theme::icon_button(
                Icon::ChevronUp,
                (index > 0).then(|| mv(index - 1)).flatten(),
                t,
            ))
            .push(theme::icon_button(
                Icon::ChevronDown,
                (index + 1 < len).then(|| mv(index + 1)).flatten(),
                t,
            ));
    }
    r.push(theme::icon_button(
        Icon::X,
        cx.on(cx.edit(DocEdit::Remove(path.clone()))),
        t,
    ))
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}

/// "Add" under a list (`union`: pick the new item's kind).
pub(super) fn add_item<'a>(
    cx: Rcx<'_>,
    list: cv_schema::Path,
    len: usize,
    union: Option<&'static cv_schema::Union>,
) -> Element<'a, Message> {
    let Some(u) = union else {
        return text_button(
            "Add",
            cx.on(cx.edit(DocEdit::InsertRecord(list, len, Vec::new()))),
            cx.t,
        );
    };
    if !cx.editable {
        return Space::new().into();
    }
    let tags: Vec<String> = u.variants.iter().map(|v| v.tag.to_owned()).collect();
    let tag_key = u.tag;
    pick_list(
        tags,
        None::<String>,
        cx.edit_fn(move |tag| {
            DocEdit::InsertRecord(
                list.clone(),
                len,
                vec![(tag_key.to_owned(), ScalarValue::Str(tag))],
            )
        }),
    )
    .placeholder(format!("Add {}…", u.name))
    .text_size(14)
    .padding([8, 10])
    .into()
}

/// One map entry's key row (rename / remove; `collapsible` for compound values).
pub(super) fn map_key<'a>(
    h: &Head,
    cx: Rcx<'_>,
    map: &cv_schema::Path,
    key: &str,
    draft: &str,
    collapsible: bool,
    expanded: bool,
) -> Element<'a, Message> {
    let (t, path) = (cx.t, &h.path);
    let commit = || {
        cx.form(FormMsg::RenameCommit {
            map: map.clone(),
            from: key.to_owned(),
        })
    };
    let mut input = text_input("key", draft)
        .size(14)
        .padding([8, 10])
        .width(Length::Fixed(LABEL_W));
    if cx.editable {
        let p = path.clone();
        input = input
            .on_input(cx.form_fn(move |text| FormMsg::RenameDraft {
                at: p.clone(),
                text,
            }))
            .on_submit(commit());
    }
    let lead: Element<'a, Message> = if collapsible {
        theme::icon_button(
            chevron(expanded),
            Some(cx.form(FormMsg::Toggle(path.clone()))),
            t,
        )
        .into()
    } else {
        Space::new().width(theme::CONTROL).into()
    };
    let rename: Element<'a, Message> = if draft != key {
        text_button("Rename", cx.on(commit()), t)
    } else {
        Space::new().into()
    };
    row![
        lead,
        input,
        rename,
        theme::icon_button(Icon::X, cx.on(cx.edit(DocEdit::Remove(path.clone()))), t),
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}

/// "Add key" under a map.
pub(super) fn add_key<'a>(
    cx: Rcx<'_>,
    map: &cv_schema::Path,
    draft: &str,
    value: &'static cv_schema::Node,
) -> Element<'a, Message> {
    let add = cx.form(FormMsg::NewKeyAdd {
        map: map.clone(),
        value,
    });
    let mut input = text_input("new key", draft)
        .size(14)
        .padding([8, 10])
        .width(Length::Fixed(LABEL_W));
    if cx.editable {
        let m = map.clone();
        input = input
            .on_input(cx.form_fn(move |text| FormMsg::NewKeyDraft {
                map: m.clone(),
                text,
            }))
            .on_submit(add.clone());
    }
    row![
        Space::new().width(theme::CONTROL),
        input,
        text_button("Add", cx.on(add).filter(|_| !draft.trim().is_empty()), cx.t),
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}
