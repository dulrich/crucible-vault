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
    /// The `editor.json` write debounce elapsed — write the latest UI-state
    /// snapshot if it still differs from the last one written (an internal completion).
    PersistDue,
    /// Plan memory-instrumentation-and-reduction WP-M2: the 5-minute `memory_report` tick.
    MemoryDue,
    /// Plan memory-instrumentation-and-reduction WP-M4: the always-on quiet-period trim tick.
    TrimTick,
    /// Plan round-10 WP-7: a key/mouse input for the bare-Alt menu-bar toggle.
    AltKey(crate::app::alt_tap::AltInput),
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
    /// plan editor-settings-tab WP-2: the Settings tab.
    Settings(crate::settings::SettingsMsg),
    /// Plan round-6-feedback-todos WP-7: the Log tab.
    Log(crate::log::LogMsg),
    /// plan editor-data-safety WP-2: the open modal dialog.
    Dialog(crate::dialog::DialogMsg),
    /// plan editor-ui-round-3 WP-4: the right-click context menu.
    Menu(crate::context_menu::MenuMsg),
    /// Plan usability-sprint-captures-links WP-3: open an external URL with `open_url_cmd`
    /// (scheme allowlist enforced in `open_url`).
    OpenUrl(String),
    /// Plan round-14 WP-3: an image tab's view / decode events.
    ImageTab(ImageTabMsg),
    /// Plan round-15 WP-3: a PDF tab.
    PdfTab(PdfTabMsg),
    /// Plan usability-sprint-captures-links WP-5: a `title` → filename rename finished
    /// (`seq` = the buffer's edit sequence when it was issued).
    TitleRenamed {
        from: String,
        to: String,
        seq: u64,
        result: Result<String, String>,
    },
    /// Keyboard modifier state, tracked once
    /// in `App.modifiers` and never dropped by a modal layer.
    Modifiers(iced::keyboard::Modifiers),
}

/// How a modal layer treats a message —
/// user `Input` is dropped while a dialog or menu is open; a `Completion` (async result,
/// timer tick, modifier state) always runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Input,
    Completion,
}

/// What a save-first flow does once its save
/// lands clean (`SourceMsg::Saved { after: Some(..) }`, handled by `App::on_save_landed`).
#[derive(Debug, Clone)]
pub enum AfterSave {
    /// Close the tab with this [`crate::app::Tab::id`].
    CloseTab(u64),
    /// One save-before-quit.
    Quit,
    /// Continue into the delete preview.
    Delete,
    /// Rename the note to this path.
    Rename(String),
    /// Run the palette call.
    RunCall(crate::palette::Call),
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
    /// Plan round-5 WP-1: `path`'s source editor was laid out showing `lines` lines.
    Viewport {
        path: String,
        lines: usize,
    },
    /// The autosave debounce for edit `seq` elapsed.
    AutosaveDue {
        path: String,
        seq: u64,
    },
    /// A save finished; `after` is the save-first continuation, if any.
    Saved {
        path: String,
        seq: u64,
        outcome: crate::source::SaveOutcome,
        after: Option<AfterSave>,
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
    /// Plan usability-sprint-captures-links WP-3: Ctrl/Cmd+left-press over the source editor of
    /// `path` at `at` (editor-local): place the cursor, then follow the link under it.
    CommandClick {
        path: String,
        at: iced::Point,
    },
    /// The PRIMARY selection read for a middle-click paste.
    PastePrimary {
        path: String,
        text: Option<String>,
    },
    /// plan editor-ui-round-3 WP-5: Ctrl+X in the focused source editor — the selection, or
    /// with none the whole current line (one undoable edit), to the clipboard.
    Cut,
    /// Plan editor-feedback-round-2 WP-9: Ctrl+V in the focused source editor (the active
    /// note) — image-aware via `clipboard_image`.
    Paste,
    /// The resolved Ctrl+V for `path` (image stored + embed, remote embed, or text paste).
    PasteResolved {
        path: String,
        outcome: crate::clipboard_image::PasteOutcome,
    },
    /// Plan round-5 WP-9: a key intercepted while the `[[` note list is open.
    LinkKey(LinkKey),
    /// Plan round-5 WP-9: a `[[` list row was clicked (its vault path).
    LinkPick(String),
}

/// Plan round-5 WP-9: keys the open `[[` note list takes from the source editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKey {
    Up,
    Down,
    Enter,
    Tab,
    Escape,
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
    /// Tab: indent the selected lines, else insert `\t` (plan editor-feedback-round-2 WP-4).
    Indent,
    /// Shift+Tab: outdent the selected lines (or the cursor's line).
    Outdent,
}

