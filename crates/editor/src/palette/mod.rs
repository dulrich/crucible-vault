//! Command palette (WP-5, Ctrl+P): daemon actions merged with client commands, typed parameter
//! prompts, action invocation with context (save-first), key chords and the job toast.
//!
//! Entries: [`ClientCmd`]s first, then `actions_list` (fetched off the UI thread on first open or
//! first chord, cached until invalidated — config reload or a change under
//! [`CONFIG_PREFIX`]). Ranking is `editor_model::rank` over `"title group"`. Invocation of an
//! action: prompt each `ParamSpec` ([`prompt::Prompt`]), then — if the active buffer is dirty and
//! the daemon is live — save it and dispatch `run_action` only on a successful save with the new
//! hash (the app's `run_call`/`after_save_first`).

pub mod chord;
pub mod hints;
pub mod prompt;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cv_protocol::{ActionContext, ActionInfo, JobState, JobStatus, ParamKind};
use editor_model::rank::rank;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{button, column, pick_list, row, text, text_editor, text_input, toggler};
use iced::{Element, Length, Subscription};
use serde_json::{Map, Value};

use crate::message::{
    FindMsg, Message, PaletteMsg, PanelsMsg, PanesMsg, ReadingMsg, SourceMsg, SwitcherMsg, TabsMsg,
    ThemeMsg,
};
use crate::panels::PanelKind;
use crate::source::SourceBuffer;
use crate::theme::{Surround, Tokens};
use chord::Chord;
use prompt::{ParamValue, Prompt};

/// The query / prompt field's widget id.
pub const INPUT_ID: &str = "palette-input";
/// Result rows shown.
pub const SHOWN: usize = 14;
/// A terminal job's toast stays this long.
pub const TOAST_TTL: Duration = Duration::from_secs(5);
/// Changes under this prefix invalidate the cached action list (definitions are hot-reloaded).
pub const CONFIG_PREFIX: &str = "_custom/crucible-vault/";
/// Path suggestions shown under a Path prompt.
pub const SUGGESTIONS: usize = 8;

/// Param-prompt inputs.
#[derive(Debug, Clone)]
pub enum PromptMsg {
    Text(String),
    Bool(bool),
    Choice(String),
    Editor(text_editor::Action),
    /// A path suggestion was picked.
    Suggest(String),
    Back,
}

/// A client-side command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientCmd {
    CycleReading,
    Surround,
    SplitRight,
    SplitDown,
    ClosePane,
    QuickSwitcher,
    Save,
    Undo,
    Redo,
    TogglePanels,
    /// editor-usability-round WP-3: debug pane borders.
    TogglePaneBorders,
    Panel(PanelKind),
    /// editor-usability-round WP-4 commands.
    Quit,
    CloseTab,
    /// Go to tab N (1-based) of the focused pane.
    GoToTab(u8),
    /// Open today / this week / this month (`period.open`; listed only when it is loaded).
    OpenPeriod(PeriodSel),
    SetSurround(Surround),
    FindInNote,
    FocusSearch,
    /// Opens the command palette (Ctrl+Shift+P); not listed in the palette itself.
    CommandPaletteKey,
    /// plan editor-settings-tab WP-2: open (or focus) the Settings tab (Ctrl+,).
    OpenSettings,
    /// plan editor-ui-round-3 WP-3: delete the active note (confirm dialog with its orphans).
    DeleteNote,
    /// plan editor-ui-round-3 WP-5: Go to top / bottom of the note (Ctrl+PgUp / Ctrl+PgDn).
    GoToTop,
    GoToBottom,
}

/// A `period.open` period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodSel {
    Day,
    Week,
    Month,
}

impl PeriodSel {
    /// The `period` param value.
    pub fn as_str(self) -> &'static str {
        match self {
            PeriodSel::Day => "day",
            PeriodSel::Week => "week",
            PeriodSel::Month => "month",
        }
    }
}

/// A daemon action with fixed params, run by a client default chord or palette entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub action: &'static str,
    /// `period.open`'s `period` (with `date` = today).
    pub period: Option<PeriodSel>,
}

/// The `period.open` action id.
pub const PERIOD_OPEN: &str = "period.open";

/// Client default chords bound to daemon actions (Decisions hotkey table). A preset whose action
/// is absent from the loaded config does nothing.
pub const DEFAULT_ACTIONS: [(&str, Preset); 2] = [
    (
        "Ctrl+O",
        Preset {
            action: PERIOD_OPEN,
            period: Some(PeriodSel::Day),
        },
    ),
    (
        "Ctrl+R",
        Preset {
            action: "lint-note",
            period: None,
        },
    ),
];

/// The preset a chord runs, if it is a default action chord.
pub fn default_action(c: &Chord) -> Option<Preset> {
    DEFAULT_ACTIONS
        .iter()
        .find(|(k, _)| chord::parse(k).as_ref() == Some(c))
        .map(|&(_, p)| p)
}

/// The client command a chord runs through the palette's chord path (`on_chord`). Ctrl+E/S/Z
/// keep their own listeners and `text_editor` bindings, so only the WP-4 chords route here.
pub fn client_chord(c: &Chord) -> Option<ClientCmd> {
    let simple = [
        ("Ctrl+P", ClientCmd::QuickSwitcher),
        ("Ctrl+Shift+P", ClientCmd::CommandPaletteKey),
        ("Ctrl+W", ClientCmd::CloseTab),
        ("Ctrl+Q", ClientCmd::Quit),
        ("Ctrl+F", ClientCmd::FindInNote),
        ("Ctrl+Shift+F", ClientCmd::FocusSearch),
        ("Ctrl+,", ClientCmd::OpenSettings),
        ("Ctrl+PageUp", ClientCmd::GoToTop),
        ("Ctrl+PageDown", ClientCmd::GoToBottom),
    ];
    if let Some((_, cmd)) = simple
        .iter()
        .find(|(k, _)| chord::parse(k).as_ref() == Some(c))
    {
        return Some(*cmd);
    }
    if c.alt && !c.command && !c.shift {
        let n: u8 = c.key.parse().ok()?;
        return (1..=8).contains(&n).then_some(ClientCmd::GoToTab(n));
    }
    None
}

