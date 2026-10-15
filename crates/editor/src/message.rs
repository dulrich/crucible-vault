//! The top-level [`Message`]: one variant per feature, each wrapping that feature's sub-enum, so
//! a later WP adds its variants to its own sub-enum plus one wiring line in `App::update`.
//! Empty sub-enums are slots owned by the WP named on them.

use cv_protocol::Notification;

use iced::widget::pane_grid::{self, Pane};

use crate::theme::Surround;
use crate::widget::virtual_list::ListWindow;

/// Every message the app handles.
#[derive(Debug, Clone)]
pub enum Message {
    Source(SourceMsg),
    Reading(ReadingMsg),
    Explorer(ExplorerMsg),
    Switcher(SwitcherMsg),
    Palette(PaletteMsg),
    Panels(PanelsMsg),
    Views(ViewsMsg),
    Nav(NavMsg),
    Status(StatusMsg),
    Tabs(TabsMsg),
    Theme(ThemeMsg),
    Panes(PanesMsg),
    /// editor-usability-round WP-4: find in note.
    Find(FindMsg),
}

/// WP-2: source editing. `Action`/`Undo`/`Redo`/`Save` target the active tab; the rest name
/// their buffer by vault path (tabs may move while a save or timer is in flight).
#[derive(Debug, Clone)]
pub enum SourceMsg {
    /// An iced `text_editor` action (edits go through the undo model).
    Action(iced::widget::text_editor::Action),
    Undo,
    Redo,
    /// Explicit save (Ctrl+S).
    Save,
    /// The autosave debounce for edit `seq` elapsed.
    AutosaveDue {
        path: String,
        seq: u64,
    },
    /// A save finished.
    Saved {
        path: String,
        seq: u64,
        outcome: crate::source::SaveOutcome,
    },
    /// Conflict bar: re-save against the on-disk hash.
    KeepMine(String),
    /// Conflict bar: reload from disk.
    TakeTheirs(String),
    /// A key the source editor handles itself as one undoable edit / smart motion (WP-1).
    Key(SourceKey),
    /// Middle-press over the source editor of `path` at `at` (editor-local coordinates): place
    /// the cursor, then paste PRIMARY. editor-usability-round WP-5: the position comes with the
    /// press (`widget::middle_click`), not from a message per pointer move.
    MiddleClick {
        path: String,
        at: iced::Point,
    },
    /// The PRIMARY selection read for a middle-click paste.
    PastePrimary {
        path: String,
        text: Option<String>,
    },
}

/// WP-1 source keys routed through `SourceMsg::Key` (each one transaction or motion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKey {
    /// Enter with list continuation.
    Enter,
    /// Ctrl+Backspace: delete to the previous word boundary (or the selection).
    DeleteWordBack,
    /// Ctrl+Delete: delete to the next word boundary (or the selection).
    DeleteWordForward,
    /// Smart Home (`select` = Shift).
    Home { select: bool },
    /// Smart End (`select` = Shift).
    End { select: bool },
}

/// WP-3: reading pane.
#[derive(Debug, Clone)]
pub enum ReadingMsg {
    /// Toggle the active tab's mode Source ↔ Reading; Split → Source (Ctrl+E; the palette too).
    CycleMode,
    /// Set a tab's mode by path.
    SetMode {
        path: String,
        mode: crate::reading::ViewMode,
    },
    /// The recompute debounce tick.
    Tick,
    /// A wiki-link / vault md-link click in the reading pane of `src`.
    OpenLink { src: String, target: String },
    /// An image sensor came into view (`width` = laid-out width).
    ImageShown {
        tab: String,
        key: String,
        width: f32,
    },
    /// An image sensor left view.
    ImageHidden { tab: String, key: String },
    /// An off-thread decode finished (`Err` = unreadable/undecodable; no note text).
    ImageLoaded {
        key: String,
        result: Result<crate::image_cache::Decoded, String>,
    },
    /// A host-note task checkbox was clicked: toggle the box at source `line` of `path`.
    ToggleTask { path: String, line: usize },
    /// The filename input of `path` changed.
    TitleInput { path: String, value: String },
    /// Enter (or focus loss) in the filename input: rename if changed.
    TitleSubmit { path: String },
    /// Escape: revert every pending filename edit.
    TitleRevert,
    /// Blur poll result for `path`'s filename input.
    TitleFocus { path: String, focused: bool },
    /// The save-before-rename finished.
    TitleSaved {
        path: String,
        to: String,
        seq: u64,
        outcome: crate::source::SaveOutcome,
    },
}