/// Plan round-14 WP-3: an image tab (vault path `path`, tab id `view`).
#[derive(Debug, Clone)]
pub enum ImageTabMsg {
    /// The tab body came into view at laid-out `width`: pin the image, decode on a miss.
    Shown { path: String, view: u64, width: f32 },
    /// The tab body left view: unpin.
    Hidden { path: String, view: u64 },
    /// The off-thread decode finished.
    Loaded {
        key: String,
        result: Result<crate::image_cache::Decoded, String>,
    },
}

/// Plan round-15 WP-3: a PDF tab (vault path `path`, tab id `view`, 0-based `page`).
#[derive(Debug, Clone)]
pub enum PdfTabMsg {
    /// The tab body came into view: read and parse the file once.
    Open { path: String },
    /// The off-thread read + parse finished: shared bytes and page sizes, or the failure.
    Opened {
        path: String,
        result: Result<(std::sync::Arc<Vec<u8>>, crate::pdf::PageSizes), String>,
    },
    /// Page `page` came within the near window at laid-out `width`.
    PageShown {
        path: String,
        view: u64,
        page: usize,
        width: f32,
    },
    /// Page `page` left the near window: unpin.
    PageHidden {
        path: String,
        view: u64,
        page: usize,
    },
    /// An off-thread page render finished.
    Rendered {
        path: String,
        view: u64,
        page: usize,
        key: String,
        result: Result<crate::image_cache::Decoded, String>,
    },
    /// "Open externally": hand the file to the system opener.
    OpenExternal(String),
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
    /// plan editor-ui-round-3 WP-6: the reading scrollable scrolled (re-probe its top block).
    /// Plan round-6 WP-1: `y` = the relative vertical offset (clears an away-from-edge state).
    Scrolled { path: String, view: u64, y: f32 },
    /// plan editor-ui-round-3 WP-6: a reading layout probe landed; `scroll_to` = then scroll
    /// that top-level block to the viewport top.
    Probed {
        path: String,
        /// The tab id whose reading view was probed (plan round-5 WP-1).
        view: u64,
        probe: Option<crate::reading::Probe>,
        scroll_to: Option<usize>,
    },
    /// The recompute debounce tick.
    Tick,
    /// A wiki-link / vault md-link click in the reading pane of `src`.
    OpenLink { src: String, target: String },
    /// The same link opened with Ctrl/Cmd held: always a new tab (plan round-14 WP-2).
    OpenLinkNew { src: String, target: String },
    /// An image sensor came into view (`width` = laid-out width).
    ImageShown {
        tab: String,
        /// The showing view (tab id): pins are per view (plan editor-feedback-round-2 WP-7).
        view: u64,
        key: String,
        width: f32,
    },
    /// An image sensor left view.
    ImageHidden { tab: String, view: u64, key: String },
    /// An off-thread decode finished (`Err` = unreadable/undecodable; no note text).
    ImageLoaded {
        key: String,
        result: Result<crate::image_cache::Decoded, String>,
    },
    /// A host-note task checkbox was clicked: toggle the box at source `line` of `path`.
    ToggleTask { path: String, line: usize },
    /// plan usability-sprint-captures-links WP-4: a frontmatter boolean checkbox was clicked:
    /// flip top-level `key` of `path`.
    ToggleProp { path: String, key: String },
    /// plan editor-ui-round-3 WP-5: Go to top / bottom (Ctrl+PgUp / Ctrl+PgDn) of the focused
    /// pane's note — the source cursor (Source/Split) and the reading scroll (Reading/Split).
    GoToEdge { bottom: bool },
    /// The filename input of `path` changed.
    TitleInput { path: String, value: String },
    /// Enter (or focus loss) in the filename input: rename if changed.
    TitleSubmit { path: String },
    /// Escape: revert every pending filename edit.
    TitleRevert,
    /// Blur poll result for `path`'s filename input.
    TitleFocus { path: String, focused: bool },
    /// Plan round-5 WP-7: tab `view`'s Reading selection of `path` changed (`None` = cleared).
    Select {
        path: String,
        view: u64,
        sel: Option<crate::widget::selectable::Selection>,
    },
    /// Plan round-5 WP-7: a selection gesture ended (mouse release): copy to PRIMARY.
    SelectDone { path: String, view: u64 },
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
    /// plan editor-ui-round-3 WP-3: "Delete note…" on the active note.
    DeleteActive,
    /// The `delete_preview` answer for `path` (opens the delete dialog).
    DeletePreview {
        path: String,
        result: Result<Vec<crate::daemon::DeletePreviewItem>, String>,
    },
    /// `delete_note` finished.
    NoteDeleted(Result<crate::daemon::DeleteNoteResult, String>),
}

