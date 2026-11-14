//! The Sources page (plan sources-publication-model WP-6): `[[publication]]` records grouped by
//! kind, one line each (name, what feeds it, an In Works toggle), expanding to the full record
//! form on the chevron only. Each kind's title line carries a "default In Works" toggle bound to
//! `[kinds.<kind>] works` in `kinds.toml`.
//!
//! Still a typed editor over the files (D20): every change is a [`DocEdit`] through the existing
//! per-file save loop. The grouping, badge, resolution and toggle logic below is pure (no
//! `iced` types); [`view`] is a thin renderer over it. A file that still has the legacy per-type
//! tables shows a banner and the generic form instead of the overview.

use cv_schema::{ConfigDoc, Path};
use iced::widget::{Space, column, row, text, toggler};
use iced::{Alignment, Element};

use super::{PageCx, generic, rcx};
use crate::icons::Icon;
use crate::message::Message;
use crate::settings::form::{self, Ctx};
use crate::settings::widgets::{self, text_button};
use crate::settings::{ConfigFile, DocEdit, FormMsg, LoadState, ScalarValue, Section, Settings};
use crate::settings::{SettingsMsg, file_header, save_bar};
use crate::theme;

/// The vault path of `sources.toml`.
pub const SOURCES_PATH: &str = cv_schema::config_path!("sources.toml");
/// The vault path of `kinds.toml`.
pub const KINDS_PATH: &str = cv_schema::config_path!("kinds.toml");

/// The banner shown over a legacy-shape file.
pub const LEGACY_BANNER: &str = "This file still uses the old per-type tables ([[youtube]], [[blog]], [[newsletter]], [[serial]]). Run `cv sources migrate` (with `--dry-run` to preview) to convert it to [[publication]] records.";

/// The group title of records with no kind.
pub const NO_KIND: &str = "No kind";

const LEGACY_TABLES: [&str; 4] = ["youtube", "blog", "newsletter", "serial"];

/// The Sources page's transient state (never written to a file; expansion lives in the form
/// state, keyed by the record's path, which `Settings::edit` keeps attached across edits).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SourcesState {
    /// The kind whose "Apply to N publications" prompt is showing. Cleared when either file
    /// reloads, or when another kind default is toggled.
    pub prompt: Option<String>,
    /// The last "Create Source from note" answer's notes and offer (plan
    /// sources-publication-model WP-7).
    pub from_note: Option<super::sources_propose::FromNote>,
}

/// Sources page messages (wrapped in [`SettingsMsg::Sources`]).
#[derive(Debug, Clone, PartialEq)]
pub enum SourcesMsg {
    /// Set `[kinds.<kind>] works` (a `kinds.toml` edit) and raise the apply prompt.
    KindDefault { kind: String, on: bool },
    /// Clear the explicit overrides that differ from the kind's default.
    Apply(String),
    /// Dismiss the prompt.
    Keep,
    /// Add the offered values to the matched record (WP-7).
    ApplyOffer,
    /// Dismiss the "Create Source from note" notes and offer.
    DismissFromNote,
}

/// A `works` value as the file holds it: a non-boolean is never read as off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    On,
    Off,
    Invalid,
}

impl Flag {
    fn of(doc: &ConfigDoc, p: &Path) -> Option<Self> {
        let item = doc.get(p)?;
        Some(match item.as_bool() {
            Some(true) => Self::On,
            Some(false) => Self::Off,
            None => Self::Invalid,
        })
    }
}

/// A kind's default in `kinds.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindDefault {
    /// The kind is not defined there (or the file is not loaded): no toggle.
    Undefined,
    /// Defined without `works`: the default is off.
    Unset,
    /// Defined with `works`.
    Value(Flag),
}

/// The page shape of the loaded `sources.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageShape {
    /// Publication-shape file: the grouped overview.
    Overview,
    /// Any legacy table present: banner + generic form, no overview.
    Legacy,
    /// Absent, malformed or not loaded: the generic page as before.
    Generic,
}

/// Which page `f` gets.
pub fn page_shape(f: &ConfigFile) -> PageShape {
    match (f.state(), f.doc()) {
        (LoadState::Loaded, Some(doc)) => {
            if LEGACY_TABLES
                .iter()
                .any(|t| doc.len(&Path::root().key(t)) > 0)
            {
                PageShape::Legacy
            } else {
                PageShape::Overview
            }
        }
        _ => PageShape::Generic,
    }
}

