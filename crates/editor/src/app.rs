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
use cv_protocol::{Mode, Notification};
use editor_model::period::NavTarget;
use iced::widget::pane_grid::{self, Pane};
use iced::widget::{PaneGrid, Space, button, column, container, row, text};
use iced::{Element, Length, Subscription, Task, Theme};

use crate::bookmarks::{self, BOOKMARKS_PATH, BookmarkMsg, Bookmarks, WriteOutcome};
use crate::daemon::{DaemonClient, DaemonError};
use crate::explorer::{self, EditKind, Explorer, FileOp, FileOpDone};
use crate::message::{
    ExplorerMsg, FindMsg, Message, NavMsg, PaletteMsg, PanelsMsg, PanesMsg, SourceMsg, StatusMsg,
    SwitcherMsg, TabsMsg, ThemeMsg,
};
use crate::nav;
use crate::panes::{self, PaneState};
use crate::switcher::{self, Switcher};
use crate::theme::{Surround, Tokens};
use crate::ui_state::{self, SplitDir, UiState};

/// One open tab: a view of the document at `path` (plan editor-feedback-round-2 WP-7). The
/// [`crate::source::SourceBuffer`] lives in the path-keyed [`Docs`] store on [`App`], so two
/// tabs of one note are mirrored views of one document (content, undo, dirty/saving, conflict,
/// cursor). `id` is unique per tab for the session: it identifies the tab across async work
/// (a close waiting on a save, audit E1) and keys per-view state (image pins). The Settings tab
/// (plan editor-settings-tab WP-2) is a tab whose `path` is the reserved
/// [`crate::settings::SETTINGS_TAB`] sentinel (not a valid vault path) and has no document;
/// [`Tab::kind`] tells them apart, and it never reaches a daemon read or write.
#[derive(Debug, Clone)]
pub struct Tab {
    pub path: String,
    pub id: u64,
}

/// The document store: one source buffer per open note path (plan editor-feedback-round-2
/// WP-7). A document lives while at least one tab shows its path.
pub type Docs = std::collections::HashMap<String, crate::source::SourceBuffer>;

/// The next [`Tab::id`].
static NEXT_TAB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Where [`App::open_into`] puts a path (plan editor-feedback-round-2 WP-7). In every mode but
/// `Restore`, a tab of the path already in the target pane is just focused, and a path open
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
}

/// What a tab shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind<'a> {
    /// A vault note at this path.
    Note(&'a str),
    Settings,
}

impl Tab {
    /// The tab's kind.
    pub fn kind(&self) -> TabKind<'_> {
        if self.is_settings() {
            TabKind::Settings
        } else {
            TabKind::Note(&self.path)
        }
    }

    /// Whether this is the Settings tab.
    pub fn is_settings(&self) -> bool {
        self.path == crate::settings::SETTINGS_TAB
    }

    /// A new tab (fresh id) showing `path`.
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            id: NEXT_TAB_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// The Settings tab (no daemon read, no document).
    pub fn settings() -> Self {
        Self::new(crate::settings::SETTINGS_TAB)
    }
}

/// The status-bar model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    pub connected: bool,
    pub mode: Mode,
    pub generation: u64,
    /// Last job update (`id: state`), for the toast slot WP-5 grows.
    pub last_job: Option<String>,
    /// Last error worth surfacing (no note text).
    pub error: Option<String>,
    /// plan editor-data-safety WP-1: disconnected with a reconnect loop running.
    pub reconnecting: bool,
}

impl StatusModel {
    /// The bar's text.
    pub fn line(&self) -> String {
        let conn = if self.connected {
            "connected"
        } else if self.reconnecting {
            "reconnecting"
        } else {
            "disconnected"
        };
        let mode = match self.mode {
            Mode::Shadow => "shadow",
            Mode::Live => "live",
        };
        let mut s = format!("{conn} · {mode} · gen {}", self.generation);
        if let Some(j) = &self.last_job {
            s.push_str(" · job ");
            s.push_str(j);
        }
        if let Some(e) = &self.error {
            s.push_str(" · ");
            s.push_str(e);
        }
        s
    }
}

/// The notification receiver, handed once to the iced subscription.
/// The connection epoch is part of its identity (plan editor-data-safety WP-1): a reconnect
/// bumps it, so iced drops the old subscription and runs a new one over the new receiver.
#[derive(Clone, Default)]
pub struct NotifHandle(Arc<Mutex<Option<Receiver<Notification>>>>, u64);

impl NotifHandle {
    /// Wraps a receiver (epoch 0).
    pub fn new(rx: Option<Receiver<Notification>>) -> Self {
        Self(Arc::new(Mutex::new(rx)), 0)
    }

    /// A reconnect's receiver at connection `epoch`.
    pub fn shared(rx: Arc<Mutex<Option<Receiver<Notification>>>>, epoch: u64) -> Self {
        Self(rx, epoch)
    }

    /// The connection epoch.
    pub fn epoch(&self) -> u64 {
        self.1
    }
}

impl Hash for NotifHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "daemon-notifications".hash(state);
        self.1.hash(state);
    }
}

/// Reconnect backoff (plan editor-data-safety WP-1): 0.5 s doubling, capped at 5 s.
pub fn reconnect_delay(attempt: u32) -> std::time::Duration {
    let ms = 500u64.saturating_mul(1u64 << attempt.min(4));
    std::time::Duration::from_millis(ms.min(5_000))
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
    pub bookmarks: Bookmarks,
    /// Tests: run RPC calls inline instead of on the blocking pool.
    pub rpc_inline: bool,
    /// Tests: the injected "today" for nav (`None` = local date).
    pub today: Option<NaiveDate>,
    /// Tests: every `on_tab_closed` path, in order.
    #[cfg(test)]
    pub closed: Vec<String>,
    /// WP-3: per-tab reading state (mode, render cache, image handles), keyed by path.
    pub reading: crate::reading::ReadingStore,
    /// WP-5: command palette, action invocation and the job toast.
    pub palette: crate::palette::Palette,
    /// WP-5: the right-hand side panels.
    pub panels: crate::panels::Panels,
    /// WP-7: `crucible-view` fence results, keyed by `(view id, embedding note)`.
    pub views: crate::views::ViewStore,
    tokens: Tokens,
    notifications: NotifHandle,
    /// plan editor-data-safety WP-1: opens a fresh client (`None` = never reconnect).
    connector: Option<crate::daemon::Connector>,
    /// Failed reconnect attempts since the last disconnect (the backoff exponent).
    reconnect_attempt: u32,
    state_path: Option<PathBuf>,
    /// `App::new` is restoring tabs: their first reading render waits for the debounce tick.
    restoring: bool,
    /// editor-usability-round WP-3: the pane whose view-mode dropdown is open.
    pub mode_menu: Option<Pane>,
    /// plan editor-round-4 WP-4: a mouse button is held (hover focus is suppressed).
    pub mouse_down: bool,
    /// A back/forward step is opening a path: it must not push onto the history.
    history_nav: bool,
    /// editor-usability-round WP-4: a quit waiting on these paths' saves (`None` = no quit).
    pub(crate) quit: Option<QuitState>,
    /// plan editor-data-safety WP-2: the open modal dialog (input priority while `Some`).
    pub dialog: Option<crate::dialog::Dialog>,
    /// Notes whose close waits on an in-flight normal save (WP-3 rework).
    /// Keyed by path; the values are the ids of the tabs to close (audit E1: a close keeps its
    /// target tab identity across the save, so closing one duplicate never closes the other).
    closing: std::collections::HashMap<String, Vec<u64>>,
    /// Plan editor-feedback-round-2 WP-7: one source buffer per open note path.
    pub docs: Docs,
    /// The app asked iced to exit (tests observe it).
    pub exiting: bool,
    /// Shift is held while a find bar is open (Shift+Enter = previous).
    find_shift: bool,
    /// plan editor-settings-tab WP-2: the Settings tab's model (file save loops, Status).
    pub settings: crate::settings::Settings,
    /// The effective `editor.toml` prefs (live; `prefs.keys` feeds the palette keymap).
    pub prefs: crate::settings::editor_prefs::Prefs,
    /// plan editor-settings-tab WP-4 (audit E2): the day / month note folders from the loaded
    /// `captures.toml` (constants when absent / unloaded / malformed).
    pub period_folders: editor_model::period::PeriodFolders,
    /// Frontmatter `title` per note path, keyed by the buffer's edit seq (plan
    /// editor-feedback-round-2 WP-6: the `tab_title = "title"` source; recomputed only when the
    /// buffer changed, so drawing the tab bar never re-parses an unchanged note).
    fm_titles: std::cell::RefCell<FmTitleCache>,
    /// plan editor-ui-round-3 WP-4: the open right-click menu (one at a time).
    pub context_menu: Option<crate::context_menu::ContextMenu>,
    /// Tests: every "Copy path" text, in order.
    #[cfg(test)]
    pub copied: Vec<String>,
    /// Tests: every "Copy image" vault path, in order.
    #[cfg(test)]
    pub copied_images: Vec<String>,
    /// plan editor-round-4 WP-4: the image clipboard writer (`xclip` in production).
    pub clip_writer: Arc<dyn crate::clipboard_image::ClipboardWriter + Send + Sync>,
}

/// Frontmatter `title` per tab path, keyed by `(edit_seq, epoch)` (plan editor-feedback-round-2 WP-6).
type FmTitleCache = std::collections::HashMap<String, ((u64, u64), Option<String>)>;

/// A quit in progress: the saves it waits on, and whether one failed.
#[derive(Debug, Default)]
pub(crate) struct QuitState {
    pending: std::collections::HashSet<String>,
    /// Settings files whose flush write has not landed yet (plan editor-feedback-round-2 WP-2).
    settings: std::collections::HashSet<String>,
    /// Notes whose normal (autosave/flush) save was already in flight at quit; resolved by
    /// their `SourceMsg::Saved` (plan editor-feedback-round-2 WP-3 rework).
    awaiting: std::collections::HashSet<String>,
    /// plan editor-data-safety WP-2: what cannot be saved (shown in the dialog once every
    /// flush has resolved).
    losses: crate::dialog::UnsavedEdits,
}

/// `captures.toml` (period folders, WP-4 audit E2).
pub const CAPTURES_PATH: &str = "_custom/crucible-vault/captures.toml";

/// The day / month note folders from the loaded `captures.toml` (`[periods.day].folder`,
/// `[periods.month].folder`); constants when absent, unloaded, malformed or not a string.
/// `[periods.week]` does not feed the editor.
pub fn period_folders(
    f: Option<&crate::settings::ConfigFile>,
) -> editor_model::period::PeriodFolders {
    let doc = f.and_then(|f| f.doc.as_ref());
    let get = |p: &str| {
        let path = cv_schema::Path::parse(p).ok()?;
        doc?.get_str(&path)
    };
    editor_model::period::PeriodFolders::from_config(
        get("periods.day.folder"),
        get("periods.month.folder"),
    )
}

fn tokens_for(s: Surround) -> Tokens {
    match s.tokens() {
        Ok(t) => t,
        // Unreachable for the vendored blocks (unit-tested); fall back to Med, then to a
        // hard-coded neutral so a vendoring slip never crashes the UI.
        Err(_) => Surround::Med.tokens().unwrap_or_else(|_| neutral_tokens()),
    }
}

fn neutral_tokens() -> Tokens {
    use crate::theme::Rgb;
    let g = Rgb(0x3a, 0x3a, 0x3b);
    let w = Rgb(0xf3, 0xf3, 0xf4);
    Tokens {
        bg: g,
        panel: g,
        panel_2: g,
        inset: g,
        line: w,
        text: w,
        text_dim: w,
        muted: w,
        text_faint: w,
        heading: w,
        accent: w,
        accent_hover: w,
        accent_text: w,
        on_accent: g,
        sel_tint: g,
        ok_text: w,
        warn_text: w,
        error_text: w,
        info_text: w,
        red: w,
        orange: w,
        yellow: w,
        green: w,
        cyan: w,
        blue: w,
        purple: w,
        pink: w,
        accent_ui: w,
        accent_ui_hover: w,
        on_accent_ui: g,
    }
}