/// Whether the focused source editor must hand a chord to the app (`PaletteMsg::Key`) instead
/// of handling it itself (Alt+digit would insert the digit): the WP-4 client chords, the default
/// action chords, Ctrl+, (Open settings) and the inert reserved Ctrl+Shift+,.
pub fn routed(c: &Chord) -> bool {
    client_chord(c).is_some() || default_action(c).is_some() || (c.command && c.key == ",")
}

/// The full default keymap: (chord, what it runs), for display and the no-duplicate check.
pub fn keymap() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = ClientCmd::all()
        .into_iter()
        .chain([ClientCmd::CommandPaletteKey])
        .flat_map(|c| {
            let t = c.title().0;
            c.keys().into_iter().map(move |k| (k, t.clone()))
        })
        .collect();
    v.extend(
        DEFAULT_ACTIONS
            .iter()
            .filter(|(_, p)| p.period.is_none())
            .map(|(k, p)| ((*k).to_owned(), p.action.to_owned())),
    );
    v.push(("Ctrl+Alt+Left".into(), "Back".into()));
    v.push(("Ctrl+Alt+Right".into(), "Forward".into()));
    v
}

impl ClientCmd {
    /// Every client command, in palette order.
    pub fn all() -> Vec<ClientCmd> {
        let mut v = vec![
            ClientCmd::CycleReading,
            ClientCmd::Surround,
            ClientCmd::SplitRight,
            ClientCmd::SplitDown,
            ClientCmd::ClosePane,
            ClientCmd::QuickSwitcher,
            ClientCmd::Save,
            ClientCmd::Undo,
            ClientCmd::Redo,
            ClientCmd::TogglePanels,
            ClientCmd::TogglePaneBorders,
            ClientCmd::Quit,
            ClientCmd::CloseTab,
        ];
        v.extend((1..=8).map(ClientCmd::GoToTab));
        v.extend(
            [PeriodSel::Day, PeriodSel::Week, PeriodSel::Month]
                .into_iter()
                .map(ClientCmd::OpenPeriod),
        );
        v.extend(Surround::ALL.into_iter().map(ClientCmd::SetSurround));
        v.extend([
            ClientCmd::FindInNote,
            ClientCmd::FocusSearch,
            ClientCmd::OpenSettings,
            ClientCmd::DeleteNote,
            ClientCmd::GoToTop,
            ClientCmd::GoToBottom,
        ]);
        v.extend(PanelKind::ALL.iter().map(|&k| ClientCmd::Panel(k)));
        v
    }

    /// Title and group.
    pub fn title(self) -> (String, &'static str) {
        let t = match self {
            ClientCmd::CycleReading => "Toggle source / reading view",
            ClientCmd::Surround => "Cycle surround (theme)",
            ClientCmd::SplitRight => "Split pane right",
            ClientCmd::SplitDown => "Split pane down",
            ClientCmd::ClosePane => "Close pane",
            ClientCmd::QuickSwitcher => "Quick switcher",
            ClientCmd::Save => "Save note",
            ClientCmd::Undo => "Undo",
            ClientCmd::Redo => "Redo",
            ClientCmd::TogglePanels => "Toggle side panels",
            ClientCmd::TogglePaneBorders => "Toggle pane borders (debug)",
            ClientCmd::Panel(k) => return (format!("Show {} panel", k.label()), "Panels"),
            ClientCmd::Quit => "Quit",
            ClientCmd::CloseTab => "Close tab",
            ClientCmd::GoToTab(n) => return (format!("Go to tab {n}"), "Editor"),
            ClientCmd::OpenPeriod(PeriodSel::Day) => return ("Open today".into(), "Periods"),
            ClientCmd::OpenPeriod(PeriodSel::Week) => return ("Open this week".into(), "Periods"),
            ClientCmd::OpenPeriod(PeriodSel::Month) => {
                return ("Open this month".into(), "Periods");
            }
            ClientCmd::SetSurround(s) => return (format!("Set surround: {}", s.label()), "Editor"),
            ClientCmd::FindInNote => "Find in note",
            ClientCmd::FocusSearch => "Search the vault",
            ClientCmd::CommandPaletteKey => "Command palette",
            ClientCmd::OpenSettings => "Open settings",
            ClientCmd::DeleteNote => "Delete note…",
            ClientCmd::GoToTop => "Go to top",
            ClientCmd::GoToBottom => "Go to bottom",
        };
        (t.to_owned(), "Editor")
    }

    /// The stable command id (`editor.toml` `palette.hidden`; WP-4 `[keys]`).
    pub fn id(self) -> String {
        let s = match self {
            ClientCmd::CycleReading => "cycle_reading",
            ClientCmd::Surround => "surround",
            ClientCmd::SplitRight => "split_right",
            ClientCmd::SplitDown => "split_down",
            ClientCmd::ClosePane => "close_pane",
            ClientCmd::QuickSwitcher => "quick_switcher",
            ClientCmd::Save => "save",
            ClientCmd::Undo => "undo",
            ClientCmd::Redo => "redo",
            ClientCmd::TogglePanels => "toggle_panels",
            ClientCmd::TogglePaneBorders => "toggle_pane_borders",
            ClientCmd::Panel(k) => return format!("panel.{}", k.label().to_lowercase()),
            ClientCmd::Quit => "quit",
            ClientCmd::CloseTab => "close_tab",
            ClientCmd::GoToTab(n) => return format!("go_to_tab.{n}"),
            ClientCmd::OpenPeriod(p) => return format!("open_period.{}", p.as_str()),
            ClientCmd::SetSurround(s) => {
                return format!("set_surround.{}", s.label().to_lowercase());
            }
            ClientCmd::FindInNote => "find_in_note",
            ClientCmd::FocusSearch => "focus_search",
            ClientCmd::CommandPaletteKey => "command_palette",
            ClientCmd::OpenSettings => "open_settings",
            ClientCmd::DeleteNote => "delete_note",
            ClientCmd::GoToTop => "go_to_top",
            ClientCmd::GoToBottom => "go_to_bottom",
        };
        s.to_owned()
    }