fn record(i: usize) -> Path {
    Path::root().key("publication").index(i)
}

/// The record's kind (`None` when unset or empty).
fn record_kind(sources: &ConfigDoc, i: usize) -> Option<String> {
    sources
        .get_str(&record(i).key("kind"))
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
}

/// `kind`'s default in `kinds` (`None` = file not loaded).
pub fn kind_default(kinds: Option<&ConfigDoc>, kind: &str) -> KindDefault {
    let Some(doc) = kinds else {
        return KindDefault::Undefined;
    };
    let kp = Path::root().key("kinds").key(kind);
    if doc.get(&kp).is_none() {
        return KindDefault::Undefined;
    }
    Flag::of(doc, &kp.key("works")).map_or(KindDefault::Unset, KindDefault::Value)
}

/// The In-Works value of record `i`: its own `works` when set, else its kind's default when it
/// has a kind and the kind defines it, else off. Mirrors
/// `cv_actions::config::resolve_in_works` (the editor does not depend on `cv-actions`); a
/// non-boolean is [`Flag::Invalid`] where the resolver reports a config error.
pub fn resolve_in_works(sources: &ConfigDoc, kinds: Option<&ConfigDoc>, i: usize) -> Flag {
    if let Some(own) = Flag::of(sources, &record(i).key("works")) {
        return own;
    }
    let Some(kind) = record_kind(sources, i) else {
        return Flag::Off;
    };
    match kind_default(kinds, &kind) {
        KindDefault::Undefined | KindDefault::Unset => Flag::Off,
        KindDefault::Value(f) => f,
    }
}

/// One record line of the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The record's index in the file.
    pub index: usize,
    pub name: String,
    pub badges: Vec<&'static str>,
    pub works: Flag,
}

/// One kind group of the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// `None` = records with no kind (shown last, no default toggle).
    pub kind: Option<String>,
    pub default: Option<KindDefault>,
    pub members: Vec<Member>,
}

impl Group {
    /// The title text of the group.
    pub fn title(&self) -> &str {
        self.kind.as_deref().unwrap_or(NO_KIND)
    }
}

fn non_empty_list(doc: &ConfigDoc, p: &Path) -> bool {
    doc.get_str_list(p).is_some_and(|l| !l.is_empty())
}

/// What feeds record `i`: `feed`, `mail`, `prefix`, `channel`, `host`, or `author` for a record
/// whose only matcher is its author.
pub fn badges(doc: &ConfigDoc, i: usize) -> Vec<&'static str> {
    let rp = record(i);
    let mut out = Vec::new();
    if non_empty_list(doc, &rp.key("feeds")) {
        out.push("feed");
    }
    if non_empty_list(doc, &rp.key("from")) || non_empty_list(doc, &rp.key("list_id")) {
        out.push("mail");
    }
    if non_empty_list(doc, &rp.key("prefix")) {
        out.push("prefix");
    }
    if doc
        .get_str(&rp.key("channel_id"))
        .is_some_and(|s| !s.is_empty())
    {
        out.push("channel");
    }
    if non_empty_list(doc, &rp.key("hosts")) {
        out.push("host");
    }
    if out.is_empty()
        && doc
            .get_str(&rp.key("author"))
            .is_some_and(|s| !s.is_empty())
    {
        out.push("author");
    }
    out
}

/// The overview's groups: by kind name case-insensitively, no-kind last; members by name
/// case-insensitively (ties by file index). Display order only.
pub fn overview(sources: &ConfigDoc, kinds: Option<&ConfigDoc>) -> Vec<Group> {
    let mut by_kind: Vec<(Option<String>, Vec<Member>)> = Vec::new();
    for i in 0..sources.len(&Path::root().key("publication")) {
        let kind = record_kind(sources, i);
        let name = sources
            .get_str(&record(i).key("name"))
            .filter(|n| !n.is_empty())
            .map_or_else(|| format!("#{}", i + 1), str::to_owned);
        let m = Member {
            index: i,
            name,
            badges: badges(sources, i),
            works: resolve_in_works(sources, kinds, i),
        };
        match by_kind.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, ms)) => ms.push(m),
            None => by_kind.push((kind, vec![m])),
        }
    }
    by_kind.sort_by_cached_key(|(k, _)| {
        (
            k.is_none(),
            k.as_ref().map(|k| (k.to_lowercase(), k.clone())),
        )
    });
    by_kind
        .into_iter()
        .map(|(kind, mut members)| {
            members.sort_by_cached_key(|m| (m.name.to_lowercase(), m.index));
            Group {
                default: kind.as_deref().map(|k| kind_default(kinds, k)),
                kind,
                members,
            }
        })
        .collect()
}