impl App {
    /// Builds the app over a connected daemon (`None` = disconnected), restoring UI state from
    /// `state_path` (`None` = don't persist, e.g. tests).
    pub fn new(daemon: Option<Arc<dyn DaemonClient>>, state_path: Option<PathBuf>) -> Self {
        let ui = state_path
            .as_deref()
            .map(ui_state::load)
            .unwrap_or_default();
        let (status, notifications) = match &daemon {
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
                        mode: h.mode,
                        generation: h.generation,
                        last_job: None,
                        error: None,
                        reconnecting: false,
                    },
                    NotifHandle::new(rx),
                )
            }
            None => (
                StatusModel {
                    connected: false,
                    mode: Mode::Shadow,
                    generation: 0,
                    last_job: None,
                    error: None,
                    reconnecting: false,
                },
                NotifHandle::default(),
            ),
        };
        let layout = ui
            .panes
            .clone()
            .unwrap_or_else(|| panes::from_flat(&ui.tabs, ui.active_tab));
        let (grid, plan, focus) = panes::restore(&layout);
        let mut app = Self {
            daemon,
            status,
            panes: grid,
            focus,
            tokens: tokens_for(ui.surround),
            ui,
            explorer: Explorer::default(),
            switcher: Switcher::default(),
            bookmarks: Bookmarks::default(),
            rpc_inline: false,
            today: None,
            #[cfg(test)]
            closed: Vec::new(),
            reading: crate::reading::ReadingStore::new(),
            palette: crate::palette::Palette::default(),
            panels: crate::panels::Panels::default(),
            views: crate::views::ViewStore::default(),
            notifications,
            connector: None,
            reconnect_attempt: 0,
            state_path,
            restoring: true,
            mode_menu: None,
            mouse_down: false,
            history_nav: false,
            quit: None,
            dialog: None,
            closing: std::collections::HashMap::new(),
            docs: Docs::new(),
            exiting: false,
            find_shift: false,
            settings: crate::settings::Settings::default(),
            prefs: crate::settings::editor_prefs::Prefs::default(),
            period_folders: editor_model::period::PeriodFolders::default(),
            fm_titles: std::cell::RefCell::default(),
            context_menu: None,
            #[cfg(test)]
            copied: Vec::new(),
            #[cfg(test)]
            copied_images: Vec::new(),
            clip_writer: Arc::new(crate::clipboard_image::Xclip),
        };
        app.load_editor_prefs();
        app.refresh_files();
        app.load_bookmarks();
        for (pane, paths, active) in plan {
            for p in &paths {
                if let Err(e) = app.open_into(pane, p, Place::Restore) {
                    app.status.error = Some(open_error(&e));
                }
            }
            if let Some(ps) = app.panes.get_mut(pane)
                && let Some(i) = active.filter(|&i| i < ps.tabs.len())
            {
                ps.active = Some(i);
            }
        }
        app.focus = focus;
        app.restoring = false;
        for (_, ps) in app.panes.iter_mut() {
            if let Some(p) = ps.active_note().map(|t| t.path.clone()) {
                ps.history.push(&p);
            }
        }
        app.sync_buffers();
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
            .filter(|t| self.reading.mode(&t.path).shows_source())
            .map(|t| t.path.clone())
            .collect();
        for (path, doc) in &mut self.docs {
            doc.set_shaped(visible.contains(path));
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        if ms >= 1.0 {
            tracing::debug!(elapsed_ms = ms, "sync_buffers");
        }
    }

    /// The mode a newly opened `path` starts in: its persisted mode, else Reading.
    fn initial_mode(&self, path: &str) -> crate::reading::ViewMode {
        self.ui
            .modes
            .get(path)
            .copied()
            .map(crate::reading::ViewMode::from)
            .unwrap_or_default()
    }

    /// A disconnected app (used if boot cannot take the prepared one).
    pub fn disconnected() -> Self {
        Self::new(None, None)
    }

    /// plan editor-data-safety WP-1: reconnect through `connector` whenever the connection is
    /// lost (or was never made).
    #[must_use]
    pub fn with_connector(mut self, connector: crate::daemon::Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    /// The connection epoch (bumped by every successful reconnect).
    pub fn connection_epoch(&self) -> u64 {
        self.notifications.epoch()
    }

    /// Starts the reconnect loop unless it is running or there is no connector.
    fn start_reconnect(&mut self) -> Task<Message> {
        if self.connector.is_none() || self.status.reconnecting {
            return Task::none();
        }
        self.status.reconnecting = true;
        self.reconnect_attempt = 0;
        self.schedule_reconnect()
    }

    fn schedule_reconnect(&mut self) -> Task<Message> {
        if self.rpc_inline {
            // Tests drive attempts with `StatusMsg::ReconnectDue`.
            return Task::none();
        }
        let delay = reconnect_delay(self.reconnect_attempt);
        Task::perform(async move { tokio::time::sleep(delay).await }, |()| {
            Message::Status(StatusMsg::ReconnectDue)
        })
    }

    fn try_reconnect(&mut self) -> Task<Message> {
        let Some(c) = self.connector.clone() else {
            return Task::none();
        };
        if !self.status.reconnecting {
            return Task::none();
        }
        if self.rpc_inline {
            let r = crate::daemon::connect(&c).map_err(|e| e.to_string());
            return self.update_status(StatusMsg::Connected(r));
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    crate::daemon::connect(&c).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            |r| Message::Status(StatusMsg::Connected(r)),
        )
    }

    /// Installs a fresh connection: new client, epoch bump (restarts the notification
    /// subscription), daemon state refetch, view/panel invalidation and open-document resync.
    /// Tasks still holding the old client fail with `Disconnected` and are not resurrected.
    fn install_connection(&mut self, conn: crate::daemon::Connection) -> Task<Message> {
        let h = conn.client.hello();
        tracing::info!(mode = ?h.mode, generation = h.generation, "reconnected to daemon");
        self.status.mode = h.mode;
        self.status.generation = h.generation;
        self.status.connected = true;
        self.status.reconnecting = false;
        self.status.error = None;
        self.reconnect_attempt = 0;
        let epoch = self.notifications.epoch() + 1;
        self.notifications = NotifHandle::shared(conn.rx, epoch);
        self.daemon = Some(conn.client);
        self.refresh_files();
        self.panels.invalidate();
        self.views.invalidate(&[]);
        self.palette.invalidate();
        self.palette.loading = false;
        let fetch = self.fetch_actions();
        let status = self.refresh_status_if_open();
        let paths: Vec<String> = self.docs.keys().cloned().collect();
        let saves: Vec<Task<Message>> = paths.iter().map(|p| self.resync_doc(p)).collect();
        Task::batch([fetch, status].into_iter().chain(saves))
    }

    /// Reconnect resync of one open document against the disk (audit E1: a document with a
    /// save in flight on the old connection waits for that save's completion).
    fn resync_doc(&mut self, path: &str) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let read_only = self.status.mode == Mode::Shadow;
        let Some(doc) = self.docs.get_mut(path) else {
            return Task::none();
        };
        if doc.is_saving() {
            doc.defer_resync();
            return Task::none();
        }
        match d.read_file(path) {
            Ok((bytes, hash)) => {
                let job = doc.resync(&bytes, hash, read_only);
                crate::source::spawn_save(Some(&d), job)
            }
            // plan editor-data-safety WP-2: a vanished file under edits → missing-file state.
            Err(DaemonError::NotFound(_)) => {
                doc.mark_missing();
                Task::none()
            }
            Err(e) => {
                tracing::warn!(error = %e, "reconnect resync read failed");
                Task::none()
            }
        }
    }

    /// The focused pane's active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.panes.get(self.focus).and_then(PaneState::active_tab)
    }

    /// The focused pane's active tab when it is a note (never the Settings tab).
    pub fn active_note(&self) -> Option<&Tab> {
        self.panes.get(self.focus).and_then(PaneState::active_note)
    }

    /// The document open at `path` (plan editor-feedback-round-2 WP-7).
    pub fn doc(&self, path: &str) -> Option<&crate::source::SourceBuffer> {
        self.docs.get(path)
    }

    /// The document open at `path`, mutably.
    pub fn doc_mut(&mut self, path: &str) -> Option<&mut crate::source::SourceBuffer> {
        self.docs.get_mut(path)
    }

    /// The focused pane's active note's document.
    pub fn active_doc(&self) -> Option<&crate::source::SourceBuffer> {
        self.active_note().and_then(|t| self.docs.get(&t.path))
    }

    /// The focused pane's active note's document, mutably.
    pub fn active_doc_mut(&mut self) -> Option<&mut crate::source::SourceBuffer> {
        let path = self.active_note()?.path.clone();
        self.docs.get_mut(&path)
    }

    /// Tests: the note saves a quit in progress waits on (started + in flight), per document.
    #[cfg(test)]
    pub(crate) fn quit_waits(&self) -> Option<usize> {
        self.quit
            .as_ref()
            .map(|q| q.pending.len() + q.awaiting.len())
    }

    /// Whether the document at `path` has unsaved edits (false when none is open).
    fn doc_dirty(&self, path: &str) -> bool {
        self.docs.get(path).is_some_and(|d| d.is_dirty())
    }

    /// Whether any tab shows `path`.
    fn path_open(&self, path: &str) -> bool {
        panes::find(&self.panes, path).is_some()
    }

    /// The iced scale factor (`editor.toml` `ui_scale`, clamped 0.5–3.0).
    pub fn scale_factor(&self) -> f32 {
        let (lo, hi) = crate::settings::editor_prefs::UI_SCALE_RANGE;
        self.prefs.ui_scale.clamp(lo, hi)
    }

    /// Every open tab path, pane by pane in layout order.
    pub fn open_paths(&self) -> Vec<String> {
        panes::order(&self.panes)
            .into_iter()
            .filter_map(|p| self.panes.get(p))
            .flat_map(|s| s.tabs.iter().map(|t| t.path.clone()))
            .collect()
    }

    fn today(&self) -> NaiveDate {
        self.today
            .unwrap_or_else(|| chrono::Local::now().date_naive())
    }

    /// Index membership for explorer inserts (plan editor-ui-round-3 WP-2, audit B1). Without
    /// a daemon (tests without one) nothing is held.
    fn indexed_fn(&self) -> impl Fn(&str) -> bool + use<> {
        let d = self.daemon.clone();
        move |p: &str| d.as_ref().is_some_and(|d| d.index().holds(p))
    }

    fn refresh_files(&mut self) {
        let Some(d) = &self.daemon else { return };
        if let Err(e) = self.explorer.build(d.index(), &self.ui.expanded) {
            self.status.error = Some(e);
        }
        if self.switcher.open {
            self.switcher.refresh(&self.explorer.paths, &self.ui.recent);
        }
    }

    /// (Re)loads `bookmarks.toml` into its shared settings [`ConfigFile`] (unless the Bookmarks
    /// page holds unsaved edits) and re-derives the panel list.
    fn load_bookmarks(&mut self) {
        let Some(d) = self.daemon.clone() else { return };
        if let Some(f) = self.settings.file_mut(BOOKMARKS_PATH) {
            if !f.dirty {
                f.load(d.as_ref());
            }
            if let crate::settings::LoadState::Malformed(e) | crate::settings::LoadState::Error(e) =
                &f.state
            {
                self.status.error = Some(format!("bookmarks: {e}"));
            }
        }
        self.sync_bookmarks();
    }

    /// Runs `call` against the daemon off the UI thread and feeds `done` its result.
    fn rpc<X: Send + 'static>(
        &mut self,
        call: impl FnOnce(&dyn DaemonClient) -> Result<X, String> + Send + 'static,
        done: impl FnOnce(Result<X, String>) -> Message + Send + 'static,
    ) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            self.status.error = Some("not connected".into());
            return Task::none();
        };
        if self.rpc_inline {
            let r = call(d.as_ref());
            return self.update(done(r));
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || call(d.as_ref()))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            done,
        )
    }

    /// Opens `path` into the focused pane (focusing an existing tab anywhere). The read is a
    /// local disk read of one note, done inline; RPC calls never run here.
    fn open(&mut self, path: &str, persist: bool) {
        self.open_with(path, Place::Focus, persist);
    }

    /// [`Self::open`] with an explicit placement (Quick Switch modes).
    fn open_with(&mut self, path: &str, how: Place, persist: bool) {
        if let Err(e) = self.open_into(self.focus, path, how) {
            self.status.error = Some(open_error(&e));
            return;
        }
        if persist {
            self.persist();
        }
    }

    /// Focuses tab `i` of `pane` (recording the visit).
    fn focus_tab(&mut self, pane: Pane, i: usize, path: &str) {
        self.focus = pane;
        if let Some(ps) = self.panes.get_mut(pane) {
            ps.active = Some(i);
        }
        self.record_visit(pane, path);
    }

    /// Opens `path` in `pane` per `how` (see [`Place`]). A note open nowhere is read into a new
    /// document; a further tab of an open note shares its document (plan
    /// editor-feedback-round-2 WP-7).
    fn open_into(&mut self, pane: Pane, path: &str, how: Place) -> Result<(), DaemonError> {
        if path == crate::settings::SETTINGS_TAB {
            self.open_settings_into(pane);
            return Ok(());
        }
        let pane = if self.panes.get(pane).is_some() {
            pane
        } else {
            self.focus
        };
        if how != Place::Restore {
            // In every mode a tab already in the target pane is just focused.
            let here = self
                .panes
                .get(pane)
                .and_then(|ps| ps.tabs.iter().position(|t| t.path == path));
            if let Some(i) = here {
                self.focus_tab(pane, i, path);
                return Ok(());
            }
            match (how, panes::find(&self.panes, path)) {
                (Place::Focus | Place::Replace, Some((p, i))) => {
                    self.focus_tab(p, i, path);
                    return Ok(());
                }
                (Place::Move, Some((p, i))) => {
                    // The tab (and so its shared document: edits, undo) moves pane.
                    let Some(tab) = self.panes.get_mut(p).and_then(|ps| ps.remove(i)) else {
                        return Ok(());
                    };
                    if let Some(ps) = self.panes.get_mut(pane) {
                        ps.tabs.push(tab);
                        let n = ps.tabs.len() - 1;
                        self.focus_tab(pane, n, path);
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        if !self.docs.contains_key(path) {
            let Some(d) = &self.daemon else {
                return Err(DaemonError::Disconnected);
            };
            let (bytes, hash) = d.read_file(path)?;
            let doc = crate::source::SourceBuffer::new(path, &bytes, hash);
            let mode = self.initial_mode(path);
            self.reading
                .register(&doc, mode, !self.restoring, self.daemon.as_ref());
            self.docs.insert(path.to_owned(), doc);
        }
        let tab = Tab::new(path);
        let mut replaced = None;
        if let Some(ps) = self.panes.get_mut(pane) {
            let active = ps.active.filter(|&i| i < ps.tabs.len());
            match active {
                Some(i)
                    if how == Place::Replace
                        && !self
                            .docs
                            .get(&ps.tabs[i].path)
                            .is_some_and(|d| d.is_dirty()) =>
                {
                    replaced = Some(std::mem::replace(&mut ps.tabs[i], tab).path);
                }
                _ => {
                    ps.tabs.push(tab);
                    ps.active = Some(ps.tabs.len() - 1);
                }
            }
        }
        self.focus = pane;
        if let Some(old) = replaced {
            self.on_tab_closed(&old);
        }
        self.record_visit(pane, path);
        self.ui.touch_recent(path);
        Ok(())
    }

    /// Pushes `path` onto `pane`'s history (not while restoring or stepping back/forward; never
    /// the Settings tab).
    fn record_visit(&mut self, pane: Pane, path: &str) {
        if self.restoring || self.history_nav || path == crate::settings::SETTINGS_TAB {
            return;
        }
        if let Some(ps) = self.panes.get_mut(pane) {
            ps.history.push(path);
        }
    }

    /// Back (`forward = false`) or forward in `pane`'s history: the target path is focused or
    /// reopened in that pane without pushing.
    fn history_step(&mut self, pane: Pane, forward: bool) {
        let Some(ps) = self.panes.get_mut(pane) else {
            return;
        };
        let target = if forward {
            ps.history.forward()
        } else {
            ps.history.back()
        };
        let Some(path) = target.map(str::to_owned) else {
            return;
        };
        self.history_nav = true;
        let r = self.open_into(pane, &path, Place::Focus);
        self.history_nav = false;
        if let Err(e) = r {
            self.status.error = Some(open_error(&e));
        }
        self.persist();
    }

    /// The single tab-body render site (WP-3 swaps this expression for the mode switch).
    fn tab_body<'a>(&'a self, tab: &'a Tab) -> Element<'a, Message> {
        if tab.is_settings() {
            return crate::settings::view(
                &self.settings,
                &self.prefs,
                &self.explorer.paths,
                self.tokens,
                self.status.mode,
            );
        }
        let Some(doc) = self.docs.get(&tab.path) else {
            return iced::widget::Space::new().into();
        };
        crate::reading::tab_body(
            &self.reading,
            &self.views,
            doc,
            tab.id,
            self.tokens,
            self.status.mode,
            self.prefs.font_size,
            &self.palette.keymap,
        )
    }

    /// Every tab removal funnels through here (button, pane close, replace, trash). The
    /// document and its reading/view state are dropped only with the last tab showing `path`
    /// (plan editor-feedback-round-2 WP-7).
    fn on_tab_closed(&mut self, path: &str) {
        #[cfg(test)]
        self.closed.push(path.to_owned());
        if self.path_open(path) {
            return;
        }
        self.docs.remove(path);
        self.closing.remove(path);
        self.reading.on_tab_closed(path);
        self.views.on_tab_closed(path);
    }

    /// `file_changed`: echo / silent reload / conflict bar (WP-2 `source::file_changed`).
    fn reload_tab(&mut self, path: &str, hash: Option<&str>) {
        let daemon = self.daemon.as_ref();
        if let Some(e) = crate::source::file_changed(&mut self.docs, path, hash, daemon) {
            self.status.error = Some(e);
        }
    }

    fn persist(&mut self) {
        let layout = panes::snapshot(&self.panes, self.focus);
        if let Some(ps) = self.panes.get(self.focus) {
            self.ui.tabs = ps.tabs.iter().map(|t| t.path.clone()).collect();
            self.ui.active_tab = ps.active;
        }
        self.ui.panes = Some(layout);
        self.ui.modes = self
            .panes
            .iter()
            .flat_map(|(_, ps)| ps.tabs.iter())
            .filter(|t| !t.is_settings())
            .map(|t| (t.path.clone(), self.reading.mode(&t.path).into()))
            .collect();
        let Some(path) = &self.state_path else { return };
        let vault = self.daemon.as_ref().map(|d| d.vault_root().to_path_buf());
        if let Err(e) = ui_state::save(path, &self.ui, vault.as_deref()) {
            tracing::warn!(error = %e, "saving editor.json failed");
        }
    }

    fn shadow_blocked(&mut self, reason: &str) -> bool {
        if self.status.mode == Mode::Shadow {
            self.status.error = Some(reason.to_owned());
            return true;
        }
        false
    }

    /// The update function.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let Some(message) = self.modal_filter(message) else {
            return Task::none();
        };
        let before = self.active_paths();
        let task = self.update_inner(message);
        if self.dialog.is_some() {
            // The dialog outranks the menu: never leave one open beneath it.
            self.context_menu = None;
        }
        let flush = self.flush_switched_away(&before);
        self.sync_buffers();
        let auto = self.schedule_settings_autosave();
        let extra: Vec<Task<Message>> = [flush, auto]
            .into_iter()
            .filter(|t| t.units() > 0)
            .collect();
        if extra.is_empty() {
            return task;
        }
        Task::batch(std::iter::once(task).chain(extra))
    }

    /// plan editor-data-safety WP-2 (audit E3): while a dialog is open it takes input
    /// priority — user input (keys, chords, tab / editor / find / switcher messages, the
    /// focus-loss flush, a repeated close request) is dropped; async completions (saves,
    /// notifications, loads) still run. Escape (the switcher's Close) is the dialog's Cancel.
    ///
    /// plan editor-ui-round-3 WP-4: an open context menu ranks next (below the dialog, above
    /// pane keys): the same user input is dropped, and Escape is the menu's Close.
    fn modal_filter(&self, m: Message) -> Option<Message> {
        use crate::context_menu::MenuMsg;
        if self.dialog.is_some() {
            return match m {
                Message::Switcher(SwitcherMsg::Close | SwitcherMsg::Escape) => {
                    Some(Message::Dialog(crate::dialog::DialogMsg::Cancel))
                }
                // WP-5: Escape is the dialog's (via the switcher listener), never the palette's.
                Message::Palette(PaletteMsg::Escape) => None,
                Message::Menu(MenuMsg::Open(..) | MenuMsg::Choose(_)) => None,
                m if is_modal_blocked(&m) => None,
                m => Some(m),
            };
        }
        if matches!(m, Message::Dialog(_)) {
            return None;
        }
        if self.context_menu.is_some() {
            return match m {
                Message::Switcher(SwitcherMsg::Close | SwitcherMsg::Escape) => {
                    Some(Message::Menu(MenuMsg::Close))
                }
                Message::Palette(PaletteMsg::Escape) => None,
                m if is_modal_blocked(&m) => None,
                m => Some(m),
            };
        }
        Some(m)
    }

    /// WP-4: the right-click menu.
    fn update_menu(&mut self, m: crate::context_menu::MenuMsg) -> Task<Message> {
        use crate::context_menu::{ContextMenu, MenuMsg};
        match m {
            MenuMsg::Open(target, at) => {
                self.mode_menu = None;
                self.context_menu = Some(ContextMenu { at, target });
            }
            MenuMsg::Close => self.context_menu = None,
            MenuMsg::Choose(item) => {
                if let Some(menu) = self.context_menu.take() {
                    return self.run_menu_item(menu.target, item);
                }
            }
            MenuMsg::ImageCopied(r) => {
                if let Err(e) = r {
                    self.status.error = Some(e);
                }
            }
        }
        Task::none()
    }

    /// Runs a chosen menu item on its target.
    fn run_menu_item(
        &mut self,
        target: crate::context_menu::Target,
        item: crate::context_menu::Item,
    ) -> Task<Message> {
        use crate::context_menu::{Item, Target};
        match (target, item) {
            (Target::Task { path, line }, Item::TaskStatus(c)) => {
                self.set_task_status(&path, line, c)
            }
            (Target::Tab { pane, id, .. }, Item::Close) => {
                match self
                    .panes
                    .get(pane)
                    .and_then(|ps| ps.tabs.iter().position(|t| t.id == id))
                {
                    Some(i) => self.close_tab(pane, i),
                    None => Task::none(),
                }
            }
            (Target::Tab { pane, id, .. }, Item::CloseOthers) => {
                let others: Vec<u64> = self
                    .panes
                    .get(pane)
                    .map(|ps| ps.tabs.iter().map(|t| t.id).filter(|t| *t != id).collect())
                    .unwrap_or_default();
                let mut tasks = Vec::new();
                for other in others {
                    let i = self
                        .panes
                        .get(pane)
                        .and_then(|ps| ps.tabs.iter().position(|t| t.id == other));
                    if let Some(i) = i {
                        tasks.push(self.close_tab(pane, i));
                    }
                }
                Task::batch(tasks)
            }
            (Target::Tab { pane, id, path }, Item::Rename) => {
                let i = self
                    .panes
                    .get(pane)
                    .and_then(|ps| ps.tabs.iter().position(|t| t.id == id));
                if let Some(i) = i {
                    self.focus_tab(pane, i, &path);
                }
                iced::widget::operation::focus(crate::reading::title_id(&path))
            }
            (Target::Tab { path, .. } | Target::Row { path, .. }, Item::Reveal) => {
                self.reveal(&path);
                Task::none()
            }
            (Target::Image { src, remote: false }, Item::CopyImage) => {
                #[cfg(test)]
                self.copied_images.push(src.clone());
                let Some(d) = self.daemon.clone() else {
                    self.status.error = Some("copy image: no daemon connection".into());
                    return Task::none();
                };
                let writer = self.clip_writer.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::clipboard_image::copy_image(d.as_ref(), writer.as_ref(), &src)
                        })
                        .await
                        .unwrap_or_else(|_| Err("copy image: worker failed".into()))
                    },
                    |r| Message::Menu(crate::context_menu::MenuMsg::ImageCopied(r)),
                )
            }
            (Target::Image { src, remote: true }, Item::CopyUrl)
            | (Target::Image { src, remote: false }, Item::CopyPath)
            | (Target::Tab { path: src, .. } | Target::Row { path: src, .. }, Item::CopyPath) => {
                #[cfg(test)]
                self.copied.push(src.clone());
                iced::clipboard::write(src)
            }
            (Target::Row { path, .. }, Item::Move) => {
                self.explorer.selected = Some(path);
                self.update_explorer(ExplorerMsg::BeginMove)
            }
            (Target::Tab { path, .. } | Target::Row { path, .. }, Item::Delete) => {
                self.begin_delete(path)
            }
            (Target::Row { path, .. }, Item::Open) => self.update_explorer(ExplorerMsg::Open(path)),
            (Target::Row { path, .. }, Item::OpenInNewPane) => {
                self.explorer.selected = Some(path.clone());
                if let Some((p, _)) = self.panes.split(
                    panes::axis_of(SplitDir::Right),
                    self.focus,
                    PaneState::default(),
                ) {
                    self.focus = p;
                }
                self.open_with(&path, Place::Duplicate, true);
                Task::none()
            }
            (Target::Row { path, .. }, Item::Rename) => {
                self.explorer.selected = Some(path);
                self.update_explorer(ExplorerMsg::BeginRename)
            }
            (Target::Row { path, .. }, Item::NewNote) => {
                self.explorer.selected = Some(path);
                self.update_explorer(ExplorerMsg::BeginNew)
            }
            _ => Task::none(),
        }
    }

    /// Reveal in explorer: open the sidebar, expand the note's ancestor folders, select it.
    /// (The virtual list has no scroll-to operation, so the row is not scrolled into view.)
    fn reveal(&mut self, path: &str) {
        self.ui.side_collapsed = false;
        let mut p = path;
        while let Some((parent, _)) = p.rsplit_once('/') {
            self.ui.expanded.insert(parent.to_owned());
            p = parent;
        }
        self.explorer.selected = Some(path.to_owned());
        self.explorer.relayout(&self.ui.expanded);
        self.persist();
    }

    /// The dialog's OK / Cancel.
    fn update_dialog(&mut self, m: crate::dialog::DialogMsg) -> Task<Message> {
        use crate::dialog::{DialogMsg, OnOk};
        if let DialogMsg::Toggle(n) = m {
            if let Some(c) = self.dialog.as_mut().and_then(|d| d.delete_confirm_mut()) {
                c.toggle(n);
            }
            return Task::none();
        }
        let Some(d) = self.dialog.take() else {
            return Task::none();
        };
        let ok = match m {
            DialogMsg::Ok => true,
            DialogMsg::Cancel | DialogMsg::Toggle(_) => false,
            DialogMsg::Default => d.enter_is_ok,
        };
        if !ok {
            return Task::none();
        }
        match d.on_ok {
            OnOk::Quit => self.exit(),
            OnOk::CloseTabs(ids) => {
                for id in ids {
                    if let Some((p, i)) = panes::find_id(&self.panes, id) {
                        self.remove_tab(p, i);
                    }
                }
                Task::none()
            }
            OnOk::DeleteNote => match d.delete_confirm() {
                Some(c) => self.run_delete_note(c.path.clone(), c.ticked(), c.mode),
                None => Task::none(),
            },
        }
    }

    /// plan editor-ui-round-3 WP-3: "Delete note…" / explorer Trash. Fetches the preview, then
    /// opens the always-confirm dialog. A note with unsaved edits is refused with a notice
    /// (save or close it first), so the delete never drops edits silently.
    fn begin_delete(&mut self, path: String) -> Task<Message> {
        if self.shadow_blocked(explorer::SHADOW_REASON) {
            return Task::none();
        }
        if self.doc_dirty(&path) {
            self.status.error = Some("save the open note first (it has unsaved edits)".into());
            return Task::none();
        }
        let p = path.clone();
        self.rpc(
            move |d| d.delete_preview(&p).map_err(|e| e.to_string()),
            move |result| Message::Explorer(ExplorerMsg::DeletePreview { path, result }),
        )
    }

    /// Dialog OK: `delete_note` with the ticked items in the `editor.toml` `delete_mode`.
    fn run_delete_note(
        &mut self,
        path: String,
        attachments: Vec<String>,
        mode: crate::daemon::DeleteMode,
    ) -> Task<Message> {
        if self.shadow_blocked(explorer::SHADOW_REASON) {
            return Task::none();
        }
        if self.doc_dirty(&path) {
            self.status.error = Some("save the open note first (it has unsaved edits)".into());
            return Task::none();
        }
        self.rpc(
            move |d| {
                d.delete_note(&path, &attachments, mode)
                    .map_err(|e| e.to_string())
            },
            |r| Message::Explorer(ExplorerMsg::NoteDeleted(r)),
        )
    }

    /// Why `path`'s edits could not be saved (`fallback` when no conflict / outage explains it).
    fn loss_reason(&self, path: &str, fallback: &str) -> crate::dialog::LossReason {
        use crate::dialog::LossReason;
        match self.docs.get(path) {
            Some(d) if d.is_missing() => LossReason::Missing,
            Some(d) if d.conflict().is_some() => LossReason::Conflict,
            _ if !self.status.connected => LossReason::Disconnected,
            _ => LossReason::SaveFailed(fallback.to_owned()),
        }
    }

    /// Opens the unsaved-edits dialog for one note's tab close.
    fn close_dialog(&mut self, path: &str, ids: Vec<u64>, fallback: &str) {
        let mut losses = crate::dialog::UnsavedEdits::default();
        losses.note(path, self.loss_reason(path, fallback));
        self.dialog = Some(crate::dialog::Dialog::unsaved(
            losses,
            crate::dialog::OnOk::CloseTabs(ids),
        ));
    }

    /// Each pane's active tab path (WP-3 tab-switch flush snapshot).
    fn active_paths(&self) -> Vec<(Pane, String)> {
        self.panes
            .iter()
            .filter_map(|(p, ps)| ps.active_tab().map(|t| (*p, t.path.clone())))
            .collect()
    }

    /// Plan editor-feedback-round-2 WP-3: every path that changes a pane's active tab (select,
    /// Alt+N, open/Quick Switch, history, closing a neighbour) runs through `update`, so one
    /// before/after comparison here flushes the note a pane switched away from — no per-path
    /// save calls. The tab must still exist (a closed tab already went through `close_tab`).
    fn flush_switched_away(&mut self, before: &[(Pane, String)]) -> Task<Message> {
        let left: Vec<String> = before
            .iter()
            .filter(|(p, path)| {
                self.panes
                    .get(*p)
                    .and_then(|ps| ps.active_tab())
                    .is_none_or(|t| &t.path != path)
            })
            .map(|(_, path)| path.clone())
            .collect();
        Task::batch(left.iter().map(|path| self.flush_note(path)))
    }

    /// Flushes the note tab at `path` now (no-op when clean, saving — re-queued — conflicted,
    /// in shadow mode, or not open).
    fn flush_note(&mut self, path: &str) -> Task<Message> {
        let read_only = self.status.mode == Mode::Shadow;
        match self.docs.get_mut(path) {
            Some(doc) => crate::source::flush(doc, self.daemon.as_ref(), read_only),
            None => Task::none(),
        }
    }

    /// WP-3: window focus loss flushes every dirty note buffer.
    fn flush_all_notes(&mut self) -> Task<Message> {
        let dirty: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, d)| d.is_dirty())
            .map(|(p, _)| p.clone())
            .collect();
        Task::batch(dirty.iter().map(|path| self.flush_note(path)))
    }

    /// Plan editor-feedback-round-2 WP-2: arms one debounce timer per settings file whose
    /// latest edit left it valid (every edit path — form, prefs, hotkeys, theme cycle — ends
    /// here). Shadow mode and a missing daemon arm nothing (the file stays dirty).
    fn schedule_settings_autosave(&mut self) -> Task<Message> {
        if self.status.mode == Mode::Shadow || self.daemon.is_none() {
            return Task::none();
        }
        let due = self.settings.due_autosaves();
        Task::batch(due.into_iter().map(|(path, seq)| {
            Task::perform(
                // Built lazily inside the async block (it needs the executor's reactor).
                async { tokio::time::sleep(crate::settings::AUTOSAVE_DELAY).await },
                move |()| {
                    Message::Settings(crate::settings::SettingsMsg::AutoSaveDue(path.clone(), seq))
                },
            )
        }))
    }

    /// Writes settings file `path` now when it is valid and dirty; behind an in-flight write it
    /// is queued (re-run when that write lands).
    fn save_settings_file(&mut self, path: String) -> Task<Message> {
        use crate::settings::SettingsMsg;
        if self.status.mode == Mode::Shadow {
            return Task::none();
        }
        let Some(f) = self.settings.file_mut(&path) else {
            return Task::none();
        };
        if f.saving {
            f.queued |= f.wants_save();
            return Task::none();
        }
        let Some(job) = f.begin_save() else {
            return Task::none();
        };
        self.rpc(
            move |d| job.run(d),
            move |r| Message::Settings(SettingsMsg::Saved(path, r)),
        )
    }

    /// Flushes every pending settings write (Settings tab close, quit). Returns the paths now
    /// writing (started or queued) and the task.
    fn flush_settings(&mut self) -> (Vec<String>, Task<Message>) {
        let paths: Vec<String> = self
            .settings
            .files
            .values()
            .filter(|f| f.wants_save() || f.saving)
            .map(|f| f.path.clone())
            .collect();
        if self.status.mode == Mode::Shadow {
            return (Vec::new(), Task::none());
        }
        let tasks: Vec<Task<Message>> = paths
            .iter()
            .map(|p| self.save_settings_file(p.clone()))
            .collect();
        (paths, Task::batch(tasks))
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tabs(m) => return self.update_tabs(m),
            Message::Dialog(m) => return self.update_dialog(m),
            Message::Menu(m) => return self.update_menu(m),
            Message::Find(m) => return self.update_find(m),
            Message::Status(m) => return self.update_status(m),
            Message::Theme(m) => {
                let s = match m {
                    ThemeMsg::Cycle => self.prefs.surround.next(),
                    ThemeMsg::Set(s) => s,
                };
                self.set_surround(s);
            }
            Message::Settings(m) => return self.update_settings(m),
            Message::Explorer(m) => return self.update_explorer(m),
            Message::Source(SourceMsg::TakeTheirs(path))
                if self.docs.get(&path).is_some_and(|d| d.is_missing()) =>
            {
                // plan editor-data-safety WP-2: the file is gone — discard and close its tabs.
                while let Some((p, i)) = panes::find(&self.panes, &path) {
                    self.remove_tab(p, i);
                }
                self.docs.remove(&path);
            }
            Message::Source(m) => {
                // plan editor-round-4 WP-2: Shift+click extends the selection.
                let m = crate::source::shift_click(m, self.switcher.shift);
                let routed = matches!(
                    &m,
                    SourceMsg::AutosaveDue { .. }
                        | SourceMsg::Saved { .. }
                        | SourceMsg::KeepMine(_)
                        | SourceMsg::TakeTheirs(_)
                        | SourceMsg::MiddleClick { .. }
                        | SourceMsg::CommandClick { .. }
                        | SourceMsg::PastePrimary { .. }
                        | SourceMsg::PasteResolved { .. }
                );
                let active = self.active_note().map(|t| t.path.clone());
                let saved = match &m {
                    SourceMsg::Saved { path, outcome, .. } => Some((
                        path.clone(),
                        matches!(outcome, crate::source::SaveOutcome::Saved(_)),
                    )),
                    _ => None,
                };
                if !routed && active.is_none() {
                    return Task::none();
                }
                // A save for a document no longer open resolves any wait as landed.
                let saved = saved.map(|(p, ok)| {
                    let ok = ok || !self.docs.contains_key(&p);
                    (p, ok)
                });
                let task = crate::source::update(
                    &mut self.docs,
                    active.as_deref(),
                    m,
                    crate::source::Ctx {
                        daemon: self.daemon.as_ref(),
                        mode: self.status.mode,
                        error: &mut self.status.error,
                    },
                );
                // plan editor-data-safety WP-1 (audit E1): a reconnect that arrived while this
                // save was in flight resyncs now. A failed completion's queued `save_again` is
                // not relaunched by `source::update`, so the resync's fresh save covers it; a
                // follow-up save already started (success path) runs on the current client.
                let resync = match &saved {
                    Some((path, _))
                        if self
                            .docs
                            .get_mut(path)
                            .is_some_and(|d| d.take_resync_pending() && !d.is_saving()) =>
                    {
                        let path = path.clone();
                        self.resync_doc(&path)
                    }
                    _ => Task::none(),
                };
                // Plan usability-sprint-captures-links WP-5: a landed save that changed `title`.
                let sync = match &saved {
                    Some((path, true)) => {
                        let path = path.clone();
                        self.title_sync(&path)
                    }
                    _ => Task::none(),
                };
                let task = Task::batch([task, resync, sync]);
                return match saved {
                    Some((path, ok)) => Task::batch([task, self.note_save_landed(&path, ok)]),
                    None => task,
                };
            }
            Message::TitleRenamed {
                from,
                to,
                seq,
                result,
            } => return self.title_renamed(&from, &to, seq, result),
            Message::OpenUrl(url) => {
                if let Err(e) = crate::open_url::open(&self.prefs.open_url_cmd, &url) {
                    self.status.error = Some(e);
                }
            }
            Message::Reading(crate::message::ReadingMsg::ToggleTask { path, line }) => {
                return self.toggle_task(&path, line);
            }
            Message::Reading(crate::message::ReadingMsg::ToggleProp { path, key }) => {
                return self.toggle_prop(&path, &key);
            }
            Message::Reading(crate::message::ReadingMsg::GoToEdge { bottom }) => {
                return self.go_to_edge(bottom);
            }
            Message::Reading(crate::message::ReadingMsg::TitleSubmit { path }) => {
                return self.title_submit(&path);
            }
            Message::Reading(crate::message::ReadingMsg::TitleFocus { path, focused }) => {
                if !focused {
                    return self.title_submit(&path);
                }
            }
            Message::Reading(crate::message::ReadingMsg::TitleSaved {
                path,
                to,
                seq,
                outcome,
            }) => return self.title_saved(&path, &to, seq, outcome),
            Message::Reading(m) => {
                let mode_change = matches!(
                    m,
                    crate::message::ReadingMsg::CycleMode
                        | crate::message::ReadingMsg::SetMode { .. }
                );
                // plan editor-ui-round-3 WP-6: the switch's (path, old mode) and the source
                // line at the top before it, carried to the new mode below.
                let carry = if mode_change {
                    self.mode_switch_origin(&m)
                } else {
                    None
                };
                let docs: Vec<&crate::source::SourceBuffer> = self.docs.values().collect();
                let active = self
                    .panes
                    .get(self.focus)
                    .and_then(PaneState::active_note)
                    .and_then(|t| self.docs.get(&t.path));
                let task = crate::reading::update(
                    &mut self.reading,
                    m,
                    &docs,
                    active,
                    self.daemon.as_ref(),
                );
                if mode_change {
                    self.mode_menu = None;
                    self.persist();
                }
                let carried = carry.map_or_else(Task::none, |(path, old, line)| {
                    self.carry_place(&path, old, line)
                });
                return Task::batch([task, carried]);
            }
            Message::Switcher(m) => return self.update_switcher(m),
            Message::Palette(m) => return self.update_palette(m),
            Message::Panels(m) => return self.update_panels(m),
            Message::Views(m) => return self.update_views(m),
            Message::Nav(m) => return self.update_nav(m),
            Message::Panes(m) => return self.update_panes(m),
        }
        Task::none()
    }

    /// plan editor-ui-round-3 WP-6: for a mode-switch message, the tab path, its current mode
    /// and the source line at the top of the view being left — the Source pane's tracked top
    /// line, or the first source line of the Reading pane's probed top block (line 0 when the
    /// reading pane is at the top / never probed).
    fn mode_switch_origin(
        &self,
        m: &crate::message::ReadingMsg,
    ) -> Option<(String, crate::reading::ViewMode, usize)> {
        let path = match m {
            crate::message::ReadingMsg::SetMode { path, .. } => path.clone(),
            _ => self
                .panes
                .get(self.focus)
                .and_then(PaneState::active_note)?
                .path
                .clone(),
        };
        let old = self.reading.mode(&path);
        let line = if old.shows_reading() {
            let doc = self.reading.doc(&path);
            self.reading
                .top_block(&path)
                .and_then(|b| doc?.model.block_lines.get(b).map(|r| r.start))
                .unwrap_or(0)
        } else {
            self.docs.get(&path)?.top_line()
        };
        Some((path, old, line))
    }

    /// plan editor-ui-round-3 WP-6 (audit E1, block granularity, never proportional): after a
    /// mode switch from `old`, puts source `line`'s block at the top of the new view — Source
    /// scrolls to the line when it was hidden (Reading → Source/Split); Reading scrolls its
    /// block (`RenderModel::block_for_line`) to the top via a measured layout probe.
    fn carry_place(
        &mut self,
        path: &str,
        old: crate::reading::ViewMode,
        line: usize,
    ) -> Task<Message> {
        let new = self.reading.mode(path);
        if new == old {
            return Task::none();
        }
        if new.shows_source()
            && !old.shows_source()
            && let Some(doc) = self.docs.get_mut(path)
        {
            doc.scroll_to_line(line);
        }
        let above_first = self
            .reading
            .doc(path)
            .and_then(|d| d.model.block_lines.first())
            .is_none_or(|r| line < r.start);
        if new.shows_reading() && above_first {
            // Frontmatter / leading lines: the properties panel heads the view.
            self.reading.clear_top_block(path);
            return iced::widget::operation::scroll_to(
                crate::find::reading_scroll_id(path),
                iced::widget::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(0.0),
                },
            );
        }
        if new.shows_reading()
            && let Some(doc) = self.reading.doc(path)
            && let Some(b) = doc.model.block_for_line(line)
        {
            let blocks = doc.model.blocks.len();
            self.reading.set_top_block(path, b);
            return crate::reading::probe(path, blocks, Some(b));
        }
        Task::none()
    }

    fn update_tabs(&mut self, m: TabsMsg) -> Task<Message> {
        match m {
            TabsMsg::Open(p) => self.open(&p, true),
            TabsMsg::Select(pane, i) => self.select_tab(pane, i),
            TabsMsg::GoTo(n) => {
                let i = usize::from(n.saturating_sub(1));
                if n >= 1
                    && self
                        .panes
                        .get(self.focus)
                        .is_some_and(|ps| i < ps.tabs.len())
                {
                    self.select_tab(self.focus, i);
                }
            }
            TabsMsg::Close(pane, i) => return self.close_tab(pane, i),
            TabsMsg::CloseActive => {
                if let Some(i) = self.panes.get(self.focus).and_then(|ps| ps.active) {
                    return self.close_tab(self.focus, i);
                }
            }
            TabsMsg::CloseSaved {
                path,
                tab,
                seq,
                outcome,
            } => {
                // Audit E1: the close removes exactly the tab it was asked for (by id).
                match self.flushed(&path, seq, outcome) {
                    Ok(()) => {
                        if let Some((pane, i)) = panes::find_id(&self.panes, tab) {
                            self.remove_tab(pane, i);
                        }
                    }
                    Err(why) => {
                        if panes::find_id(&self.panes, tab).is_some() {
                            self.close_dialog(&path, vec![tab], &why);
                        }
                    }
                }
            }
            TabsMsg::Quit => return self.quit(),
            TabsMsg::FlushAll => return self.flush_all_notes(),
            TabsMsg::QuitSaved { path, seq, outcome } => {
                let r = self.flushed(&path, seq, outcome);
                let reason = r.err().map(|why| self.loss_reason(&path, &why));
                let Some(q) = &mut self.quit else {
                    return Task::none();
                };
                q.pending.remove(&path);
                if let Some(reason) = reason {
                    q.losses.note(&path, reason);
                }
                return self.quit_progress();
            }
        }
        Task::none()
    }

    /// Exits once every quit flush (notes and settings) has landed; anything that could not
    /// be saved opens the unsaved-edits dialog instead (plan editor-data-safety WP-2).
    fn quit_progress(&mut self) -> Task<Message> {
        let Some(q) = &self.quit else {
            return Task::none();
        };
        if q.pending.is_empty() && q.settings.is_empty() && q.awaiting.is_empty() {
            let losses = self.quit.take().map(|q| q.losses).unwrap_or_default();
            if losses.is_empty() {
                return self.exit();
            }
            self.dialog = Some(crate::dialog::Dialog::unsaved(
                losses,
                crate::dialog::OnOk::Quit,
            ));
        }
        Task::none()
    }

    fn select_tab(&mut self, pane: Pane, i: usize) {
        if let Some(ps) = self.panes.get_mut(pane)
            && i < ps.tabs.len()
        {
            ps.active = Some(i);
            let path = ps.tabs[i].path.clone();
            self.focus = pane;
            self.record_visit(pane, &path);
            self.persist();
        }
    }

    fn remove_tab(&mut self, pane: Pane, i: usize) {
        if let Some(tab) = self.panes.get_mut(pane).and_then(|ps| ps.remove(i)) {
            self.on_tab_closed(&tab.path);
            self.persist();
        }
    }

    /// Close tab `i` of `pane`; a dirty buffer is flushed through the normal CAS save first and
    /// the close happens only once that save lands with nothing newer typed (Decisions). Plan
    /// editor-feedback-round-2 WP-7 (audit E1): the close is tracked by the tab's id, so with
    /// two tabs of one note the save closes exactly the tab asked for.
    fn close_tab(&mut self, pane: Pane, i: usize) -> Task<Message> {
        let Some(tab) = self.panes.get(pane).and_then(|ps| ps.tabs.get(i)).cloned() else {
            return Task::none();
        };
        if tab.is_settings() {
            // WP-2: closing the Settings tab flushes pending settings writes (the model and
            // its save loops outlive the tab, so the writes land after the close).
            self.remove_tab(pane, i);
            return self.flush_settings().1;
        }
        let Some(doc) = self.docs.get_mut(&tab.path) else {
            self.remove_tab(pane, i);
            return Task::none();
        };
        if !doc.is_dirty() {
            self.remove_tab(pane, i);
            return Task::none();
        }
        if doc.conflict().is_some() || !self.status.connected {
            self.close_dialog(&tab.path, vec![tab.id], "not connected");
            return Task::none();
        }
        let Some(job) = doc.begin_save(false) else {
            // WP-3 rework: a save is in flight (`begin_save` queued the follow-up); the close
            // completes when it lands clean (`note_save_landed`).
            if doc.is_saving() {
                let ids = self.closing.entry(tab.path.clone()).or_default();
                if !ids.contains(&tab.id) {
                    ids.push(tab.id);
                }
            } else {
                // A dirty buffer whose save cannot start.
                self.close_dialog(&tab.path, vec![tab.id], "the save could not start");
            }
            return Task::none();
        };
        let (seq, path, id) = (job.seq, tab.path, tab.id);
        self.rpc(
            move |d| Ok(job.run(d)),
            move |r| {
                Message::Tabs(TabsMsg::CloseSaved {
                    path,
                    tab: id,
                    seq,
                    outcome: r.unwrap_or_else(crate::source::SaveOutcome::Failed),
                })
            },
        )
    }

    /// WP-3 rework: a normal note save (`SourceMsg::Saved`, already applied to the buffer)
    /// landed; resolves a close or quit waiting on it. Clean → done; saved but a queued
    /// follow-up is in flight → keep waiting; conflict, failure or a newer unsaved edit →
    /// cancel with a status message. Every await ends here, so the wait is bounded.
    fn note_save_landed(&mut self, path: &str, ok: bool) -> Task<Message> {
        let waiting_quit = self
            .quit
            .as_ref()
            .is_some_and(|q| q.awaiting.contains(path));
        let waiting_close = self.closing.contains_key(path);
        if !waiting_quit && !waiting_close {
            return Task::none();
        }
        let (saving, dirty) = self
            .docs
            .get(path)
            .map_or((false, false), |d| (d.is_saving(), d.is_dirty()));
        if ok && saving {
            return Task::none();
        }
        let done = ok && !dirty;
        let why = if !ok {
            "save failed"
        } else {
            "the note changed while saving"
        };
        if waiting_close {
            let ids = self.closing.remove(path).unwrap_or_default();
            if done {
                for id in ids {
                    if let Some((p, i)) = panes::find_id(&self.panes, id) {
                        self.remove_tab(p, i);
                    }
                }
            } else {
                let ids: Vec<u64> = ids
                    .into_iter()
                    .filter(|id| panes::find_id(&self.panes, *id).is_some())
                    .collect();
                if !ids.is_empty() && self.quit.is_none() {
                    self.close_dialog(path, ids, why);
                }
            }
        }
        if waiting_quit {
            let reason = (!done).then(|| self.loss_reason(path, why));
            if let Some(q) = &mut self.quit {
                q.awaiting.remove(path);
                if let Some(reason) = reason {
                    q.losses.note(path, reason);
                }
            }
            return self.quit_progress();
        }
        Task::none()
    }

    /// A save-before-close/quit finished: `Ok` only when it saved and the buffer is clean;
    /// otherwise why not (the caller turns it into a dialog entry).
    fn flushed(
        &mut self,
        path: &str,
        seq: u64,
        outcome: crate::source::SaveOutcome,
    ) -> Result<(), String> {
        use crate::source::SaveOutcome;
        let Some(doc) = self.docs.get_mut(path) else {
            return Ok(());
        };
        let ok = matches!(outcome, SaveOutcome::Saved(_));
        doc.finish_save(seq, outcome)?;
        if !ok {
            return Err("save conflict".into());
        }
        if doc.is_dirty() {
            return Err("the note changed while saving".into());
        }
        Ok(())
    }

    /// Quit: flush every dirty document (once per document, however many tabs show it — plan
    /// editor-feedback-round-2 WP-7), exit once all saves land; any failure cancels the quit.
    fn quit(&mut self) -> Task<Message> {
        if self.quit.is_some() || self.dialog.is_some() {
            return Task::none();
        }
        let dirty: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, d)| d.is_dirty())
            .map(|(p, _)| p.clone())
            .collect();
        let settings_pending = self
            .settings
            .files
            .values()
            .any(|f| f.wants_save() || f.saving);
        // plan editor-data-safety WP-2 (audit M1): dirty settings files the flush cannot
        // write (validation errors, malformed / unloaded) are losses, never written (D20).
        let mut losses = crate::dialog::UnsavedEdits::default();
        for f in self.settings.files.values() {
            if f.dirty && !f.wants_save() && !f.saving {
                losses.setting(&f.path);
            }
        }
        if dirty.is_empty()
            && losses.is_empty()
            && (!settings_pending || self.status.mode == Mode::Shadow)
        {
            return self.exit();
        }
        let connected = self.status.connected && self.daemon.is_some();
        let mut jobs = Vec::new();
        let mut awaiting = std::collections::HashSet::new();
        for path in dirty {
            if !connected {
                losses.note(&path, crate::dialog::LossReason::Disconnected);
                continue;
            }
            let reason = self.loss_reason(&path, "the save could not start");
            if let Some(doc) = self.docs.get_mut(&path) {
                match doc.begin_save(false) {
                    Some(job) => jobs.push((path, job)),
                    // WP-3 rework: in flight — `begin_save` queued the follow-up; await it.
                    None if doc.is_saving() => {
                        awaiting.insert(path);
                    }
                    // Conflict / missing file (or a save that cannot start).
                    None => losses.note(&path, reason),
                }
            }
        }
        if !connected {
            for f in self.settings.files.values() {
                if f.wants_save() || f.saving {
                    losses.setting(&f.path);
                }
            }
        }
        self.quit = Some(QuitState {
            pending: jobs.iter().map(|(p, _)| p.clone()).collect(),
            settings: std::collections::HashSet::new(),
            awaiting,
            losses,
        });
        if !connected {
            return self.quit_progress();
        }
        // WP-2: settings writes are flushed and awaited like note saves.
        // Await exactly the paths the flush started or queued (none in shadow mode, so a
        // shadow quit never waits on a settings write that cannot run).
        let (flushing, settings_task) = self.flush_settings();
        if let Some(q) = &mut self.quit {
            q.settings.extend(flushing);
        }
        if self.quit.is_none() {
            // Every write landed inline and the quit already exited.
            return settings_task;
        }
        let mut tasks: Vec<Task<Message>> = vec![settings_task, self.quit_progress()];
        tasks.extend(jobs.into_iter().map(|(path, job)| {
            let seq = job.seq;
            self.rpc(
                move |d| Ok(job.run(d)),
                move |r| {
                    Message::Tabs(TabsMsg::QuitSaved {
                        path,
                        seq,
                        outcome: r.unwrap_or_else(crate::source::SaveOutcome::Failed),
                    })
                },
            )
        }));
        Task::batch(tasks)
    }

    fn exit(&mut self) -> Task<Message> {
        self.persist();
        self.exiting = true;
        iced::exit()
    }

    // ---- editor-usability-round WP-4: find in note ----

    fn update_find(&mut self, m: FindMsg) -> Task<Message> {
        if let FindMsg::Focus(p) = m {
            // `operation::focus` unfocuses every other focusable, the source editor included. A
            // press on the bar's close button closed it first (Close keeps the editor refocus).
            if self.panes.get(p).is_none_or(|ps| ps.find.is_none()) {
                return Task::none();
            }
            return iced::widget::operation::focus(crate::find::input_id(p));
        }
        let pane = self.focus;
        let Some(ps) = self.panes.get_mut(pane) else {
            return Task::none();
        };
        match m {
            FindMsg::Open => {
                if ps.active_note().is_none() {
                    return Task::none();
                }
                ps.find.get_or_insert_with(Default::default);
                return iced::widget::operation::focus(crate::find::input_id(pane));
            }
            FindMsg::Close => {
                if ps.find.take().is_none() {
                    return Task::none();
                }
                for t in &ps.tabs {
                    if let Some(d) = self.docs.get_mut(&t.path) {
                        d.set_find_highlight(None);
                        d.set_find_matches(Vec::new());
                    }
                    self.reading.set_find(&t.path, None);
                }
                self.find_shift = false;
                return self.focus_editor();
            }
            FindMsg::Shift(b) => self.find_shift = b,
            FindMsg::Focus(_) => {}
            FindMsg::Query(q) => {
                let Some(text) = ps
                    .active_tab()
                    .and_then(|t| self.docs.get(&t.path))
                    .map(crate::source::SourceBuffer::text)
                else {
                    return Task::none();
                };
                let path = ps.active_tab().map(|t| t.path.clone()).unwrap_or_default();
                let Some(bar) = &mut ps.find else {
                    return Task::none();
                };
                bar.query = q;
                bar.current = None;
                bar.search_in(&text, reading_find_model(&self.reading, &path));
                if bar.visible.is_some() {
                    // plan editor-round-4 WP-3: Reading starts at the first visible occurrence
                    // at or below the viewport's top block.
                    let top = self.reading.top_block(&path).unwrap_or(0);
                    let v = bar.visible.as_deref().unwrap_or_default();
                    bar.current =
                        (!v.is_empty()).then(|| v.iter().position(|&(b, _)| b >= top).unwrap_or(0));
                    return self.show_match();
                }
                let at = ps
                    .active_tab()
                    .and_then(|t| self.docs.get(&t.path))
                    .map_or(0, |d| {
                        let c = d.cursor().position;
                        editor_model::undo::offset_of(
                            &text,
                            editor_model::undo::Pos::new(c.line, c.column),
                        )
                        .unwrap_or(0)
                    });
                if let Some(bar) = &mut ps.find {
                    bar.current = editor_model::find::first_from(&bar.matches, at);
                }
                return self.show_match();
            }
            FindMsg::Submit | FindMsg::Next | FindMsg::Prev => {
                let back =
                    matches!(m, FindMsg::Prev) || (matches!(m, FindMsg::Submit) && self.find_shift);
                let Some(ps) = self.panes.get_mut(pane) else {
                    return Task::none();
                };
                let Some(text) = ps
                    .active_tab()
                    .and_then(|t| self.docs.get(&t.path))
                    .map(crate::source::SourceBuffer::text)
                else {
                    return Task::none();
                };
                let path = ps.active_tab().map(|t| t.path.clone()).unwrap_or_default();
                let Some(bar) = &mut ps.find else {
                    return Task::none();
                };
                bar.search_in(&text, reading_find_model(&self.reading, &path));
                let n = bar.len();
                bar.current = if back {
                    editor_model::find::prev(bar.current, n)
                } else {
                    editor_model::find::next(bar.current, n)
                };
                return self.show_match();
            }
        }
        Task::none()
    }

    /// Puts the focused pane's current match on screen: the source selection (Source/Split) —
    /// whose cursor move scrolls the shaped editor to it — plus a painted find span (the
    /// unfocused `text_editor` draws no selection; plan editor-feedback-round-2 WP-4), and in
    /// Reading/Split the query's occurrences painted plus a proportional snap of the scrollable.
    /// Positions are byte columns end to end (no char conversion).
    fn show_match(&mut self) -> Task<Message> {
        let pane = self.focus;
        let Some(ps) = self.panes.get_mut(pane) else {
            return Task::none();
        };
        let Some(i) = ps.active.filter(|&i| i < ps.tabs.len()) else {
            return Task::none();
        };
        let range = ps.find.as_ref().and_then(crate::find::FindBar::range);
        let query = ps.find.as_ref().map(|b| b.query.clone());
        // plan editor-round-4 WP-3: Reading steps through visible occurrences — the active one
        // is drawn strong and its block scrolled to; invisible source matches never count.
        let occurrence = ps
            .find
            .as_ref()
            .filter(|b| b.visible.is_some())
            .map(crate::find::FindBar::occurrence);
        let all = ps
            .find
            .as_ref()
            .map(|b| b.matches.clone())
            .unwrap_or_default();
        let path = ps.tabs[i].path.clone();
        let Some(doc) = self.docs.get_mut(&path) else {
            return Task::none();
        };
        let text = doc.text();
        // plan editor-ui-round-3 WP-5: every match painted light (byte columns end to end).
        let spans = all
            .iter()
            .map(|m| {
                (
                    editor_model::find::line_col(&text, m.start),
                    editor_model::find::line_col(&text, m.end),
                )
            })
            .collect();
        doc.set_find_matches(spans);
        if let Some(occ) = occurrence {
            doc.set_find_highlight(None);
            self.reading.set_find(&path, query);
            self.reading.set_find_active_at(&path, occ);
            let Some((b, _)) = occ else {
                return Task::none();
            };
            self.reading.set_top_block(&path, b);
            let n = self.reading.doc(&path).map_or(0, |d| d.model.blocks.len());
            return crate::reading::probe(&path, n, Some(b));
        }
        let Some(r) = range else {
            doc.set_find_highlight(None);
            self.reading.set_find(&path, query);
            self.reading.set_find_active(&path, &text, None);
            return Task::none();
        };
        let (l0, c0) = editor_model::find::line_col(&text, r.start);
        let (l1, c1) = editor_model::find::line_col(&text, r.end);
        let mode = self.reading.mode(&path);
        if mode.shows_source() {
            doc.select_range((l0, c0), (l1, c1));
        }
        doc.set_find_highlight(Some(((l0, c0), (l1, c1))));
        self.reading.set_find(&path, query);
        self.reading.set_find_active(&path, &text, Some(r.clone()));
        if mode.shows_reading() {
            let lines = text.matches('\n').count().max(1);
            #[allow(clippy::cast_precision_loss)]
            let y = (l0 as f32 / lines as f32).clamp(0.0, 1.0);
            return iced::widget::operation::snap_to(
                crate::find::reading_scroll_id(&path),
                iced::widget::scrollable::RelativeOffset { x: 0.0, y },
            );
        }
        Task::none()
    }

    /// plan editor-ui-round-3 WP-5: Go to top / bottom. Source/Split: the cursor moves to the
    /// buffer's start / end (the shaped editor scrolls to its cursor; the selection drops);
    /// Reading/Split: the reading scrollable snaps to its top / bottom.
    fn go_to_edge(&mut self, bottom: bool) -> Task<Message> {
        let Some(path) = self.active_note().map(|t| t.path.clone()) else {
            return Task::none();
        };
        let mode = self.reading.mode(&path);
        if mode.shows_source()
            && let Some(doc) = self.docs.get_mut(&path)
        {
            doc.go_to_edge(bottom);
        }
        if mode.shows_reading() {
            let y = if bottom { 1.0 } else { 0.0 };
            // plan editor-round-4 WP-2: a programmatic snap emits no `on_scroll`, so the stored
            // top block is re-probed after it (else a view switch carries a stale place).
            return iced::widget::operation::snap_to(
                crate::find::reading_scroll_id(&path),
                iced::widget::scrollable::RelativeOffset { x: 0.0, y },
            )
            .chain(self.reprobe(&path));
        }
        Task::none()
    }

    /// Re-probes `path`'s reading top block (after a programmatic scroll).
    fn reprobe(&self, path: &str) -> Task<Message> {
        self.reading.doc(path).map_or_else(Task::none, |d| {
            crate::reading::probe(path, d.model.blocks.len(), None)
        })
    }

    /// plan editor-round-4 WP-2: the scrollable plain PageUp/PageDown pages in the focused
    /// pane — Reading (incl. Split's reading side; a focused source editor captures the key
    /// first) or the Settings page; `None` with a modal open or a Source-only note. The second
    /// field is the note to re-probe afterwards.
    pub fn page_target(&self) -> Option<(iced::widget::Id, Option<String>)> {
        if self.palette.open || self.switcher.open || self.dialog.is_some() {
            return None;
        }
        match self.active_tab()?.kind() {
            TabKind::Settings => {
                Some((crate::settings::body_scroll_id(self.settings.section), None))
            }
            TabKind::Note(path) => self
                .reading
                .mode(path)
                .shows_reading()
                .then(|| (crate::find::reading_scroll_id(path), Some(path.to_owned()))),
        }
    }

    fn page(&self, down: bool) -> Task<Message> {
        let Some((id, note)) = self.page_target() else {
            return Task::none();
        };
        let t = crate::page::page(id, down);
        match note {
            Some(p) => t.chain(self.reprobe(&p)),
            None => t,
        }
    }

    /// Refocuses the focused pane's source editor (when it shows one).
    fn focus_editor(&self) -> Task<Message> {
        match self.active_note() {
            Some(t) if self.reading.mode(&t.path).shows_source() => {
                iced::widget::operation::focus(crate::source::editor_id(&t.path))
            }
            _ => Task::none(),
        }
    }

    /// plan editor-round-4 WP-4: `focus_follows_mouse` — the pointer entered `pane`. Ignored
    /// while any modal / menu is open or a mouse button is held (drag, selection). Besides
    /// `self.focus` (which routes source actions via `active_note`), native widget focus moves
    /// too (audit E3): the entered pane's source editor is focused, or — when it shows none —
    /// every focusable is unfocused (`operation::focus` on an id no widget carries), so keys
    /// never land in the previous pane's editor.
    fn hover_focus(&mut self, pane: Pane) -> Task<Message> {
        let modal = self.palette.open
            || self.switcher.open
            || self.dialog.is_some()
            || self.context_menu.is_some()
            || self.mode_menu.is_some();
        if !self.prefs.focus_follows_mouse
            || modal
            || self.mouse_down
            || self.focus == pane
            || self.panes.get(pane).is_none()
        {
            return Task::none();
        }
        self.focus = pane;
        self.persist();
        match self.active_note() {
            Some(t) if self.reading.mode(&t.path).shows_source() => self.focus_editor(),
            _ => iced::widget::operation::focus(iced::widget::Id::new("hover-focus-none")),
        }
    }

    fn update_panes(&mut self, m: PanesMsg) -> Task<Message> {
        match m {
            PanesMsg::SplitRight | PanesMsg::SplitDown => {
                let dir = if matches!(m, PanesMsg::SplitRight) {
                    SplitDir::Right
                } else {
                    SplitDir::Down
                };
                if let Some((p, _)) =
                    self.panes
                        .split(panes::axis_of(dir), self.focus, PaneState::default())
                {
                    self.focus = p;
                }
                self.persist();
            }
            PanesMsg::Close(pane) => {
                let closed = if self.panes.len() > 1 {
                    match self.panes.close(pane) {
                        Some((state, sibling)) => {
                            if self.focus == pane || self.panes.get(self.focus).is_none() {
                                self.focus = sibling;
                            }
                            state.tabs
                        }
                        None => Vec::new(),
                    }
                } else {
                    self.panes.get_mut(pane).map_or_else(Vec::new, |ps| {
                        ps.active = None;
                        std::mem::take(&mut ps.tabs)
                    })
                };
                for t in &closed {
                    self.on_tab_closed(&t.path);
                }
                self.persist();
            }
            PanesMsg::Focus(pane) => {
                if self.focus != pane && self.panes.get(pane).is_some() {
                    self.focus = pane;
                    self.persist();
                }
            }
            PanesMsg::Hover(pane) => return self.hover_focus(pane),
            PanesMsg::MouseButton(down) => self.mouse_down = down,
            PanesMsg::Resized(ev) => self.panes.resize(ev.split, ev.ratio),
            PanesMsg::Bookmark(b) => return self.update_bookmarks(b),
            PanesMsg::Back(pane) => self.history_step(pane, false),
            PanesMsg::Forward(pane) => self.history_step(pane, true),
            PanesMsg::Page(down) => return self.page(down),
            PanesMsg::BackFocused => self.history_step(self.focus, false),
            PanesMsg::ForwardFocused => self.history_step(self.focus, true),
            PanesMsg::ToggleSide => {
                self.ui.side_collapsed = !self.ui.side_collapsed;
                self.persist();
            }
            PanesMsg::ToggleBorders => {
                self.ui.pane_borders = !self.ui.pane_borders;
                self.persist();
            }
            PanesMsg::ModeMenu(p) => self.mode_menu = p,
        }
        Task::none()
    }

    fn update_bookmarks(&mut self, m: BookmarkMsg) -> Task<Message> {
        match m {
            BookmarkMsg::Open(p) => return self.open_bookmark(&p),
            BookmarkMsg::Written(r) => {
                let d = self.daemon.clone();
                let err = match &r {
                    Ok(WriteOutcome::Saved(_)) => None,
                    Ok(WriteOutcome::Conflict) => {
                        Some("bookmarks changed on disk; reloaded (edit not saved)".to_owned())
                    }
                    Err(e) => Some(format!("bookmarks: {e}")),
                };
                if let Some(f) = self.settings.file_mut(BOOKMARKS_PATH) {
                    let failed = r.is_err();
                    f.finish_save(r, d.as_deref());
                    if failed && let Some(d) = &d {
                        f.load(d.as_ref());
                    }
                }
                if err.is_some() {
                    self.status.error = err;
                }
                self.sync_bookmarks();
                // WP-2: a Settings-page auto-save that fired during this write re-runs now.
                if self
                    .settings
                    .file(BOOKMARKS_PATH)
                    .is_some_and(|f| f.queued && f.can_save())
                {
                    return self.save_settings_file(BOOKMARKS_PATH.to_owned());
                }
            }
            edit => {
                if self.shadow_blocked("bookmark edits are disabled in shadow mode") {
                    return Task::none();
                }
                let active = self.active_note().map(|t| t.path.clone());
                let Some(f) = self.settings.file_mut(BOOKMARKS_PATH) else {
                    return Task::none();
                };
                if f.saving {
                    return Task::none();
                }
                if f.dirty {
                    self.status.error = Some(
                        "bookmarks have unsaved edits on the Settings page (saving or invalid)"
                            .into(),
                    );
                    return Task::none();
                }
                let e = match &edit {
                    BookmarkMsg::SetKeys(path, keys) => f
                        .doc
                        .as_ref()
                        .and_then(|d| bookmarks::keys_edit(d, path, keys.as_deref())),
                    _ => bookmarks::doc_edit(&edit, &self.bookmarks.items, active.as_deref()),
                };
                let Some(e) = e else {
                    return Task::none();
                };
                if let Err(err) = self.settings.edit(BOOKMARKS_PATH, &e) {
                    self.status.error = Some(format!("bookmarks: {err}"));
                    return Task::none();
                }
                let job = self
                    .settings
                    .file_mut(BOOKMARKS_PATH)
                    .and_then(crate::settings::ConfigFile::begin_save);
                self.sync_bookmarks();
                let Some(job) = job else {
                    self.status.error = Some(
                        "bookmarks: the file has form errors; fix them on the Settings page".into(),
                    );
                    if let (Some(d), Some(f)) =
                        (self.daemon.clone(), self.settings.file_mut(BOOKMARKS_PATH))
                    {
                        f.load(d.as_ref());
                    }
                    self.sync_bookmarks();
                    return Task::none();
                };
                return self.rpc(
                    move |d| job.run(d),
                    |r| Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Written(r))),
                );
            }
        }
        Task::none()
    }

    /// Opens a bookmark (panel click, palette command or chord); a bookmark whose path the
    /// index does not hold gives a status notice instead of an error tab (plan
    /// editor-ui-round-3 WP-9).
    fn open_bookmark(&mut self, path: &str) -> Task<Message> {
        if self.bookmarks.missing.contains(path) {
            self.status.error = Some(format!("bookmark target missing: {path}"));
            return Task::none();
        }
        self.open(path, true);
        Task::none()
    }

    /// WP-9: recomputes which bookmarks the index lacks (load, `FileChanged`,
    /// `IndexGeneration`). Without a daemon nothing is known, so nothing is marked.
    fn refresh_bookmark_missing(&mut self) {
        if self.daemon.is_none() {
            self.bookmarks.missing.clear();
            return;
        }
        let held = self.indexed_fn();
        self.bookmarks.refresh_missing(held);
    }

    fn dispatch_op(&mut self, op: FileOp) -> Task<Message> {
        // A rename/move/trash re-reads (or closes) the open tab, which would drop unsaved edits.
        let target = match &op {
            FileOp::Rename { from, .. } => Some(from.as_str()),
            FileOp::Delete { path } => Some(path.as_str()),
            _ => None,
        };
        if let Some(t) = target
            && self.doc_dirty(t)
        {
            self.status.error = Some("save the open note first (it has unsaved edits)".into());
            return Task::none();
        }
        if let FileOp::Rename { from, to } = &op {
            self.explorer.expect_rename(from, to);
        }
        let back = op.clone();
        self.rpc(
            move |d| op.run(d),
            move |result| Message::Explorer(ExplorerMsg::Done(FileOpDone { op: back, result })),
        )
    }

    fn update_explorer(&mut self, m: ExplorerMsg) -> Task<Message> {
        match m {
            ExplorerMsg::Refresh => self.refresh_files(),
            ExplorerMsg::Toggle(f) => {
                if !self.ui.expanded.remove(&f) {
                    self.ui.expanded.insert(f.clone());
                }
                self.explorer.selected = Some(f);
                self.explorer.relayout(&self.ui.expanded);
                self.persist();
            }
            ExplorerMsg::Select(p) => self.explorer.selected = Some(p),
            ExplorerMsg::Open(p) => {
                self.explorer.selected = Some(p.clone());
                self.open(&p, true);
            }
            ExplorerMsg::Scrolled(w) => self.explorer.window = w,
            ExplorerMsg::BeginNew | ExplorerMsg::BeginRename | ExplorerMsg::BeginMove => {
                if self.shadow_blocked(explorer::SHADOW_REASON) {
                    return Task::none();
                }
                let kind = match m {
                    ExplorerMsg::BeginNew => EditKind::New,
                    ExplorerMsg::BeginRename => EditKind::Rename,
                    _ => EditKind::Move,
                };
                if !self.explorer.begin(kind) {
                    self.status.error = Some("select a file first".into());
                }
            }
            ExplorerMsg::Trash => {
                if self.shadow_blocked(explorer::SHADOW_REASON) {
                    return Task::none();
                }
                match self
                    .explorer
                    .selected
                    .clone()
                    .filter(|s| self.explorer.contains(s))
                {
                    // plan editor-ui-round-3 WP-3: always through the delete dialog.
                    Some(path) => return self.begin_delete(path),
                    None => self.status.error = Some("select a file first".into()),
                }
            }
            ExplorerMsg::DeleteActive => match self.active_note().map(|t| t.path.clone()) {
                Some(path) => return self.begin_delete(path),
                None => self.status.error = Some("no active note".into()),
            },
            ExplorerMsg::DeletePreview { path, result } => match result {
                Ok(items) => {
                    let c = crate::dialog::DeleteConfirm::new(path, self.prefs.delete_mode, items);
                    self.dialog = Some(crate::dialog::Dialog::delete(c));
                }
                Err(e) => self.status.error = Some(e),
            },
            ExplorerMsg::NoteDeleted(result) => match result {
                Ok(r) => {
                    let indexed = self.indexed_fn();
                    for p in &r.removed {
                        let op = FileOp::Delete { path: p.clone() };
                        self.explorer.apply_op(&op, p, &self.ui.expanded, &indexed);
                        self.close_path(p);
                    }
                    if !r.skipped.is_empty() {
                        self.status.error = Some(format!(
                            "kept {} item(s) linked elsewhere: {}",
                            r.skipped.len(),
                            r.skipped.join(", ")
                        ));
                    }
                }
                Err(e) => self.status.error = Some(e),
            },
            ExplorerMsg::Input(s) => {
                if let Some(e) = &mut self.explorer.edit {
                    e.input = s;
                }
            }
            ExplorerMsg::Submit => {
                if self.shadow_blocked(explorer::SHADOW_REASON) {
                    self.explorer.edit = None;
                    return Task::none();
                }
                match self.explorer.submit() {
                    Ok(Some(op)) => return self.dispatch_op(op),
                    Ok(None) => {}
                    Err(e) => self.status.error = Some(e),
                }
            }
            ExplorerMsg::Cancel => self.explorer.edit = None,
            ExplorerMsg::Done(FileOpDone { op, result }) => match result {
                Ok(final_path) => {
                    let indexed = self.indexed_fn();
                    self.explorer
                        .apply_op(&op, &final_path, &self.ui.expanded, &indexed);
                    match &op {
                        FileOp::Create { .. } => self.open(&final_path, true),
                        FileOp::Rename { from, to } => self.retarget(from, to),
                        FileOp::Delete { path } => self.close_path(path),
                    }
                }
                Err(e) => self.status.error = Some(e),
            },
        }
        Task::none()
    }

    // ---- editor-usability-round WP-2: Reading-view edits ----

    /// plan editor-ui-round-3 WP-4: sets a host-note task's status char in `path`'s buffer
    /// (one undoable edit; normal autosave; the toggle's read-only rule).
    fn set_task_status(&mut self, path: &str, line: usize, to: char) -> Task<Message> {
        let read_only = self.status.mode == Mode::Shadow;
        let Some(doc) = self.docs.get_mut(path) else {
            return Task::none();
        };
        if read_only {
            self.status.error = Some("edits are disabled in shadow mode".into());
            return Task::none();
        }
        if !doc.set_task_status(line, to, false, std::time::Instant::now()) {
            return Task::none();
        }
        let seq = doc.edit_seq();
        self.reading.recompute(doc, self.daemon.as_ref());
        crate::source::autosave_timer(path.to_owned(), seq)
    }

    /// Toggles a host-note task box in `path`'s buffer (one undoable edit; normal autosave).
    fn toggle_task(&mut self, path: &str, line: usize) -> Task<Message> {
        let read_only = self.status.mode == Mode::Shadow;
        let Some(doc) = self.docs.get_mut(path) else {
            return Task::none();
        };
        if read_only {
            self.status.error = Some("edits are disabled in shadow mode".into());
            return Task::none();
        }
        if !doc.toggle_task(line, false, std::time::Instant::now()) {
            return Task::none();
        }
        let seq = doc.edit_seq();
        // Re-render now from the updated buffer (not on the next debounce tick).
        self.reading.recompute(doc, self.daemon.as_ref());
        crate::source::autosave_timer(path.to_owned(), seq)
    }

    /// plan usability-sprint-captures-links WP-4: flips a frontmatter boolean in `path`'s
    /// buffer — the same edit → autosave → CAS-save path as `toggle_task`.
    fn toggle_prop(&mut self, path: &str, key: &str) -> Task<Message> {
        let read_only = self.status.mode == Mode::Shadow;
        let Some(doc) = self.docs.get_mut(path) else {
            return Task::none();
        };
        if read_only {
            self.status.error = Some("edits are disabled in shadow mode".into());
            return Task::none();
        }
        if !doc.toggle_frontmatter_bool(key, false, std::time::Instant::now()) {
            return Task::none();
        }
        let seq = doc.edit_seq();
        self.reading.recompute(doc, self.daemon.as_ref());
        crate::source::autosave_timer(path.to_owned(), seq)
    }

    /// Enter / blur in the filename input: rename through the explorer path when changed,
    /// flushing a dirty buffer through the normal save first.
    fn title_submit(&mut self, path: &str) -> Task<Message> {
        let Some(input) = self.reading.take_title(path) else {
            return Task::none();
        };
        let Some(to) = crate::reading::rename_target(path, &input) else {
            return Task::none();
        };
        if input.contains('/') {
            self.status.error = Some("invalid name".into());
            return Task::none();
        }
        if self.shadow_blocked(explorer::SHADOW_REASON) {
            return Task::none();
        }
        let Some(doc) = self.docs.get_mut(path) else {
            return Task::none();
        };
        if !doc.is_dirty() {
            return self.dispatch_op(FileOp::Rename {
                from: path.to_owned(),
                to,
            });
        }
        let Some(job) = doc.begin_save(false) else {
            self.status.error =
                Some("rename not done (the note is saving or has a conflict)".into());
            return Task::none();
        };
        let seq = job.seq;
        let from = path.to_owned();
        self.rpc(
            move |d| Ok(job.run(d)),
            move |r| {
                Message::Reading(crate::message::ReadingMsg::TitleSaved {
                    path: from,
                    to,
                    seq,
                    outcome: r.unwrap_or_else(crate::source::SaveOutcome::Failed),
                })
            },
        )
    }

    /// The save-before-rename finished: rename on success, else abort and surface it.
    fn title_saved(
        &mut self,
        path: &str,
        to: &str,
        seq: u64,
        outcome: crate::source::SaveOutcome,
    ) -> Task<Message> {
        use crate::source::SaveOutcome;
        let Some(doc) = self.docs.get_mut(path) else {
            return Task::none();
        };
        let ok = matches!(outcome, SaveOutcome::Saved(_));
        if let Err(e) = doc.finish_save(seq, outcome) {
            self.status.error = Some(format!("rename not done (save failed: {e})"));
            return Task::none();
        }
        if !ok || doc.is_dirty() {
            self.status.error = Some(if ok {
                "rename not done (the note changed while saving)".into()
            } else {
                "rename not done (save conflict: the file changed on disk)".into()
            });
            return Task::none();
        }
        self.dispatch_op(FileOp::Rename {
            from: path.to_owned(),
            to: to.to_owned(),
        })
    }

    /// A renamed file's open tabs follow it (re-read at the new path). Plan
    /// editor-feedback-round-2 WP-7: the document key and every tab of `from` move to `to`
    /// (tab ids kept); `from`'s reading / title / image-pin state is torn down. Plan
    /// editor-feedback-round-2 WP-8: the explorer rename, the breadcrumb rename and an action's
    /// `move` (via [`Self::follow_open`]) all end here.
    fn retarget(&mut self, from: &str, to: &str) {
        if !self.path_open(from) {
            return;
        }
        let Some(d) = &self.daemon else { return };
        match d.read_file(to) {
            Ok((bytes, hash)) => {
                let doc = crate::source::SourceBuffer::new(to, &bytes, hash);
                self.docs.remove(from);
                self.move_path_state(from, to, doc);
            }
            Err(e) => self.status.error = Some(open_error(&e)),
        }
    }

    /// The shared tail of every rename follow: every tab of `from` (and its history) moves to
    /// `to`, `doc` is installed under `to`, per-path state is re-keyed. Hosted view results move
    /// with the host and go stale (re-run against `to` while the old result stays shown — no
    /// flash), and every other view goes stale too, since a rename changes link targets they
    /// may list (WP-8).
    fn move_path_state(&mut self, from: &str, to: &str, doc: crate::source::SourceBuffer) {
        let mode = self.reading.mode(from);
        for (_, ps) in self.panes.iter_mut() {
            for t in ps.tabs.iter_mut().filter(|t| t.path == from) {
                to.clone_into(&mut t.path);
            }
            ps.history.retarget(from, to);
        }
        #[cfg(test)]
        self.closed.push(from.to_owned());
        if let Some(ids) = self.closing.remove(from) {
            self.closing.insert(to.to_owned(), ids);
        }
        self.fm_titles.borrow_mut().remove(from);
        self.reading.on_tab_closed(from);
        self.views.rehost(from, to);
        self.views.invalidate(&[]);
        self.reading
            .register(&doc, mode, true, self.daemon.as_ref());
        self.docs.insert(to.to_owned(), doc);
        self.ui.touch_recent(to);
        self.persist();
    }

    /// Plan usability-sprint-captures-links WP-5: after a save of `path` landed, renames the note
    /// when its on-disk `title` changed and the filename was the old title's form. Not while
    /// closing/quitting, in shadow mode (silently), or while a follow-up save or another rename
    /// is in flight (the next landed save re-evaluates). A target that exists refuses the rename.
    fn title_sync(&mut self, path: &str) -> Task<Message> {
        if self.closing.contains_key(path)
            || self.quit.is_some()
            || self.status.mode == Mode::Shadow
        {
            return Task::none();
        }
        let Some(doc) = self.docs.get(path) else {
            return Task::none();
        };
        if doc.is_saving() || doc.is_renaming() {
            return Task::none();
        }
        let Some(to) = doc.title_rename() else {
            return Task::none();
        };
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        // Case-only renames target the same file on a case-insensitive disk: no collision check.
        if to.to_lowercase() != path.to_lowercase()
            && !matches!(d.read_file(&to), Err(DaemonError::NotFound(_)))
        {
            if let Some(doc) = self.docs.get_mut(path) {
                doc.settle_title();
            }
            self.status.error = Some(format!("title changed; not renamed: {to} already exists"));
            return Task::none();
        }
        let Some(doc) = self.docs.get_mut(path) else {
            return Task::none();
        };
        doc.set_renaming(true);
        let seq = doc.edit_seq();
        self.explorer.expect_rename(path, &to);
        let op = FileOp::Rename {
            from: path.to_owned(),
            to: to.clone(),
        };
        let from = path.to_owned();
        self.rpc(
            move |d| op.run(d),
            move |result| Message::TitleRenamed {
                from,
                to,
                seq,
                result,
            },
        )
    }

    /// WP-5: a title rename finished. The tab follows (edits typed while it was in flight are
    /// kept, behind the conflict bar); a failure leaves the tab and relaunches a held save.
    fn title_renamed(
        &mut self,
        from: &str,
        to: &str,
        seq: u64,
        result: Result<String, String>,
    ) -> Task<Message> {
        let held = self
            .docs
            .get_mut(from)
            .is_some_and(|d| d.set_renaming(false));
        match result {
            Ok(final_path) => {
                let indexed = self.indexed_fn();
                let op = FileOp::Rename {
                    from: from.to_owned(),
                    to: to.to_owned(),
                };
                self.explorer
                    .apply_op(&op, &final_path, &self.ui.expanded, &indexed);
                let edited = self
                    .docs
                    .get(from)
                    .is_some_and(|d| d.is_dirty() || d.edit_seq() != seq);
                self.follow_moved(from, to, edited);
                Task::none()
            }
            Err(e) => {
                self.status.error = Some(format!("title changed; rename failed: {e}"));
                let Some(doc) = self.docs.get_mut(from) else {
                    return Task::none();
                };
                doc.settle_title();
                if held {
                    crate::source::autosave_timer(from.to_owned(), doc.edit_seq())
                } else {
                    Task::none()
                }
            }
        }
    }

    /// The Quick Switch placement from `editor.toml` `quick_switch_open` (plan
    /// editor-feedback-round-2 WP-7).
    fn quick_switch_place(&self) -> Place {
        use crate::settings::editor_prefs::QuickSwitchOpen;
        match self.prefs.quick_switch_open {
            QuickSwitchOpen::Switch => Place::Focus,
            QuickSwitchOpen::Move => Place::Move,
            QuickSwitchOpen::Duplicate => Place::Duplicate,
        }
    }

    /// Closes every tab on `path` (trashed files).
    fn close_path(&mut self, path: &str) {
        while let Some((pane, i)) = panes::find(&self.panes, path) {
            match self.panes.get_mut(pane).and_then(|ps| ps.remove(i)) {
                Some(t) => self.on_tab_closed(&t.path),
                None => break,
            }
        }
        self.persist();
    }

    fn update_switcher(&mut self, m: SwitcherMsg) -> Task<Message> {
        let s = &mut self.switcher;
        match m {
            SwitcherMsg::Open => {
                s.open = true;
                s.move_from = None;
                s.query.clear();
                s.selected = 0;
                s.refresh(&self.explorer.paths, &self.ui.recent);
                return iced::widget::operation::focus(switcher::INPUT_ID);
            }
            SwitcherMsg::OpenMove => return self.open_move_note(),
            SwitcherMsg::Close => s.open = false,
            // plan editor-ui-round-3 WP-5: two-stage Escape.
            SwitcherMsg::Escape if s.open && !s.query.is_empty() => {
                s.query.clear();
                s.selected = 0;
                s.refresh(&self.explorer.paths, &self.ui.recent);
                // plan editor-round-4 WP-1: iced unfocused the input on Escape.
                return iced::widget::operation::focus(switcher::INPUT_ID);
            }
            SwitcherMsg::Escape => s.open = false,
            SwitcherMsg::Query(q) => {
                s.query = q;
                s.selected = 0;
                s.refresh(&self.explorer.paths, &self.ui.recent);
            }
            SwitcherMsg::Up => s.selected = s.selected.saturating_sub(1),
            SwitcherMsg::Down => {
                if s.selected + 1 < s.results.len() {
                    s.selected += 1;
                }
            }
            SwitcherMsg::Modifiers(m) => s.shift = switcher::shift(m),
            SwitcherMsg::Submit => {
                if !s.open {
                    return Task::none();
                }
                if s.move_from.is_some() {
                    return match s.current().map(str::to_owned) {
                        Some(f) => self.move_note_to(&f),
                        None => Task::none(),
                    };
                }
                match s.current().map(str::to_owned) {
                    Some(p) if !s.shift => {
                        s.open = false;
                        self.open_with(&p, self.quick_switch_place(), true);
                    }
                    _ => return self.update_switcher(SwitcherMsg::Create),
                }
            }
            SwitcherMsg::Pick(p) if s.move_from.is_some() => return self.move_note_to(&p),
            SwitcherMsg::Pick(p) => {
                s.open = false;
                self.open_with(&p, self.quick_switch_place(), true);
            }
            SwitcherMsg::Create if s.move_from.is_some() => {}
            SwitcherMsg::Create => {
                if self.shadow_blocked("creating notes is disabled in shadow mode") {
                    return Task::none();
                }
                let Some(path) = switcher::create_path(&self.switcher.query) else {
                    return Task::none();
                };
                return self.rpc(
                    move |d| {
                        d.run_action(switcher::create_params(&path))
                            .map(|r| r.open_path)
                            .map_err(|e| e.to_string())
                    },
                    |r| Message::Switcher(SwitcherMsg::Created(r)),
                );
            }
            SwitcherMsg::Created(r) => match r {
                Ok(Some(p)) => {
                    self.switcher.open = false;
                    self.open(&p, true);
                }
                Ok(None) => self.status.error = Some("note.create returned no path".into()),
                Err(e) => self.status.error = Some(format!("note.create: {e}")),
            },
        }
        Task::none()
    }

    /// WP-6 (usability-sprint-captures-links): "Move note…" — the switcher in folder mode for
    /// the active note.
    fn open_move_note(&mut self) -> Task<Message> {
        if self.shadow_blocked(explorer::SHADOW_REASON) {
            return Task::none();
        }
        let Some(from) = self.active_note().map(|t| t.path.clone()) else {
            self.status.error = Some("move note: no active note".into());
            return Task::none();
        };
        let s = &mut self.switcher;
        s.folders = switcher::folder_candidates(&self.explorer.paths, &from);
        s.move_from = Some(from);
        s.open = true;
        s.query.clear();
        s.selected = 0;
        s.refresh(&self.explorer.paths, &self.ui.recent);
        iced::widget::operation::focus(switcher::INPUT_ID)
    }

    /// WP-6: moves the folder-mode note into `folder` (a [`switcher::folder_candidates`] label)
    /// through [`Self::dispatch_op`]; completion follows via `ExplorerMsg::Done` → `retarget`.
    fn move_note_to(&mut self, folder: &str) -> Task<Message> {
        let Some(from) = self.switcher.move_from.take() else {
            return Task::none();
        };
        self.switcher.open = false;
        if self.shadow_blocked(explorer::SHADOW_REASON) {
            return Task::none();
        }
        let to = switcher::move_target(&from, folder);
        if to == from {
            return Task::none();
        }
        let Some(d) = self.daemon.clone() else {
            self.status.error = Some("move note: not connected".into());
            return Task::none();
        };
        match d.read_file(&to) {
            Err(DaemonError::NotFound(_)) => {}
            _ => {
                self.status.error = Some(format!("move note: {to} already exists"));
                return Task::none();
            }
        }
        self.dispatch_op(FileOp::Rename { from, to })
    }

    fn update_nav(&mut self, m: NavMsg) -> Task<Message> {
        match m {
            NavMsg::Go { pane, target } => match target {
                NavTarget::Path(p) => {
                    if let Err(e) = self.open_into(pane, p, Place::Replace) {
                        self.status.error = Some(open_error(&e));
                    }
                    self.persist();
                }
                NavTarget::Period(period, date) => {
                    let params = nav::period_params(period, date);
                    return self.rpc(
                        move |d| {
                            d.run_action(params)
                                .map(|r| r.open_path)
                                .map_err(|e| e.to_string())
                        },
                        move |result| Message::Nav(NavMsg::Opened { pane, result }),
                    );
                }
            },
            NavMsg::Opened { pane, result } => match result {
                Ok(Some(p)) => {
                    match self.open_into(pane, &p, Place::Replace) {
                        Ok(()) => {}
                        Err(DaemonError::NotFound(_)) if self.status.mode == Mode::Shadow => {
                            self.status.error = Some("not materialized (shadow)".into());
                        }
                        Err(e) => self.status.error = Some(open_error(&e)),
                    }
                    self.persist();
                }
                Ok(None) => self.status.error = Some("period.open returned no path".into()),
                Err(e) => self.status.error = Some(format!("period.open: {e}")),
            },
        }
        Task::none()
    }

    // ---- WP-5: command palette and action invocation ----

    fn update_palette(&mut self, m: PaletteMsg) -> Task<Message> {
        use crate::palette::prompt::Step;
        match m {
            PaletteMsg::Open => {
                self.palette.show();
                let fetch = self.fetch_actions();
                return Task::batch([
                    fetch,
                    iced::widget::operation::focus(crate::palette::INPUT_ID),
                ]);
            }
            PaletteMsg::Escape
                if self.settings.hotkeys.recording.is_none()
                    && self.palette.open
                    && self.palette.prompt.is_none()
                    && !self.palette.query.is_empty() =>
            {
                // plan editor-ui-round-3 WP-5: two-stage Escape (clear, then close).
                self.palette.set_query(String::new());
                // plan editor-round-4 WP-1: iced unfocused the input on Escape.
                return iced::widget::operation::focus(crate::palette::INPUT_ID);
            }
            PaletteMsg::Close | PaletteMsg::Escape if self.settings.hotkeys.recording.is_some() => {
                self.settings.hotkeys.cancel();
            }
            PaletteMsg::Close | PaletteMsg::Escape => self.palette.close(),
            PaletteMsg::Query(q) => self.palette.set_query(q),
            PaletteMsg::Up if self.palette.prompt.is_none() => {
                self.palette.selected = self.palette.selected.saturating_sub(1);
            }
            PaletteMsg::Down if self.palette.prompt.is_none() => {
                if self.palette.selected + 1 < self.palette.results.len() {
                    self.palette.selected += 1;
                }
            }
            PaletteMsg::Up | PaletteMsg::Down => {}
            // plan editor-round-4 WP-1: Enter submits only steps without an `on_submit` input.
            PaletteMsg::Enter => {
                let non_text = self.palette.open
                    && self
                        .palette
                        .prompt
                        .as_ref()
                        .and_then(|p| p.current())
                        .is_some_and(|s| {
                            matches!(
                                s.kind,
                                cv_protocol::ParamKind::Enum | cv_protocol::ParamKind::Bool
                            )
                        });
                if non_text {
                    return self.update_palette(PaletteMsg::Submit);
                }
            }
            PaletteMsg::Submit | PaletteMsg::CtrlEnter => {
                if !self.palette.open {
                    return Task::none();
                }
                let Some(p) = &mut self.palette.prompt else {
                    if matches!(m, PaletteMsg::Submit)
                        && let Some(i) = self.palette.current()
                    {
                        return self.invoke_entry(i);
                    }
                    return Task::none();
                };
                match p.advance() {
                    Err(e) => self.palette.prompt_error = Some(e.to_string()),
                    Ok(Step::More) => {
                        let snapshot = p.clone();
                        self.palette.sync_step(&snapshot);
                    }
                    Ok(Step::Done(params)) => {
                        let a = p.action.clone();
                        self.palette.close();
                        return self.run_call(crate::palette::Call {
                            action_id: a.id,
                            title: a.title,
                            params,
                            needs: a.needs,
                        });
                    }
                }
            }
            PaletteMsg::Pick(i) => return self.invoke_entry(i),
            PaletteMsg::Loaded(r) => {
                self.palette.loading = false;
                match r {
                    Ok(a) => self.palette.set_actions(a),
                    Err(e) => self.status.error = Some(format!("actions: {e}")),
                }
                self.sync_hotkeys();
                if let Some(c) = self.palette.pending_chord.take()
                    && self.palette.actions.is_some()
                {
                    return self.on_chord(&c);
                }
            }
            PaletteMsg::Key(c) => return self.on_chord(&c),
            PaletteMsg::Prompt(pm) => {
                self.palette
                    .prompt_input(pm, &self.explorer.paths, &self.ui.recent);
            }
            PaletteMsg::SaveDone {
                path,
                seq,
                outcome,
                call,
            } => return self.after_save_first(&path, seq, outcome, call),
            PaletteMsg::Ran {
                title,
                origin,
                result,
            } => match result {
                Ok(r) => {
                    self.palette.job_titles.insert(r.job_id.clone(), title);
                    let early = self.palette.early_open.remove(&r.job_id);
                    if let Some(p) = r.open_path.or(early) {
                        self.follow_open(origin, &p);
                    } else if let Some(o) = origin {
                        self.palette.job_origins.insert(r.job_id, o);
                    }
                }
                Err(e) => self.status.error = Some(format!("{title}: {e}")),
            },
            PaletteMsg::Tick => self.palette.expire_toast(std::time::Instant::now()),
            PaletteMsg::RunPreset(p) => return self.run_preset(p),
        }
        Task::none()
    }

    fn fetch_actions(&mut self) -> Task<Message> {
        if self.palette.actions.is_some() || self.palette.loading || self.daemon.is_none() {
            return Task::none();
        }
        self.palette.loading = true;
        self.rpc(
            |d| d.actions_list().map_err(|e| e.to_string()),
            |r| Message::Palette(PaletteMsg::Loaded(r)),
        )
    }

    /// A chord from the palette key listener or a focused source editor, decided from the
    /// effective keymap (plan editor-settings-tab WP-4, audit E1). While the Hotkeys page is
    /// recording, the chord goes to the capture instead.
    fn on_chord(&mut self, c: &crate::palette::chord::Chord) -> Task<Message> {
        use crate::palette::Bound;
        if self.settings.hotkeys.recording.is_some() {
            return self.capture_chord(c);
        }
        match self.palette.keymap.get(c).cloned() {
            Some(Bound::Client(cmd)) => return self.update(cmd.message(self.focus)),
            Some(Bound::Preset(preset)) => {
                if self.palette.actions.is_none() {
                    self.palette.pending_chord = Some(c.clone());
                    return self.fetch_actions();
                }
                return self.run_preset(preset);
            }
            Some(Bound::Action(id)) => return self.invoke_action(&id),
            Some(Bound::Bookmark(p)) => return self.open_bookmark(&p),
            None => {}
        }
        if crate::palette::chord::text_editing(c) || self.palette.actions.is_some() {
            return Task::none();
        }
        self.palette.pending_chord = Some(c.clone());
        self.fetch_actions()
    }

    /// Runs a preset action with its fixed params; a no-op when the action is not loaded.
    fn run_preset(&mut self, p: crate::palette::Preset) -> Task<Message> {
        let Some(info) = self.palette.action(p.action).cloned() else {
            return Task::none();
        };
        let mut params = serde_json::Map::new();
        if let Some(period) = p.period {
            params.insert("period".into(), period.as_str().into());
            params.insert(
                "date".into(),
                editor_model::period::iso(self.today()).into(),
            );
        }
        self.palette.close();
        self.run_call(crate::palette::Call {
            action_id: info.id,
            title: info.title,
            params,
            needs: info.needs,
        })
    }

    fn invoke_entry(&mut self, i: usize) -> Task<Message> {
        let Some(e) = self.palette.entries.get(i).cloned() else {
            return Task::none();
        };
        match e.id {
            crate::palette::EntryId::Client(c) => {
                self.palette.close();
                self.update(c.message(self.focus))
            }
            crate::palette::EntryId::Action(id) => self.invoke_action(&id),
            crate::palette::EntryId::Bookmark(p) => {
                self.palette.close();
                self.open_bookmark(&p)
            }
        }
    }

    /// Invokes an action exactly as the palette does: prompt its params, then run.
    fn invoke_action(&mut self, id: &str) -> Task<Message> {
        let Some(info) = self.palette.action(id).cloned() else {
            self.status.error = Some(format!("unknown action {id}"));
            return Task::none();
        };
        if !info.params.is_empty() {
            let today = self.today();
            self.palette.begin_prompt(info, today);
            return iced::widget::operation::focus(crate::palette::INPUT_ID);
        }
        self.palette.close();
        self.run_call(crate::palette::Call {
            action_id: info.id,
            title: info.title,
            params: serde_json::Map::new(),
            needs: info.needs,
        })
    }

    /// Save-first (live mode, dirty active buffer), then `run_action`.
    fn run_call(&mut self, call: crate::palette::Call) -> Task<Message> {
        let shadow = self.status.mode == Mode::Shadow;
        let Some(ps) = self.panes.get_mut(self.focus) else {
            return self.dispatch_call(call);
        };
        let Some(i) = ps
            .active
            .filter(|&i| ps.tabs.get(i).is_some_and(|t| !t.is_settings()))
        else {
            if call.needs.iter().any(|n| n == "active_path") {
                self.status.error = Some(format!("{}: open a note first", call.title));
                return Task::none();
            }
            return self.dispatch_call(call);
        };
        let path = ps.tabs[i].path.clone();
        let Some(doc) = self.docs.get_mut(&path) else {
            return self.dispatch_call(call);
        };
        if shadow || !doc.is_dirty() {
            return self.dispatch_call(call);
        }
        let Some(job) = doc.begin_save(false) else {
            self.status.error = Some(format!(
                "{}: not run (the note is saving or has a conflict)",
                call.title
            ));
            return Task::none();
        };
        let seq = job.seq;
        self.rpc(
            move |d| Ok(job.run(d)),
            move |r| {
                Message::Palette(PaletteMsg::SaveDone {
                    path,
                    seq,
                    outcome: r.unwrap_or_else(crate::source::SaveOutcome::Failed),
                    call,
                })
            },
        )
    }

    fn after_save_first(
        &mut self,
        path: &str,
        seq: u64,
        outcome: crate::source::SaveOutcome,
        call: crate::palette::Call,
    ) -> Task<Message> {
        use crate::source::SaveOutcome;
        let Some(tab) = self.docs.get_mut(path) else {
            self.status.error = Some(format!("{}: not run (note closed)", call.title));
            return Task::none();
        };
        let ok = matches!(outcome, SaveOutcome::Saved(_));
        let again = match tab.finish_save(seq, outcome.clone()) {
            Ok(again) => again,
            Err(e) => {
                self.status.error = Some(format!("{}: not run (save failed: {e})", call.title));
                return Task::none();
            }
        };
        if !ok {
            self.status.error = Some(format!(
                "{}: not run (save conflict: the file changed on disk)",
                call.title
            ));
            return Task::none();
        }
        let ctx = crate::palette::context(Some(&*tab));
        let follow = if again { tab.begin_save(false) } else { None };
        let follow = match follow {
            Some(job) => {
                let done = job.clone();
                self.rpc(
                    move |d| Ok(job.run(d)),
                    move |r| done.done(r.unwrap_or_else(SaveOutcome::Failed)),
                )
            }
            None => Task::none(),
        };
        let run = self.dispatch_with(call, ctx);
        Task::batch([follow, run])
    }

    fn dispatch_call(&mut self, call: crate::palette::Call) -> Task<Message> {
        let ctx = crate::palette::context(self.active_doc());
        self.dispatch_with(call, ctx)
    }

    fn dispatch_with(
        &mut self,
        call: crate::palette::Call,
        context: cv_protocol::ActionContext,
    ) -> Task<Message> {
        let params = cv_protocol::RunActionParams {
            action_id: call.action_id,
            params: call.params,
            context,
        };
        let title = call.title;
        let origin = params
            .context
            .active_path
            .as_ref()
            .and_then(|p| Some((p.clone(), self.docs.get(p)?.edit_seq())));
        self.rpc(
            move |d| d.run_action(params).map_err(|e| e.to_string()),
            move |result| {
                Message::Palette(PaletteMsg::Ran {
                    title,
                    origin,
                    result,
                })
            },
        )
    }

    /// A terminal `JobUpdated`: follow its `open_path` (WP-5). A job whose `run_action` reply has
    /// not arrived yet is remembered for [`PaletteMsg::Ran`].
    fn job_finished(&mut self, j: &cv_protocol::JobStatus) {
        // WP-6 (usability-sprint-captures-links): `Pending`/`Running` are not terminal — the
        // daemon publishes `Running` when the runner starts a durable job, usually after the
        // `run_action` reply; dropping the origin there left the tab at the vanished path.
        if !crate::palette::terminal(j.status) {
            return;
        }
        if j.status != cv_protocol::JobState::Done {
            self.palette.job_origins.remove(&j.job_id);
            return;
        }
        let Some(open) = j.open_path.clone() else {
            self.palette.job_origins.remove(&j.job_id);
            return;
        };
        if self.palette.job_titles.contains_key(&j.job_id) {
            if let Some(origin) = self.palette.job_origins.remove(&j.job_id) {
                self.follow_open(Some(origin), &open);
            }
            // A non-durable run already followed its reply's `open_path`; a durable run started
            // without a tab has no origin and does not open anything unasked.
        } else {
            if self.palette.early_open.len() >= 64 {
                self.palette.early_open.clear();
            }
            self.palette.early_open.insert(j.job_id.clone(), open);
        }
    }

    /// An action asks the client to open `to`. If the invoking tab is still open at its original
    /// path and that file no longer exists, the tab follows the note (retarget, as explorer
    /// renames do); edits made during the run are kept and surfaced as a conflict instead of
    /// being replaced (audit M2). Otherwise `to` opens as a new tab.
    fn follow_open(&mut self, origin: Option<(String, u64)>, to: &str) {
        let Some((from, seq)) = origin.filter(|(p, _)| p != to) else {
            self.open(to, true);
            return;
        };
        if !self.path_open(&from) {
            self.open(to, true);
            return;
        }
        let Some(d) = self.daemon.clone() else { return };
        match d.read_file(&from) {
            Err(DaemonError::NotFound(_)) => {}
            _ => {
                self.open(to, true);
                return;
            }
        }
        let edited = self.docs.get(&from).is_some_and(|d| d.edit_seq() != seq);
        self.follow_moved(&from, to, edited);
    }

    /// The tail of a move follow ([`Self::follow_open`], WP-5 title renames): unedited →
    /// [`Self::retarget`]; edited since the move was issued → the buffer follows to `to` keeping
    /// its text, behind the conflict bar (audit M2 / usability-sprint audit E2).
    fn follow_moved(&mut self, from: &str, to: &str, edited: bool) {
        if !edited {
            self.retarget(from, to);
            return;
        }
        let Some(d) = self.daemon.clone() else { return };
        let hash = match d.read_file(to) {
            Ok((_, h)) => h,
            Err(e) => {
                self.status.error = Some(open_error(&e));
                return;
            }
        };
        let Some(mut doc) = self.docs.remove(from) else {
            return;
        };
        doc.follow_move(to, hash);
        self.move_path_state(from, to, doc);
        self.status.error = Some(format!(
            "{from} moved to {to} while you were editing: keep mine saves your text there"
        ));
    }

    // ---- WP-5: side panels ----

    fn update_panels(&mut self, m: PanelsMsg) -> Task<Message> {
        match m {
            PanelsMsg::Toggle => {
                self.panels.visible = !self.panels.visible;
                return self.sync_panels();
            }
            PanelsMsg::Show(k) => {
                self.panels.visible = true;
                self.panels.kind = k;
                let t = self.sync_panels();
                if k == crate::panels::PanelKind::Search {
                    return Task::batch([
                        t,
                        iced::widget::operation::focus(crate::panels::SEARCH_ID),
                    ]);
                }
                return t;
            }
            PanelsMsg::Tick => return self.sync_panels(),
            PanelsMsg::OutlineJump(line) => {
                if let Some(doc) = self.active_doc_mut() {
                    doc.move_to(iced::widget::text_editor::Cursor {
                        position: iced::widget::text_editor::Position { line, column: 0 },
                        selection: None,
                    });
                }
            }
            PanelsMsg::Open(p) => self.open(&p, true),
            PanelsMsg::Backlinks { path, result } => match result {
                Ok(b) if self.panels.path.as_deref() == Some(path.as_str()) => {
                    self.panels.backlinks = b;
                }
                Ok(_) => {}
                Err(e) => self.status.error = Some(format!("backlinks: {e}")),
            },
            PanelsMsg::TagPick(tag) => {
                if let Some(d) = self.daemon.clone()
                    && let Err(e) = self.panels.pick_tag(d.as_ref(), &tag)
                {
                    self.status.error = Some(format!("tags: {e}"));
                }
            }
            PanelsMsg::TagBack => self.panels.tag_files = None,
            PanelsMsg::SearchInput(q) => {
                self.panels.search.query = q;
                self.panels.search.seq += 1;
                let seq = self.panels.search.seq;
                let wait = self.prefs.search_debounce();
                return Task::perform(async move { tokio::time::sleep(wait).await }, move |()| {
                    Message::Panels(PanelsMsg::SearchDue(seq))
                });
            }
            PanelsMsg::SearchDue(seq) => {
                if seq != self.panels.search.seq {
                    return Task::none();
                }
                let q = self.panels.search.query.clone();
                if q.trim().is_empty() {
                    self.panels.search.results.clear();
                    return Task::none();
                }
                let limit = self.prefs.search_limit;
                return self.rpc(
                    move |d| crate::panels::run_search(d, &q, limit),
                    move |result| Message::Panels(PanelsMsg::SearchDone { seq, result }),
                );
            }
            PanelsMsg::SearchDone { seq, result } => {
                if seq == self.panels.search.seq {
                    match result {
                        Ok(r) => {
                            self.panels.search.results = r;
                            self.panels.search.error = None;
                        }
                        Err(e) => self.panels.search.error = Some(e),
                    }
                }
            }
            PanelsMsg::Scrolled(id, w) => self.panels.set_window(id, w),
        }
        Task::none()
    }

    /// Brings the panels up to date with the focused pane's active tab (cheap when unchanged).
    fn sync_panels(&mut self) -> Task<Message> {
        if !self.panels.visible {
            return Task::none();
        }
        let t0 = std::time::Instant::now();
        let generation = self.status.generation;
        let d = self.daemon.clone();
        let tab = self
            .panes
            .get(self.focus)
            .and_then(PaneState::active_note)
            .and_then(|t| self.docs.get(&t.path));
        self.panels.sync_buffer(tab, d.as_deref(), generation);
        if let Some(d) = &d
            && let Err(e) = self.panels.sync_tags(d.as_ref(), generation)
        {
            self.status.error = Some(format!("tags: {e}"));
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        if ms >= 1.0 {
            tracing::debug!(elapsed_ms = ms, "sync_panels");
        }
        match self.panels.backlinks_due(generation) {
            Some(path) => {
                let p = path.clone();
                self.rpc(
                    move |d| crate::panels::load_backlinks(d, &p),
                    move |result| Message::Panels(PanelsMsg::Backlinks { path, result }),
                )
            }
            None => Task::none(),
        }
    }

    /// WP-7: view fences (`crate::views`); `view_run` runs off the UI thread.
    fn update_views(&mut self, m: crate::message::ViewsMsg) -> Task<Message> {
        match crate::views::update(&mut self.views, m) {
            crate::views::Effect::None => Task::none(),
            crate::views::Effect::Run { id, host } => {
                let (rid, rhost) = (id.clone(), host.clone());
                self.rpc(
                    move |d| {
                        d.view_run(&rid, rhost.as_deref())
                            .map(|r| r.0)
                            .map_err(|e| e.to_string())
                    },
                    move |result| {
                        Message::Views(crate::message::ViewsMsg::Loaded { id, host, result })
                    },
                )
            }
            crate::views::Effect::Open { path, line } => {
                self.open(&path, true);
                let line = line.and_then(|l| usize::try_from(l.saturating_sub(1)).ok());
                if let Some(line) = line
                    && self.active_note().is_some_and(|t| t.path == path)
                    && let Some(doc) = self.docs.get_mut(&path)
                {
                    doc.move_to(iced::widget::text_editor::Cursor {
                        position: iced::widget::text_editor::Position { line, column: 0 },
                        selection: None,
                    });
                }
                Task::none()
            }
        }
    }

    fn update_status(&mut self, m: StatusMsg) -> Task<Message> {
        match m {
            StatusMsg::Disconnected => {
                self.status.connected = false;
                return self.start_reconnect();
            }
            StatusMsg::ReconnectDue => return self.try_reconnect(),
            StatusMsg::Connected(Ok(conn)) => {
                if !self.status.reconnecting {
                    return Task::none();
                }
                return self.install_connection(conn);
            }
            StatusMsg::Connected(Err(e)) => {
                tracing::debug!(error = %e, attempt = self.reconnect_attempt, "reconnect failed");
                self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
                return self.schedule_reconnect();
            }
            StatusMsg::Notification(n) => {
                self.status.connected = true;
                match n {
                    Notification::ModeChanged { mode } => self.status.mode = mode,
                    Notification::IndexGeneration { n } => {
                        self.status.generation = n;
                        if let Some(d) = &self.daemon
                            && let Err(e) = d.index().on_generation(n)
                        {
                            self.status.error = Some(format!("index: {e}"));
                        }
                        // WP-2 (audit B1): every generation re-reads membership from the index
                        // (a `.gitignore` rebuild drops/restores paths).
                        self.refresh_files();
                        self.refresh_bookmark_missing();
                    }
                    Notification::JobUpdated(j) => {
                        let state = serde_json::to_value(j.status)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .unwrap_or_default();
                        self.status.last_job = Some(format!("{}: {state}", j.job_id));
                        self.palette.job_updated(&j, std::time::Instant::now());
                        self.job_finished(&j);
                    }
                    Notification::FileChanged {
                        path, kind, hash, ..
                    } => {
                        if kind != cv_protocol::ChangeKind::Modified {
                            // Before `reload_tab`: a localize pass creates the attachments,
                            // then rewrites the note, whose re-lower must see them.
                            self.reading.invalidate_resolver();
                            self.panels.invalidate_resolver();
                        }
                        self.reload_tab(&path, hash.as_deref());
                        self.panels.invalidate();
                        if path.starts_with(crate::palette::CONFIG_PREFIX) {
                            self.palette.invalidate();
                        }
                        if kind == cv_protocol::ChangeKind::Renamed {
                            // WP-8: a rename changes link targets any view may list; shown
                            // fences re-run while their old result stays up (no flash).
                            self.views.invalidate(&[]);
                        }
                        let indexed = self.indexed_fn();
                        self.explorer
                            .file_changed(&path, kind, &self.ui.expanded, &indexed);
                        if crate::settings::is_view_path(&path)
                            && panes::find(&self.panes, crate::settings::SETTINGS_TAB).is_some()
                        {
                            self.sync_view_files();
                        }
                        self.settings_file_changed(&path, hash.as_deref());
                        self.refresh_bookmark_missing();
                    }
                    Notification::OpenRequested { path } => self.open(&path, true),
                    Notification::ViewsInvalidated { view_ids } => self.views.invalidate(&view_ids),
                    Notification::ConfigReloaded { errors } => {
                        self.palette.invalidate();
                        self.settings.route_reload_errors(&errors);
                        // WP-1: refetch now (the daemon has reloaded actions.toml) so the
                        // keymap and the Hotkeys rows follow the new action keys; a fetch
                        // in flight from before the reload is superseded.
                        self.palette.loading = false;
                        let fetch = self.fetch_actions();
                        return Task::batch([fetch, self.refresh_status_if_open()]);
                    }
                    Notification::ShadowWrite { .. } => {}
                }
            }
        }
        Task::none()
    }

    // ---- plan editor-settings-tab WP-2: the Settings tab ----

    /// Loads `editor.toml` (boot): seeds `surround` from the old `UiState` once, applies prefs.
    fn load_editor_prefs(&mut self) {
        use crate::settings::editor_prefs::{self, EDITOR_PATH};
        // WP-4 (audit E2): `captures.toml` is loaded at boot too — navigation reads its period
        // folders, not only the Captures page.
        if let (Some(d), Some(f)) = (&self.daemon, self.settings.files.get_mut(CAPTURES_PATH)) {
            f.load(d.as_ref());
        }
        if let (Some(d), Some(f)) = (&self.daemon, self.settings.files.get_mut(EDITOR_PATH)) {
            f.load(d.as_ref());
            if editor_prefs::seed_surround(f, self.ui.surround) {
                self.settings.editor_note = Some(
                    "surround taken from the local editor state; it is written to editor.toml \
                     with the next Save"
                        .into(),
                );
            }
        }
        self.apply_prefs();
    }

    /// Recomputes the effective prefs from the `editor.toml` doc and applies them live.
    fn apply_prefs(&mut self) {
        use crate::settings::editor_prefs::{self, EDITOR_PATH};
        let prefs = match self.settings.file(EDITOR_PATH) {
            Some(f) => editor_prefs::effective(f, self.ui.surround),
            None => editor_prefs::Prefs {
                surround: self.ui.surround,
                ..editor_prefs::Prefs::default()
            },
        };
        self.tokens = tokens_for(prefs.surround);
        self.palette.set_hidden(prefs.palette_hidden.clone());
        // WP-4: `[keys]` overrides rebuild the effective keymap live (no restart).
        self.palette.set_keys(prefs.keys.clone());
        self.prefs = prefs;
        self.period_folders = period_folders(self.settings.file(CAPTURES_PATH));
        self.sync_hotkeys();
    }

    /// Refreshes the Hotkeys page's keymap / action snapshots from the palette.
    fn sync_hotkeys(&mut self) {
        let h = &mut self.settings.hotkeys;
        if !std::sync::Arc::ptr_eq(&h.keymap, &self.palette.keymap) {
            h.keymap = self.palette.keymap.clone();
        }
        // Plan editor-feedback-round-2 WP-1: an invalidated (refetching) action cache is not
        // an empty action list — keep the last snapshot until the refetch lands, else every
        // action row vanishes from the page.
        let Some(actions) = self.palette.actions.as_deref() else {
            return;
        };
        if h.actions != actions {
            h.actions = actions.to_vec();
        }
    }

    /// A chord captured while the Hotkeys page records: becomes the row's chord (in the doc;
    /// auto-saved, plan editor-feedback-round-2 WP-2), or a note when it cannot be bound.
    fn capture_chord(&mut self, c: &crate::palette::chord::Chord) -> Task<Message> {
        use crate::palette::chord;
        use crate::settings::pages::hotkeys::{self, ACTIONS_PATH, Target};
        let Some(target) = self.settings.hotkeys.recording.take() else {
            return Task::none();
        };
        let actions_doc = self
            .settings
            .file(ACTIONS_PATH)
            .and_then(|f| f.doc.as_ref());
        if !chord::text_editing(c)
            && let Some(other) = hotkeys::owner(&self.settings.hotkeys, actions_doc, &target, c)
        {
            // Plan editor-feedback-round-2 WP-1: the existing binding wins; nothing is edited.
            self.settings.hotkeys.note = Some(format!("{c} is already assigned to {other}"));
            return Task::none();
        }
        if let Target::Bookmark(path) = target {
            // WP-9: the chord goes on the bookmark record, written at once (shared CAS write).
            return match hotkeys::check(c) {
                Ok(()) => {
                    self.update_bookmarks(BookmarkMsg::SetKeys(path, Some(vec![c.to_string()])))
                }
                Err(n) => {
                    self.settings.hotkeys.note = Some(n);
                    Task::none()
                }
            };
        }
        match hotkeys::record_edit(&target, c, actions_doc) {
            Ok((file, edit)) => self.hotkey_edit(&file, &edit),
            Err(n) => self.settings.hotkeys.note = Some(n),
        }
        Task::none()
    }

    /// Applies a Hotkeys document edit and re-derives prefs / keymap.
    fn hotkey_edit(&mut self, file: &str, edit: &crate::settings::DocEdit) {
        if let Err(e) = self.settings.edit(file, edit) {
            self.settings.hotkeys.note = Some(e);
        }
        self.apply_prefs();
    }

    fn update_hotkeys(&mut self, m: crate::settings::pages::hotkeys::HotkeyMsg) -> Task<Message> {
        use crate::settings::pages::hotkeys::{self, ACTIONS_PATH, HotkeyMsg, Target};
        let doc = |s: &crate::settings::Settings, p: &str| s.file(p).and_then(|f| f.doc.clone());
        match m {
            HotkeyMsg::Clear(Target::Bookmark(path)) => {
                return self.update_bookmarks(BookmarkMsg::SetKeys(path, None));
            }
            HotkeyMsg::Record(t) => self.settings.hotkeys.record(t),
            HotkeyMsg::Cancel => self.settings.hotkeys.cancel(),
            HotkeyMsg::Clear(t) => {
                let ad = doc(&self.settings, ACTIONS_PATH);
                match hotkeys::clear_edit(&t, ad.as_ref()) {
                    Ok(Some((f, e))) => self.hotkey_edit(&f, &e),
                    Ok(None) => {}
                    Err(n) => self.settings.hotkeys.note = Some(n),
                }
            }
            HotkeyMsg::Reset(id) => {
                let ed = doc(&self.settings, crate::settings::editor_prefs::EDITOR_PATH);
                if let Some((f, e)) = hotkeys::reset_edit(&id, ed.as_ref()) {
                    self.hotkey_edit(&f, &e);
                }
            }
        }
        Task::none()
    }

    /// The surround switch: edits `editor.toml`'s `surround` (auto-saved after the debounce,
    /// plan editor-feedback-round-2 WP-2) and keeps the local
    /// `UiState` copy in step.
    fn set_surround(&mut self, s: crate::theme::Surround) {
        use crate::settings::editor_prefs::{self, EDITOR_PATH, PrefEdit};
        let edit = PrefEdit::Surround(s);
        let edited = self
            .settings
            .file_mut(EDITOR_PATH)
            .is_some_and(|f| f.edit(|doc| editor_prefs::apply(doc, &edit)).is_ok());
        if !edited {
            tracing::debug!("surround: editor.toml not editable; local state only");
        }
        self.ui.surround = s;
        self.apply_prefs();
        self.persist();
    }

    /// Opens the Settings tab in `pane`, or focuses it where it is already open (at most one).
    fn open_settings_into(&mut self, pane: Pane) {
        let sentinel = crate::settings::SETTINGS_TAB;
        if let Some((p, i)) = panes::find(&self.panes, sentinel) {
            self.focus = p;
            if let Some(ps) = self.panes.get_mut(p) {
                ps.active = Some(i);
            }
            return;
        }
        let pane = if self.panes.get(pane).is_some() {
            pane
        } else {
            self.focus
        };
        if let Some(ps) = self.panes.get_mut(pane) {
            ps.tabs.push(Tab::settings());
            ps.active = Some(ps.tabs.len() - 1);
        }
        self.focus = pane;
        self.sync_view_files();
    }

    /// Registers the `views/*.toml` files the explorer knows and loads every unloaded page.
    fn sync_view_files(&mut self) {
        self.settings.sync_views(&self.explorer.paths);
        if let Some(d) = &self.daemon {
            self.settings.load_unloaded(d.as_ref());
        }
    }

    /// Re-derives the bookmarks panel list from the shared `bookmarks.toml` [`ConfigFile`].
    fn sync_bookmarks(&mut self) {
        if let Some(f) = self.settings.file(BOOKMARKS_PATH) {
            self.bookmarks.sync(f);
        }
        // WP-9: the bookmark commands / chords follow the list.
        self.palette.set_bookmarks(self.bookmarks.items.clone());
        self.settings.hotkeys.bookmarks = self.bookmarks.items.clone();
        self.refresh_bookmark_missing();
        self.sync_hotkeys();
    }

    /// A `file_changed` for one of the settings files: reload when clean (external edits).
    fn settings_file_changed(&mut self, path: &str, hash: Option<&str>) {
        let Some(d) = self.daemon.clone() else {
            return;
        };
        let reloaded = self
            .settings
            .file_mut(path)
            .is_some_and(|f| f.file_changed(hash, d.as_ref()));
        if reloaded {
            self.settings.form.clear_drafts(path);
        }
        if path == BOOKMARKS_PATH {
            self.sync_bookmarks();
        }
        if reloaded && path == crate::settings::editor_prefs::EDITOR_PATH {
            self.settings.hidden_draft = None;
            self.apply_prefs();
        }
        if reloaded && path == CAPTURES_PATH {
            self.period_folders = period_folders(self.settings.file(CAPTURES_PATH));
        }
    }

    /// Fetches `status` off the UI thread when the Settings tab shows the Status page.
    fn refresh_status_if_open(&mut self) -> Task<Message> {
        let open = panes::find(&self.panes, crate::settings::SETTINGS_TAB).is_some();
        if open && self.settings.section == crate::settings::Section::Status {
            return self.fetch_status();
        }
        Task::none()
    }

    fn fetch_status(&mut self) -> Task<Message> {
        use crate::settings::SettingsMsg;
        if self.daemon.is_none() {
            self.settings.status.error = Some("not connected".into());
            return Task::none();
        }
        self.settings.status.loading = true;
        self.rpc(
            |d| d.status().map_err(|e| e.to_string()),
            |r| Message::Settings(SettingsMsg::StatusLoaded(r)),
        )
    }

    fn update_settings(&mut self, m: crate::settings::SettingsMsg) -> Task<Message> {
        use crate::settings::editor_prefs::{self, EDITOR_PATH};
        use crate::settings::{Section, SettingsMsg};
        match m {
            SettingsMsg::Open => {
                self.open_settings_into(self.focus);
                self.persist();
                if self.settings.section == Section::Hotkeys {
                    return self.fetch_actions();
                }
                return self.refresh_status_if_open();
            }
            SettingsMsg::Hotkey(m) => return self.update_hotkeys(m),
            SettingsMsg::Section(s) => {
                self.settings.section = s;
                self.settings.hotkeys.cancel();
                if s == Section::Views {
                    self.sync_view_files();
                }
                if s == Section::Hotkeys {
                    self.sync_view_files();
                    return self.fetch_actions();
                }
                if s == Section::Status {
                    return self.fetch_status();
                }
            }
            SettingsMsg::Pref(e) => {
                if let editor_prefs::PrefEdit::PaletteHidden(s) = &e {
                    self.settings.hidden_draft = Some(s.clone());
                }
                if let Some(f) = self.settings.file_mut(EDITOR_PATH)
                    && let Err(err) = f.edit(|doc| editor_prefs::apply(doc, &e))
                {
                    f.notice = Some(err);
                }
                if let editor_prefs::PrefEdit::Surround(s) = e {
                    self.ui.surround = s;
                    self.persist();
                }
                self.apply_prefs();
            }
            SettingsMsg::Edit(path, edit) => {
                let _ = self.settings.edit(&path, &edit);
                if path == EDITOR_PATH {
                    self.apply_prefs();
                }
            }
            SettingsMsg::Form(path, m) => {
                self.settings.form_msg(&path, m);
                if path == EDITOR_PATH {
                    self.apply_prefs();
                }
            }
            SettingsMsg::ViewSelect(p) => self.settings.view_selected = Some(p),
            SettingsMsg::NewViewName(n) => self.settings.new_view = n,
            SettingsMsg::NewViewCreate => {
                let name = std::mem::take(&mut self.settings.new_view);
                if let Err(e) = self.settings.new_view_file(&name) {
                    self.settings.new_view = name;
                    self.status.error = Some(e);
                }
            }
            SettingsMsg::Save(path) => {
                if self.shadow_blocked("settings saves are disabled in shadow mode") {
                    return Task::none();
                }
                return self.save_settings_file(path);
            }
            SettingsMsg::AutoSaveDue(path, seq) => {
                // Coalescing: only the timer of the latest edit writes.
                if self.settings.file(&path).is_some_and(|f| f.edit_seq == seq) {
                    return self.save_settings_file(path);
                }
            }
            SettingsMsg::Saved(path, r) => {
                let d = self.daemon.clone();
                let mut again = false;
                if let Some(f) = self.settings.file_mut(&path) {
                    let failed = !matches!(r, Ok(crate::settings::WriteOutcome::Saved(_)));
                    f.finish_save(r, d.as_deref());
                    again = f.queued && f.can_save();
                    if !again {
                        f.queued = false;
                    }
                    if let Some(q) = &mut self.quit
                        && q.settings.contains(&path)
                    {
                        if failed && !again {
                            q.losses.setting(&path);
                        }
                        if !again {
                            q.settings.remove(&path);
                        }
                    }
                }
                let mut tasks = Vec::new();
                if again {
                    tasks.push(self.save_settings_file(path.clone()));
                } else {
                    tasks.push(self.quit_progress());
                }
                if path == EDITOR_PATH {
                    self.settings.hidden_draft = None;
                    self.settings.editor_note = None;
                    self.apply_prefs();
                }
                if path == BOOKMARKS_PATH {
                    self.sync_bookmarks();
                }
                self.period_folders = period_folders(self.settings.file(CAPTURES_PATH));
                return Task::batch(tasks);
            }
            SettingsMsg::Revert(path) => {
                if let (Some(d), Some(f)) = (self.daemon.clone(), self.settings.file_mut(&path)) {
                    f.load(d.as_ref());
                    f.notice = None;
                }
                self.settings.form.clear_drafts(&path);
                if path == BOOKMARKS_PATH {
                    self.sync_bookmarks();
                }
                if path == EDITOR_PATH {
                    self.settings.hidden_draft = None;
                    self.apply_prefs();
                }
            }
            SettingsMsg::StartOver(path) => {
                if let Some(f) = self.settings.file_mut(&path) {
                    f.start_over();
                }
                if path == EDITOR_PATH {
                    self.apply_prefs();
                }
            }
            SettingsMsg::RefreshStatus => return self.fetch_status(),
            SettingsMsg::StatusLoaded(r) => self.settings.status.loaded(r),
        }
        // WP-4 (audit E2): a captures.toml edit / save / revert re-derives the period folders.
        self.period_folders = period_folders(self.settings.file(CAPTURES_PATH));
        Task::none()
    }

    /// Daemon notifications and global keys as iced subscriptions.
    pub fn subscription(&self) -> Subscription<Message> {
        let captured_esc = if self.switcher.open {
            iced::event::listen_with(switcher::captured_escape)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            Subscription::run_with(self.notifications.clone(), notification_stream),
            captured_esc,
            // WP-4: track held mouse buttons only while hover focus is on (drag guard).
            if self.prefs.focus_follows_mouse {
                iced::event::listen_with(mouse_button_event)
            } else {
                Subscription::none()
            },
            iced::keyboard::listen().filter_map(switcher::key_event),
            iced::keyboard::listen().filter_map(|e| crate::dialog::key_event(&e)),
            self.reading.subscription(),
            crate::palette::subscription(&self.palette, self.panels.visible),
            crate::find::subscription(self.panes.iter().any(|(_, ps)| ps.find.is_some())),
            // Window close goes through Quit (dirty buffers flush first).
            iced::window::close_requests().map(|_| Message::Tabs(TabsMsg::Quit)),
            // WP-3: focus loss flushes every dirty note buffer.
            iced::window::events().filter_map(|(_, e)| {
                matches!(e, iced::window::Event::Unfocused)
                    .then_some(Message::Tabs(TabsMsg::FlushAll))
            }),
        ])
    }

    /// The iced theme for the current surround.
    pub fn theme(&self) -> Theme {
        let name = match self.prefs.surround {
            Surround::Dark => "N1 Dark",
            Surround::Med => "N1 Med",
            Surround::Light => "N1 Light",
        };
        self.tokens.iced_theme(name)
    }

    /// The backdrop-click / close message for the open overlay (`None` = no overlay open).
    pub fn overlay_close(&self) -> Option<Message> {
        if self.dialog.is_some() {
            Some(Message::Dialog(crate::dialog::DialogMsg::Cancel))
        } else if self.switcher.open {
            Some(Message::Switcher(SwitcherMsg::Close))
        } else if self.palette.open {
            Some(Message::Palette(PaletteMsg::Close))
        } else {
            None
        }
    }

    /// `tab`'s frontmatter `title` (cached per edit seq and reload epoch — an external reload
    /// does not bump `edit_seq`).
    pub fn frontmatter_title(&self, tab: &Tab) -> Option<String> {
        let doc = self.docs.get(&tab.path)?;
        let seq = (doc.edit_seq(), doc.epoch());
        let mut cache = self.fm_titles.borrow_mut();
        if let Some((s, t)) = cache.get(&tab.path)
            && *s == seq
        {
            return t.clone();
        }
        let t = crate::header::frontmatter_title(&doc.text());
        cache.insert(tab.path.clone(), (seq, t.clone()));
        t
    }

    /// The header text of `ps`'s tab `tab` under the `tab_title` pref.
    pub fn tab_header_title(&self, ps: &PaneState, tab: &Tab) -> String {
        match tab.kind() {
            TabKind::Settings => "Settings".to_owned(),
            TabKind::Note(p) => {
                let siblings: Vec<&str> = ps
                    .tabs
                    .iter()
                    .filter(|t| !t.is_settings())
                    .map(|t| t.path.as_str())
                    .collect();
                let fm = match self.prefs.tab_title {
                    crate::header::TabTitle::Title => self.frontmatter_title(tab),
                    _ => None,
                };
                crate::header::resolve(self.prefs.tab_title, p, &siblings, fm.as_deref())
            }
        }
    }

    /// The tab bar: the pane's tab headers (fixed height, clipped), then the close-pane button.
    fn tab_bar<'a>(&'a self, pane: Pane, ps: &'a PaneState) -> Element<'a, Message> {
        use crate::header::{BAR_HEIGHT, MARKER_WIDTH, TITLE_MAX_CHARS, ellipsize, marker};
        use crate::icons::Icon;
        use crate::theme::{FONT_FAMILY, control_label, icon_button};
        let t = self.tokens;
        let focused = pane == self.focus;
        let headers = row(ps.tabs.iter().enumerate().map(|(i, tab)| {
            let active = ps.active == Some(i);
            // Focus cue (borders are debug-only): the focused pane's active tab is an
            // `accent_text` label on a `panel_2` block; an unfocused pane's active tab keeps the
            // block with a `text` label; inactive tabs are `text_dim`.
            let colour = match (active, focused) {
                (true, true) => t.accent_text,
                (true, false) => t.text,
                _ => t.text_dim,
            };
            let (dirty, saving) = if tab.is_settings() {
                (self.settings.any_dirty(), false)
            } else {
                self.docs
                    .get(&tab.path)
                    .map_or((false, false), |d| (d.is_dirty(), d.is_saving()))
            };
            let title = ellipsize(&self.tab_header_title(ps, tab), TITLE_MAX_CHARS);
            let lbl = control_label(title, 14.0)
                .font(iced::Font::with_name(FONT_FAMILY))
                .wrapping(iced::widget::text::Wrapping::None)
                .color(colour.to_color());
            // The marker owns a fixed-width slot, blank when clean: the header never resizes.
            let mark =
                container(control_label(marker(dirty, saving), 14.0).color(colour.to_color()))
                    .width(MARKER_WIDTH)
                    .height(BAR_HEIGHT)
                    .center_x(MARKER_WIDTH);
            let select = button(row![lbl, mark].spacing(2).height(BAR_HEIGHT))
                .height(BAR_HEIGHT)
                .padding([0, 6])
                .style(button::text)
                .on_press(Message::Tabs(TabsMsg::Select(pane, i)));
            let hover = if tab.is_settings() {
                "Settings".to_owned()
            } else {
                tab.path.clone()
            };
            let select = iced::widget::tooltip(
                select,
                container(text(hover).size(13))
                    .padding(6)
                    .style(crate::theme::seg_group_style(t)),
                iced::widget::tooltip::Position::Bottom,
            );
            let chip = row![
                select,
                button(control_label("×", 14.0))
                    .height(BAR_HEIGHT)
                    .padding([0, 4])
                    .style(button::text)
                    .on_press(Message::Tabs(TabsMsg::Close(pane, i))),
            ]
            .spacing(0)
            .height(BAR_HEIGHT)
            .align_y(iced::Alignment::Center);
            // WP-4: right-click a note tab for its context menu.
            let chip: Element<'a, Message> = if tab.is_settings() {
                chip.into()
            } else {
                crate::context_menu::on_right(
                    chip,
                    crate::context_menu::Target::Tab {
                        pane,
                        id: tab.id,
                        path: tab.path.clone(),
                    },
                )
            };
            container(chip)
                .height(BAR_HEIGHT)
                .clip(true)
                .style(move |_: &Theme| container::Style {
                    background: active.then(|| t.panel_2.to_color().into()),
                    border: iced::Border {
                        radius: crate::theme::RADIUS.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                })
                .into()
        }))
        .spacing(4)
        .height(BAR_HEIGHT)
        .align_y(iced::Alignment::Center);
        let headers = container(headers)
            .width(Length::Fill)
            .height(BAR_HEIGHT)
            .clip(true);
        // The close-pane button is pane-level, so it lives at the tab bar's right end.
        let close: Element<'a, Message> = if self.panes.len() > 1 {
            icon_button(Icon::X, Some(Message::Panes(PanesMsg::Close(pane))), t).into()
        } else {
            Space::new().into()
        };
        row![headers, close]
            .spacing(4)
            .height(BAR_HEIGHT)
            .align_y(iced::Alignment::Center)
            .into()
    }

    /// The note control bar: back/forward seg-group, centred breadcrumb (folders + editable
    /// filename), the view-mode control. Only for note tabs.
    fn note_bar<'a>(
        &'a self,
        pane: Pane,
        ps: &'a PaneState,
        tab: &'a Tab,
        mode_btn: Element<'a, Message>,
    ) -> Element<'a, Message> {
        use crate::header::{BAR_HEIGHT, folders};
        use crate::icons::Icon;
        use crate::theme::{FONT_FAMILY, SEGMENT, control_label, icon_text};
        let t = self.tokens;
        let seg = |icon: Icon, msg: Option<Message>| {
            let enabled = msg.is_some();
            button(icon_text(icon))
                .width(SEGMENT)
                .height(SEGMENT)
                .padding(0)
                .style(move |th: &Theme, st| {
                    let mut s = crate::theme::segment_style(t, false)(th, st);
                    if !enabled {
                        s.text_color = t.text_faint.to_color();
                    }
                    s
                })
                .on_press_maybe(msg)
        };
        let history = container(
            row![
                seg(
                    Icon::ChevronLeft,
                    ps.history
                        .can_back()
                        .then_some(Message::Panes(PanesMsg::Back(pane))),
                ),
                seg(
                    Icon::ChevronRight,
                    ps.history
                        .can_forward()
                        .then_some(Message::Panes(PanesMsg::Forward(pane))),
                ),
            ]
            .spacing(0),
        )
        .padding(1)
        .style(crate::theme::seg_group_style(t));
        let path = tab.path.as_str();
        let mut crumbs: Vec<Element<'a, Message>> = Vec::new();
        for f in folders(path) {
            crumbs.push(
                control_label(f, 14.0)
                    .font(iced::Font::with_name(FONT_FAMILY))
                    .wrapping(iced::widget::text::Wrapping::None)
                    .color(t.text_dim.to_color())
                    .into(),
            );
            crumbs.push(
                control_label("/", 14.0)
                    .color(t.text_faint.to_color())
                    .into(),
            );
        }
        let name = self.reading.title(path);
        // Sized to its text (approximate advance) so the breadcrumb stays centred.
        #[allow(clippy::cast_precision_loss)]
        let name_w = (name.chars().count().max(4) as f32)
            .mul_add(8.5, 20.0)
            .min(360.0);
        let p_in = path.to_owned();
        crumbs.push(
            iced::widget::text_input("note name", name)
                .id(crate::reading::title_id(path))
                .size(14)
                .width(name_w)
                .on_input(move |value| {
                    Message::Reading(crate::message::ReadingMsg::TitleInput {
                        path: p_in.clone(),
                        value,
                    })
                })
                .on_submit(Message::Reading(crate::message::ReadingMsg::TitleSubmit {
                    path: path.to_owned(),
                }))
                .style(
                    move |theme: &Theme, status| iced::widget::text_input::Style {
                        border: iced::Border {
                            width: 0.0,
                            ..iced::widget::text_input::default(theme, status).border
                        },
                        background: iced::Background::Color(iced::Color::TRANSPARENT),
                        value: t.text.to_color(),
                        ..iced::widget::text_input::default(theme, status)
                    },
                )
                .into(),
        );
        let breadcrumb = container(
            row(crumbs)
                .spacing(4)
                .height(BAR_HEIGHT)
                .align_y(iced::Alignment::Center),
        )
        .center_x(Length::Fill)
        .height(BAR_HEIGHT)
        .clip(true);
        row![history, breadcrumb, mode_btn]
            .spacing(4)
            .height(BAR_HEIGHT)
            .align_y(iced::Alignment::Center)
            .into()
    }

    fn pane_view<'a>(&'a self, pane: Pane, ps: &'a PaneState) -> pane_grid::Content<'a, Message> {
        use crate::icons::Icon;
        use crate::theme::{icon_button_style, icon_text};
        let t = self.tokens;
        let focused = pane == self.focus;
        let mode = ps.active_note().map(|tab| self.reading.mode(&tab.path));
        let mode_icon = match mode {
            Some(crate::reading::ViewMode::Source) => Icon::Code,
            Some(crate::reading::ViewMode::Split) => Icon::Columns2,
            Some(crate::reading::ViewMode::Reading) | None => Icon::BookOpen,
        };
        let menu_open = self.mode_menu == Some(pane) && mode.is_some();
        let mode_btn = button(icon_text(mode_icon))
            .width(crate::theme::CONTROL)
            .height(crate::theme::CONTROL)
            .padding(0)
            .style(icon_button_style(t, menu_open))
            .on_press_maybe(
                mode.map(|_| Message::Panes(PanesMsg::ModeMenu((!menu_open).then_some(pane)))),
            );
        // Stable slots: tab bar, note control bar (`Space` for Settings / no tab), find bar,
        // period nav bar (`Space` when the note gets none), body — so the body's tree position
        // (and the editor's widget state) never depends on which bars are showing.
        let note_bar: Element<'a, Message> = match ps.active_note() {
            Some(tab) => self.note_bar(pane, ps, tab, mode_btn.into()),
            None => Space::new().into(),
        };
        let nav_bar: Element<'a, Message> = ps
            .active_note()
            .and_then(|tab| nav::bar(pane, &tab.path, self.today(), &self.period_folders, t))
            .map_or_else(
                || Space::new().into(),
                |bar| {
                    container(bar)
                        .height(crate::header::BAR_HEIGHT)
                        .width(Length::Fill)
                        .clip(true)
                        .into()
                },
            );
        let body: Element<'a, Message> = match ps.active_tab() {
            Some(tab) => self.tab_body(tab),
            None => text("No file open").color(t.text_faint.to_color()).into(),
        };
        let col = column![
            self.tab_bar(pane, ps),
            note_bar,
            // Stable slot: an empty `Space` while the find bar is closed.
            crate::find::view(pane, ps.find.as_ref(), t),
            nav_bar,
            body,
        ]
        .spacing(4);
        // The dropdown sits in a stack layer above the pane body (layer 0 never moves).
        let menu: Element<'a, Message> = match (menu_open, ps.active_note(), mode) {
            (true, Some(tab), Some(current)) => {
                use crate::reading::ViewMode;
                let item = |label: &'static str, icon: Icon, m: ViewMode| {
                    let on = m == current;
                    button(
                        row![
                            text(icon.glyph().to_string())
                                .font(crate::icons::FONT)
                                .size(crate::theme::ICON_SIZE),
                            text(label).size(14),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center),
                    )
                    .height(crate::theme::SEGMENT)
                    .width(Length::Fill)
                    .padding([0, 10])
                    .style(crate::theme::segment_style(t, on))
                    .on_press(Message::Reading(
                        crate::message::ReadingMsg::SetMode {
                            path: tab.path.clone(),
                            mode: m,
                        },
                    ))
                };
                let list = container(
                    column![
                        item("Source", Icon::Code, ViewMode::Source),
                        item("Split", Icon::Columns2, ViewMode::Split),
                        item("Reading", Icon::BookOpen, ViewMode::Reading),
                    ]
                    .spacing(0),
                )
                .padding(1)
                .width(160)
                .style(crate::theme::seg_group_style(t));
                iced::widget::mouse_area(
                    container(iced::widget::opaque(list))
                        .padding(iced::Padding {
                            // Below the tab bar + note control bar (`BAR_HEIGHT` each, 4 apart).
                            top: 2.0f32.mul_add(crate::header::BAR_HEIGHT, 8.0),
                            ..iced::Padding::ZERO
                        })
                        .align_x(iced::alignment::Horizontal::Right)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .on_press(Message::Panes(PanesMsg::ModeMenu(None)))
                .into()
            }
            _ => Space::new().into(),
        };
        let body = iced::widget::stack![col, menu];
        let border = if !self.ui.pane_borders {
            None
        } else if focused {
            Some(t.accent_ui)
        } else {
            Some(t.line)
        };
        // plan editor-round-4 WP-4: the pointer entering a pane may focus it. The wrapper is
        // always present (stable widget tree); only its `on_enter` follows the pref.
        let mut hover = iced::widget::mouse_area(container(body).padding(8).height(Length::Fill));
        if self.prefs.focus_follows_mouse && !focused {
            hover = hover.on_enter(Message::Panes(PanesMsg::Hover(pane)));
        }
        pane_grid::Content::new(hover).style(move |_: &Theme| container::Style {
            border: iced::Border {
                color: border.map_or(iced::Color::TRANSPARENT, crate::theme::Rgb::to_color),
                width: if border.is_some() { 1.0 } else { 0.0 },
                radius: 0.0.into(),
            },
            ..container::Style::default()
        })
    }

    fn side_view(&self) -> Element<'_, Message> {
        use crate::icons::Icon;
        use crate::theme::icon_button;
        let t = self.tokens;
        let collapsed = self.ui.side_collapsed;
        // Collapsed: a 46px icon strip; any icon expands. The full column stays in the tree (a
        // zero-width clipped container) so the explorer's scroll offset and focus survive.
        let strip: Element<'_, Message> = if collapsed {
            let expand = || Some(Message::Panes(PanesMsg::ToggleSide));
            column![
                icon_button(Icon::PanelLeftOpen, expand(), t),
                icon_button(Icon::FolderTree, expand(), t),
                icon_button(Icon::Bookmark, expand(), t),
            ]
            .spacing(6)
            .padding(6)
            .into()
        } else {
            Space::new().into()
        };
        let header = row![
            Space::new().width(Length::Fill),
            icon_button(
                Icon::PanelLeftClose,
                Some(Message::Panes(PanesMsg::ToggleSide)),
                t,
            ),
        ];
        let full = column![
            header,
            container(explorer::view(&self.explorer, t, self.status.mode)).height(Length::Fill),
            bookmarks::view(&self.bookmarks, t, self.status.mode),
        ]
        .spacing(8)
        .padding(4);
        let full = container(full)
            .width(if collapsed {
                Length::Fixed(0.0)
            } else {
                Length::Fill
            })
            .height(Length::Fill)
            .clip(true);
        row![strip, full].height(Length::Fill).into()
    }

    /// The view.
    pub fn view(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let panel = move |_: &Theme| container::Style {
            background: Some(t.panel.to_color().into()),
            text_color: Some(t.text.to_color()),
            ..container::Style::default()
        };
        let inset = move |_: &Theme| container::Style {
            background: Some(t.inset.to_color().into()),
            text_color: Some(t.text_dim.to_color()),
            ..container::Style::default()
        };

        let top = row![
            button(text("Quick switch (Ctrl+P)").size(14))
                .style(button::text)
                .on_press(Message::Switcher(SwitcherMsg::Open)),
            button(text("Command palette (Ctrl+Shift+P)").size(14))
                .style(button::text)
                .on_press(Message::Palette(PaletteMsg::Open)),
            button(text("Panels").size(14))
                .style(button::text)
                .on_press(Message::Panels(PanelsMsg::Toggle)),
            button(text("Split right").size(14))
                .style(button::text)
                .on_press(Message::Panes(PanesMsg::SplitRight)),
            button(text("Split down").size(14))
                .style(button::text)
                .on_press(Message::Panes(PanesMsg::SplitDown)),
            button(text("Settings (Ctrl+,)").size(14))
                .style(button::text)
                .on_press(Message::Settings(crate::settings::SettingsMsg::Open)),
        ]
        .spacing(8);

        let grid = PaneGrid::new(&self.panes, |pane, ps, _| self.pane_view(pane, ps))
            .spacing(4)
            .on_click(|p| Message::Panes(PanesMsg::Focus(p)))
            .on_resize(8, |e| Message::Panes(PanesMsg::Resized(e)));

        let toast = self
            .palette
            .toast
            .as_ref()
            .map(|t| format!("▸ {}", t.line()))
            .unwrap_or_default();
        let status = row![
            text(self.status.line()).size(16),
            text(toast).size(16).color(t.accent_text.to_color()),
            button(text(format!("Surround: {}", self.prefs.surround.label())).size(16))
                .style(button::text)
                .on_press(Message::Theme(ThemeMsg::Cycle)),
        ]
        .spacing(16);

        let main = column![container(top).padding(4).width(Length::Fill).style(panel)];
        let side_width = if self.ui.side_collapsed {
            Length::Fixed(46.0)
        } else {
            Length::FillPortion(1)
        };
        let mut body = row![
            container(self.side_view())
                .style(inset)
                .width(side_width)
                .height(Length::Fill),
            container(grid)
                .padding(4)
                .width(Length::FillPortion(4))
                .height(Length::Fill),
        ];
        if self.panels.visible {
            body = body.push(
                container(crate::panels::view(&self.panels, t))
                    .style(inset)
                    .width(Length::FillPortion(1))
                    .height(Length::Fill),
            );
        }
        let base = main.push(body.height(Length::Fill)).push(
            container(status)
                .padding(4)
                .width(Length::Fill)
                .style(panel),
        );

        // Widget state is matched by tree position: the base layer never changes shape when the
        // switcher / palette opens; they render as a centred card in layer 1 (`Space` when
        // closed). The backdrop closes on click; the card itself swallows clicks (`opaque`).
        let overlay: Element<'_, Message> = match self.overlay_close() {
            Some(close) => {
                let mut card = column![].spacing(6).width(Length::Fill);
                if let Some(d) = &self.dialog {
                    card = card.push(crate::dialog::view(d, t));
                } else if self.switcher.open {
                    card = card.push(switcher::view(&self.switcher, t, self.status.mode));
                }
                if self.palette.open && self.dialog.is_none() {
                    card = card.push(crate::palette::view(&self.palette, t));
                }
                let card = container(card)
                    .padding(10)
                    .width(640)
                    .style(move |_: &Theme| container::Style {
                        background: Some(t.panel.to_color().into()),
                        text_color: Some(t.text.to_color()),
                        border: iced::Border {
                            color: t.line.to_color(),
                            width: 1.0,
                            radius: crate::theme::RADIUS.into(),
                        },
                        ..container::Style::default()
                    });
                iced::widget::mouse_area(
                    container(iced::widget::opaque(card))
                        .padding(iced::Padding {
                            top: 80.0,
                            ..iced::Padding::ZERO
                        })
                        .align_x(iced::alignment::Horizontal::Center)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .on_press(close)
                .into()
            }
            None => Space::new().into(),
        };
        // WP-4: the context menu is layer 2 (`Space` when closed; never beneath a dialog).
        let menu: Element<'_, Message> = match (&self.context_menu, &self.dialog) {
            (Some(m), None) => crate::context_menu::view(m, t),
            _ => Space::new().into(),
        };
        iced::widget::stack![base, overlay, menu].into()
    }
}