/// WP-4: the file explorer.
#[derive(Debug, Clone)]
pub enum ExplorerMsg {
    /// Full rebuild from the index.
    Refresh,
    /// Expand/collapse a folder.
    Toggle(String),
    /// Select a row (file or folder) without opening it.
    Select(String),
    /// Select and open a file in the focused pane.
    Open(String),
    /// The virtual list scrolled.
    Scrolled(ListWindow),
    /// Context actions on the selection.
    BeginNew,
    BeginRename,
    BeginMove,
    Trash,
    /// The inline name/folder field.
    Input(String),
    Submit,
    Cancel,
    /// An RPC file operation finished.
    Done(crate::explorer::FileOpDone),
}

/// WP-4: quick switcher (Ctrl+P).
#[derive(Debug, Clone)]
pub enum SwitcherMsg {
    Open,
    Close,
    Query(String),
    Up,
    Down,
    /// Enter: open the selection (Shift held, or no match → create).
    Submit,
    /// "Create <query>".
    Create,
    /// Click on a result.
    Pick(String),
    /// Modifier state (Shift+Enter detection).
    Modifiers(iced::keyboard::Modifiers),
    /// `note.create` finished: `open_path` or an error.
    Created(Result<Option<String>, String>),
}

/// WP-5: command palette (Ctrl+Shift+P) and action invocation.
#[derive(Debug, Clone)]
pub enum PaletteMsg {
    Open,
    /// Esc: closes the list or cancels the whole invocation (prompt included).
    Close,
    Query(String),
    Up,
    Down,
    /// Enter: invoke the selection, or advance the param prompt.
    Submit,
    /// Click on entry `i` (index into `Palette::entries`).
    Pick(usize),
    /// `actions_list` finished.
    Loaded(Result<Vec<cv_protocol::ActionInfo>, String>),
    /// A global key chord (Ctrl/Alt held).
    Key(crate::palette::chord::Chord),
    /// Ctrl+Enter: submit a multiline prompt.
    CtrlEnter,
    /// Param prompt input.
    Prompt(crate::palette::PromptMsg),
    /// The save-first write finished; `call` runs only on success.
    SaveDone {
        path: String,
        seq: u64,
        outcome: crate::source::SaveOutcome,
        call: crate::palette::Call,
    },
    /// `run_action` returned.
    Ran {
        title: String,
        /// The invoking tab's `(path, edit_seq)` at dispatch (WP-5 follow-the-note).
        origin: Option<(String, u64)>,
        result: Result<cv_protocol::RunActionResult, String>,
    },
    /// Toast expiry tick.
    Tick,
    /// Run a preset action (client default chord / palette entry).
    RunPreset(crate::palette::Preset),
}

/// WP-5: side panels.
#[derive(Debug, Clone)]
pub enum PanelsMsg {
    /// Show/hide the column.
    Toggle,
    /// Show the column on a panel.
    Show(crate::panels::PanelKind),
    /// Recompute tick (buffer-keyed; cheap when nothing changed).
    Tick,
    /// Outline click: move the source cursor to this 0-based line.
    OutlineJump(usize),
    /// Open a path in the focused pane.
    Open(String),
    /// Backlinks for `path` loaded off-thread.
    Backlinks {
        path: String,
        result: Result<Vec<crate::panels::Backlink>, String>,
    },
    TagPick(String),
    TagBack,
    SearchInput(String),
    /// The search debounce for keystroke `seq` elapsed.
    SearchDue(u64),
    SearchDone {
        seq: u64,
        result: Result<Vec<crate::panels::SearchHit>, String>,
    },
    Scrolled(crate::panels::ListId, ListWindow),
}

