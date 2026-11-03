//! The iced application state, `update` and `view` (plan WP-1 skeleton).
//!
//! Layout (WP-4): a top bar, the quick switcher (when open), a side column (explorer tree +
//! bookmarks) beside a `pane_grid` whose panes each own a tab strip, an optional day/month nav
//! bar and the active tab's body ([`App::tab_body`]), and the status bar.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use cv_protocol::Notification;
use editor_model::period::NavTarget;
use iced::widget::pane_grid::{self, Pane};
use iced::widget::{PaneGrid, Space, button, container, row, text};
use iced::{Element, Length, Subscription, Task, Theme};

use crate::bookmarks::{self, BOOKMARKS_PATH, BookmarkMsg, Bookmarks};
use crate::daemon::{DaemonClient, DaemonError};
use crate::explorer::{self, EditKind, Explorer, FileOp, FileOpDone};
use crate::message::{
    AfterSave, ExplorerMsg, FindMsg, Message, NavMsg, PaletteMsg, PanelsMsg, PanesMsg, SourceMsg,
    SourceTarget, StatusMsg, SwitcherMsg, TabsMsg, ThemeMsg,
};
use crate::nav;
use crate::panes::{self, PaneState};
use crate::settings::{CAPTURES_PATH, period_folders};
use crate::switcher::{self, Switcher};
use crate::theme::{Surround, Tokens, tokens_for};
use crate::ui_state::{self, SplitDir, UiState};

mod actions;
mod config;
mod connection;
mod files;
mod holds;
mod image_tab;
mod menu;
pub mod pdf_tab;
mod saves;
pub(crate) mod scroll;
mod status;
pub mod tab_drag;
mod tabs;
mod update;
mod view;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub use connection::RecordingSink;
pub use connection::{NoopSink, NotifHandle, OpenerNotices, RpcExec, TestSink, reconnect_delay};
pub use status::{NOTICE_INLINE_CHARS, Notice, Severity, StatusModel};

/// One open tab (plan editor-feedback-round-2 WP-7): a view of a note's document, or one of
/// the singleton tabs (Settings, Log). A note's [`crate::source::SourceBuffer`] lives in the
/// path-keyed [`OpenDoc`] store on [`App`], so two tabs of one note are mirrored views of one
/// document (content, undo, dirty/saving, conflict, cursor). `id` is unique per tab for the
/// session: it identifies the tab across async work (a close waiting on a save, audit E1) and
/// keys per-view state (image pins, Reading place). The kind is a type, never a
/// reserved path; singleton tabs never reach a daemon read or write.
#[derive(Debug, Clone)]
pub struct Tab {
    pub id: u64,
    pub kind: TabKind,
}

pub use crate::ui_state::{TabKind, Viewer};

/// One open document: everything per note path lives here, so a close is one
/// `remove` and a rename one re-key. A document lives while at least one tab shows its path.
/// Its Reading state ([`crate::reading::DocReading`]) lives here too, so it closes and renames
/// with the document.
#[derive(Debug)]
pub struct OpenDoc {
    pub buffer: crate::source::SourceBuffer,
    /// Tabs (by [`Tab::id`]) whose close waits on this note's in-flight normal save (WP-3
    /// rework; audit E1: a close keeps its target tab identity across the save, so closing one
    /// duplicate never closes the other).
    pub close_waiters: Vec<u64>,
    /// The document's Reading state (mode, rendered model, per-tab places).
    pub reading: crate::reading::DocReading,
}

/// The document store: one [`OpenDoc`] per open note path (plan editor-feedback-round-2 WP-7).
pub type Docs = std::collections::HashMap<String, OpenDoc>;

/// The source buffer of the document open at `path` (a field-level borrow of [`App::docs`]).
fn buf<'a>(docs: &'a Docs, path: &str) -> Option<&'a crate::source::SourceBuffer> {
    docs.get(path).map(|d| &d.buffer)
}

/// [`buf`], mutably.
fn buf_mut<'a>(docs: &'a mut Docs, path: &str) -> Option<&'a mut crate::source::SourceBuffer> {
    docs.get_mut(path).map(|d| &mut d.buffer)
}

/// The `editor.json` write debounce: a burst of UI-state changes (hover focus,
/// tab switches) is written once.
pub const PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// A note tab, borrowed: its id and vault path ([`Tab::as_note`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteRef<'a> {
    pub id: u64,
    pub path: &'a str,
}

