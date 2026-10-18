//! Editor prefs: `_custom/crucible-vault/editor.toml` (plan `editor-settings-tab`, Decisions
//! locked: prefs and the keymap live in the vault; machine-local window/tab session state stays
//! in `UiState`). Schema: `cv_schema::editor_schema()`.
//!
//! Parsing is field-wise and forgiving: a missing, mistyped or out-of-range value falls back to
//! its default (the form-level `validate` errors say why), so a half-valid file still applies
//! what it can. A file that does not parse as TOML yields [`Prefs::default`] and is never
//! overwritten until the user saves.
//!
//! `[keys]` (command id → chords; `[]` unbinds) is parsed and held in [`Prefs::keys`]; WP-4
//! applies it in the binding resolver.

use std::collections::BTreeMap;

use cv_schema::{ConfigDoc, DocError, Path};
use iced::widget::{Space, button, column, container, row, text, text_input};
use iced::{Element, Length};

use super::{ConfigFile, LoadState, SettingsMsg, file_header, save_bar};
use crate::daemon::DeleteMode;
pub use crate::header::QuickSwitchOpen;
use crate::header::TabTitle;
use crate::message::Message;
use crate::theme::{Surround, Tokens};

/// The vault file.
pub const EDITOR_PATH: &str = "_custom/crucible-vault/editor.toml";

/// Default base text size (px) — `theme::BASE_TEXT_SIZE`.
pub const DEFAULT_FONT_SIZE: f32 = crate::theme::BASE_TEXT_SIZE;
/// `font_size` range (schema).
pub const FONT_SIZE_RANGE: (f32, f32) = (8.0, 64.0);
/// `ui_scale` range (schema; iced `scale_factor`).
pub const UI_SCALE_RANGE: (f32, f32) = (0.5, 3.0);
/// Default search result cap (was `panels::SEARCH_LIMIT`).
pub const DEFAULT_SEARCH_LIMIT: usize = 50;
/// Default search debounce (was `panels::SEARCH_DEBOUNCE`).
pub const DEFAULT_SEARCH_DEBOUNCE_MS: u64 = 250;

/// The effective editor prefs.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub font_size: f32,
    pub ui_scale: f32,
    pub surround: Surround,
    /// `tab_title`: what tab headers show (plan editor-feedback-round-2 WP-6).
    pub tab_title: TabTitle,
    /// `quick_switch_open` (plan editor-feedback-round-2 WP-7).
    pub quick_switch_open: QuickSwitchOpen,
    pub search_limit: usize,
    pub search_debounce_ms: u64,
    /// `delete_mode` (plan editor-ui-round-3 WP-3; replaces `confirm_trash`, now ignored).
    pub delete_mode: DeleteMode,
    /// `missing_link` (plan round-6-feedback-todos WP-4): a click on an unresolved vault link.
    pub missing_link: MissingLink,
    /// `focus_follows_mouse` (plan editor-round-4 WP-4): hovering a pane focuses it.
    pub focus_follows_mouse: bool,
    /// Command/action ids omitted from the palette list (their chords still work).
    pub palette_hidden: Vec<String>,
    /// `[keys]`: command id → chords (WP-4 consumes it).
    pub keys: BTreeMap<String, Vec<String>>,
    /// `open_url_cmd`: program + args that open an external URL (plan
    /// usability-sprint-captures-links WP-3); the URL is appended last. Absent, mistyped or
    /// empty → [`default_open_url_cmd`].
    pub open_url_cmd: Vec<String>,
}

/// `missing_link`: what a click on a link to a missing note does (plan round-6-feedback-todos WP-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingLink {
    /// Raise the `unresolved link: …` notice.
    #[default]
    Warn,
    /// Create the note and open it.
    Create,
}

impl MissingLink {
    /// The `editor.toml` value.
    pub fn key(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Create => "create",
        }
    }
}