    /// The editor's default chord, if any.
    pub fn keys(self) -> Vec<String> {
        let k = match self {
            ClientCmd::CycleReading => "Ctrl+E",
            ClientCmd::QuickSwitcher => "Ctrl+P",
            ClientCmd::CommandPaletteKey => "Ctrl+Shift+P",
            ClientCmd::Save => "Ctrl+S",
            ClientCmd::Undo => "Ctrl+Z",
            ClientCmd::Redo => "Ctrl+Shift+Z",
            ClientCmd::Quit => "Ctrl+Q",
            ClientCmd::CloseTab => "Ctrl+W",
            ClientCmd::FindInNote => "Ctrl+F",
            ClientCmd::FocusSearch => "Ctrl+Shift+F",
            ClientCmd::OpenSettings => "Ctrl+,",
            ClientCmd::GoToTop => "Ctrl+PageUp",
            ClientCmd::GoToBottom => "Ctrl+PageDown",
            ClientCmd::GoToTab(n) => return vec![format!("Alt+{n}")],
            ClientCmd::OpenPeriod(p) => {
                return DEFAULT_ACTIONS
                    .iter()
                    .filter(|(_, d)| d.action == PERIOD_OPEN && d.period == Some(p))
                    .map(|(k, _)| (*k).to_owned())
                    .collect();
            }
            _ => return Vec::new(),
        };
        vec![k.to_owned()]
    }

    /// The message it sends (`focus` = the focused pane for "close pane").
    pub fn message(self, focus: iced::widget::pane_grid::Pane) -> Message {
        match self {
            ClientCmd::CycleReading => Message::Reading(ReadingMsg::CycleMode),
            ClientCmd::Surround => Message::Theme(ThemeMsg::Cycle),
            ClientCmd::SplitRight => Message::Panes(PanesMsg::SplitRight),
            ClientCmd::SplitDown => Message::Panes(PanesMsg::SplitDown),
            ClientCmd::ClosePane => Message::Panes(PanesMsg::Close(focus)),
            ClientCmd::QuickSwitcher => Message::Switcher(SwitcherMsg::Open),
            ClientCmd::Save => Message::Source(SourceMsg::Save),
            ClientCmd::Undo => Message::Source(SourceMsg::Undo),
            ClientCmd::Redo => Message::Source(SourceMsg::Redo),
            ClientCmd::TogglePanels => Message::Panels(PanelsMsg::Toggle),
            ClientCmd::TogglePaneBorders => Message::Panes(PanesMsg::ToggleBorders),
            ClientCmd::Panel(k) => Message::Panels(PanelsMsg::Show(k)),
            ClientCmd::Quit => Message::Tabs(TabsMsg::Quit),
            ClientCmd::CloseTab => Message::Tabs(TabsMsg::CloseActive),
            ClientCmd::GoToTab(n) => Message::Tabs(TabsMsg::GoTo(n)),
            ClientCmd::OpenPeriod(p) => Message::Palette(PaletteMsg::RunPreset(Preset {
                action: PERIOD_OPEN,
                period: Some(p),
            })),
            ClientCmd::SetSurround(s) => Message::Theme(ThemeMsg::Set(s)),
            ClientCmd::FindInNote => Message::Find(FindMsg::Open),
            ClientCmd::FocusSearch => Message::Panels(PanelsMsg::Show(PanelKind::Search)),
            ClientCmd::CommandPaletteKey => Message::Palette(PaletteMsg::Open),
            ClientCmd::OpenSettings => Message::Settings(crate::settings::SettingsMsg::Open),
            ClientCmd::DeleteNote => Message::Explorer(crate::message::ExplorerMsg::DeleteActive),
            ClientCmd::GoToTop => Message::Reading(ReadingMsg::GoToEdge { bottom: false }),
            ClientCmd::GoToBottom => Message::Reading(ReadingMsg::GoToEdge { bottom: true }),
        }
    }
}

/// What an entry runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryId {
    Client(ClientCmd),
    Action(String),
    /// Plan editor-ui-round-3 WP-9: "Open bookmark: <name>" (the bookmark's path).
    Bookmark(String),
}

/// The stable bindable id of the bookmark at `path` (WP-9): keyed by the stored path, so it
/// survives reorders and renames of other bookmarks.
pub fn bookmark_id(path: &str) -> String {
    format!("bookmark:{path}")
}

/// The palette rows of `bookmarks` (WP-9), with their stored chords.
pub fn bookmark_entries(bookmarks: &[crate::bookmarks::Bookmark]) -> Vec<Entry> {
    bookmarks
        .iter()
        .map(|b| Entry {
            id: EntryId::Bookmark(b.path.clone()),
            title: format!("Open bookmark: {}", b.name),
            group: "Bookmarks".into(),
            keys: b.keys.clone(),
        })
        .collect()
}

/// One palette row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub title: String,
    pub group: String,
    pub keys: Vec<String>,
}

/// An action ready to run (params collected).
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub action_id: String,
    pub title: String,
    pub params: Map<String, Value>,
    pub needs: Vec<String>,
}

/// The job toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub state: JobState,
    /// When the job reached a terminal state (expiry clock).
    pub ended: Option<Instant>,
}

