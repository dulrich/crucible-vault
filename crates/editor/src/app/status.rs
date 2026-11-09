//! The status-bar model, notices and the status / log message handlers.

use super::*;

/// The status-bar model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    pub connected: bool,
    pub generation: u64,
    /// Last job update (`id: state`), for the toast slot WP-5 grows.
    pub last_job: Option<String>,
    /// plan round-5 WP-3: the current status notice (replaces the sticky error string).
    pub notice: Option<Notice>,
    /// plan editor-data-safety WP-1: disconnected with a reconnect loop running.
    pub reconnecting: bool,
    /// Plan round-6-feedback-todos WP-7: every notice, in a bounded ring (the Log tab).
    pub log: crate::log::Log,
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
        let mut s = format!("{conn} · gen {}", self.generation);
        if let Some(j) = &self.last_job {
            s.push_str(" · job ");
            s.push_str(j);
        }
        s
    }

    /// Raises a notice of `severity`, expiring per [`Severity::ttl`].
    pub fn notify(&mut self, severity: Severity, text: impl Into<String>) {
        self.notify_at(severity, text, std::time::Instant::now());
    }

    /// [`Self::notify`] with an explicit clock (tests). A lower-severity notice does not
    /// replace a live higher-severity one.
    pub fn notify_at(
        &mut self,
        severity: Severity,
        text: impl Into<String>,
        now: std::time::Instant,
    ) {
        self.notify_from_at(severity, text, None, now);
    }

    /// [`Self::notify`] with a source (job title/id, path) recorded in the log.
    pub fn notify_from(
        &mut self,
        severity: Severity,
        text: impl Into<String>,
        source: impl Into<String>,
    ) {
        let now = std::time::Instant::now();
        self.notify_from_at(severity, text, Some(source.into()), now);
    }

    fn notify_from_at(
        &mut self,
        severity: Severity,
        text: impl Into<String>,
        source: Option<String>,
        now: std::time::Instant,
    ) {
        let text = text.into();
        self.log.push(severity, text.clone(), source);
        if let Some(n) = &self.notice
            && n.severity > severity
            && n.expires > now
        {
            return;
        }
        self.notice = Some(Notice {
            text,
            severity,
            expires: now + severity.ttl(),
        });
    }

    /// Logs without raising a notice (events with their own display: connection state,
    /// the Settings file notice).
    pub fn record(&mut self, severity: Severity, text: impl Into<String>, source: Option<String>) {
        self.log.push(severity, text.into(), source);
    }

    /// An Error notice.
    pub fn set_error(&mut self, text: String) {
        self.notify(Severity::Error, text);
    }

    /// The current Error notice's text, if any.
    pub fn error_text(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|n| n.severity == Severity::Error)
            .map(|n| n.text.as_str())
    }

    /// Drops an expired notice.
    pub fn expire_notice(&mut self, now: std::time::Instant) {
        if self.notice.as_ref().is_some_and(|n| n.expires <= now) {
            self.notice = None;
        }
    }
}

/// Notice severity, ordered Info < Warning < Error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    /// How long a notice of this severity stays: info 5 s, warning 10 s, error 30 s.
    pub fn ttl(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Severity::Info => 5,
            Severity::Warning => 10,
            Severity::Error => 30,
        })
    }
}

/// A status-bar notice (plan round-5 WP-3): text, severity and expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub severity: Severity,
    pub expires: std::time::Instant,
}

/// Max chars of a notice shown inline; the full text is in the hover tooltip.
pub const NOTICE_INLINE_CHARS: usize = 80;

impl Notice {
    /// The one-line, truncated bar text.
    pub fn inline(&self) -> String {
        let one: String = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if one.chars().count() > NOTICE_INLINE_CHARS {
            let cut: String = one.chars().take(NOTICE_INLINE_CHARS - 1).collect();
            format!("{cut}…")
        } else {
            one
        }
    }
}

impl App {
    pub(super) fn update_status(&mut self, m: StatusMsg) -> Task<Message> {
        match m {
            StatusMsg::Disconnected => {
                if self.status.connected {
                    self.status
                        .record(Severity::Warning, "daemon connection lost", None);
                }
                self.status.connected = false;
                let released = self.clear_holds();
                return Task::batch([released, self.start_reconnect()]);
            }
            StatusMsg::ReconnectDue => return self.try_reconnect(),
            StatusMsg::OpenerFailed(msg) => self.status.set_error(msg),
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
                    Notification::IndexGeneration { n } => {
                        self.status.generation = n;
                        if let Some(d) = &self.daemon
                            && let Err(e) = d.index().on_generation(n)
                        {
                            self.status.set_error(format!("index: {e}"));
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
                        self.palette.jobs.job_updated(&j, std::time::Instant::now());
                        self.job_finished(&j);
                        return self.hold_job_updated(&j, std::time::Instant::now());
                    }
                    Notification::FileChanged {
                        path,
                        kind,
                        hash,
                        from,
                        replaced_by,
                        seq,
                        ..
                    } => {
                        if let Some(from) = from.as_deref() {
                            // Round 9 WP-3 (Decision 4): a daemon move re-keys the open tabs
                            // (the bridge sends this before the source `Deleted`). Plan
                            // note-path-continuity D3: only when the event is later than the
                            // document's lineage cursor.
                            self.follow_move_event(from, &path, seq);
                        }
                        let deleted = kind == cv_protocol::ChangeKind::Deleted;
                        let mut handled = false;
                        if deleted && let Some(by) = replaced_by.as_deref() {
                            // Round 11 WP-1 (Decision 1): an absorbed note's tabs land on the
                            // note that absorbed it (a dirty one keeps the conflict path below).
                            if self.event_stale(&path, seq) {
                                handled = true;
                            } else {
                                self.follow_replaced(&path, by);
                            }
                        } else if deleted {
                            // D3: a delete of an open path asks lineage where the note went
                            // before the tab may report "deleted".
                            handled = self.resolve_deleted(&path, seq);
                        }
                        self.reading.file_changed(&path);
                        if kind != cv_protocol::ChangeKind::Modified {
                            // Before `reload_tab`: a localize pass creates the attachments,
                            // then rewrites the note, whose re-lower must see them.
                            self.resolver.invalidate();
                        }
                        if !handled {
                            self.reload_tab(&path, hash.as_deref());
                        }
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
                        self.explorer.file_changed(
                            &path,
                            kind,
                            from.as_deref(),
                            &self.ui.expanded,
                            &indexed,
                        );
                        if crate::settings::is_view_path(&path)
                            && panes::find_kind(&self.panes, &TabKind::Settings).is_some()
                        {
                            self.sync_view_files();
                        }
                        self.settings_file_changed(&path, hash.as_deref());
                        self.refresh_bookmark_missing();
                    }
                    Notification::OpenRequested { path } => self.open(&path),
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
                }
            }
        }
        Task::none()
    }

    pub(super) fn update_log(&mut self, m: crate::log::LogMsg) -> Task<Message> {
        use crate::log::LogMsg;
        match m {
            LogMsg::Open => {
                self.open_singleton(TabKind::Log, self.focus);
            }
            LogMsg::Filter(f) => self.status.log.filter = f,
            LogMsg::Clear => self.status.log.clear(),
            LogMsg::Copy => return iced::clipboard::write(self.status.log.copy_text()),
        }
        Task::none()
    }
}