/// The default `open_url_cmd`: `["xdg-open"]`.
pub fn default_open_url_cmd() -> Vec<String> {
    vec!["xdg-open".to_owned()]
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            ui_scale: 1.0,
            surround: Surround::default(),
            tab_title: TabTitle::default(),
            quick_switch_open: QuickSwitchOpen::default(),
            search_limit: DEFAULT_SEARCH_LIMIT,
            search_debounce_ms: DEFAULT_SEARCH_DEBOUNCE_MS,
            delete_mode: DeleteMode::Trash,
            missing_link: MissingLink::Warn,
            focus_follows_mouse: false,
            palette_hidden: Vec::new(),
            keys: BTreeMap::new(),
            open_url_cmd: default_open_url_cmd(),
        }
    }
}

impl Prefs {
    /// The Reading text scale: a paragraph renders at `font_size` (plan editor-ui-round-3 WP-6).
    pub fn text_scale(&self) -> f32 {
        crate::reading::view::reading_scale(self.font_size)
    }

    /// The search debounce.
    pub fn search_debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.search_debounce_ms)
    }
}

fn p(s: &str) -> Path {
    Path::parse(s).unwrap_or_default()
}

fn in_range(v: f64, (lo, hi): (f32, f32)) -> Option<f32> {
    #[allow(clippy::cast_possible_truncation)]
    let f = v as f32;
    (f.is_finite() && f >= lo && f <= hi).then_some(f)
}

fn surround_of(s: &str) -> Option<Surround> {
    Surround::ALL
        .into_iter()
        .find(|x| x.label().eq_ignore_ascii_case(s))
}

/// The surround's `editor.toml` spelling.
pub fn surround_key(s: Surround) -> &'static str {
    match s {
        Surround::Dark => "dark",
        Surround::Med => "med",
        Surround::Light => "light",
    }
}

/// Prefs from a parsed document; `fallback_surround` (the old `UiState.surround`) applies when
/// the file has no `surround`.
pub fn from_doc(doc: &ConfigDoc, fallback_surround: Surround) -> Prefs {
    let d = Prefs::default();
    let int_in = |k: &str, lo: i64, hi: i64| doc.get_int(&p(k)).filter(|v| (lo..=hi).contains(v));
    Prefs {
        font_size: doc
            .get_float(&p("font_size"))
            .and_then(|v| in_range(v, FONT_SIZE_RANGE))
            .unwrap_or(d.font_size),
        ui_scale: doc
            .get_float(&p("ui_scale"))
            .and_then(|v| in_range(v, UI_SCALE_RANGE))
            .unwrap_or(d.ui_scale),
        surround: doc
            .get_str(&p("surround"))
            .and_then(surround_of)
            .unwrap_or(fallback_surround),
        tab_title: doc
            .get_str(&p("tab_title"))
            .and_then(TabTitle::of)
            .unwrap_or(d.tab_title),
        quick_switch_open: doc
            .get_str(&p("quick_switch_open"))
            .and_then(QuickSwitchOpen::of)
            .unwrap_or(d.quick_switch_open),
        search_limit: int_in("search.limit", 1, 1000)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(d.search_limit),
        search_debounce_ms: int_in("search.debounce_ms", 0, 5000)
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(d.search_debounce_ms),
        delete_mode: match doc.get_str(&p("delete_mode")) {
            Some("permanent") => DeleteMode::Permanent,
            Some("trash") => DeleteMode::Trash,
            _ => d.delete_mode,
        },
        missing_link: match doc.get_str(&p("missing_link")) {
            Some("create") => MissingLink::Create,
            Some("warn") => MissingLink::Warn,
            _ => d.missing_link,
        },
        focus_follows_mouse: doc
            .get_bool(&p("focus_follows_mouse"))
            .unwrap_or(d.focus_follows_mouse),
        palette_hidden: doc.get_str_list(&p("palette.hidden")).unwrap_or_default(),
        keys: doc
            .keys(&p("keys"))
            .into_iter()
            .filter_map(|k| {
                let chords = doc.get_str_list(&Path::root().key("keys").key(&k))?;
                Some((k, chords))
            })
            .collect(),
        open_url_cmd: doc
            .get_str_list(&p("open_url_cmd"))
            .filter(|v| v.first().is_some_and(|c| !c.trim().is_empty()))
            .unwrap_or(d.open_url_cmd),
    }
}

