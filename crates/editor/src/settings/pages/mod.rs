//! One page per config file (plan `editor-settings-tab` WP-3 "Pages"), each the WP-2 file
//! header + a schema-driven form ([`super::form`], [`super::widgets`]) + the Save/Revert bar.
//! Lint / localize / search settings are the `lint` / `localize` / `search_*` step forms inside
//! Actions (no separate pages).

pub mod actions;
pub mod agents;
pub mod bookmarks;
pub mod captures;
pub mod hotkeys;
pub mod kinds;
pub mod providers;
pub mod schedules;
pub mod views;

use iced::Element;
use iced::widget::{Space, column, text};

use super::form::{self, Ctx, Row};
use super::widgets::{self, Rcx};
use super::{ConfigFile, Settings, file_header, save_bar};
use crate::message::Message;
use crate::theme::Tokens;

/// What a page needs besides its file.
#[derive(Clone, Copy)]
pub struct PageCx<'a> {
    pub s: &'a Settings,
    pub paths: &'a [String],
    pub t: Tokens,
    pub mode: cv_protocol::Mode,
}

/// The rows of `f`'s whole form (empty when the file has no doc or schema).
pub fn file_rows(s: &Settings, f: &ConfigFile) -> Vec<Row> {
    match (&f.doc, f.schema) {
        (Some(doc), Some(schema)) => form::rows(
            &Ctx {
                file: &f.path,
                doc,
                errors: &f.errors,
                form: &s.form,
            },
            schema,
        ),
        _ => Vec::new(),
    }
}

/// The render context of `f`.
pub fn rcx<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Rcx<'a> {
    Rcx {
        file: &f.path,
        t: cx.t,
        editable: f.doc.is_some() && !f.saving,
        paths: cx.paths,
        settings: cx.s,
        active: cx.s.form.typeahead.as_ref().filter(|a| a.file == f.path),
    }
}

/// Header + an intro line + `rows` + the save bar.
pub fn frame<'a>(
    cx: PageCx<'a>,
    f: &'a ConfigFile,
    intro: &'a str,
    rows: Vec<Row>,
) -> Element<'a, Message> {
    let intro: Element<'a, Message> = if intro.is_empty() {
        Space::new().into()
    } else {
        text(intro)
            .size(13)
            .color(cx.t.text_faint.to_color())
            .into()
    };
    column![
        file_header(f, cx.t),
        intro,
        widgets::render(rows, rcx(cx, f)),
        save_bar(f, cx.t, cx.mode),
    ]
    .spacing(10)
    .into()
}

/// The generic page: the whole schema as one form.
pub fn generic<'a>(cx: PageCx<'a>, f: &'a ConfigFile, intro: &'a str) -> Element<'a, Message> {
    frame(cx, f, intro, file_rows(cx.s, f))
}
