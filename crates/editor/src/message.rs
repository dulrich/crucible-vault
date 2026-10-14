//! The top-level [`Message`]: one variant per feature, each wrapping that feature's sub-enum, so
//! a later WP adds its variants to its own sub-enum plus one wiring line in `App::update`.
//! Empty sub-enums are slots owned by the WP named on them.

use cv_protocol::Notification;

use crate::theme::Surround;

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
}

/// WP-3: reading pane.
#[derive(Debug, Clone)]
pub enum ReadingMsg {}

/// WP-4 replaces this with the real explorer; WP-1 only refreshes a flat file list.
#[derive(Debug, Clone)]
pub enum ExplorerMsg {
    Refresh,
}

/// WP-4: quick switcher.
#[derive(Debug, Clone)]
pub enum SwitcherMsg {}

/// WP-5: command palette.
#[derive(Debug, Clone)]
pub enum PaletteMsg {}

/// WP-5: side panels.
#[derive(Debug, Clone)]
pub enum PanelsMsg {}

/// WP-7: view fences.
#[derive(Debug, Clone)]
pub enum ViewsMsg {}

/// WP-4: day/month nav.
#[derive(Debug, Clone)]
pub enum NavMsg {}

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
    Select(usize),
    Close(usize),
}

/// The surround switch (client command, `surround.ts:75`).
#[derive(Debug, Clone)]
pub enum ThemeMsg {
    Cycle,
    Set(Surround),
}