/// The edit a record's In Works toggle emits: the opposite of the resolved value; removes the
/// override when the opposite is the kind default, else sets it explicitly. `None` for a
/// non-boolean `works` (nothing to flip).
pub fn record_toggle_edit(
    sources: &ConfigDoc,
    kinds: Option<&ConfigDoc>,
    i: usize,
) -> Option<DocEdit> {
    let new = match resolve_in_works(sources, kinds, i) {
        Flag::Invalid => return None,
        Flag::On => false,
        Flag::Off => true,
    };
    let default = match record_kind(sources, i).map(|k| kind_default(kinds, &k)) {
        None | Some(KindDefault::Undefined | KindDefault::Unset) => Some(false),
        Some(KindDefault::Value(Flag::On)) => Some(true),
        Some(KindDefault::Value(Flag::Off)) => Some(false),
        Some(KindDefault::Value(Flag::Invalid)) => None,
    };
    let wp = record(i).key("works");
    Some(if default == Some(new) {
        DocEdit::Remove(wp)
    } else {
        DocEdit::Set(wp, ScalarValue::Bool(new))
    })
}

/// The `kinds.toml` edit of a kind default toggle.
pub fn kind_toggle_edit(kind: &str, on: bool) -> DocEdit {
    DocEdit::Set(
        Path::root().key("kinds").key(kind).key("works"),
        ScalarValue::Bool(on),
    )
}

/// The records of `kind` holding an explicit boolean `works` that differs from the kind's
/// current default (file indices).
pub fn pending_overrides(sources: &ConfigDoc, kinds: Option<&ConfigDoc>, kind: &str) -> Vec<usize> {
    let default = match kind_default(kinds, kind) {
        KindDefault::Value(Flag::On) => true,
        KindDefault::Unset | KindDefault::Value(Flag::Off) => false,
        KindDefault::Undefined | KindDefault::Value(Flag::Invalid) => return Vec::new(),
    };
    (0..sources.len(&Path::root().key("publication")))
        .filter(|&i| {
            record_kind(sources, i).as_deref() == Some(kind)
                && sources
                    .get(&record(i).key("works"))
                    .and_then(|w| w.as_bool())
                    .is_some_and(|w| w != default)
        })
        .collect()
}

impl Settings {
    fn kinds_doc(&self) -> Option<&ConfigDoc> {
        self.file(KINDS_PATH).and_then(ConfigFile::doc)
    }

    /// Applies a Sources page message (the edits go through [`Settings::edit`], so the normal
    /// auto-save loop writes them).
    pub fn sources_msg(&mut self, m: SourcesMsg) {
        match m {
            SourcesMsg::KindDefault { kind, on } => {
                // Never create a kind from here.
                if kind_default(self.kinds_doc(), &kind) == KindDefault::Undefined {
                    return;
                }
                self.sources.prompt = None;
                if self.edit(KINDS_PATH, &kind_toggle_edit(&kind, on)).is_err() {
                    return;
                }
                let n = self
                    .file(SOURCES_PATH)
                    .and_then(ConfigFile::doc)
                    .map_or(0, |d| pending_overrides(d, self.kinds_doc(), &kind).len());
                self.sources.prompt = (n > 0).then_some(kind);
            }
            SourcesMsg::Apply(kind) => {
                self.sources.prompt = None;
                let idx = self
                    .file(SOURCES_PATH)
                    .and_then(ConfigFile::doc)
                    .map(|d| pending_overrides(d, self.kinds_doc(), &kind))
                    .unwrap_or_default();
                for i in idx {
                    // Removing a key never shifts a record index.
                    let _ = self.edit(SOURCES_PATH, &DocEdit::Remove(record(i).key("works")));
                }
            }
            SourcesMsg::Keep => self.sources.prompt = None,
            SourcesMsg::ApplyOffer => self.apply_offer(),
            SourcesMsg::DismissFromNote => self.sources.from_note = None,
        }
    }

