//! The iced application state, `update` and `view` (plan WP-1 skeleton).
//!
//! WP-1 renders: a tab strip, a flat file list (placeholder until WP-4's explorer), the active tab
//! as plain read-only text (WP-2 owns editing), and a connection/mode status bar.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use cv_protocol::{Mode, Notification};
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Length, Subscription, Task, Theme};

use crate::daemon::{DaemonClient, DaemonError};
use crate::message::{ExplorerMsg, Message, StatusMsg, TabsMsg, ThemeMsg};
use crate::theme::{Surround, Tokens};
use crate::ui_state::{self, UiState};

/// Cap on the WP-1 flat file list.
const FILE_LIST_CAP: usize = 2000;

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
    pub tabs: Vec<Tab>,
    pub active: Option<usize>,
    pub ui: UiState,
    pub files: Vec<String>,
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
        let mut app = Self {
            daemon,
            status,
            tabs: Vec::new(),
            active: None,
            tokens: tokens_for(ui.surround),
            ui,
            files: Vec::new(),
            notifications,
            state_path,
        };
        app.refresh_files();
        let restore: Vec<String> = app.ui.tabs.clone();
        let want_active = app.ui.active_tab;
        for p in restore {
            app.open(&p, false);
        }
        if let Some(i) = want_active.filter(|&i| i < app.tabs.len()) {
            app.active = Some(i);
        }
        app
    }

    /// A disconnected app (used if boot cannot take the prepared one).
    pub fn disconnected() -> Self {
        Self::new(None, None)
    }

    /// The active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    fn refresh_files(&mut self) {
        let Some(d) = &self.daemon else { return };
        let res = d.index().with_query(|q| {
            let mut out = Vec::new();
            let mut stack = vec![String::new()];
            while let Some(folder) = stack.pop() {
                for f in q.folder_files(&folder)? {
                    if f.is_md && out.len() < FILE_LIST_CAP {
                        out.push(f.path);
                    }
                }
                stack.extend(q.subfolders(&folder)?);
            }
            out.sort();
            Ok(out)
        });
        match res {
            Ok(files) => self.files = files,
            Err(e) => self.status.error = Some(format!("index: {e}")),
        }
    }

    /// Opens `path` into a tab (focusing an existing one). The read is a local disk read of one
    /// note, done inline; RPC calls never run here.
    fn open(&mut self, path: &str, persist: bool) {
        if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
            self.active = Some(i);
        } else {
            let Some(d) = &self.daemon else {
                self.status.error = Some("not connected".into());
                return;
            };
            match d.read_file(path) {
                Ok((bytes, hash)) => {
                    self.tabs.push(Tab {
                        path: path.to_owned(),
                        source: crate::source::SourceBuffer::new(path, &bytes, hash),
                    });
                    self.active = Some(self.tabs.len() - 1);
                    self.ui.touch_recent(path);
                }
                Err(e) => {
                    self.status.error = Some(open_error(&e));
                    return;
                }
            }
        }
        if persist {
            self.persist();
        }
    }

    /// `file_changed`: echo / silent reload / conflict bar (WP-2 `source::file_changed`).
    fn reload_tab(&mut self, path: &str, hash: Option<&str>) {
        if let Some(e) =
            crate::source::file_changed(&mut self.tabs, path, hash, self.daemon.as_ref())
        {
            self.status.error = Some(e);
        }
    }

    fn persist(&mut self) {
        self.ui.tabs = self.tabs.iter().map(|t| t.path.clone()).collect();
        self.ui.active_tab = self.active;
        let Some(path) = &self.state_path else { return };
        let vault = self.daemon.as_ref().map(|d| d.vault_root().to_path_buf());
        if let Err(e) = ui_state::save(path, &self.ui, vault.as_deref()) {
            tracing::warn!(error = %e, "saving editor.json failed");
        }
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
            Message::Explorer(ExplorerMsg::Refresh) => self.refresh_files(),
            Message::Source(m) => {
                return crate::source::update(
                    &mut self.tabs,
                    self.active,
                    m,
                    crate::source::Ctx {
                        daemon: self.daemon.as_ref(),
                        mode: self.status.mode,
                        error: &mut self.status.error,
                    },
                );
            }
            Message::Reading(m) => match m {},
            Message::Switcher(m) => match m {},
            Message::Palette(m) => match m {},
            Message::Panels(m) => match m {},
            Message::Views(m) => match m {},
            Message::Nav(m) => match m {},
        }
        Task::none()
    }

    fn update_tabs(&mut self, m: TabsMsg) {
        match m {
            TabsMsg::Open(p) => self.open(&p, true),
            TabsMsg::Select(i) if i < self.tabs.len() => {
                self.active = Some(i);
                self.persist();
            }
            TabsMsg::Select(_) => {}
            TabsMsg::Close(i) if i < self.tabs.len() => {
                self.tabs.remove(i);
                self.active = match self.active {
                    _ if self.tabs.is_empty() => None,
                    Some(a) if a > i || a >= self.tabs.len() => Some(a.saturating_sub(1)),
                    other => other,
                };
                self.persist();
            }
            TabsMsg::Close(_) => {}
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
                        self.refresh_files();
                    }
                    Notification::JobUpdated(j) => {
                        let state = serde_json::to_value(j.status)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .unwrap_or_default();
                        self.status.last_job = Some(format!("{}: {state}", j.job_id));
                    }
                    Notification::FileChanged { path, hash, .. } => {
                        self.reload_tab(&path, hash.as_deref());
                    }
                    Notification::OpenRequested { path } => self.open(&path, true),
                    Notification::ConfigReloaded { .. }
                    | Notification::ShadowWrite { .. }
                    | Notification::ViewsInvalidated { .. } => {}
                }
            }
        }
    }

    /// Daemon notifications as an iced subscription.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with(self.notifications.clone(), notification_stream)
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

        let tabs = row(self.tabs.iter().enumerate().map(|(i, tab)| {
            let label = if self.active == Some(i) {
                format!("● {}", tab.path)
            } else {
                tab.path.clone()
            };
            row![
                button(text(label)).on_press(Message::Tabs(TabsMsg::Select(i))),
                button(text("×")).on_press(Message::Tabs(TabsMsg::Close(i))),
            ]
            .spacing(2)
            .into()
        }))
        .spacing(8);

        let files = scrollable(column(self.files.iter().map(|p| {
            button(text(p.as_str()).size(16))
                .style(button::text)
                .on_press(Message::Tabs(TabsMsg::Open(p.clone())))
                .into()
        })))
        .width(Length::FillPortion(1));

        let body: Element<'_, Message> = match self.active_tab() {
            Some(tab) => crate::source::view(&tab.source, t, self.status.mode),
            None => text("No file open").color(t.text_faint.to_color()).into(),
        };

        let status = row![
            text(self.status.line()).size(16),
            button(text(format!("Surround: {}", self.ui.surround.label())).size(16))
                .style(button::text)
                .on_press(Message::Theme(ThemeMsg::Cycle)),
        ]
        .spacing(16);

        column![
            container(tabs).padding(4).width(Length::Fill).style(panel),
            row![
                container(files).style(inset).height(Length::Fill),
                container(body)
                    .padding(12)
                    .width(Length::FillPortion(4))
                    .height(Length::Fill),
            ]
            .height(Length::Fill),
            container(status)
                .padding(4)
                .width(Length::Fill)
                .style(panel),
        ]
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