impl Toast {
    /// The status-bar text.
    pub fn line(&self) -> String {
        let s = serde_json::to_value(self.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        format!("{}: {s}", self.title)
    }
}

/// Whether a job state is final.
pub fn terminal(s: JobState) -> bool {
    matches!(
        s,
        JobState::Done | JobState::Failed | JobState::Cancelled | JobState::Deferred
    )
}

/// Client commands then actions.
pub fn build_entries(actions: &[ActionInfo]) -> Vec<Entry> {
    let has = |id: &str| actions.iter().any(|a| a.id == id);
    let mut v: Vec<Entry> = ClientCmd::all()
        .into_iter()
        .filter(|c| !matches!(c, ClientCmd::OpenPeriod(_)) || has(PERIOD_OPEN))
        .map(|c| {
            let (title, group) = c.title();
            Entry {
                id: EntryId::Client(c),
                title,
                group: group.to_owned(),
                keys: c.keys(),
            }
        })
        .collect();
    v.extend(actions.iter().map(|a| {
        Entry {
            id: EntryId::Action(a.id.clone()),
            title: a.title.clone(),
            group: a.group.clone().unwrap_or_else(|| "Actions".into()),
            keys: DEFAULT_ACTIONS
                .iter()
                .filter(|(_, p)| p.period.is_none() && p.action == a.id)
                .map(|(k, _)| (*k).to_owned())
                .chain(a.keys.iter().cloned())
                .collect(),
        }
    }));
    v
}

/// `entries` without the ids in `hidden` (client command ids or action ids).
pub fn visible_entries(entries: Vec<Entry>, hidden: &[String]) -> Vec<Entry> {
    if hidden.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .filter(|e| {
            let id = match &e.id {
                EntryId::Client(c) => c.id(),
                EntryId::Action(a) => a.clone(),
                EntryId::Bookmark(p) => bookmark_id(p),
            };
            !hidden.contains(&id)
        })
        .collect()
}

/// Ranks `entries` against `query` (fuzzy over `"title group"`), best first, as indices.
pub fn rank_entries(query: &str, entries: &[Entry]) -> Vec<usize> {
    let hays = haystacks(entries);
    let by_hay: HashMap<String, usize> = hays
        .iter()
        .enumerate()
        .map(|(i, h)| (h.clone(), i))
        .collect();
    if query.trim().is_empty() {
        return (0..entries.len()).collect();
    }
    rank(query, &hays, &[], |_| false, entries.len())
        .into_iter()
        .filter_map(|r| by_hay.get(&r.path).copied())
        .collect()
}

/// The ranking haystacks of `entries` (parallel): `rank` scores path-like strings; '/' would
/// split a "basename", so it is neutralized, and a per-entry suffix keeps haystacks unique.
pub(crate) fn haystacks(entries: &[Entry]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut h = format!("{} {}", e.title, e.group).replace('/', " ");
            if seen.contains(&h) {
                h.push_str(&" ".repeat(i + 1));
            }
            seen.insert(h.clone());
            h
        })
        .collect()
}

/// What a bound chord runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    Client(ClientCmd),
    /// A default action preset (`period.open` / `lint-note`): fetches the action list first.
    Preset(Preset),
    /// A config action (`actions.toml` `keys`).
    Action(String),
    /// WP-9: open the bookmark at this path (`bookmarks.toml` `keys`).
    Bookmark(String),
}

/// Where a Hotkeys row's chords come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The editor default.
    Default,
    /// An `editor.toml` `[keys]` override.
    EditorToml,
    /// The action's `keys` in `actions.toml`.
    ActionsToml,
    /// WP-9: the bookmark's `keys` in `bookmarks.toml`.
    BookmarksToml,
}

impl KeySource {
    /// The Hotkeys page label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::EditorToml => "editor.toml",
            Self::ActionsToml => "actions.toml",
            Self::BookmarksToml => "bookmarks.toml",
        }
    }
}

/// One editor-owned bindable command: a client command or a default-action preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindable {
    /// The `[keys]` id ([`ClientCmd::id`] / [`Preset::id`]).
    pub id: String,
    pub title: String,
    pub run: Bound,
    /// The editor default chords.
    pub defaults: Vec<String>,
    /// Save / Undo / Redo: their chords are `text_editor` editing chords (not rebindable).
    pub fixed: bool,
}

/// Every editor-owned bindable command, in palette order (client commands, then the
/// non-period default-action presets; the period presets are the `open_period.*` commands).
pub fn bindables() -> Vec<Bindable> {
    let mut v: Vec<Bindable> = ClientCmd::all()
        .into_iter()
        .chain([ClientCmd::CommandPaletteKey])
        .map(|c| Bindable {
            id: c.id(),
            title: c.title().0,
            run: match c {
                ClientCmd::OpenPeriod(p) => Bound::Preset(Preset {
                    action: PERIOD_OPEN,
                    period: Some(p),
                }),
                _ => Bound::Client(c),
            },
            defaults: c.keys(),
            fixed: matches!(c, ClientCmd::Save | ClientCmd::Undo | ClientCmd::Redo),
        })
        .collect();
    v.extend(
        DEFAULT_ACTIONS
            .iter()
            .filter(|(_, p)| p.period.is_none())
            .map(|&(k, p)| Bindable {
                id: p.id(),
                title: format!("Run {} (default chord)", p.action),
                run: Bound::Preset(p),
                defaults: vec![k.to_owned()],
                fixed: false,
            }),
    );
    v
}

impl Preset {
    /// The stable `[keys]` id: a period preset is the matching `open_period.<p>` client
    /// command; any other is `preset.<action>`.
    pub fn id(self) -> String {
        match self.period {
            Some(p) => ClientCmd::OpenPeriod(p).id(),
            None => format!("preset.{}", self.action),
        }
    }
}

