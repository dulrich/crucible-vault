//! The client command registry: one [`CmdSpec`] table — id literal, title,
//! group, default chords, fixed, message — is the single source of [`ClientCmd`]'s id / title /
//! keys / message, of [`ClientCmd::all`], of `bindables()` and of [`keymap`]. Adding a command
//! touches the enum and one table row; a parameterised family (tabs, periods, surrounds,
//! panels) is generated once by [`family`]. Ids are literals (never derived from titles or
//! labels): they are persisted in `editor.toml` `[keys]` / `palette.hidden`.

use std::sync::LazyLock;

use iced::widget::pane_grid::Pane;

use crate::message::{
    ExplorerMsg, FindMsg, Message, PaletteMsg, PanelsMsg, PanesMsg, ReadingMsg, SourceMsg,
    SwitcherMsg, TabsMsg, ThemeMsg,
};
use crate::panels::PanelKind;
use crate::theme::Surround;

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
    /// Plan round-6-feedback-todos WP-7: open (or focus) the Log tab (no default chord).
    OpenLog,
    /// plan editor-ui-round-3 WP-3: delete the active note (confirm dialog with its orphans).
    DeleteNote,
    /// Plan usability-sprint-captures-links WP-6: move the active note to a picked folder.
    MoveNote,
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

/// What a command sends (`focus` = the focused pane, for "close pane").
type Run = Box<dyn Fn(Pane) -> Message + Send + Sync>;

/// One client command's registry row.
pub struct CmdSpec {
    pub cmd: ClientCmd,
    /// The stable id (`editor.toml` `palette.hidden` / `[keys]`).
    pub id: String,
    pub title: String,
    pub group: &'static str,
    /// The editor default chords.
    pub keys: Vec<String>,
    /// Save / Undo / Redo: their chords are `text_editor` editing chords (not rebindable).
    pub fixed: bool,
    /// Listed in the palette ([`ClientCmd::all`]); the palette opener is bindable only.
    pub listed: bool,
    run: Run,
}

/// A plain command row (group "Editor").
fn cmd(cmd: ClientCmd, id: &str, title: &str, keys: &[&str], run: fn(Pane) -> Message) -> CmdSpec {
    CmdSpec {
        cmd,
        id: id.to_owned(),
        title: title.to_owned(),
        group: "Editor",
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        fixed: false,
        listed: true,
        run: Box::new(run),
    }
}

/// A fixed (text-editing) command row.
fn fixed(c: ClientCmd, id: &str, title: &str, key: &str, run: fn(Pane) -> Message) -> CmdSpec {
    CmdSpec {
        fixed: true,
        ..cmd(c, id, title, &[key], run)
    }
}

/// A parameterised family: one row per `(member, id suffix, title)`, id `<prefix>.<suffix>`.
fn family<T: Copy + Send + Sync + 'static>(
    members: Vec<(T, String, String)>,
    prefix: &str,
    group: &'static str,
    mk: fn(T) -> ClientCmd,
    keys: fn(T) -> Vec<String>,
    run: fn(T) -> Message,
) -> impl Iterator<Item = CmdSpec> {
    let prefix = prefix.to_owned();
    members.into_iter().map(move |(m, suffix, title)| CmdSpec {
        cmd: mk(m),
        id: format!("{prefix}.{suffix}"),
        title,
        group,
        keys: keys(m),
        fixed: false,
        listed: true,
        run: Box::new(move |_| run(m)),
    })
}

/// Literal `(member, id suffix, title)` rows.
fn lit<T>(rows: &[(T, &str, &str)]) -> Vec<(T, String, String)>
where
    T: Copy,
{
    rows.iter()
        .map(|&(m, s, t)| (m, s.to_owned(), t.to_owned()))
        .collect()
}