/// WP-4: quick switcher (Ctrl+P).
#[derive(Debug, Clone)]
pub enum SwitcherMsg {
    Open,
    /// Plan usability-sprint-captures-links WP-6: "Move note…" — the switcher in folder mode
    /// for the active note.
    OpenMove,
    Close,
    /// plan editor-ui-round-3 WP-5: the Escape key — clears a non-empty query, else closes.
    Escape,
    Query(String),
    Up,
    Down,
    /// Enter: open the selection (Shift held, or no match → create).
    Submit,
    /// "Create <query>".
    Create,
    /// Click on a result.
    Pick(String),
    /// `note.create` finished: `open_path` or an error.
    Created(Result<Option<String>, String>),
}

/// WP-5: command palette (Ctrl+Shift+P) and action invocation.
#[derive(Debug, Clone)]
pub enum PaletteMsg {
    Open,
    /// Closes the list or cancels the whole invocation (prompt included).
    Close,
    /// plan editor-ui-round-3 WP-5: the Escape key — with the list open and a non-empty query,
    /// clears it; otherwise [`PaletteMsg::Close`].
    Escape,
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
    /// plan editor-round-4 WP-1: plain Enter not captured by a widget — submits a prompt step
    /// whose input has no `on_submit` (`Enum` pick list, `Bool` toggler); otherwise a no-op.
    Enter,
    /// Param prompt input.
    Prompt(crate::palette::PromptMsg),
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
    /// The search command: opens the side panel or the centre modal per `search.location`.
    Search,
    /// Close the search modal.
    CloseSearchModal,
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
    /// Plan round-12-feedback-todos WP-7: a `possible-duplicate` button — run `dupes-resolve`
    /// with `keep` (the panel's note), `other` (the entry's resolved path) and `verdict`.
    ResolveDup {
        keep: String,
        other: String,
        verdict: String,
    },
}