/// The next [`Tab::id`].
static NEXT_TAB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Where [`App::open_into`] puts a path (plan editor-feedback-round-2 WP-7). In every mode but
/// `Restore` and `NewTab`, a tab of the path already in the target pane is just focused, and a path open
/// nowhere opens a new tab there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// Focus an existing tab wherever it is (links, explorer, history; Quick Switch `switch`).
    Focus,
    /// Like `Focus`, but a new tab replaces the pane's active tab when that one is clean.
    Replace,
    /// Session restore: always add a tab to the pane (the same path may be in two panes).
    Restore,
    /// Quick Switch `move`: an existing tab in another pane moves into the target pane.
    Move,
    /// Quick Switch `duplicate`: a second tab of the note opens in the target pane.
    Duplicate,
    /// Ctrl/Cmd+click on a link (plan round-14 WP-2): always add a new tab in the target pane,
    /// even when the note is already open there or elsewhere (it then shares that document).
    NewTab,
}

impl Tab {
    /// A new tab (fresh id) of `kind`.
    pub fn new(kind: TabKind) -> Self {
        Self {
            id: NEXT_TAB_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            kind,
        }
    }

    /// A new note tab (fresh id) showing `path`.
    pub fn note(path: &str) -> Self {
        Self::new(TabKind::Note(path.to_owned()))
    }

    /// The note path, when this is a note tab.
    pub fn path(&self) -> Option<&str> {
        match &self.kind {
            TabKind::Note(p) => Some(p),
            TabKind::File { .. } | TabKind::Settings | TabKind::Log => None,
        }
    }

    /// WP-R2 (review E6): the vault path a file tab (image or PDF) shows.
    pub fn file_path(&self) -> Option<&str> {
        match &self.kind {
            TabKind::File { path: p, .. } => Some(p),
            _ => None,
        }
    }

    /// This tab as a note (`None` for Settings / Log).
    pub fn as_note(&self) -> Option<NoteRef<'_>> {
        self.path().map(|path| NoteRef { id: self.id, path })
    }

    /// Plan round-15 WP-2: this tab's file moved to `to`. The one retarget rule: a file tab
    /// keeps its kind (a note stays a note, an image an image — a later PDF tab joins here);
    /// Settings / Log show no file and are untouched.
    pub fn retarget(&mut self, to: &str) {
        match &mut self.kind {
            TabKind::Note(p) | TabKind::File { path: p, .. } => to.clone_into(p),
            TabKind::Settings | TabKind::Log => {}
        }
    }

    /// Whether this tab shows the note (or, plan round-14 WP-3, the image) at `path`.
    pub fn shows(&self, path: &str) -> bool {
        self.path().or_else(|| self.file_path()) == Some(path)
    }
}

#[cfg(test)]
impl App {
    /// Tests: queue RPC completions (drained by [`App::pump`]) and record sink events.
    pub fn queue_rpcs(&mut self) {
        if !matches!(self.rpc_exec, RpcExec::Queue(_)) {
            self.rpc_exec = RpcExec::Queue(std::collections::VecDeque::new());
        }
    }

    /// Tests: feeds every queued RPC completion through `update` (in FIFO order, including
    /// completions those updates queue) until the queue is empty.
    pub fn pump(&mut self) {
        for _ in 0..10_000 {
            let next = match &mut self.rpc_exec {
                RpcExec::Queue(q) => q.pop_front(),
                RpcExec::Pool => None,
            };
            let Some(m) = next else { return };
            let _ = self.update(m);
        }
        panic!("App::pump: RPC completions did not settle");
    }

    /// Tests: `update` followed by [`App::pump`] — the production ordering (the message and
    /// its post-pass finish before any completion it started is delivered).
    pub fn send(&mut self, m: Message) -> Task<Message> {
        let t = self.update(m);
        self.pump();
        t
    }

    fn rec(&self) -> &RecordingSink {
        self.sink
            .as_any()
            .downcast_ref::<RecordingSink>()
            .expect("tests install a RecordingSink")
    }

    /// Tests: every `on_tab_closed` path, in order.
    pub fn closed(&self) -> Vec<String> {
        self.rec().closed.lock().unwrap().clone()
    }

    pub fn clear_closed(&self) {
        self.rec().closed.lock().unwrap().clear();
    }

    /// Tests: every clipboard text copy, in order.
    pub fn copied(&self) -> Vec<String> {
        self.rec().copied.lock().unwrap().clone()
    }

    /// Tests: every "Copy image" vault path, in order.
    pub fn copied_images(&self) -> Vec<String> {
        self.rec().copied_images.lock().unwrap().clone()
    }

    /// Tests: every absolute path handed to the system opener, in order.
    pub fn opened_files(&self) -> Vec<String> {
        self.rec().opened_files.lock().unwrap().clone()
    }
}

