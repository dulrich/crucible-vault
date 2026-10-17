//! The form widget set (plan `editor-settings-tab` WP-3 "Widgets"): renders [`form::Row`]s as
//! iced elements under the fleet design rules — one 34px control height
//! ([`theme::CONTROL`]), `seg_group_style`/`segment_style` seg-groups, lucide glyphs through
//! [`theme::icon_button`], help and errors as stable lines under the control.
//!
//! Every control emits [`SettingsMsg::Edit`] (a [`DocEdit`] on the file) or
//! [`SettingsMsg::Form`] (UI state / typed input); nothing is written until Save.

use iced::widget::{Space, button, column, container, pick_list, row, text, text_input, toggler};
use iced::{Alignment, Element, Font, Length};

use super::form::{ActiveTa, Kind, Row};
use super::{DocEdit, FormMsg, ScalarValue, Settings, SettingsMsg, pickers};
use crate::icons::Icon;
use crate::message::Message;
use crate::theme::{self, Tokens};
use crate::widget::typeahead;

/// Label column width.
pub const LABEL_W: f32 = 220.0;
/// Control width for text-like fields.
pub const FIELD_W: f32 = 420.0;
/// Indent per nesting level.
pub const INDENT: f32 = 18.0;

/// Render context.
#[derive(Clone, Copy)]
pub struct Rcx<'a> {
    /// The file the rows edit (vault path).
    pub file: &'a str,
    pub t: Tokens,
    /// The doc is editable (loaded, not saving).
    pub editable: bool,
    /// Vault paths for path / folder typeaheads (the explorer's list).
    pub paths: &'a [String],
    /// The settings model (picker candidates and labels, plan round-5 WP-4).
    pub settings: &'a Settings,
    /// The active typeahead, when it is on this file.
    pub active: Option<&'a ActiveTa>,
}

impl<'a> Rcx<'a> {
    fn edit(&self, e: DocEdit) -> Message {
        Message::Settings(SettingsMsg::Edit(self.file.to_owned(), e))
    }

    fn form(&self, m: FormMsg) -> Message {
        Message::Settings(SettingsMsg::Form(self.file.to_owned(), m))
    }

    fn on<T>(&self, m: T) -> Option<T> {
        self.editable.then_some(m)
    }

    /// The active typeahead when it is the field at `path`.
    fn active_at(&self, path: &cv_schema::Path) -> Option<&'a ActiveTa> {
        self.active.filter(|a| a.path == *path)
    }

    /// The open typeahead list for the field at `path` (empty element when closed).
    fn ta_list<'b>(
        &self,
        path: &cv_schema::Path,
        pick: impl Fn(String) -> Message,
    ) -> Option<Element<'b, Message>> {
        let a = self.active_at(path).filter(|a| a.ta.open)?;
        let visible = self.settings.ta_visible(self.paths);
        Some(typeahead::list(
            &visible,
            a.ta.highlight,
            &self.settings.ta_status(),
            pick,
            self.t,
        ))
    }
}

/// The rows as one column.
pub fn render<'a>(rows: Vec<Row>, cx: Rcx<'_>) -> Element<'a, Message> {
    let mut col = column![].spacing(6);
    for r in rows {
        col = col.push(row_el(r, cx));
    }
    col.into()
}

pub fn small(s: impl Into<String>, t: Tokens) -> Element<'static, Message> {
    text(s.into())
        .size(12)
        .color(t.text_faint.to_color())
        .into()
}

pub fn text_button<'a>(
    label: impl Into<String>,
    msg: Option<Message>,
    t: Tokens,
) -> Element<'a, Message> {
    button(theme::control_label(label.into(), 14.0))
        .height(theme::CONTROL)
        .padding([0, 12])
        .style(theme::icon_button_style(t, false))
        .on_press_maybe(msg)
        .into()
}