/// WP-7: view fences, keyed by `(view id, embedding note)`.
#[derive(Debug, Clone)]
pub enum ViewsMsg {
    /// Plan round-11 WP-5: a table column header click (`column` = the column key): cycles
    /// that column ASC → DESC → cleared and re-runs the view.
    SortBy {
        id: String,
        host: Option<String>,
        column: String,
    },
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
    /// Plan round-6-feedback-todos WP-6: a status-cell click / status-menu pick on a
    /// `tracker_items` row (`key` = the row's opaque `views::result::TrackerKey`): set its task
    /// status (`None` = open).
    SetStatus { key: String, status: Option<char> },
    /// `tracker_item_set_status` finished.
    StatusSet(Result<(), String>),
    /// Plan round-8-intake-views-anchor WP-3: a `checkbox = true` cell click — set frontmatter
    /// boolean `key` of note `path` to `value` (open buffer, else the daemon's `note_set_bool`).
    SetBool {
        path: String,
        key: String,
        value: bool,
    },
    /// `note_set_bool` finished.
    BoolSet(Result<(), String>),
    /// Plan round-9-feedback-todos WP-5: flip the "Only open" toggle of view `id`.
    ToggleOpenOnly { id: String },
    /// Plan round-13 WP-4: a serial row's Name click in a `sources` section of note `host` —
    /// selects `value` for the host's `select_by` sections (the same value again clears).
    Select { host: String, value: String },
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
        new_tab: bool,
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
    /// plan editor-round-4 WP-4: the pointer entered a pane (`focus_follows_mouse`).
    Hover(Pane),
    /// WP-4: a mouse button went down (`true`) / up — the hover drag guard.
    MouseButton(bool),
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
    /// plan editor-round-4 WP-2: plain PageUp (`false`) / PageDown (`true`) no widget
    /// captured — pages the focused pane's scrollable (Reading, Settings).
    Page(bool),
}

/// Daemon connection and notifications.
#[derive(Debug, Clone)]
pub enum StatusMsg {
    /// A daemon notification.
    Notification(Notification),
    /// The notification channel closed (connection lost), or there never was one.
    Disconnected,
    /// plan editor-data-safety WP-1: the reconnect backoff elapsed; try the connector.
    ReconnectDue,
    /// Plan round-15 WP-2: the system opener exited non-zero (status-bar error text).
    OpenerFailed(String),
    /// A reconnect attempt's result (`Err` = failed; retry after the next backoff).
    Connected(Result<crate::daemon::Connection, String>),
}

/// Tabs.
#[derive(Debug, Clone)]
pub enum TabsMsg {
    /// Open a vault-relative path (or focus its tab).
    Open(String),
    /// Ctrl/Cmd+click on a link (plan round-14 WP-2): open `path` as a new tab in the focused pane.
    OpenNew(String),
    Select(Pane, usize),
    /// Close (the tab ×): a dirty buffer saves first; a failed save cancels.
    Close(Pane, usize),
    /// Ctrl+W: close the focused pane's active tab.
    CloseActive,
    /// Alt+N: the focused pane's tab N (1-based; out of range = no-op).
    GoTo(u8),
    /// Ctrl+Q / window close: flush every dirty buffer, then exit.
    Quit,
    /// Window focus lost: flush every dirty note buffer (plan editor-feedback-round-2 WP-3).
    FlushAll,
    /// Plan round-10 WP-8: a tab header was pressed (activates it and arms a drag).
    DragPress(Pane, usize),
    /// The cursor moved while a press is armed.
    DragMove(iced::Point),
    /// The cursor entered a drop target.
    DragEnter(crate::app::tab_drag::DropTarget),
    /// The cursor left a drop target.
    DragLeave(crate::app::tab_drag::DropTarget),
    /// The left button went up while a press is armed.
    DragRelease,
    /// Esc while a press is armed.
    DragCancel,
    /// The cursor entered a tab header (by tab id): its rollover fill.
    HeaderEnter(u64),
    /// The cursor left a tab header.
    HeaderLeave(u64),
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
    /// A press on the pane's find input: make it the sole focused widget (the source editor
    /// would otherwise stay focused too and take the bar's keys; bug fix editor-find-return-leak).
    Focus(Pane),
}