/// WP-4: mouse press / release → [`PanesMsg::MouseButton`] (captured or not).
fn mouse_button_event(
    e: iced::Event,
    _: iced::event::Status,
    _: iced::window::Id,
) -> Option<Message> {
    match e {
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_)) => {
            Some(Message::Panes(PanesMsg::MouseButton(true)))
        }
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(_)) => {
            Some(Message::Panes(PanesMsg::MouseButton(false)))
        }
        _ => None,
    }
}

/// User input a modal layer (dialog, then context menu) drops: keys, chords, tab / editor /
/// find / switcher messages, the focus-loss flush, a repeated close request. Async
/// completions (saves, autosave ticks) still run.
fn is_modal_blocked(m: &Message) -> bool {
    match m {
        Message::Switcher(_) | Message::Find(_) | Message::Palette(PaletteMsg::Key(_)) => true,
        Message::Tabs(TabsMsg::QuitSaved { .. } | TabsMsg::CloseSaved { .. }) => false,
        Message::Tabs(_) => true,
        Message::Source(SourceMsg::Saved { .. } | SourceMsg::AutosaveDue { .. }) => false,
        Message::Source(_) => true,
        _ => false,
    }
}

fn open_error(e: &DaemonError) -> String {
    match e {
        DaemonError::NotFound(_) => "file not found".into(),
        DaemonError::InvalidPath => "invalid path".into(),
        other => format!("open failed: {other}"),
    }
}