/// WP-7: view fences, keyed by `(view id, embedding note)`.
#[derive(Debug, Clone)]
pub enum ViewsMsg {
    /// A fence came into view (or was invalidated while shown): request its result.
    Need { id: String, host: Option<String> },
    /// `view_run` finished: the raw `ViewResult` JSON or an error.
    Loaded {
        id: String,
        host: Option<String>,
        result: Result<serde_json::Value, String>,
    },
    /// The fence's virtual list scrolled.
    Scrolled {
        id: String,
        host: Option<String>,
        window: ListWindow,
    },
    /// A row / link cell click: open `path`, cursor at 1-based `line` when set.
    Open { path: String, line: Option<i64> },
}

/// WP-4: day/month nav. The bar belongs to `pane`; results open in that pane's active tab.
#[derive(Debug, Clone)]
pub enum NavMsg {
    Go {
        pane: Pane,
        target: editor_model::period::NavTarget,
    },
    /// `period.open` finished: `open_path` or an error.
    Opened {
        pane: Pane,
        result: Result<Option<String>, String>,
    },
}

/// WP-4: pane grid and the pane-side bookmarks list.
#[derive(Debug, Clone)]
pub enum PanesMsg {
    SplitRight,
    SplitDown,
    Close(Pane),
    Focus(Pane),
    Resized(pane_grid::ResizeEvent),
    Bookmark(crate::bookmarks::BookmarkMsg),
    /// editor-usability-round WP-3: per-pane history.
    Back(Pane),
    Forward(Pane),
    /// The focused pane's back/forward (WP-4 binds these; Ctrl+Alt+Left/Right).
    BackFocused,
    ForwardFocused,
    /// Collapse / expand the side column to its icon strip.
    ToggleSide,
    /// Debug pane borders on/off.
    ToggleBorders,
    /// Open / close the view-mode dropdown of a pane (`None` closes).
    ModeMenu(Option<Pane>),
}

/// Daemon connection and notifications.
#[derive(Debug, Clone)]
pub enum StatusMsg {
    /// A daemon notification.
    Notification(Notification),
    /// The notification channel closed (connection lost).
    Disconnected,
}

/// Tabs.
#[derive(Debug, Clone)]
pub enum TabsMsg {
    /// Open a vault-relative path (or focus its tab).
    Open(String),
    Select(Pane, usize),
    /// Close (the tab ×): a dirty buffer saves first; a failed save cancels.
    Close(Pane, usize),
    /// Ctrl+W: close the focused pane's active tab.
    CloseActive,
    /// The save-before-close finished.
    CloseSaved {
        path: String,
        seq: u64,
        outcome: crate::source::SaveOutcome,
    },
    /// Alt+N: the focused pane's tab N (1-based; out of range = no-op).
    GoTo(u8),
    /// Ctrl+Q / window close: flush every dirty buffer, then exit.
    Quit,
    /// One save-before-quit finished.
    QuitSaved {
        path: String,
        seq: u64,
        outcome: crate::source::SaveOutcome,
    },
}

/// editor-usability-round WP-4: the focused pane's find bar.
#[derive(Debug, Clone)]
pub enum FindMsg {
    /// Ctrl+F: open (or refocus) the bar.
    Open,
    /// Escape: close and refocus the editor.
    Close,
    Query(String),
    /// Enter (Shift held → previous).
    Submit,
    Next,
    Prev,
    /// Shift state while the bar is open (Shift+Enter detection).
    Shift(bool),
}

/// The surround switch (client command, `surround.ts:75`).
#[derive(Debug, Clone)]
pub enum ThemeMsg {
    Cycle,
    Set(Surround),
}
