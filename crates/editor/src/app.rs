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
use iced::widget::{PaneGrid, button, column, container, row, text};
use iced::{Element, Length, Subscription, Task, Theme};

use crate::bookmarks::{self, BOOKMARKS_PATH, BookmarkMsg, Bookmarks, WriteOutcome};
use crate::daemon::{DaemonClient, DaemonError};
use crate::explorer::{self, EditKind, Explorer, FileOp, FileOpDone};
use crate::message::{
    ExplorerMsg, Message, NavMsg, PaletteMsg, PanelsMsg, PanesMsg, SourceMsg, StatusMsg,
    SwitcherMsg, TabsMsg, ThemeMsg,
};
use crate::nav;
use crate::panes::{self, PaneState};
use crate::switcher::{self, Switcher};
use crate::theme::{Surround, Tokens};
use crate::ui_state::{self, SplitDir, UiState};

/// One open tab: its path and source buffer (WP-2 `source::SourceBuffer`).
#[derive(Debug, Clone)]
pub struct Tab {
    pub path: String,
    pub source: crate::source::SourceBuffer,
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
}

impl StatusModel {
    /// The bar's text.
    pub fn line(&self) -> String {
        let conn = if self.connected {
            "connected"
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
#[derive(Clone, Default)]
pub struct NotifHandle(Arc<Mutex<Option<Receiver<Notification>>>>);

impl NotifHandle {
    /// Wraps a receiver.
    pub fn new(rx: Option<Receiver<Notification>>) -> Self {
        Self(Arc::new(Mutex::new(rx)))
    }
}

impl Hash for NotifHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "daemon-notifications".hash(state);
    }
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
    state_path: Option<PathBuf>,
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
            state_path,
        };
        app.refresh_files();
        app.load_bookmarks();
        for (pane, paths, active) in plan {
            for p in &paths {
                if let Err(e) = app.open_into(pane, p, false) {
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
        app
    }

    /// A disconnected app (used if boot cannot take the prepared one).
    pub fn disconnected() -> Self {
        Self::new(None, None)
    }

    /// The focused pane's active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.panes.get(self.focus).and_then(PaneState::active_tab)
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

    fn refresh_files(&mut self) {
        let Some(d) = &self.daemon else { return };
        if let Err(e) = self.explorer.build(d.index(), &self.ui.expanded) {
            self.status.error = Some(e);
        }
        if self.switcher.open {
            self.switcher.refresh(&self.explorer.paths, &self.ui.recent);
        }
    }

    fn load_bookmarks(&mut self) {
        let Some(d) = &self.daemon else { return };
        if let Err(e) = self.bookmarks.load(d.as_ref()) {
            self.status.error = Some(e);
        }
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
        if let Err(e) = self.open_into(self.focus, path, false) {
            self.status.error = Some(open_error(&e));
            return;
        }
        if persist {
            self.persist();
        }
    }

    /// Opens `path` in `pane`: an already-open tab anywhere is focused instead; with `replace`
    /// the pane's active tab is replaced (unless it has unsaved edits, then a new tab opens).
    fn open_into(&mut self, pane: Pane, path: &str, replace: bool) -> Result<(), DaemonError> {
        if let Some((p, i)) = panes::find(&self.panes, path) {
            self.focus = p;
            if let Some(ps) = self.panes.get_mut(p) {
                ps.active = Some(i);
            }
            return Ok(());
        }
        let Some(d) = &self.daemon else {
            return Err(DaemonError::Disconnected);
        };
        let (bytes, hash) = d.read_file(path)?;
        let pane = if self.panes.get(pane).is_some() {
            pane
        } else {
            self.focus
        };
        let tab = Tab {
            path: path.to_owned(),
            source: crate::source::SourceBuffer::new(path, &bytes, hash),
        };
        let mut replaced = None;
        if let Some(ps) = self.panes.get_mut(pane) {
            match ps.active.filter(|&i| i < ps.tabs.len()) {
                Some(i) if replace && !ps.tabs[i].source.is_dirty() => {
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
        self.ui.touch_recent(path);
        Ok(())
    }

    /// The single tab-body render site (WP-3 swaps this expression for the mode switch).
    fn tab_body<'a>(&'a self, tab: &'a Tab) -> Element<'a, Message> {
        crate::reading::tab_body(
            &self.reading,
            &self.views,
            tab,
            self.tokens,
            self.status.mode,
        )
    }

    /// Every tab removal funnels through here (button, pane close, replace, trash).
    fn on_tab_closed(&mut self, path: &str) {
        #[cfg(test)]
        self.closed.push(path.to_owned());
        self.reading.on_tab_closed(path);
        self.views.on_tab_closed(path);
    }

    /// `file_changed`: echo / silent reload / conflict bar (WP-2 `source::file_changed`).
    fn reload_tab(&mut self, path: &str, hash: Option<&str>) {
        let daemon = self.daemon.as_ref();
        for (_, ps) in self.panes.iter_mut() {
            if let Some(e) = crate::source::file_changed(&mut ps.tabs, path, hash, daemon) {
                self.status.error = Some(e);
            }
        }
    }

    fn persist(&mut self) {
        let layout = panes::snapshot(&self.panes, self.focus);
        if let Some(ps) = self.panes.get(self.focus) {
            self.ui.tabs = ps.tabs.iter().map(|t| t.path.clone()).collect();
            self.ui.active_tab = ps.active;
        }
        self.ui.panes = Some(layout);
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
        match message {
            Message::Tabs(m) => self.update_tabs(m),
            Message::Status(m) => self.update_status(m),
            Message::Theme(m) => {
                self.ui.surround = match m {
                    ThemeMsg::Cycle => self.ui.surround.next(),
                    ThemeMsg::Set(s) => s,
                };
                self.tokens = tokens_for(self.ui.surround);
                self.persist();
            }
            Message::Explorer(m) => return self.update_explorer(m),
            Message::Source(m) => {
                let pane = match &m {
                    SourceMsg::AutosaveDue { path, .. }
                    | SourceMsg::Saved { path, .. }
                    | SourceMsg::KeepMine(path)
                    | SourceMsg::TakeTheirs(path) => panes::find(&self.panes, path).map(|(p, _)| p),
                    _ => Some(self.focus),
                };
                let Some(ps) = pane.and_then(|p| self.panes.get_mut(p)) else {
                    return Task::none();
                };
                return crate::source::update(
                    &mut ps.tabs,
                    ps.active,
                    m,
                    crate::source::Ctx {
                        daemon: self.daemon.as_ref(),
                        mode: self.status.mode,
                        error: &mut self.status.error,
                    },
                );
            }
            Message::Reading(m) => {
                let tabs: Vec<&Tab> = self
                    .panes
                    .iter()
                    .flat_map(|(_, ps)| ps.tabs.iter())
                    .collect();
                return crate::reading::update(
                    &mut self.reading,
                    m,
                    &tabs,
                    self.panes.get(self.focus).and_then(PaneState::active_tab),
                    self.daemon.as_ref(),
                );
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

    fn update_tabs(&mut self, m: TabsMsg) {
        match m {
            TabsMsg::Open(p) => self.open(&p, true),
            TabsMsg::Select(pane, i) => {
                if let Some(ps) = self.panes.get_mut(pane)
                    && i < ps.tabs.len()
                {
                    ps.active = Some(i);
                    self.focus = pane;
                    self.persist();
                }
            }
            TabsMsg::Close(pane, i) => {
                if let Some(tab) = self.panes.get_mut(pane).and_then(|ps| ps.remove(i)) {
                    self.on_tab_closed(&tab.path);
                    self.persist();
                }
            }
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
            PanesMsg::Resized(ev) => self.panes.resize(ev.split, ev.ratio),
            PanesMsg::Bookmark(b) => return self.update_bookmarks(b),
        }
        Task::none()
    }

    fn update_bookmarks(&mut self, m: BookmarkMsg) -> Task<Message> {
        match m {
            BookmarkMsg::Open(p) => self.open(&p, true),
            BookmarkMsg::Written(r) => {
                self.bookmarks.writing = false;
                match r {
                    Ok(WriteOutcome::Saved(h)) => self.bookmarks.hash = Some(h),
                    Ok(WriteOutcome::Created) => self.load_bookmarks(),
                    Ok(WriteOutcome::Conflict) => {
                        self.load_bookmarks();
                        self.status.error =
                            Some("bookmarks changed on disk; reloaded (edit not saved)".into());
                    }
                    Err(e) => {
                        self.load_bookmarks();
                        self.status.error = Some(format!("bookmarks: {e}"));
                    }
                }
            }
            edit => {
                if self.shadow_blocked("bookmark edits are disabled in shadow mode")
                    || self.bookmarks.writing
                {
                    return Task::none();
                }
                let active = self.active_tab().map(|t| t.path.clone());
                if let Some(job) = self.bookmarks.edit(&edit, active.as_deref()) {
                    return self.rpc(
                        move |d| job.run(d),
                        |r| Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Written(r))),
                    );
                }
            }
        }
        Task::none()
    }

    fn dispatch_op(&mut self, op: FileOp) -> Task<Message> {
        // A rename/move/trash re-reads (or closes) the open tab, which would drop unsaved edits.
        let target = match &op {
            FileOp::Rename { from, .. } => Some(from.as_str()),
            FileOp::Delete { path } => Some(path.as_str()),
            _ => None,
        };
        if let Some(t) = target
            && self.panes.iter().any(|(_, ps)| {
                ps.tabs
                    .iter()
                    .any(|tab| tab.path == t && tab.source.is_dirty())
            })
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
                    Some(path) => return self.dispatch_op(FileOp::Delete { path }),
                    None => self.status.error = Some("select a file first".into()),
                }
            }
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
                    self.explorer.apply_op(&op, &final_path, &self.ui.expanded);
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

    /// A renamed file's open tab follows it (re-read at the new path).
    fn retarget(&mut self, from: &str, to: &str) {
        let Some((pane, i)) = panes::find(&self.panes, from) else {
            return;
        };
        let Some(d) = &self.daemon else { return };
        match d.read_file(to) {
            Ok((bytes, hash)) => {
                if let Some(ps) = self.panes.get_mut(pane)
                    && let Some(slot) = ps.tabs.get_mut(i)
                {
                    *slot = Tab {
                        path: to.to_owned(),
                        source: crate::source::SourceBuffer::new(to, &bytes, hash),
                    };
                }
                self.on_tab_closed(from);
                self.ui.touch_recent(to);
                self.persist();
            }
            Err(e) => self.status.error = Some(open_error(&e)),
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
                s.query.clear();
                s.selected = 0;
                s.refresh(&self.explorer.paths, &self.ui.recent);
                return iced::widget::operation::focus(switcher::INPUT_ID);
            }
            SwitcherMsg::Close => s.open = false,
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
                match s.current().map(str::to_owned) {
                    Some(p) if !s.shift => {
                        s.open = false;
                        self.open(&p, true);
                    }
                    _ => return self.update_switcher(SwitcherMsg::Create),
                }
            }
            SwitcherMsg::Pick(p) => {
                s.open = false;
                self.open(&p, true);
            }
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

    fn update_nav(&mut self, m: NavMsg) -> Task<Message> {
        match m {
            NavMsg::Go { pane, target } => match target {
                NavTarget::Path(p) => {
                    if let Err(e) = self.open_into(pane, p, true) {
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
                    match self.open_into(pane, &p, true) {
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
            PaletteMsg::Close => self.palette.close(),
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
            PaletteMsg::Ran { title, result } => match result {
                Ok(r) => {
                    self.palette.job_titles.insert(r.job_id.clone(), title);
                    if let Some(p) = r.open_path {
                        self.open(&p, true);
                    }
                }
                Err(e) => self.status.error = Some(format!("{title}: {e}")),
            },
            PaletteMsg::Tick => self.palette.expire_toast(std::time::Instant::now()),
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

    fn on_chord(&mut self, c: &crate::palette::chord::Chord) -> Task<Message> {
        if crate::palette::chord::reserved(c) {
            return Task::none();
        }
        if self.palette.actions.is_none() {
            self.palette.pending_chord = Some(c.clone());
            return self.fetch_actions();
        }
        match self.palette.bindings.get(c).cloned() {
            Some(id) => self.invoke_action(&id),
            None => Task::none(),
        }
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
        let Some(i) = ps.active.filter(|&i| i < ps.tabs.len()) else {
            if call.needs.iter().any(|n| n == "active_path") {
                self.status.error = Some(format!("{}: open a note first", call.title));
                return Task::none();
            }
            return self.dispatch_call(call);
        };
        let tab = &mut ps.tabs[i];
        if shadow || !tab.source.is_dirty() {
            return self.dispatch_call(call);
        }
        let path = tab.path.clone();
        let Some(job) = tab.source.begin_save(false) else {
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
        let Some((pane, i)) = panes::find(&self.panes, path) else {
            self.status.error = Some(format!("{}: not run (note closed)", call.title));
            return Task::none();
        };
        let Some(tab) = self.panes.get_mut(pane).and_then(|ps| ps.tabs.get_mut(i)) else {
            return Task::none();
        };
        let ok = matches!(outcome, SaveOutcome::Saved(_));
        let again = match tab.source.finish_save(seq, outcome.clone()) {
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
        let ctx = crate::palette::context(Some(tab));
        let follow = if again {
            tab.source.begin_save(false)
        } else {
            None
        };
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
        let ctx = crate::palette::context(self.active_tab());
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
        self.rpc(
            move |d| d.run_action(params).map_err(|e| e.to_string()),
            move |result| Message::Palette(PaletteMsg::Ran { title, result }),
        )
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
                if let Some(ps) = self.panes.get_mut(self.focus)
                    && let Some(i) = ps.active.filter(|&i| i < ps.tabs.len())
                {
                    ps.tabs[i]
                        .source
                        .move_to(iced::widget::text_editor::Cursor {
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
                return Task::perform(
                    async { tokio::time::sleep(crate::panels::SEARCH_DEBOUNCE).await },
                    move |()| Message::Panels(PanelsMsg::SearchDue(seq)),
                );
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
                return self.rpc(
                    move |d| crate::panels::run_search(d, &q),
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
        let generation = self.status.generation;
        let d = self.daemon.clone();
        let tab = self.panes.get(self.focus).and_then(PaneState::active_tab);
        self.panels.sync_buffer(tab, d.as_deref(), generation);
        if let Some(d) = &d
            && let Err(e) = self.panels.sync_tags(d.as_ref(), generation)
        {
            self.status.error = Some(format!("tags: {e}"));
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
                if let (Some(line), Some(ps)) = (line, self.panes.get_mut(self.focus))
                    && let Some(tab) = ps.active.and_then(|i| ps.tabs.get_mut(i))
                    && tab.path == path
                {
                    tab.source.move_to(iced::widget::text_editor::Cursor {
                        position: iced::widget::text_editor::Position { line, column: 0 },
                        selection: None,
                    });
                }
                Task::none()
            }
        }
    }

    fn update_status(&mut self, m: StatusMsg) {
        match m {
            StatusMsg::Disconnected => self.status.connected = false,
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
                        if self.explorer.stale {
                            self.refresh_files();
                        }
                    }
                    Notification::JobUpdated(j) => {
                        let state = serde_json::to_value(j.status)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .unwrap_or_default();
                        self.status.last_job = Some(format!("{}: {state}", j.job_id));
                        self.palette.job_updated(&j, std::time::Instant::now());
                    }
                    Notification::FileChanged {
                        path, kind, hash, ..
                    } => {
                        self.reload_tab(&path, hash.as_deref());
                        self.panels.invalidate();
                        if path.starts_with(crate::palette::CONFIG_PREFIX) {
                            self.palette.invalidate();
                        }
                        self.explorer.file_changed(&path, kind, &self.ui.expanded);
                        if path == BOOKMARKS_PATH
                            && !self.bookmarks.writing
                            && hash.as_deref() != self.bookmarks.hash.as_ref().map(|h| h.as_str())
                        {
                            self.load_bookmarks();
                        }
                    }
                    Notification::OpenRequested { path } => self.open(&path, true),
                    Notification::ViewsInvalidated { view_ids } => self.views.invalidate(&view_ids),
                    Notification::ConfigReloaded { .. } => self.palette.invalidate(),
                    Notification::ShadowWrite { .. } => {}
                }
            }
        }
    }

    /// Daemon notifications and global keys as iced subscriptions.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(self.notifications.clone(), notification_stream),
            iced::keyboard::listen().filter_map(switcher::key_event),
            self.reading.subscription(),
            crate::palette::subscription(&self.palette, self.panels.visible),
        ])
    }

    /// The iced theme for the current surround.
    pub fn theme(&self) -> Theme {
        let name = match self.ui.surround {
            Surround::Dark => "N1 Dark",
            Surround::Med => "N1 Med",
            Surround::Light => "N1 Light",
        };
        self.tokens.iced_theme(name)
    }

    fn pane_view<'a>(&'a self, pane: Pane, ps: &'a PaneState) -> pane_grid::Content<'a, Message> {
        let t = self.tokens;
        let focused = pane == self.focus;
        let mut strip = row(ps.tabs.iter().enumerate().map(|(i, tab)| {
            let label = if ps.active == Some(i) {
                format!("● {}", tab.path)
            } else {
                tab.path.clone()
            };
            let mut lbl = text(label).size(14);
            if focused && ps.active == Some(i) {
                lbl = lbl.color(t.accent_text.to_color());
            }
            row![
                button(lbl)
                    .style(button::text)
                    .on_press(Message::Tabs(TabsMsg::Select(pane, i))),
                button(text("×").size(14))
                    .style(button::text)
                    .on_press(Message::Tabs(TabsMsg::Close(pane, i))),
            ]
            .spacing(2)
            .into()
        }))
        .spacing(8)
        .width(Length::Fill);
        if self.panes.len() > 1 {
            strip = strip.push(
                button(text("close pane").size(13))
                    .style(button::text)
                    .on_press(Message::Panes(PanesMsg::Close(pane))),
            );
        }
        let mut col = column![container(strip).padding(2).width(Length::Fill)].spacing(4);
        match ps.active_tab() {
            Some(tab) => {
                if let Some(bar) = nav::bar(pane, &tab.path, self.today(), t) {
                    col = col.push(bar);
                }
                col = col.push(self.tab_body(tab));
            }
            None => col = col.push(text("No file open").color(t.text_faint.to_color())),
        }
        let border = if focused { t.accent_ui } else { t.line };
        pane_grid::Content::new(container(col).padding(8).height(Length::Fill)).style(
            move |_: &Theme| container::Style {
                border: iced::Border {
                    color: border.to_color(),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..container::Style::default()
            },
        )
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
            button(text("Quick switch (Ctrl+O)").size(14))
                .style(button::text)
                .on_press(Message::Switcher(SwitcherMsg::Open)),
            button(text("Command palette (Ctrl+P)").size(14))
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
        ]
        .spacing(8);

        let side = column![
            container(explorer::view(&self.explorer, t, self.status.mode)).height(Length::Fill),
            bookmarks::view(&self.bookmarks, t, self.status.mode),
        ]
        .spacing(8)
        .padding(4);

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
            button(text(format!("Surround: {}", self.ui.surround.label())).size(16))
                .style(button::text)
                .on_press(Message::Theme(ThemeMsg::Cycle)),
        ]
        .spacing(16);

        let mut main = column![container(top).padding(4).width(Length::Fill).style(panel)];
        if self.switcher.open {
            main = main.push(
                container(switcher::view(&self.switcher, t, self.status.mode))
                    .padding(6)
                    .width(Length::Fill)
                    .style(inset),
            );
        }
        if self.palette.open {
            main = main.push(
                container(crate::palette::view(&self.palette, t))
                    .padding(6)
                    .width(Length::Fill)
                    .style(inset),
            );
        }
        let mut body = row![
            container(side)
                .style(inset)
                .width(Length::FillPortion(1))
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
        main.push(body.height(Length::Fill))
            .push(
                container(status)
                    .padding(4)
                    .width(Length::Fill)
                    .style(panel),
            )
            .into()
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