/// Parses an `editor.toml` body (the drift test's entry point; the app goes through
/// [`ConfigFile`] + [`from_doc`]).
pub fn parse(text: &str, fallback_surround: Surround) -> Result<Prefs, String> {
    ConfigDoc::parse(text)
        .map(|d| from_doc(&d, fallback_surround))
        .map_err(|e| format!("editor.toml: {e}"))
}

/// The effective prefs of the loaded file (defaults when malformed / not loaded).
pub fn effective(f: &ConfigFile, fallback_surround: Surround) -> Prefs {
    match &f.doc {
        Some(doc) => from_doc(doc, fallback_surround),
        None => Prefs {
            surround: fallback_surround,
            ..Prefs::default()
        },
    }
}

/// The one-time surround migration: a loaded (or absent) `editor.toml` with no `surround`
/// takes the old `UiState.surround` into its doc, in memory only (persisted with the next
/// Save; no write here). Returns whether it seeded.
pub fn seed_surround(f: &mut ConfigFile, s: Surround) -> bool {
    if !matches!(f.state, LoadState::Loaded | LoadState::Absent) {
        return false;
    }
    let Some(doc) = &mut f.doc else {
        return false;
    };
    if doc.get(&p("surround")).is_some() {
        return false;
    }
    doc.set(&p("surround"), surround_key(s)).is_ok()
}

/// An Editor-page control edit.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefEdit {
    FontSize(f32),
    UiScale(f32),
    Surround(Surround),
    TabTitle(TabTitle),
    QuickSwitchOpen(QuickSwitchOpen),
    SearchLimit(i64),
    SearchDebounceMs(i64),
    DeleteMode(DeleteMode),
    MissingLink(MissingLink),
    FocusFollowsMouse(bool),
    /// The comma-separated hidden-id list as typed.
    PaletteHidden(String),
}

/// Applies `e` to the document.
pub fn apply(doc: &mut ConfigDoc, e: &PrefEdit) -> Result<(), DocError> {
    let round = |v: f32| f64::from((v * 100.0).round() / 100.0);
    match e {
        PrefEdit::FontSize(v) => doc.set(&p("font_size"), round(*v)),
        PrefEdit::UiScale(v) => doc.set(&p("ui_scale"), round(*v)),
        PrefEdit::Surround(s) => doc.set(&p("surround"), surround_key(*s)),
        PrefEdit::TabTitle(m) => doc.set(&p("tab_title"), m.key()),
        PrefEdit::QuickSwitchOpen(m) => doc.set(&p("quick_switch_open"), m.key()),
        PrefEdit::SearchLimit(v) => doc.set(&p("search.limit"), *v),
        PrefEdit::SearchDebounceMs(v) => doc.set(&p("search.debounce_ms"), *v),
        PrefEdit::DeleteMode(m) => doc.set(
            &p("delete_mode"),
            match m {
                DeleteMode::Trash => "trash",
                DeleteMode::Permanent => "permanent",
            },
        ),
        PrefEdit::MissingLink(m) => doc.set(&p("missing_link"), m.key()),
        PrefEdit::FocusFollowsMouse(b) => doc.set(&p("focus_follows_mouse"), *b),
        PrefEdit::PaletteHidden(s) => {
            let mut a = cv_schema::toml_edit::Array::new();
            for id in s.split(',').map(str::trim).filter(|x| !x.is_empty()) {
                a.push(id);
            }
            doc.set(&p("palette.hidden"), a)
        }
    }
}

fn stepper<'a>(
    label: &'a str,
    value: String,
    dec: Option<PrefEdit>,
    inc: Option<PrefEdit>,
    t: Tokens,
) -> Element<'a, Message> {
    use crate::icons::Icon;
    use crate::theme::icon_button;
    let m = |e: Option<PrefEdit>| e.map(|e| Message::Settings(SettingsMsg::Pref(e)));
    row![
        text(label).size(14).width(Length::Fixed(200.0)),
        icon_button(Icon::ChevronDown, m(dec), t),
        container(text(value).size(14))
            .width(Length::Fixed(80.0))
            .center_x(Length::Fixed(80.0)),
        icon_button(Icon::ChevronUp, m(inc), t),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center)
    .into()
}

fn step_f(v: f32, d: f32, (lo, hi): (f32, f32)) -> Option<f32> {
    let n = ((v + d) * 100.0).round() / 100.0;
    (n >= lo - 1e-4 && n <= hi + 1e-4 && (n - v).abs() > 1e-6).then_some(n.clamp(lo, hi))
}