/// The effective keymap (plan `editor-feedback-round-2` WP-1 "Hotkey conflict policy", which
/// replaced `editor-settings-tab`'s "override > editor default > config action keys"): every
/// bindable claims its chords (an `editor.toml` `[keys]` override replaces the command's editor
/// default; `[]` releases it), and a chord claimed by two or more distinct bindables is bound
/// to none of them. Every key handler (`App::on_chord`, the focused source editor's
/// `key_binding`) decides from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    map: HashMap<Chord, Bound>,
    /// Every malformed / reserved / conflicting chord, as one line each.
    pub notices: Vec<String>,
    /// id → the chords it effectively holds (canonical spelling), for display.
    pub chords: HashMap<String, Vec<String>>,
    /// id → its notices (Hotkeys inline conflicts).
    pub row_notices: HashMap<String, Vec<String>>,
    /// The ids an override applies to.
    pub overridden: std::collections::BTreeSet<String>,
    /// Chord → every bindable id claiming it (registered or conflicted), in claim order.
    pub claims: HashMap<Chord, Vec<String>>,
    /// The ids holding at least one conflicted (unregistered) chord.
    pub conflicted: std::collections::BTreeSet<String>,
}

impl Default for Keymap {
    fn default() -> Self {
        resolve(&std::collections::BTreeMap::new(), &[])
    }
}

impl Keymap {
    /// What `c` runs.
    pub fn get(&self, c: &Chord) -> Option<&Bound> {
        self.map.get(c)
    }

    /// Whether the focused source editor must hand `c` to the app (`PaletteMsg::Key`) instead
    /// of handling it (Alt+digit would insert the digit): chords bound to a client command or
    /// preset, Ctrl+, and the inert Ctrl+Shift+,.
    pub fn routed(&self, c: &Chord) -> bool {
        matches!(
            self.map.get(c),
            Some(Bound::Client(_) | Bound::Preset(_) | Bound::Bookmark(_))
        ) || (c.command && c.key == ",")
    }

    /// Chord → config action id (the old `bindings` map).
    pub fn actions(&self) -> HashMap<Chord, String> {
        self.map
            .iter()
            .filter_map(|(c, b)| match b {
                Bound::Action(id) => Some((c.clone(), id.clone())),
                _ => None,
            })
            .collect()
    }

    fn note(&mut self, id: &str, n: String) {
        self.row_notices
            .entry(id.to_owned())
            .or_default()
            .push(n.clone());
        self.notices.push(n);
    }

    /// Records `id`'s claim on `c` (a chord listed twice by one bindable is collapsed).
    fn claim(&mut self, claims: &mut Vec<(Chord, String, Bound)>, id: &str, c: Chord, b: Bound) {
        if claims.iter().any(|(x, i, _)| *x == c && i == id) {
            return;
        }
        self.claims
            .entry(c.clone())
            .or_default()
            .push(id.to_owned());
        claims.push((c, id.to_owned(), b));
    }
}

/// Resolves the effective keymap from `[keys]` `overrides` (id → chords; `[]` unbinds), the
/// editor defaults and the config actions' `keys`. Malformed and reserved
/// ([`chord::text_editing`]) chords become notices; the same chord listed twice by one
/// bindable is collapsed; a chord claimed by two or more distinct bindables is registered for
/// none of them, every claimant is marked [`Keymap::conflicted`] and gets a notice naming the
/// chord.
pub fn resolve(
    overrides: &std::collections::BTreeMap<String, Vec<String>>,
    actions: &[ActionInfo],
) -> Keymap {
    resolve_all(overrides, actions, &[])
}

/// [`resolve`] with the bookmarks' chords as claimants too (plan editor-ui-round-3 WP-9): the
/// same conflict policy covers them against every other claimant.
pub fn resolve_all(
    overrides: &std::collections::BTreeMap<String, Vec<String>>,
    actions: &[ActionInfo],
    bookmarks: &[crate::bookmarks::Bookmark],
) -> Keymap {
    let mut km = Keymap {
        map: HashMap::new(),
        notices: Vec::new(),
        chords: HashMap::new(),
        row_notices: HashMap::new(),
        overridden: std::collections::BTreeSet::new(),
        claims: HashMap::new(),
        conflicted: std::collections::BTreeSet::new(),
    };
    let rows = bindables();
    for id in overrides.keys() {
        if !rows.iter().any(|r| &r.id == id) {
            km.note(id, format!("{id}: unknown command id (override ignored)"));
        }
    }
    let mut claims: Vec<(Chord, String, Bound)> = Vec::new();
    // Editor commands and presets: an override replaces the default.
    for r in &rows {
        km.chords.entry(r.id.clone()).or_default();
        let over = overrides.get(&r.id);
        if over.is_some() && r.fixed {
            km.note(
                &r.id,
                format!("{}: a text-editing command (override ignored)", r.id),
            );
        }
        if let Some(list) = over.filter(|_| !r.fixed) {
            km.overridden.insert(r.id.clone());
            for k in list {
                let Some(c) = chord::parse(k) else {
                    km.note(&r.id, format!("{}: unreadable key \"{k}\"", r.id));
                    continue;
                };
                if chord::text_editing(&c) {
                    km.note(&r.id, format!("{}: {c} is reserved (not bound)", r.id));
                } else if !c.command && !c.alt {
                    km.note(
                        &r.id,
                        format!("{}: {c} needs Ctrl or Alt (not bound)", r.id),
                    );
                } else {
                    km.claim(&mut claims, &r.id, c, r.run.clone());
                }
            }
            continue;
        }
        for k in &r.defaults {
            let Some(c) = chord::parse(k) else { continue };
            if r.fixed {
                // Handled by the source editor's own bindings; shown, never routed.
                km.chords
                    .entry(r.id.clone())
                    .or_default()
                    .push(c.to_string());
            } else {
                km.claim(&mut claims, &r.id, c, r.run.clone());
            }
        }
    }
    // Config actions.
    for a in actions {
        km.chords.entry(a.id.clone()).or_default();
        for k in &a.keys {
            let Some(c) = chord::parse(k) else {
                km.note(&a.id, format!("{}: unreadable key \"{k}\"", a.id));
                continue;
            };
            if chord::text_editing(&c) {
                km.note(&a.id, format!("{}: {c} is reserved (not bound)", a.id));
            } else {
                km.claim(&mut claims, &a.id, c, Bound::Action(a.id.clone()));
            }
        }
    }
    // Bookmarks (WP-9): their record's `keys`, checked like an action's.
    for b in bookmarks {
        let id = bookmark_id(&b.path);
        km.chords.entry(id.clone()).or_default();
        for k in &b.keys {
            let Some(c) = chord::parse(k) else {
                km.note(&id, format!("{id}: unreadable key \"{k}\""));
                continue;
            };
            if chord::text_editing(&c) {
                km.note(&id, format!("{id}: {c} is reserved (not bound)"));
            } else {
                km.claim(&mut claims, &id, c, Bound::Bookmark(b.path.clone()));
            }
        }
    }
    // Register the uncontested chords; a contested chord is registered for nobody.
    for (c, id, b) in claims {
        let others: Vec<String> = km
            .claims
            .get(&c)
            .into_iter()
            .flatten()
            .filter(|o| **o != id)
            .cloned()
            .collect();
        if others.is_empty() {
            km.chords.entry(id).or_default().push(c.to_string());
            km.map.insert(c, b);
        } else {
            km.conflicted.insert(id.clone());
            let n = format!("{id}: {c} conflicts with {} (not bound)", others.join(", "));
            km.note(&id, n);
        }
    }
    km
}