#[cfg(test)]
fn default_sink() -> Arc<dyn TestSink> {
    Arc::new(RecordingSink::default())
}

#[cfg(not(test))]
fn default_sink() -> Arc<dyn TestSink> {
    Arc::new(NoopSink)
}

/// The application.
pub struct App {
    pub daemon: Option<Arc<dyn DaemonClient>>,
    pub status: StatusModel,
    /// The pane grid; each pane owns its tabs.
    pub panes: pane_grid::State<PaneState>,
    /// The focused pane (explorer / switcher / bookmark opens target it).
    pub focus: Pane,
    pub ui: UiState,
    pub explorer: Explorer,
    pub switcher: Switcher,
    /// Plan round-5 WP-9: the open `[[` note list (focused source editor only).
    pub link_ta: Option<crate::link_ta::LinkTa>,
    pub bookmarks: Bookmarks,
    /// SR13: how `rpc` runs daemon calls — the blocking pool in production; tests queue the
    /// completion `Message`s and drain them explicitly with `App::pump`.
    pub rpc_exec: RpcExec,
    /// SR13: test recorders (closed tabs, copies) behind an injected sink; no-op in production.
    pub sink: Arc<dyn TestSink>,
    /// Tests: the injected "today" for nav (`None` = local date).
    pub today: Option<NaiveDate>,
    /// WP-3: per-tab reading state (mode, render cache, image handles), keyed by path.
    pub reading: crate::reading::ReadingStore,
    /// WP-5: command palette, action invocation and the job toast.
    pub palette: crate::palette::Palette,
    /// WP-5: the right-hand side panels.
    pub panels: crate::panels::Panels,
    /// Plan round-17 WP-6: the last open sidebar section (what `ToggleSide` reopens).
    pub side_last: crate::ui_state::SideSection,
    /// The one link-resolver cache, shared by Reading and the panels.
    pub resolver: std::sync::Arc<crate::reading::resolver::ResolverCache>,
    /// WP-7: `crucible-view` fence results, keyed by `(view id, embedding note)`.
    pub views: crate::views::ViewStore,
    /// Plan memory-instrumentation-and-reduction WP-M2: the gauge registry behind `memory_report`.
    pub memory: Arc<crate::memory::Memory>,
    /// Plan memory-instrumentation-and-reduction WP-M4: when the last quiet-period trim ran.
    pub last_trim: Option<std::time::Instant>,
    pub(crate) tokens: Tokens,
    notifications: NotifHandle,
    /// Plan round-15 WP-2: system-opener failures, delivered by a subscription.
    opener_notices: OpenerNotices,
    /// plan editor-data-safety WP-1: opens a fresh client (`None` = never reconnect).
    connector: Option<crate::daemon::Connector>,
    /// Failed reconnect attempts since the last disconnect (the backoff exponent).
    reconnect_attempt: u32,
    state_path: Option<PathBuf>,
    /// `App::new` is restoring tabs: their first reading render waits for the debounce tick.
    restoring: bool,
    /// Plan round-17 WP-2: note holds by daemon job id ([`holds`]).
    pub(crate) holds: std::collections::HashMap<String, holds::Hold>,
    /// Plan round-18 WP-2: tab closes of dirty held notes `(note path, tab id)`, resumed (saved,
    /// then closed) when the hold releases.
    pub(crate) hold_closes: Vec<(String, u64)>,
    /// The hold overlay's spinner frame.
    hold_frame: u32,
    /// editor-usability-round WP-3: the pane whose view-mode dropdown is open.
    pub mode_menu: Option<Pane>,
    /// plan editor-round-4 WP-4: a mouse button is held (hover focus is suppressed).
    pub mouse_down: bool,
    /// Plan round-10 WP-8: the tab-drag tracker.
    pub(super) tab_drag: tab_drag::TabDrag,
    /// Plan round-21 WP-1: the pane whose tab strip has keyboard focus (Left/Right scroll it).
    /// Set by a press on a tab header, cleared by any other user input
    /// ([`App::drops_strip_focus`]).
    pub(super) strip_focus: Option<Pane>,
    /// A back/forward step is opening a path: it must not push onto the history.
    history_nav: bool,
    /// editor-usability-round WP-4: a quit waiting on these paths' saves (`None` = no quit).
    pub(crate) quit: Option<QuitState>,
    /// plan editor-data-safety WP-2: the open modal dialog (input priority while `Some`).
    pub dialog: Option<crate::dialog::Dialog>,
    /// Plan editor-feedback-round-2 WP-7: one [`OpenDoc`] per open note path.
    pub docs: Docs,
    /// The UI state last written to `editor.json` (`None` = never written).
    persisted: Option<UiState>,
    /// A debounced `editor.json` write is scheduled.
    persist_pending: bool,
    /// Tests: `editor.json` writes so far.
    pub ui_writes: u64,
    /// The app asked iced to exit (tests observe it).
    pub exiting: bool,
    /// The held keyboard modifiers (Shift+Enter in find / switcher, Shift+click).
    pub modifiers: iced::keyboard::Modifiers,
    /// plan editor-settings-tab WP-2: the Settings tab's model (file save loops, Status).
    pub settings: crate::settings::Settings,
    /// The effective `editor.toml` prefs (live; `prefs.keys` feeds the palette keymap).
    pub prefs: crate::settings::editor_prefs::Prefs,
    /// plan editor-settings-tab WP-4 (audit E2): the day / month note folders from the loaded
    /// `captures.toml` (constants when absent / unloaded / malformed).
    pub period_folders: editor_model::period::PeriodFolders,
    /// plan editor-ui-round-3 WP-4: the open right-click menu (one at a time).
    pub context_menu: Option<crate::context_menu::ContextMenu>,
    /// plan editor-round-4 WP-4: the image clipboard writer (`xclip` in production).
    pub clip_writer: Arc<dyn crate::clipboard_image::ClipboardWriter + Send + Sync>,
    /// WP-R2 (review E1): the one PDF store — documents, tab and embed consumers (only
    /// `Send` data).
    pub pdf: crate::pdf::store::PdfStore<iced::widget::image::Handle>,
}