fn step_i(v: i64, d: i64, lo: i64, hi: i64) -> Option<i64> {
    let n = (v + d).clamp(lo, hi);
    (n != v).then_some(n)
}

/// The Editor page: typed controls over `editor.toml` (WP-3 builds the generic renderer).
pub fn view<'a>(
    f: &'a ConfigFile,
    prefs: &'a Prefs,
    note: Option<&'a str>,
    hidden_draft: Option<&'a str>,
    t: Tokens,
    mode: cv_protocol::Mode,
) -> Element<'a, Message> {
    let editable = f.doc.is_some();
    let on = |e: PrefEdit| editable.then_some(e);
    let fs = prefs.font_size;
    let us = prefs.ui_scale;
    let lim = i64::try_from(prefs.search_limit).unwrap_or(50);
    let deb = i64::try_from(prefs.search_debounce_ms).unwrap_or(250);
    let surround = {
        let seg = |s: Surround| {
            let active = prefs.surround == s;
            button(crate::theme::control_label(s.label(), 14.0))
                .height(crate::theme::SEGMENT)
                .padding([0, 12])
                .style(crate::theme::segment_style(t, active))
                .on_press_maybe(
                    on(PrefEdit::Surround(s)).map(|e| Message::Settings(SettingsMsg::Pref(e))),
                )
        };
        row![
            text("Surround").size(14).width(Length::Fixed(200.0)),
            container(row(Surround::ALL.into_iter().map(|s| seg(s).into())).spacing(0))
                .padding(1)
                .style(crate::theme::seg_group_style(t)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
    };
    let tab_title = {
        let seg = |m: TabTitle| {
            button(crate::theme::control_label(m.label(), 14.0))
                .height(crate::theme::SEGMENT)
                .padding([0, 12])
                .style(crate::theme::segment_style(t, prefs.tab_title == m))
                .on_press_maybe(
                    on(PrefEdit::TabTitle(m)).map(|e| Message::Settings(SettingsMsg::Pref(e))),
                )
        };
        row![
            text("Tab title").size(14).width(Length::Fixed(200.0)),
            container(row(TabTitle::ALL.into_iter().map(|m| seg(m).into())).spacing(0))
                .padding(1)
                .style(crate::theme::seg_group_style(t)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
    };
    let quick_switch = {
        let seg = |m: QuickSwitchOpen| {
            button(crate::theme::control_label(m.label(), 14.0))
                .height(crate::theme::SEGMENT)
                .padding([0, 12])
                .style(crate::theme::segment_style(t, prefs.quick_switch_open == m))
                .on_press_maybe(
                    on(PrefEdit::QuickSwitchOpen(m))
                        .map(|e| Message::Settings(SettingsMsg::Pref(e))),
                )
        };
        row![
            text("Quick Switch opens")
                .size(14)
                .width(Length::Fixed(200.0)),
            container(row(QuickSwitchOpen::ALL.into_iter().map(|m| seg(m).into())).spacing(0))
                .padding(1)
                .style(crate::theme::seg_group_style(t)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
    };
    let confirm = {
        let seg = |m: DeleteMode, label: &'static str| {
            button(crate::theme::control_label(label, 14.0))
                .height(crate::theme::SEGMENT)
                .padding([0, 12])
                .style(crate::theme::segment_style(t, prefs.delete_mode == m))
                .on_press_maybe(
                    on(PrefEdit::DeleteMode(m)).map(|e| Message::Settings(SettingsMsg::Pref(e))),
                )
        };
        row![
            text("Delete moves to").size(14).width(Length::Fixed(200.0)),
            container(
                row![
                    seg(DeleteMode::Trash, ".trash/"),
                    seg(DeleteMode::Permanent, "Permanent delete"),
                ]
                .spacing(0)
            )
            .padding(1)
            .style(crate::theme::seg_group_style(t)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
    };
    let missing = {
        let seg = |m: MissingLink, label: &'static str| {
            button(crate::theme::control_label(label, 14.0))
                .height(crate::theme::SEGMENT)
                .padding([0, 12])
                .style(crate::theme::segment_style(t, prefs.missing_link == m))
                .on_press_maybe(
                    on(PrefEdit::MissingLink(m)).map(|e| Message::Settings(SettingsMsg::Pref(e))),
                )
        };
        column![
            row![
                text("Missing-note links")
                    .size(14)
                    .width(Length::Fixed(200.0)),
                container(
                    row![
                        seg(MissingLink::Warn, "Warn"),
                        seg(MissingLink::Create, "Create note"),
                    ]
                    .spacing(0)
                )
                .padding(1)
                .style(crate::theme::seg_group_style(t)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            text("A click on a link to a missing note: warn, or create it (New-note folder, or the link's folder path) and open it")
                .size(13)
                .color(t.text_faint.to_color()),
        ]
        .spacing(2)
    };
    let follow = {
        let seg = |v: bool, label: &'static str| {
            button(crate::theme::control_label(label, 14.0))
                .height(crate::theme::SEGMENT)
                .padding([0, 12])
                .style(crate::theme::segment_style(
                    t,
                    prefs.focus_follows_mouse == v,
                ))
                .on_press_maybe(
                    on(PrefEdit::FocusFollowsMouse(v))
                        .map(|e| Message::Settings(SettingsMsg::Pref(e))),
                )
        };
        row![
            text("Focus follows mouse")
                .size(14)
                .width(Length::Fixed(200.0)),
            container(row![seg(false, "Off"), seg(true, "On")].spacing(0))
                .padding(1)
                .style(crate::theme::seg_group_style(t)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
    };
    let joined;
    let hidden_text = match hidden_draft {
        Some(d) => d,
        None => {
            joined = prefs.palette_hidden.join(", ");
            joined.as_str()
        }
    };
    let mut hidden_in = text_input("command ids, comma-separated", hidden_text)
        .size(14)
        .width(Length::Fixed(420.0));
    if editable {
        hidden_in = hidden_in
            .on_input(|s| Message::Settings(SettingsMsg::Pref(PrefEdit::PaletteHidden(s))));
    }
    let hidden = row![
        text("Hidden palette commands")
            .size(14)
            .width(Length::Fixed(200.0)),
        hidden_in,
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    let keys_line = if prefs.keys.is_empty() {
        "no key overrides".to_owned()
    } else {
        format!(
            "{} key override(s) (edited on the Hotkeys page)",
            prefs.keys.len()
        )
    };
    let note_el: Element<'a, Message> = match note {
        Some(n) => text(n).size(13).color(t.text_faint.to_color()).into(),
        None => Space::new().into(),
    };
    column![
        file_header(f, t),
        note_el,
        stepper(
            "Font size",
            format!("{fs:.0}"),
            step_f(fs, -1.0, FONT_SIZE_RANGE).and_then(|v| on(PrefEdit::FontSize(v))),
            step_f(fs, 1.0, FONT_SIZE_RANGE).and_then(|v| on(PrefEdit::FontSize(v))),
            t,
        ),
        stepper(
            "UI scale",
            format!("{us:.2}"),
            step_f(us, -0.25, UI_SCALE_RANGE).and_then(|v| on(PrefEdit::UiScale(v))),
            step_f(us, 0.25, UI_SCALE_RANGE).and_then(|v| on(PrefEdit::UiScale(v))),
            t,
        ),
        surround,
        tab_title,
        quick_switch,
        stepper(
            "Search result limit",
            lim.to_string(),
            step_i(lim, -10, 1, 1000).and_then(|v| on(PrefEdit::SearchLimit(v))),
            step_i(lim, 10, 1, 1000).and_then(|v| on(PrefEdit::SearchLimit(v))),
            t,
        ),
        stepper(
            "Search debounce (ms)",
            deb.to_string(),
            step_i(deb, -50, 0, 5000).and_then(|v| on(PrefEdit::SearchDebounceMs(v))),
            step_i(deb, 50, 0, 5000).and_then(|v| on(PrefEdit::SearchDebounceMs(v))),
            t,
        ),
        confirm,
        missing,
        follow,
        hidden,
        text(keys_line).size(13).color(t.text_faint.to_color()),
        save_bar(f, t, mode),
    ]
    .spacing(10)
    .into()
}