    /// For WP-7: open the Sources section with the record named `name` expanded. A record that
    /// is not in the file yet is appended first with `new_fields` (its `name` is added when
    /// missing). Returns the record's index, or the reason the file cannot take it. Only state
    /// changes here; the caller dispatches `SettingsMsg::Section` for the app-level section
    /// switch effects (scroll restore).
    pub fn open_publication(
        &mut self,
        name: &str,
        mut new_fields: Vec<(String, ScalarValue)>,
    ) -> Result<usize, String> {
        let Some(doc) = self.file(SOURCES_PATH).and_then(ConfigFile::doc) else {
            return Err("sources.toml is not loaded".to_owned());
        };
        let list = Path::root().key("publication");
        let len = doc.len(&list);
        let found = (0..len).find(|&i| doc.get_str(&record(i).key("name")) == Some(name));
        self.section = Section::Sources;
        if let Some(i) = found {
            let ip = record(i);
            if !self.form.is_expanded(SOURCES_PATH, &ip) {
                self.form.toggle(SOURCES_PATH, &ip);
            }
            return Ok(i);
        }
        if !new_fields.iter().any(|(k, _)| k == "name") {
            new_fields.insert(0, ("name".to_owned(), ScalarValue::Str(name.to_owned())));
        }
        self.edit(SOURCES_PATH, &DocEdit::InsertRecord(list, len, new_fields))?;
        Ok(len)
    }
}

fn msg(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

/// The Sources page.
pub fn view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    let info = Section::Sources.info();
    match page_shape(f) {
        PageShape::Generic => generic(cx, f, info, info.intro),
        PageShape::Legacy => column![
            text(LEGACY_BANNER)
                .size(13)
                .color(cx.t.warn_text.to_color()),
            generic(cx, f, info, info.intro),
        ]
        .spacing(10)
        .into(),
        PageShape::Overview => overview_view(cx, f),
    }
}

fn badge<'a>(label: &'static str, t: theme::Tokens) -> Element<'a, Message> {
    text(label).size(11).color(t.text_faint.to_color()).into()
}

fn overview_view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    let t = cx.t;
    let (Some(doc), Some(schema)) = (f.doc(), f.schema()) else {
        return Space::new().into();
    };
    let kinds = cx.s.kinds_doc();
    let editable = f.editable();
    let mut col = column![file_header(f, t)].spacing(8);
    if let Some(fnote) = &cx.s.sources.from_note {
        col = col.push(from_note_view(fnote, editable, t));
    }
    for g in overview(doc, kinds) {
        col = col.push(group_title(cx, &g, doc, kinds));
        for m in &g.members {
            let ip = record(m.index);
            let expanded = cx.s.form.is_expanded(SOURCES_PATH, &ip);
            let chev = if expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            let toggle_msg = msg(SettingsMsg::Form(
                SOURCES_PATH.to_owned(),
                FormMsg::Toggle(ip.clone()),
            ));
            let mut line = row![
                theme::icon_button(chev, Some(toggle_msg), t),
                theme::control_label(m.name.clone(), 14.0),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            for b in &m.badges {
                line = line.push(badge(b, t));
            }
            line = line.push(Space::new().width(iced::Length::Fill));
            line = line.push(works_toggle(
                m.works,
                editable
                    .then(|| record_toggle_edit(doc, kinds, m.index))
                    .flatten()
                    .map(|e| msg(SettingsMsg::Edit(SOURCES_PATH.to_owned(), e))),
                t,
            ));
            line = line.push(theme::icon_button(
                Icon::X,
                editable.then(|| {
                    msg(SettingsMsg::Edit(
                        SOURCES_PATH.to_owned(),
                        DocEdit::Remove(ip.clone()),
                    ))
                }),
                t,
            ));
            col = col.push(line);
            if expanded {
                let rows = form::item_rows(
                    &Ctx {
                        file: SOURCES_PATH,
                        doc,
                        errors: f.errors(),
                        form: &cx.s.form,
                    },
                    schema,
                    &ip,
                );
                col = col.push(widgets::render(rows, rcx(cx, f)));
            }
        }
    }
    let len = doc.len(&Path::root().key("publication"));
    col = col.push(text_button(
        "Add publication",
        editable.then(|| {
            msg(SettingsMsg::Edit(
                SOURCES_PATH.to_owned(),
                DocEdit::InsertRecord(Path::root().key("publication"), len, Vec::new()),
            ))
        }),
        t,
    ));
    col.push(save_bar(f, t)).into()
}