/// A quit in progress: the saves it waits on, and whether one failed.
#[derive(Debug, Default)]
pub(crate) struct QuitState {
    pending: std::collections::HashSet<String>,
    /// Settings files whose flush write has not landed yet (plan editor-feedback-round-2 WP-2).
    settings: std::collections::HashSet<String>,
    /// Notes whose normal (autosave/flush) save was already in flight at quit; resolved by
    /// their `SourceMsg::Saved` (plan editor-feedback-round-2 WP-3 rework).
    awaiting: std::collections::HashSet<String>,
    /// Plan round-18 WP-2: dirty notes a running job holds; saved (then counted `pending`) when
    /// the hold releases, a loss on disconnect (a second quit offers "quit anyway").
    held: std::collections::HashSet<String>,
    /// plan editor-data-safety WP-2: what cannot be saved (shown in the dialog once every
    /// flush has resolved).
    losses: crate::dialog::UnsavedEdits,
}

impl App {
    /// Builds the app over a connected daemon (`None` = disconnected), restoring UI state from
    /// `state_path` (`None` = don't persist, e.g. tests).
    pub fn new(daemon: Option<Arc<dyn DaemonClient>>, state_path: Option<PathBuf>) -> Self {
        let ui = state_path
            .as_deref()
            .map(ui_state::load)
            .unwrap_or_default();
        let view_filters = ui.view_filters.clone();
        let view_sorts = ui.view_sorts.clone();
        let (mut status, notifications) = match &daemon {
            Some(d) => {
                let h = d.hello();
                let rx = match d.subscribe(&crate::daemon::ALL_TOPICS) {
                    Ok(rx) => Some(rx),
                    Err(e) => {
                        tracing::warn!(error = %e, "subscribe failed");
                        None
                    }
                };
                (
                    StatusModel {
                        connected: rx.is_some(),
                        generation: h.generation,
                        last_job: None,
                        notice: None,
                        reconnecting: false,
                        log: crate::log::Log::default(),
                    },
                    NotifHandle::new(rx),
                )
            }
            None => (
                StatusModel {
                    connected: false,
                    generation: 0,
                    last_job: None,
                    notice: None,
                    reconnecting: false,
                    log: crate::log::Log::default(),
                },
                NotifHandle::default(),
            ),
        };
        if !status.connected {
            status.record(
                Severity::Warning,
                "started without a daemon connection",
                None,
            );
        }
        let layout = ui
            .panes
            .clone()
            .unwrap_or_else(|| panes::from_flat(&ui.tabs, ui.active_tab));
        let (grid, plan, focus) = panes::restore(&layout);
        let resolver = std::sync::Arc::new(crate::reading::resolver::ResolverCache::default());
        let shared = resolver.clone();
        let mut app = Self {
            daemon,
            status,
            panes: grid,
            focus,
            tokens: tokens_for(ui.surround),
            ui,
            explorer: Explorer::default(),
            switcher: Switcher::default(),
            link_ta: None,
            bookmarks: Bookmarks::default(),
            rpc_exec: RpcExec::Pool,
            sink: default_sink(),
            today: None,
            reading: crate::reading::ReadingStore::new(),
            palette: crate::palette::Palette::default(),
            panels: crate::panels::Panels::default(),
            side_last: crate::ui_state::SideSection::Files,
            resolver,
            views: crate::views::ViewStore {
                filters: view_filters,
                sorts: view_sorts,
                ..Default::default()
            },
            memory: Arc::new(crate::memory::Memory::new()),
            last_trim: None,
            notifications,
            opener_notices: OpenerNotices::default(),
            connector: None,
            reconnect_attempt: 0,
            state_path,
            restoring: true,
            holds: std::collections::HashMap::new(),
            hold_closes: Vec::new(),
            hold_frame: 0,
            mode_menu: None,
            mouse_down: false,
            tab_drag: tab_drag::TabDrag::default(),
            strip_focus: None,
            history_nav: false,
            quit: None,
            dialog: None,
            docs: Docs::new(),
            persisted: None,
            persist_pending: false,
            ui_writes: 0,
            exiting: false,
            modifiers: iced::keyboard::Modifiers::empty(),
            settings: crate::settings::Settings::default(),
            prefs: crate::settings::editor_prefs::Prefs::default(),
            period_folders: editor_model::period::PeriodFolders::default(),
            context_menu: None,
            clip_writer: Arc::new(crate::clipboard_image::Xclip),
            pdf: crate::pdf::store::PdfStore::default(),
        };
        // Plan round-17 WP-6: the restored section; a panel section makes the panels live.
        if let Some(s) = app.ui.side.0 {
            app.side_last = s;
            if let Some(k) = s.panel() {
                app.panels.visible = true;
                app.panels.kind = k;
            }
        }
        app.reading.resolver = shared.clone();
        app.panels.resolver = shared;
        app.load_editor_prefs();
        app.refresh_files();
        app.load_bookmarks();
        for (pane, tabs, active) in plan {
            for kind in tabs {
                match kind {
                    TabKind::Note(p) | TabKind::File { path: p, .. } => {
                        if let Err(e) = app.open_into(pane, &p, Place::Restore) {
                            app.status.set_error(open_error(&e));
                        }
                    }
                    TabKind::Settings | TabKind::Log => app.open_singleton(kind, pane),
                }
            }
            if let Some(ps) = app.panes.get_mut(pane)
                && let Some(i) = active.filter(|&i| i < ps.tabs.len())
            {
                ps.activate(i);
            }
            if let Some(ps) = app.panes.get_mut(pane) {
                ps.reset_mru_to_active();
            }
        }
        app.focus = focus;
        app.restoring = false;
        for (_, ps) in app.panes.iter_mut() {
            if let Some(p) = ps.active_note().map(|t| t.path.to_owned()) {
                ps.history.push(&p);
            }
        }
        app.sync_buffers();
        // The restored state is what is on disk: only a later change writes.
        app.sync_ui_state();
        app.persisted = Some(app.ui.clone());
        app
    }