/// Chord → action id for every parseable, non-conflicting `ActionInfo.keys`, plus a notice per
/// conflict (a chord claimed by several bindables is bound to none) — [`resolve`]
/// with no overrides.
pub fn bindings(actions: &[ActionInfo]) -> (HashMap<Chord, String>, Vec<String>) {
    let km = resolve(&std::collections::BTreeMap::new(), actions);
    (km.actions(), km.notices)
}

/// `entries` with each entry's chords taken from the effective keymap (an action entry shows
/// its default-action preset chords first, as [`build_entries`] does).
pub fn apply_keys(entries: &mut [Entry], km: &Keymap) {
    for e in entries {
        let get = |id: &str| km.chords.get(id).cloned().unwrap_or_default();
        e.keys = match &e.id {
            EntryId::Client(c) => {
                if km.chords.contains_key(&c.id()) {
                    get(&c.id())
                } else {
                    continue;
                }
            }
            EntryId::Action(a) => {
                let mut k = get(&format!("preset.{a}"));
                k.extend(get(a));
                k
            }
            EntryId::Bookmark(p) => get(&bookmark_id(p)),
        };
    }
}

/// The invocation context for the active tab: path, 1-based cursor line and buffer hash always;
/// the selection when non-empty.
pub fn context(tab: Option<&SourceBuffer>) -> ActionContext {
    let Some(tab) = tab else {
        return ActionContext::default();
    };
    let line = u32::try_from(tab.cursor().position.line + 1).ok();
    ActionContext {
        active_path: Some(tab.path().to_owned()),
        selection: tab.selection().filter(|s| !s.is_empty()),
        value: None,
        line,
        buffer_hash: Some(tab.base_hash().as_str().to_owned()),
    }
}

/// The palette model.
pub struct Palette {
    pub open: bool,
    pub query: String,
    /// Ranked indices into `entries`.
    pub results: Vec<usize>,
    pub selected: usize,
    pub entries: Vec<Entry>,
    /// Cached `actions_list` (`None` = not fetched / invalidated).
    pub actions: Option<Vec<ActionInfo>>,
    pub loading: bool,
    /// The effective keymap (WP-4): rebuilt on action reload and `[keys]` change; shared with
    /// the source editors' `key_binding` closures.
    pub keymap: std::sync::Arc<Keymap>,
    /// The `editor.toml` `[keys]` overrides the keymap was built from.
    pub overrides: std::collections::BTreeMap<String, Vec<String>>,
    /// Key-binding conflicts, shown in the palette.
    pub notice: Vec<String>,
    pub prompt: Option<Prompt>,
    pub prompt_error: Option<String>,
    /// Backing store for a Multiline prompt.
    pub multiline: text_editor::Content,
    /// Path suggestions for a Path prompt.
    pub suggestions: Vec<String>,
    /// A chord pressed before the action list arrived.
    pub pending_chord: Option<Chord>,
    pub toast: Option<Toast>,
    /// job id → title of the action that started it.
    pub job_titles: HashMap<String, String>,
    /// editor-usability-round WP-5 (audit B1/M2): job id → the invoking tab's
    /// `(path, edit_seq)` at dispatch, so a terminal `open_path` can follow the note.
    pub job_origins: HashMap<String, (String, u64)>,
    /// Terminal `open_path`s that arrived before the `run_action` reply (job id → path).
    pub early_open: HashMap<String, String>,
    /// `editor.toml` `palette.hidden`: ids left out of the list (chords still work).
    pub hidden: Vec<String>,
    /// WP-8: the cached minimum-subsequence hint per entry (parallel to `entries`), recomputed
    /// in `rebuild` (entry-set change) only.
    pub hints: Vec<Option<String>>,
    /// WP-9: the bookmarks whose "Open bookmark" commands and chords join the list / keymap.
    pub bookmarks: Vec<crate::bookmarks::Bookmark>,
}

impl Default for Palette {
    fn default() -> Self {
        let entries = build_entries(&[]);
        Self {
            hints: hints::compute(&entries),
            open: false,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            entries,
            actions: None,
            loading: false,
            keymap: std::sync::Arc::new(Keymap::default()),
            overrides: std::collections::BTreeMap::new(),
            notice: Vec::new(),
            prompt: None,
            prompt_error: None,
            multiline: text_editor::Content::new(),
            suggestions: Vec::new(),
            pending_chord: None,
            toast: None,
            job_titles: HashMap::new(),
            job_origins: HashMap::new(),
            early_open: HashMap::new(),
            hidden: Vec::new(),
            bookmarks: Vec::new(),
        }
    }
}