/// The "Create Source from note" notes, and the offer of the matched record's missing values with
/// its explicit Add control (plan sources-publication-model WP-7).
fn from_note_view<'a>(
    f: &super::sources_propose::FromNote,
    editable: bool,
    t: theme::Tokens,
) -> Element<'a, Message> {
    let mut col = column![].spacing(4);
    for n in &f.notes {
        col = col.push(text(n.clone()).size(12).color(t.text_faint.to_color()));
    }
    let mut line = row![].spacing(8).align_y(Alignment::Center);
    if let Some((name, m)) = &f.offer {
        line = line
            .push(
                text(format!(
                    "Add to {name}: {}",
                    super::sources_propose::offer_summary(m)
                ))
                .size(12),
            )
            .push(text_button(
                "Add",
                editable.then(|| msg(SettingsMsg::Sources(SourcesMsg::ApplyOffer))),
                t,
            ));
    }
    line = line.push(text_button(
        "Dismiss",
        Some(msg(SettingsMsg::Sources(SourcesMsg::DismissFromNote))),
        t,
    ));
    col.push(line).into()
}

/// The In Works toggle; a non-boolean value shows as invalid, never as off.
fn works_toggle<'a>(w: Flag, on_toggle: Option<Message>, t: theme::Tokens) -> Element<'a, Message> {
    match w {
        Flag::Invalid => text("works: not a boolean")
            .size(12)
            .color(t.warn_text.to_color())
            .into(),
        Flag::On | Flag::Off => {
            let tg = toggler(w == Flag::On).on_toggle_maybe(on_toggle.map(|m| move |_| m.clone()));
            row![text("In Works").size(12).color(t.text_faint.to_color()), tg]
                .spacing(6)
                .align_y(Alignment::Center)
                .into()
        }
    }
}

fn group_title<'a>(
    cx: PageCx<'a>,
    g: &Group,
    doc: &ConfigDoc,
    kinds: Option<&ConfigDoc>,
) -> Element<'a, Message> {
    let t = cx.t;
    let mut line = row![
        text(g.title().to_owned())
            .size(15)
            .color(t.heading.to_color()),
        text(format!("{}", g.members.len()))
            .size(12)
            .color(t.text_faint.to_color()),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let Some(kind) = g.kind.clone() else {
        return line.into();
    };
    line = line.push(Space::new().width(iced::Length::Fill));
    match g.default.unwrap_or(KindDefault::Undefined) {
        KindDefault::Undefined => {
            line = line.push(works_toggle(Flag::Off, None, t));
            line = line.push(
                text("not defined; add it in Kinds to set a default")
                    .size(12)
                    .color(t.text_faint.to_color()),
            );
        }
        KindDefault::Unset | KindDefault::Value(Flag::Off) => {
            line = line.push(default_toggle(&kind, false, cx));
        }
        KindDefault::Value(Flag::On) => line = line.push(default_toggle(&kind, true, cx)),
        KindDefault::Value(Flag::Invalid) => {
            line = line.push(works_toggle(Flag::Invalid, None, t));
        }
    }
    if cx.s.sources.prompt.as_deref() == Some(kind.as_str()) {
        let n = pending_overrides(doc, kinds, &kind).len();
        if n > 0 {
            line = line
                .push(text(format!("Apply to {n} {kind} publications")).size(12))
                .push(text_button(
                    "Apply",
                    Some(msg(SettingsMsg::Sources(SourcesMsg::Apply(kind.clone())))),
                    t,
                ))
                .push(text_button(
                    "Keep",
                    Some(msg(SettingsMsg::Sources(SourcesMsg::Keep))),
                    t,
                ));
        }
    }
    line.into()
}

fn default_toggle<'a>(kind: &str, on: bool, cx: PageCx<'a>) -> Element<'a, Message> {
    let kind = kind.to_owned();
    let editable = cx.s.file(KINDS_PATH).is_some_and(ConfigFile::editable);
    let tg = toggler(on).on_toggle_maybe(editable.then_some(move |v| {
        msg(SettingsMsg::Sources(SourcesMsg::KindDefault {
            kind: kind.clone(),
            on: v,
        }))
    }));

    row![
        text("default In Works")
            .size(12)
            .color(cx.t.text_faint.to_color()),
        tg
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}