    /// Lazy source buffers (plan editor-cutover-blockers WP-2): a document keeps its shaped
    /// iced `Content` only while some pane shows it as the active tab in Source or Split; every
    /// other document holds plain text. Plan editor-feedback-round-2 WP-7 (audit E2): a shared
    /// document is shaped once, from the union of its tabs' visibility — never shaped and
    /// unshaped within one sync. Idempotent; run after every update.
    fn sync_buffers(&mut self) {
        let t0 = std::time::Instant::now();
        let visible: std::collections::HashSet<String> = self
            .panes
            .iter()
            .filter_map(|(_, ps)| ps.active_note())
            .filter(|t| self.reading.mode(&self.docs, t.path).shows_source())
            .map(|t| t.path.to_owned())
            .collect();
        for (path, doc) in &mut self.docs {
            doc.buffer.set_shaped(visible.contains(path));
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        if ms >= 1.0 {
            tracing::debug!(elapsed_ms = ms, "sync_buffers");
        }
    }

    /// The mode a newly opened `path` starts in: its persisted mode, else Reading.
    fn initial_mode(&self, path: &str) -> crate::reading::ViewMode {
        self.ui.modes.get(path).copied().unwrap_or_default()
    }
}

fn open_error(e: &DaemonError) -> String {
    match e {
        DaemonError::NotFound(_) => "file not found".into(),
        DaemonError::InvalidPath => "invalid path".into(),
        other => format!("open failed: {other}"),
    }
}