impl Palette {
    /// Opens the list with an empty query.
    pub fn show(&mut self) {
        self.open = true;
        self.prompt = None;
        self.prompt_error = None;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    /// Closes and cancels any prompt.
    pub fn close(&mut self) {
        self.open = false;
        self.prompt = None;
        self.prompt_error = None;
        self.suggestions.clear();
    }

    /// Re-ranks.
    pub fn refresh(&mut self) {
        self.results = rank_entries(&self.query, &self.entries);
        self.selected = self.selected.min(self.results.len().saturating_sub(1));
    }

    /// New query.
    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.selected = 0;
        self.refresh();
    }

    /// The highlighted entry index.
    pub fn current(&self) -> Option<usize> {
        self.results.get(self.selected).copied()
    }

    /// Installs a fetched action list (entries + bindings).
    pub fn set_actions(&mut self, actions: Vec<ActionInfo>) {
        self.actions = Some(actions);
        self.loading = false;
        self.rebuild();
    }

    /// Sets the `[keys]` overrides and rebuilds the keymap (a no-op when unchanged).
    pub fn set_keys(&mut self, overrides: std::collections::BTreeMap<String, Vec<String>>) {
        if overrides == self.overrides {
            return;
        }
        self.overrides = overrides;
        self.rebuild();
    }

    /// Rebuilds keymap, notices and entries from the overrides and the cached actions.
    fn rebuild(&mut self) {
        let actions = self.actions.as_deref().unwrap_or_default();
        let km = resolve_all(&self.overrides, actions, &self.bookmarks);
        self.notice = km.notices.clone();
        let mut all = build_entries(actions);
        all.extend(bookmark_entries(&self.bookmarks));
        apply_keys(&mut all, &km);
        let entries = visible_entries(all, &self.hidden);
        if entries != self.entries || self.hints.len() != entries.len() {
            self.hints = hints::compute(&entries);
        }
        self.entries = entries;
        self.keymap = std::sync::Arc::new(km);
        self.refresh();
    }

    /// Sets the bookmarks (WP-9) and rebuilds keymap + entries (a no-op when unchanged).
    pub fn set_bookmarks(&mut self, bookmarks: Vec<crate::bookmarks::Bookmark>) {
        if bookmarks == self.bookmarks {
            return;
        }
        self.bookmarks = bookmarks;
        self.rebuild();
    }

    /// Sets the hidden ids and rebuilds the list (a no-op when unchanged).
    pub fn set_hidden(&mut self, hidden: Vec<String>) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        self.rebuild();
    }

    /// Drops the cached action list (refetched on next use).
    pub fn invalidate(&mut self) {
        self.actions = None;
    }

    /// An action by id from the cache.
    pub fn action(&self, id: &str) -> Option<&ActionInfo> {
        self.actions.as_ref()?.iter().find(|a| a.id == id)
    }

    /// Starts prompting `info`'s params.
    pub fn begin_prompt(&mut self, info: ActionInfo, today: chrono::NaiveDate) {
        let p = Prompt::new(info, today);
        self.open = true;
        self.prompt_error = None;
        self.sync_step(&p);
        self.prompt = Some(p);
    }

    /// Re-seeds per-step widgets (multiline content) from the prompt's current value.
    pub fn sync_step(&mut self, p: &Prompt) {
        self.multiline = text_editor::Content::with_text(p.text());
        self.suggestions.clear();
    }

    /// Applies a prompt input. `paths` feeds Path suggestions.
    pub fn prompt_input(&mut self, m: PromptMsg, paths: &[String], recent: &[String]) {
        let Some(p) = &mut self.prompt else { return };
        self.prompt_error = None;
        match m {
            PromptMsg::Text(s) | PromptMsg::Choice(s) | PromptMsg::Suggest(s) => {
                let is_path = p.current().is_some_and(|s| s.kind == ParamKind::Path);
                if is_path {
                    self.suggestions =
                        rank(&s, paths, recent, crate::switcher::deranked, SUGGESTIONS)
                            .into_iter()
                            .map(|r| r.path)
                            .collect();
                }
                p.set_text(s);
            }
            PromptMsg::Bool(b) => p.set_bool(b),
            PromptMsg::Editor(a) => {
                self.multiline.perform(a);
                p.set_text(self.multiline.text());
            }
            PromptMsg::Back => {
                p.back();
                let snapshot = p.clone();
                self.sync_step(&snapshot);
            }
        }
    }

    /// A `job_updated`: refreshes the toast (title from the invoking action when known).
    pub fn job_updated(&mut self, j: &JobStatus, now: Instant) {
        let title = self
            .job_titles
            .get(&j.job_id)
            .cloned()
            .unwrap_or_else(|| j.job_id.clone());
        self.toast = Some(Toast {
            title,
            state: j.status,
            ended: terminal(j.status).then_some(now),
        });
    }

    /// Clears a terminal toast older than [`TOAST_TTL`].
    pub fn expire_toast(&mut self, now: Instant) {
        if self
            .toast
            .as_ref()
            .and_then(|t| t.ended)
            .is_some_and(|e| now.duration_since(e) >= TOAST_TTL)
        {
            self.toast = None;
        }
    }
}

/// Global keys: Esc closes/cancels, Up/Down move, Ctrl+Enter submits a multiline
/// prompt, any other Ctrl/Alt chord is offered to the action bindings.
pub fn key_event(ev: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = ev else {
        return None;
    };
    let p = |m| Some(Message::Palette(m));
    match key.as_ref() {
        Key::Named(Named::Escape) => p(PaletteMsg::Escape),
        Key::Named(Named::ArrowUp) => p(PaletteMsg::Up),
        Key::Named(Named::ArrowDown) => p(PaletteMsg::Down),
        Key::Named(Named::Enter) if modifiers.command() => p(PaletteMsg::CtrlEnter),
        // editor-usability-round WP-3: focused-pane back/forward (`chord` has no arrow names,
        // so these never reach action bindings).
        Key::Named(Named::ArrowLeft) if modifiers.command() && modifiers.alt() => {
            Some(Message::Panes(PanesMsg::BackFocused))
        }
        Key::Named(Named::ArrowRight) if modifiers.command() && modifiers.alt() => {
            Some(Message::Panes(PanesMsg::ForwardFocused))
        }
        // Ctrl+P / Ctrl+Shift+P and the other client chords resolve in `on_chord`
        // (`client_chord`).
        _ if modifiers.command() || modifiers.alt() => {
            chord::from_event(&key, modifiers).map(|c| Message::Palette(PaletteMsg::Key(c)))
        }
        _ => None,
    }
}