fn notification_stream(handle: &NotifHandle) -> impl iced::futures::Stream<Item = Message> + use<> {
    use iced::futures::{SinkExt, StreamExt};
    let rx = handle.0.lock().ok().and_then(|mut g| g.take());
    iced::stream::channel(64, async move |mut out| {
        let Some(rx) = rx else {
            // No connection (boot without a daemon, or a failed subscribe): start the
            // reconnect loop (plan editor-data-safety WP-1).
            let _ = out.send(Message::Status(StatusMsg::Disconnected)).await;
            return;
        };
        // Bridge the blocking std receiver onto an async channel from a dedicated thread.
        let (tx, mut arx) = iced::futures::channel::mpsc::unbounded();
        let spawned = std::thread::Builder::new()
            .name("cv-notify-bridge".into())
            .spawn(move || {
                while let Ok(n) = rx.recv() {
                    if tx.unbounded_send(n).is_err() {
                        break;
                    }
                }
            });
        if spawned.is_ok() {
            while let Some(n) = arx.next().await {
                if out
                    .send(Message::Status(StatusMsg::Notification(n)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        let _ = out.send(Message::Status(StatusMsg::Disconnected)).await;
    })
}

/// plan editor-round-4 WP-3: the rendered model the find bar counts visible occurrences in —
/// `path` in Reading mode with a render; `None` (source matches drive the bar) otherwise.
fn reading_find_model<'a>(
    reading: &'a crate::reading::ReadingStore,
    path: &str,
) -> Option<&'a editor_model::render::RenderModel> {
    (reading.mode(path) == crate::reading::ViewMode::Reading)
        .then(|| reading.doc(path).map(|d| &d.model))
        .flatten()
}