/// The surround switch (client command, `surround.ts:75`).
#[derive(Debug, Clone)]
pub enum ThemeMsg {
    Cycle,
    Set(Surround),
}

/// Which document a [`SourceMsg`] acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTarget<'a> {
    /// The note at this path, wherever its tabs are.
    Path(&'a str),
    /// The focused pane's active note (the focused source editor).
    Active,
    /// Handled by `App` itself (never reaches `source::update`).
    App,
}

impl SourceMsg {
    /// The document this message acts on.
    pub fn target(&self) -> SourceTarget<'_> {
        match self {
            Self::AutosaveDue { path, .. }
            | Self::KeepMine(path)
            | Self::TakeTheirs(path)
            | Self::MiddleClick { path, .. }
            | Self::CommandClick { path, .. }
            | Self::PastePrimary { path, .. }
            | Self::PasteResolved { path, .. } => SourceTarget::Path(path),
            Self::Action(_)
            | Self::Undo
            | Self::Redo
            | Self::Save
            | Self::Key(_)
            | Self::Cut
            | Self::Paste => SourceTarget::Active,
            Self::Viewport { .. } | Self::Saved { .. } | Self::LinkKey(_) | Self::LinkPick(_) => {
                SourceTarget::App
            }
        }
    }

    /// A user motion, edit or key in the focused source editor: it leaves a Reading edge,
    /// makes Source the most recent selecting side and re-evaluates the `[[` list.
    pub fn is_user_motion(&self) -> bool {
        match self {
            Self::Action(_) | Self::Key(_) => true,
            Self::Undo
            | Self::Redo
            | Self::Save
            | Self::Viewport { .. }
            | Self::AutosaveDue { .. }
            | Self::Saved { .. }
            | Self::KeepMine(_)
            | Self::TakeTheirs(_)
            | Self::MiddleClick { .. }
            | Self::CommandClick { .. }
            | Self::PastePrimary { .. }
            | Self::Cut
            | Self::Paste
            | Self::PasteResolved { .. }
            | Self::LinkKey(_)
            | Self::LinkPick(_) => false,
        }
    }

    pub fn kind(&self) -> MsgKind {
        match self {
            Self::AutosaveDue { .. }
            | Self::Saved { .. }
            | Self::PastePrimary { .. }
            | Self::PasteResolved { .. }
            | Self::Viewport { .. } => MsgKind::Completion,
            Self::Action(_)
            | Self::Undo
            | Self::Redo
            | Self::Save
            | Self::KeepMine(_)
            | Self::TakeTheirs(_)
            | Self::Key(_)
            | Self::MiddleClick { .. }
            | Self::CommandClick { .. }
            | Self::Cut
            | Self::Paste
            | Self::LinkKey(_)
            | Self::LinkPick(_) => MsgKind::Input,
        }
    }
}

impl SwitcherMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Created(_) => MsgKind::Completion,
            Self::Open
            | Self::OpenMove
            | Self::Close
            | Self::Escape
            | Self::Query(_)
            | Self::Up
            | Self::Down
            | Self::Submit
            | Self::Create
            | Self::Pick(_) => MsgKind::Input,
        }
    }
}

impl TabsMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Open(_)
            | Self::OpenNew(_)
            | Self::Select(..)
            | Self::Close(..)
            | Self::CloseActive
            | Self::GoTo(_)
            | Self::Quit
            | Self::FlushAll
            | Self::DragPress(..)
            | Self::DragMove(_)
            | Self::DragEnter(_)
            | Self::DragLeave(_)
            | Self::DragRelease
            | Self::DragCancel
            | Self::HeaderEnter(_)
            | Self::HeaderLeave(_) => MsgKind::Input,
        }
    }
}

impl FindMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Open
            | Self::Close
            | Self::Query(_)
            | Self::Submit
            | Self::Next
            | Self::Prev
            | Self::Focus(_) => MsgKind::Input,
        }
    }
}
