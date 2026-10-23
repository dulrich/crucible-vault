//! The form widget set (plan `editor-settings-tab` WP-3 "Widgets"): renders [`form::Row`]s as
//! iced elements under the fleet design rules — one 34px control height
//! ([`theme::CONTROL`]), `seg_group_style`/`segment_style` seg-groups, lucide glyphs through
//! [`theme::icon_button`], help and errors as stable lines under the control.
//!
//! Every control emits [`SettingsMsg::Edit`] (a [`DocEdit`] on the file) or
//! [`SettingsMsg::Form`] (UI state / typed input); a valid edit is auto-saved after a short debounce.
//!
//! One function per [`Kind`] group: [`text`] (text, number), [`choice`] (toggle,
//! enum, variant), [`chips`] and [`structure`] (heading, read-only, list items, map keys).
//! [`Rcx::form_fn`] / [`Rcx::edit_fn`] build every message closure.
//!
//! [`form::Row`]: super::form::Row

mod chips;
mod choice;
mod structure;
mod text;

use iced::widget::{Space, button, column, row};
use iced::{Element, Length};

use super::form::{ActiveTa, Kind, Row};
use super::{DocEdit, FormMsg, Settings, SettingsMsg};
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

    /// A message closure over this file's form messages (owns the file clone).
    fn form_fn<T>(&self, f: impl Fn(T) -> FormMsg + 'static) -> impl Fn(T) -> Message + 'static {
        let file = self.file.to_owned();
        move |v| Message::Settings(SettingsMsg::Form(file.clone(), f(v)))
    }

    /// A message closure over this file's document edits (owns the file clone).
    fn edit_fn<T>(&self, f: impl Fn(T) -> DocEdit + 'static) -> impl Fn(T) -> Message + 'static {
        let file = self.file.to_owned();
        move |v| Message::Settings(SettingsMsg::Edit(file.clone(), f(v)))
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

/// A row's label, help and path (what every kind's renderer shares).
struct Head {
    label: String,
    help: &'static str,
    path: cv_schema::Path,
}

impl Head {
    /// The label column (label + help line).
    fn label_col<'a>(&self, t: Tokens) -> Element<'a, Message> {
        let mut c = column![iced::widget::text(self.label.clone()).size(14)].spacing(2);
        if !self.help.is_empty() {
            c = c.push(small(self.help, t));
        }
        c.width(Length::Fixed(LABEL_W)).into()
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
    iced::widget::text(s.into())
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
    let h = Head {
        label: r.label,
        help: r.help,
        path: r.path,
    };
    let body: Element<'a, Message> = match r.kind {
        Kind::Heading => structure::heading(&h, t),
        Kind::Text {
            value,
            placeholder,
            mono,
            picker,
            required,
        } => text::text_field(&h, cx, &value, placeholder, mono, picker, required),
        Kind::Toggle { value } => choice::toggle(&h, cx, value),
        Kind::Number {
            text: value,
            placeholder,
            float,
            step,
        } => text::number(&h, cx, &value, &placeholder, float, step),
        Kind::Enum {
            choices,
            value,
            required,
        } => choice::enumeration(&h, cx, choices, value, required),
        Kind::Chips {
            items,
            choices,
            draft,
            picker,
        } => chips::chips(&h, cx, &items, choices, &draft, picker),
        Kind::ReadOnly { toml } => structure::read_only(&h, t, toml),
        Kind::Item {
            list,
            index,
            len,
            summary,
            expanded,
            reorder,
        } => structure::item(&h, cx, &list, index, len, summary, expanded, reorder),
        Kind::AddItem { list, len, union } => structure::add_item(cx, list, len, union),
        Kind::Variant { union, tag } => choice::variant(&h, cx, union, tag),
        Kind::MapKey {
            map,
            key,
            draft,
            collapsible,
            expanded,
        } => structure::map_key(&h, cx, &map, &key, &draft, collapsible, expanded),
        Kind::AddKey { map, draft, value } => structure::add_key(cx, &map, &draft, value),
    };
    let mut c = column![body].spacing(2);
    for e in r.errors {
        c = c.push(
            iced::widget::text(e)
                .size(12)
                .color(t.error_text.to_color()),
        );
    }
    row![indent, c].into()
}