/// The palette/panels subscription: the key listener, the toast expiry tick while a toast is
/// up, and the panels recompute tick while the column is visible.
pub fn subscription(palette: &Palette, panels_visible: bool) -> Subscription<Message> {
    let mut subs = vec![iced::keyboard::listen().filter_map(key_event)];
    if palette.toast.is_some() {
        subs.push(
            iced::time::every(Duration::from_secs(1)).map(|_| Message::Palette(PaletteMsg::Tick)),
        );
    }
    if panels_visible {
        subs.push(
            iced::time::every(Duration::from_millis(300)).map(|_| Message::Panels(PanelsMsg::Tick)),
        );
    }
    Subscription::batch(subs)
}

fn prompt_view<'a>(p: &'a Palette, pr: &'a Prompt, t: Tokens) -> Element<'a, Message> {
    let pm = |m| Message::Palette(PaletteMsg::Prompt(m));
    let Some(spec) = pr.current() else {
        return text("").into();
    };
    let header = text(format!(
        "{} — {} ({}/{}){}",
        pr.action.title,
        spec.label,
        pr.step + 1,
        pr.action.params.len(),
        if spec.required { " *" } else { "" }
    ))
    .size(14);
    let input: Element<'a, Message> = match spec.kind {
        ParamKind::Bool => {
            let on = matches!(pr.value(), Some(ParamValue::Bool(true)));
            toggler(on)
                .label(spec.label.as_str())
                .on_toggle(move |b| pm(PromptMsg::Bool(b)))
                .into()
        }
        ParamKind::Enum => pick_list(
            spec.choices.as_slice(),
            Some(pr.text().to_owned()).filter(|s| !s.is_empty()),
            move |c: String| pm(PromptMsg::Choice(c)),
        )
        .into(),
        ParamKind::Multiline => text_editor(&p.multiline)
            .height(120)
            .on_action(move |a| pm(PromptMsg::Editor(a)))
            .into(),
        ParamKind::Text | ParamKind::Path | ParamKind::Date => {
            let ph = if spec.kind == ParamKind::Date {
                "YYYY-MM-DD"
            } else {
                ""
            };
            text_input(ph, pr.text())
                .id(INPUT_ID)
                .on_input(move |s| pm(PromptMsg::Text(s)))
                .on_submit(Message::Palette(PaletteMsg::Submit))
                .into()
        }
    };
    let mut col = column![header, input].spacing(4).width(Length::Fill);
    for s in &p.suggestions {
        col = col.push(
            button(text(s.as_str()).size(13))
                .style(button::text)
                .on_press(pm(PromptMsg::Suggest(s.clone()))),
        );
    }
    if let Some(e) = &p.prompt_error {
        col = col.push(text(e.as_str()).size(12).color(t.warn_text.to_color()));
    }
    let hint = if spec.kind == ParamKind::Multiline {
        "Ctrl+Enter: next · Esc: cancel"
    } else {
        "Enter: next · Esc: cancel"
    };
    col.push(
        row![
            button(text("Back").size(13))
                .style(button::text)
                .on_press(pm(PromptMsg::Back)),
            button(text("Next").size(13))
                .style(button::text)
                .on_press(Message::Palette(PaletteMsg::Submit)),
            text(hint).size(12).color(t.text_faint.to_color()),
        ]
        .spacing(8),
    )
    .into()
}

/// The palette overlay (shown above the panes while open).
pub fn view<'a>(p: &'a Palette, t: Tokens) -> Element<'a, Message> {
    if let Some(pr) = &p.prompt {
        return prompt_view(p, pr, t);
    }
    let mut col = column![
        text_input("Run a command…", &p.query)
            .id(INPUT_ID)
            .on_input(|q| Message::Palette(PaletteMsg::Query(q)))
            .on_submit(Message::Palette(PaletteMsg::Submit))
    ]
    .spacing(2)
    .width(Length::Fill);
    if p.loading {
        col = col.push(
            text("loading actions…")
                .size(12)
                .color(t.text_faint.to_color()),
        );
    }
    for (row_i, &i) in p.results.iter().take(SHOWN).enumerate() {
        let Some(e) = p.entries.get(i) else { continue };
        let mut title = text(e.title.as_str()).size(14);
        if row_i == p.selected {
            title = title.color(t.accent_text.to_color());
        }
        let hint = p.hints.get(i).cloned().flatten().unwrap_or_default();
        let hint_color = if !hint.is_empty() && p.query.trim().to_lowercase() == hint {
            t.accent_text
        } else {
            t.text_faint
        };
        col = col.push(
            button(
                row![
                    title.width(Length::Fill),
                    text(hint)
                        .size(12)
                        .font(iced::Font::MONOSPACE)
                        .color(hint_color.to_color()),
                    text(e.group.as_str())
                        .size(12)
                        .color(t.text_faint.to_color()),
                    text(e.keys.join(" ")).size(12).color(t.text_dim.to_color()),
                ]
                .spacing(12),
            )
            .style(button::text)
            .width(Length::Fill)
            .on_press(Message::Palette(PaletteMsg::Pick(i))),
        );
    }
    for n in &p.notice {
        col = col.push(text(n.as_str()).size(12).color(t.warn_text.to_color()));
    }
    col.into()
}