/// The registry, in palette order (the unlisted palette opener last).
static TABLE: LazyLock<Vec<CmdSpec>> = LazyLock::new(|| {
    use ClientCmd as C;
    let mut v = vec![
        cmd(
            C::CycleReading,
            "cycle_reading",
            "Toggle source / reading view",
            &["Ctrl+E"],
            |_| Message::Reading(ReadingMsg::CycleMode),
        ),
        cmd(
            C::Surround,
            "surround",
            "Cycle surround (theme)",
            &[],
            |_| Message::Theme(ThemeMsg::Cycle),
        ),
        cmd(
            C::SplitRight,
            "split_right",
            "Split pane right",
            &[],
            |_| Message::Panes(PanesMsg::SplitRight),
        ),
        cmd(C::SplitDown, "split_down", "Split pane down", &[], |_| {
            Message::Panes(PanesMsg::SplitDown)
        }),
        cmd(C::ClosePane, "close_pane", "Close pane", &[], |f| {
            Message::Panes(PanesMsg::Close(f))
        }),
        cmd(
            C::QuickSwitcher,
            "quick_switcher",
            "Quick switcher",
            &["Ctrl+P"],
            |_| Message::Switcher(SwitcherMsg::Open),
        ),
        fixed(C::Save, "save", "Save note", "Ctrl+S", |_| {
            Message::Source(SourceMsg::Save)
        }),
        fixed(C::Undo, "undo", "Undo", "Ctrl+Z", |_| {
            Message::Source(SourceMsg::Undo)
        }),
        fixed(C::Redo, "redo", "Redo", "Ctrl+Shift+Z", |_| {
            Message::Source(SourceMsg::Redo)
        }),
        cmd(
            C::TogglePanels,
            "toggle_panels",
            "Toggle side panels",
            &[],
            |_| Message::Panels(PanelsMsg::Toggle),
        ),
        cmd(
            C::TogglePaneBorders,
            "toggle_pane_borders",
            "Toggle pane borders (debug)",
            &[],
            |_| Message::Panes(PanesMsg::ToggleBorders),
        ),
        cmd(C::Quit, "quit", "Quit", &["Ctrl+Q"], |_| {
            Message::Tabs(TabsMsg::Quit)
        }),
        cmd(C::CloseTab, "close_tab", "Close tab", &["Ctrl+W"], |_| {
            Message::Tabs(TabsMsg::CloseActive)
        }),
    ];
    let tabs = (1..=8u8).map(|n| (n, n.to_string(), format!("Go to tab {n}")));
    v.extend(family(
        tabs.collect(),
        "go_to_tab",
        "Editor",
        C::GoToTab,
        |n| vec![format!("Alt+{n}")],
        |n| Message::Tabs(TabsMsg::GoTo(n)),
    ));
    let periods = [
        (PeriodSel::Day, "day", "Open today"),
        (PeriodSel::Week, "week", "Open this week"),
        (PeriodSel::Month, "month", "Open this month"),
    ];
    v.extend(family(
        lit(&periods),
        "open_period",
        "Periods",
        C::OpenPeriod,
        |p| {
            DEFAULT_ACTIONS
                .iter()
                .filter(|(_, d)| d.action == PERIOD_OPEN && d.period == Some(p))
                .map(|(k, _)| (*k).to_owned())
                .collect()
        },
        |p| {
            Message::Palette(PaletteMsg::RunPreset(Preset {
                action: PERIOD_OPEN,
                period: Some(p),
            }))
        },
    ));
    let surrounds = [
        (Surround::Dark, "dark", "Set surround: Dark"),
        (Surround::Med, "med", "Set surround: Med"),
        (Surround::Light, "light", "Set surround: Light"),
    ];
    v.extend(family(
        lit(&surrounds),
        "set_surround",
        "Editor",
        C::SetSurround,
        |_| Vec::new(),
        |s| Message::Theme(ThemeMsg::Set(s)),
    ));
    v.extend([
        cmd(
            C::FindInNote,
            "find_in_note",
            "Find in note",
            &["Ctrl+F"],
            |_| Message::Find(FindMsg::Open),
        ),
        cmd(
            C::FocusSearch,
            "focus_search",
            "Search the vault",
            &["Ctrl+Shift+F"],
            |_| Message::Panels(PanelsMsg::Search),
        ),
        cmd(
            C::OpenSettings,
            "open_settings",
            "Open settings",
            &["Ctrl+,"],
            |_| Message::Settings(crate::settings::SettingsMsg::Open),
        ),
        cmd(C::OpenLog, "open_log", "Open log", &[], |_| {
            Message::Log(crate::log::LogMsg::Open)
        }),
        cmd(C::DeleteNote, "delete_note", "Delete note…", &[], |_| {
            Message::Explorer(ExplorerMsg::DeleteActive)
        }),
        cmd(C::MoveNote, "move_note", "Move note…", &[], |_| {
            Message::Switcher(SwitcherMsg::OpenMove)
        }),
        cmd(
            C::GoToTop,
            "go_to_top",
            "Go to top",
            &["Ctrl+PageUp"],
            |_| Message::Reading(ReadingMsg::GoToEdge { bottom: false }),
        ),
        cmd(
            C::GoToBottom,
            "go_to_bottom",
            "Go to bottom",
            &["Ctrl+PageDown"],
            |_| Message::Reading(ReadingMsg::GoToEdge { bottom: true }),
        ),
    ]);
    let panels = [
        (PanelKind::Outline, "outline", "Show Outline panel"),
        (PanelKind::Links, "links", "Show Links panel"),
        (PanelKind::Backlinks, "backlinks", "Show Backlinks panel"),
        (PanelKind::Tags, "tags", "Show Tags panel"),
        (PanelKind::Properties, "properties", "Show Properties panel"),
        (PanelKind::Search, "search", "Show Search panel"),
    ];
    v.extend(family(
        lit(&panels),
        "panel",
        "Panels",
        C::Panel,
        |_| Vec::new(),
        |k| Message::Panels(PanelsMsg::Show(k)),
    ));
    v.push(CmdSpec {
        listed: false,
        ..cmd(
            C::CommandPaletteKey,
            "command_palette",
            "Command palette",
            &["Ctrl+Shift+P"],
            |_| Message::Palette(PaletteMsg::Open),
        )
    });
    v
});

/// Every registry row, in palette order (the unlisted palette opener last).
pub fn table() -> &'static [CmdSpec] {
    &TABLE
}

/// The full default keymap: (chord, what it runs), for display and the no-duplicate check.
pub fn keymap() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = table()
        .iter()
        .flat_map(|s| s.keys.iter().map(|k| (k.clone(), s.title.clone())))
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
    /// This command's registry row.
    pub fn spec(self) -> &'static CmdSpec {
        table()
            .iter()
            .find(|s| s.cmd == self)
            .expect("every ClientCmd has a registry row")
    }

    /// Every listed client command, in palette order.
    pub fn all() -> Vec<ClientCmd> {
        table().iter().filter(|s| s.listed).map(|s| s.cmd).collect()
    }

    /// Title and group.
    pub fn title(self) -> (String, &'static str) {
        let s = self.spec();
        (s.title.clone(), s.group)
    }

    /// The stable command id (`editor.toml` `palette.hidden`; WP-4 `[keys]`).
    pub fn id(self) -> String {
        self.spec().id.clone()
    }

    /// The editor's default chords.
    pub fn keys(self) -> Vec<String> {
        self.spec().keys.clone()
    }

    /// The message it sends (`focus` = the focused pane for "close pane").
    pub fn message(self, focus: Pane) -> Message {
        (self.spec().run)(focus)
    }
}