fn row_el<'a>(r: Row, cx: Rcx<'_>) -> Element<'a, Message> {
    let t = cx.t;
    let indent = Space::new().width(Length::Fixed(f32::from(r.depth) * INDENT));
    let errors: Vec<Element<'a, Message>> = r
        .errors
        .iter()
        .map(|e| {
            text(e.clone())
                .size(12)
                .color(t.error_text.to_color())
                .into()
        })
        .collect();
    let label_col = |label: String, help: &'static str| -> Element<'a, Message> {
        let mut c = column![text(label).size(14)].spacing(2);
        if !help.is_empty() {
            c = c.push(small(help, t));
        }
        c.width(Length::Fixed(LABEL_W)).into()
    };
    let path = r.path.clone();
    let body: Element<'a, Message> = match r.kind {
        Kind::Heading => {
            let mut c = column![text(r.label).size(15).color(t.heading.to_color())].spacing(2);
            if !r.help.is_empty() {
                c = c.push(small(r.help, t));
            }
            c.into()
        }
        Kind::Text {
            value,
            placeholder,
            mono,
            picker,
            required,
        } => {
            let p2 = path.clone();
            let shown = match (picker, cx.active_at(&path)) {
                (Some(_), Some(a)) => a.draft.clone(),
                (Some(pk), None) => {
                    pickers::label_of(cx.settings, pk, &value).unwrap_or(value.clone())
                }
                (None, _) => value.clone(),
            };
            // WP-6: an empty sources.toml `channel` shows the fetched title (never written).
            let placeholder = if value.is_empty() {
                pickers::source_channel_placeholder(cx.settings, cx.file, &path)
                    .unwrap_or(placeholder)
            } else {
                placeholder
            };
            let mut input = text_input(&placeholder, &shown)
                .size(14)
                .padding([8, 10])
                .width(Length::Fixed(FIELD_W));
            if mono {
                input = input.font(Font::MONOSPACE);
            }
            if cx.editable {
                let file = cx.file.to_owned();
                input = input.on_input(move |text| {
                    Message::Settings(SettingsMsg::Form(
                        file.clone(),
                        FormMsg::Text {
                            path: p2.clone(),
                            text,
                            required,
                            picker,
                        },
                    ))
                });
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
                row![input, typeahead::clear_button(clear, t)]
                    .spacing(4)
                    .align_y(Alignment::Center)
                    .into()
            } else {
                input.into()
            };
            let mut control = column![field].spacing(2);
            let (file, p3) = (cx.file.to_owned(), path.clone());
            if let Some(l) = cx.ta_list(&path, move |value| {
                Message::Settings(SettingsMsg::Form(
                    file.clone(),
                    FormMsg::Pick {
                        path: p3.clone(),
                        value,
                    },
                ))
            }) {
                control = control.push(container(l).width(Length::Fixed(FIELD_W)));
            }
            row![label_col(r.label, r.help), control]
                .spacing(8)
                .align_y(Alignment::Start)
                .into()
        }
        Kind::Toggle { value } => {
            let file = cx.file.to_owned();
            let p2 = path.clone();
            let tg = toggler(value).on_toggle_maybe(cx.editable.then_some(move |b| {
                Message::Settings(SettingsMsg::Edit(
                    file.clone(),
                    DocEdit::Set(p2.clone(), ScalarValue::Bool(b)),
                ))
            }));
            row![
                label_col(r.label, r.help),
                container(tg)
                    .height(theme::CONTROL)
                    .align_y(Alignment::Center)
            ]
            .spacing(8)
            .into()
        }
        Kind::Number {
            text: value,
            placeholder,
            float,
        } => {
            let file = cx.file.to_owned();
            let p2 = path.clone();
            let mut input = text_input(&placeholder, &value)
                .size(14)
                .padding([8, 10])
                .width(Length::Fixed(160.0));
            if cx.editable {
                input = input.on_input(move |text| {
                    Message::Settings(SettingsMsg::Form(
                        file.clone(),
                        FormMsg::Number {
                            path: p2.clone(),
                            text,
                            float,
                        },
                    ))
                });
            }
            row![label_col(r.label, r.help), input].spacing(8).into()
        }
        Kind::Enum {
            choices,
            value,
            required,
        } => {
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
            } else {
                const UNSET: &str = "(unset)";
                let mut opts: Vec<String> = Vec::new();
                if !required {
                    opts.push(UNSET.to_owned());
                }
                opts.extend(choices.iter().map(|c| (*c).to_owned()));
                let file = cx.file.to_owned();
                let p2 = path.clone();
                let editable = cx.editable;
                let pl = pick_list(opts, value.clone(), move |c: String| {
                    let e = if c == UNSET {
                        DocEdit::Remove(p2.clone())
                    } else {
                        DocEdit::Set(p2.clone(), ScalarValue::Str(c))
                    };
                    Message::Settings(SettingsMsg::Edit(file.clone(), e))
                })
                .text_size(14)
                .padding([8, 10]);
                if editable {
                    pl.into()
                } else {
                    text(value.unwrap_or_default()).size(14).into()
                }
            };
            row![label_col(r.label, r.help), control]
                .spacing(8)
                .align_y(Alignment::Center)
                .into()
        }
        Kind::Chips {
            items,
            choices,
            draft,
            picker,
        } => {
            let chans = match picker {
                Some(pickers::Picker::Channel) => pickers::channels(cx.settings),
                _ => Vec::new(),
            };
            let mut chips = row![].spacing(4).align_y(Alignment::Center);
            for (i, it) in items.iter().enumerate() {
                chips = chips.push(
                    container(
                        row![
                            text(if picker == Some(pickers::Picker::Channel) {
                                pickers::chip_label(cx.settings, &chans, it)
                            } else {
                                pickers::channel_label(&chans, it)
                            })
                            .size(13),
                            button(theme::icon_text(Icon::X).size(12))
                                .style(button::text)
                                .padding(2)
                                .on_press_maybe(cx.on(cx.form(FormMsg::ChipRemove {
                                    path: path.clone(),
                                    index: i,
                                }))),
                        ]
                        .spacing(4)
                        .align_y(Alignment::Center),
                    )
                    .padding([2, 8])
                    .style(theme::seg_group_style(t)),
                );
            }
            let adder: Element<'a, Message> = if choices.is_empty() {
                let file = cx.file.to_owned();
                let p2 = path.clone();
                let mut input = text_input("add…", &draft)
                    .size(14)
                    .padding([8, 10])
                    .width(Length::Fixed(160.0));
                if cx.editable {
                    input = input
                        .on_input(move |text| {
                            Message::Settings(SettingsMsg::Form(
                                file.clone(),
                                FormMsg::ChipDraft {
                                    path: p2.clone(),
                                    text,
                                    picker,
                                },
                            ))
                        })
                        .on_submit(if picker.is_some() {
                            Message::Settings(SettingsMsg::Typeahead(typeahead::Key::Enter))
                        } else {
                            cx.form(FormMsg::ChipAdd {
                                path: path.clone(),
                                value: None,
                            })
                        });
                }
                let (file, p3) = (cx.file.to_owned(), path.clone());
                match cx.ta_list(&path, move |v| {
                    Message::Settings(SettingsMsg::Form(
                        file.clone(),
                        FormMsg::ChipAdd {
                            path: p3.clone(),
                            value: Some(v),
                        },
                    ))
                }) {
                    Some(l) => column![input, container(l).width(Length::Fixed(FIELD_W))]
                        .spacing(2)
                        .into(),
                    None => input.into(),
                }
            } else {
                let left: Vec<String> = choices
                    .iter()
                    .filter(|c| !items.iter().any(|i| i == *c))
                    .map(|c| (*c).to_owned())
                    .collect();
                let file = cx.file.to_owned();
                let p2 = path.clone();
                if cx.editable && !left.is_empty() {
                    pick_list(left, None::<String>, move |c| {
                        Message::Settings(SettingsMsg::Form(
                            file.clone(),
                            FormMsg::ChipAdd {
                                path: p2.clone(),
                                value: Some(c),
                            },
                        ))
                    })
                    .placeholder("add…")
                    .text_size(14)
                    .padding([8, 10])
                    .into()
                } else {
                    Space::new().into()
                }
            };
            row![label_col(r.label, r.help), chips.push(adder)]
                .spacing(8)
                .align_y(Alignment::Center)
                .into()
        }
        Kind::ReadOnly { toml } => row![
            label_col(r.label, r.help),
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
        .into(),
        Kind::Item {
            list,
            index,
            len,
            summary,
            expanded,
        } => {
            let chev = if expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            row![
                theme::icon_button(chev, Some(cx.form(FormMsg::Toggle(path.clone()))), t),
                button(theme::control_label(summary, 14.0))
                    .style(button::text)
                    .height(theme::CONTROL)
                    .width(Length::Fill)
                    .on_press(cx.form(FormMsg::Toggle(path.clone()))),
                theme::icon_button(
                    Icon::ChevronUp,
                    (index > 0)
                        .then(|| cx.on(cx.edit(DocEdit::Move(list.clone(), index, index - 1))))
                        .flatten(),
                    t
                ),
                theme::icon_button(
                    Icon::ChevronDown,
                    (index + 1 < len)
                        .then(|| cx.on(cx.edit(DocEdit::Move(list.clone(), index, index + 1))))
                        .flatten(),
                    t
                ),
                theme::icon_button(Icon::X, cx.on(cx.edit(DocEdit::Remove(path.clone()))), t),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into()
        }
        Kind::AddItem { list, len, union } => match union {
            None => text_button(
                "Add",
                cx.on(cx.edit(DocEdit::InsertRecord(list, len, Vec::new()))),
                t,
            ),
            Some(u) => {
                let tags: Vec<String> = u.variants.iter().map(|v| v.tag.to_owned()).collect();
                let file = cx.file.to_owned();
                let tag_key = u.tag;
                if cx.editable {
                    pick_list(tags, None::<String>, move |tag| {
                        Message::Settings(SettingsMsg::Edit(
                            file.clone(),
                            DocEdit::InsertRecord(
                                list.clone(),
                                len,
                                vec![(tag_key.to_owned(), ScalarValue::Str(tag))],
                            ),
                        ))
                    })
                    .placeholder(format!("Add {}…", u.name))
                    .text_size(14)
                    .padding([8, 10])
                    .into()
                } else {
                    Space::new().into()
                }
            }
        },
        Kind::Variant { union, tag } => {
            let tags: Vec<String> = union.variants.iter().map(|v| v.tag.to_owned()).collect();
            let file = cx.file.to_owned();
            let rec = cv_schema::Path(path.0[..path.0.len().saturating_sub(1)].to_vec());
            let control: Element<'a, Message> = if cx.editable {
                pick_list(tags, tag.clone(), move |new| {
                    Message::Settings(SettingsMsg::Edit(
                        file.clone(),
                        DocEdit::SwitchVariant(rec.clone(), union, new),
                    ))
                })
                .placeholder("choose a kind")
                .text_size(14)
                .padding([8, 10])
                .into()
            } else {
                text(tag.unwrap_or_default()).size(14).into()
            };
            row![label_col(r.label, r.help), control]
                .spacing(8)
                .align_y(Alignment::Center)
                .into()
        }
        Kind::MapKey {
            map,
            key,
            draft,
            collapsible,
            expanded,
        } => {
            let file = cx.file.to_owned();
            let p2 = path.clone();
            let mut input = text_input("key", &draft)
                .size(14)
                .padding([8, 10])
                .width(Length::Fixed(LABEL_W));
            if cx.editable {
                input = input
                    .on_input(move |text| {
                        Message::Settings(SettingsMsg::Form(
                            file.clone(),
                            FormMsg::RenameDraft {
                                at: p2.clone(),
                                text,
                            },
                        ))
                    })
                    .on_submit(cx.form(FormMsg::RenameCommit {
                        map: map.clone(),
                        from: key.clone(),
                    }));
            }
            let lead: Element<'a, Message> = if collapsible {
                let chev = if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                };
                theme::icon_button(chev, Some(cx.form(FormMsg::Toggle(path.clone()))), t).into()
            } else {
                Space::new().width(theme::CONTROL).into()
            };
            let renamed = draft != key;
            let rename: Element<'a, Message> = if renamed {
                text_button(
                    "Rename",
                    cx.on(cx.form(FormMsg::RenameCommit {
                        map: map.clone(),
                        from: key.clone(),
                    })),
                    t,
                )
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
        Kind::AddKey { map, draft, value } => {
            let file = cx.file.to_owned();
            let m2 = map.clone();
            let add = cx.form(FormMsg::NewKeyAdd {
                map: map.clone(),
                value,
            });
            let mut input = text_input("new key", &draft)
                .size(14)
                .padding([8, 10])
                .width(Length::Fixed(LABEL_W));
            if cx.editable {
                input = input
                    .on_input(move |text| {
                        Message::Settings(SettingsMsg::Form(
                            file.clone(),
                            FormMsg::NewKeyDraft {
                                map: m2.clone(),
                                text,
                            },
                        ))
                    })
                    .on_submit(add.clone());
            }
            row![
                Space::new().width(theme::CONTROL),
                input,
                text_button("Add", cx.on(add).filter(|_| !draft.trim().is_empty()), t),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into()
        }
    };
    let mut c = column![body].spacing(2);
    for e in errors {
        c = c.push(e);
    }
    row![indent, c].into()
}
