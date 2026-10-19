//! Editor prefs: `_custom/crucible-vault/editor.toml` (plan `editor-settings-tab`, Decisions
//! locked: prefs and the keymap live in the vault; machine-local window/tab session state stays
//! in `UiState`). Schema: `cv_schema::editor_schema()`; the Settings page is the generic schema
//! form (plan tn-review-baseline-remediation WP-R20), and this module is the typed reader.
//!
//! Parsing is field-wise and forgiving: a missing, mistyped or out-of-range value falls back to
//! its default (the form-level `validate` errors say why), so a half-valid file still applies
//! what it can. A file that does not parse as TOML yields [`Prefs::default`] and is never
//! overwritten until the user saves.
//!
//! `[keys]` (command id → chords; `[]` unbinds) is parsed and held in [`Prefs::keys`]; WP-4
//! applies it in the binding resolver.

use std::collections::BTreeMap;

use cv_schema::{ConfigDoc, Node, Path};

use super::{ConfigFile, LoadState};
use crate::daemon::DeleteMode;
pub use crate::header::QuickSwitchOpen;
use crate::header::TabTitle;
use crate::theme::Surround;

/// The vault file.
pub const EDITOR_PATH: &str = cv_schema::config_path!("editor.toml");

/// Default base text size (px) — `theme::BASE_TEXT_SIZE`.
pub const DEFAULT_FONT_SIZE: f32 = crate::theme::BASE_TEXT_SIZE;
/// The inclusive bounds of the numeric `editor.toml` field at dotted `path`, read from the
/// schema (`editor_schema()`; the one source of the ranges — WP-R20). Unbounded → the f64
/// range.
pub fn schema_bounds(path: &str) -> (f64, f64) {
    let mut node = cv_schema::editor_schema();
    for k in path.split('.') {
        let Node::Record(r) = node else { break };
        match r.field(k) {
            Some(f) => node = &f.node,
            None => break,
        }
    }
    node.bounds().unwrap_or((f64::MIN, f64::MAX))
}

/// `ui_scale` range (schema; iced `scale_factor`).
pub fn ui_scale_range() -> (f32, f32) {
    let (lo, hi) = schema_bounds("ui_scale");
    #[allow(clippy::cast_possible_truncation)]
    (lo as f32, hi as f32)
}
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
    /// `scroll_anchor` (plan round-8-intake-views-anchor WP-4): the viewport height, in percent
    /// (0 = top), whose block/line a Reading ↔ Source switch carries.
    pub scroll_anchor: u8,
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
            scroll_anchor: 0,
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

    /// `scroll_anchor` as a fraction of the viewport height (0.0–1.0).
    pub fn anchor(&self) -> f32 {
        f32::from(self.scroll_anchor.min(100)) / 100.0
    }

    /// The search debounce.
    pub fn search_debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.search_debounce_ms)
    }
}

fn p(s: &str) -> Path {
    Path::parse(s).unwrap_or_default()
}

fn in_range(v: f64, key: &str) -> Option<f32> {
    let (lo, hi) = schema_bounds(key);
    #[allow(clippy::cast_possible_truncation)]
    let (f, lo, hi) = (v as f32, lo as f32, hi as f32);
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
    #[allow(clippy::cast_precision_loss)]
    let int_in = |k: &str| {
        let (lo, hi) = schema_bounds(k);
        doc.get_int(&p(k))
            .filter(|v| (lo..=hi).contains(&(*v as f64)))
    };
    Prefs {
        font_size: doc
            .get_float(&p("font_size"))
            .and_then(|v| in_range(v, "font_size"))
            .unwrap_or(d.font_size),
        ui_scale: doc
            .get_float(&p("ui_scale"))
            .and_then(|v| in_range(v, "ui_scale"))
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
        search_limit: int_in("search.limit")
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(d.search_limit),
        search_debounce_ms: int_in("search.debounce_ms")
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
        scroll_anchor: int_in("scroll_anchor")
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(d.scroll_anchor),
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
