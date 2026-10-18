//! Plan round-6-feedback-todos WP-7: the in-memory notice log behind the Log tab.
//!
//! Every status notice ([`crate::app::StatusModel::notify_at`]) also lands here, in a bounded
//! ring (oldest dropped first). Not persisted across restarts. The Log tab is a sentinel-path
//! tab ([`LOG_TAB`]), the same pattern as the Settings tab.

use std::collections::VecDeque;

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length};

use crate::app::Severity;
use crate::message::Message;

/// The Log tab's reserved path (not a valid vault path).
pub const LOG_TAB: &str = "/:log";

/// Ring capacity (plan: ≥ 200).
pub const RING_CAP: usize = 500;

/// One logged notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub at: chrono::DateTime<chrono::Local>,
    pub severity: Severity,
    pub text: String,
    /// Where it came from (job title/id, path), when known.
    pub source: Option<String>,
}

impl LogEntry {
    /// One copyable line: `HH:MM:SS SEVERITY [source] text`.
    pub fn line(&self) -> String {
        let src = self
            .source
            .as_deref()
            .map(|s| format!("[{s}] "))
            .unwrap_or_default();
        format!(
            "{} {} {src}{}",
            self.at.format("%H:%M:%S"),
            severity_label(self.severity),
            self.text
        )
    }
}

/// The severity filter: entries at or above `min` show (`None` = all).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Log {
    entries: VecDeque<LogEntry>,
    /// Warnings + errors appended since the Log tab was last opened.
    pub unread: usize,
    /// Minimum severity shown in the Log tab (`None` = all).
    pub filter: Option<Severity>,
}

impl Log {
    /// Appends an entry, dropping the oldest past [`RING_CAP`].
    pub fn push(&mut self, severity: Severity, text: String, source: Option<String>) {
        if self.entries.len() >= RING_CAP {
            self.entries.pop_front();
        }
        if severity >= Severity::Warning {
            self.unread = self.unread.saturating_add(1);
        }
        self.entries.push_back(LogEntry {
            at: chrono::Local::now(),
            severity,
            text,
            source,
        });
    }

    /// Number of entries held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries passing the filter, newest first.
    pub fn visible(&self) -> impl Iterator<Item = &LogEntry> {
        let min = self.filter;
        self.entries
            .iter()
            .rev()
            .filter(move |e| min.is_none_or(|m| e.severity >= m))
    }

    /// Drops every entry (and the unread count).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.unread = 0;
    }

    /// The visible entries as text, newest first (the Copy action).
    pub fn copy_text(&self) -> String {
        self.visible()
            .map(LogEntry::line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Log tab messages.
#[derive(Debug, Clone)]
pub enum LogMsg {
    /// Open or focus the Log tab (status-bar click, palette "Open log"); resets unread.
    Open,
    Filter(Option<Severity>),
    Clear,
    /// Copy the visible entries to the clipboard.
    Copy,
}

fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Info => "INFO",
        Severity::Warning => "WARN",
        Severity::Error => "ERROR",
    }
}

/// The Log tab body.
pub fn view(log: &Log, t: crate::theme::Tokens) -> Element<'_, Message> {
    let chip = |label: &'static str, f: Option<Severity>| {
        let b = button(text(label).size(14)).padding([2, 8]);
        let b = if log.filter == f {
            b.style(button::primary)
        } else {
            b.style(button::text)
        };
        b.on_press(Message::Log(LogMsg::Filter(f)))
    };
    let bar = row![
        chip("All", None),
        chip("Warnings+", Some(Severity::Warning)),
        chip("Errors", Some(Severity::Error)),
        Space::new().width(Length::Fill),
        button(text("Copy").size(14))
            .padding([2, 8])
            .style(button::text)
            .on_press(Message::Log(LogMsg::Copy)),
        button(text("Clear").size(14))
            .padding([2, 8])
            .style(button::text)
            .on_press(Message::Log(LogMsg::Clear)),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    let rows = column(log.visible().map(|e| {
        let color = match e.severity {
            Severity::Info => t.text_dim,
            Severity::Warning => t.warn_text,
            Severity::Error => t.error_text,
        };
        let src = e.source.clone().unwrap_or_default();
        row![
            text(e.at.format("%H:%M:%S").to_string())
                .size(13)
                .color(t.text_dim.to_color()),
            text(severity_label(e.severity))
                .size(13)
                .width(48)
                .color(color.to_color()),
            text(src).size(13).color(t.text_dim.to_color()),
            text(e.text.clone()).size(14).width(Length::Fill),
        ]
        .spacing(10)
        .into()
    }))
    .spacing(4);
    let body: Element<'_, Message> = if log.is_empty() {
        text("No notices yet.").size(14).into()
    } else {
        scrollable(rows).height(Length::Fill).into()
    };
    container(column![bar, body].spacing(8))
        .padding(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use cv_protocol::{JobState, JobStatus, Notification};

    use super::*;
    use crate::app::{App, TabKind};
    use crate::message::StatusMsg;

    fn log_tabs(app: &App) -> usize {
        app.panes
            .iter()
            .flat_map(|(_, ps)| ps.tabs.iter())
            .filter(|t| t.kind() == TabKind::Log)
            .count()
    }

    #[test]
    fn ring_is_bounded() {
        let mut log = Log::default();
        for i in 0..RING_CAP + 50 {
            log.push(Severity::Info, format!("n{i}"), None);
        }
        assert_eq!(log.len(), RING_CAP);
        let newest = log.visible().next().map(|e| e.text.clone());
        assert_eq!(
            newest.as_deref(),
            Some(format!("n{}", RING_CAP + 49).as_str())
        );
        assert_eq!(log.visible().last().map(|e| e.text.as_str()), Some("n50"));
    }

    #[test]
    fn job_failure_lands_in_the_ring_with_its_reason() {
        let mut app = App::new(None, None);
        app.palette
            .job_titles
            .insert("j1".into(), "File as kind".into());
        let _ = app.update(Message::Status(StatusMsg::Notification(
            Notification::JobUpdated(JobStatus {
                job_id: "j1".into(),
                status: JobState::Failed,
                message: Some("already exists: stack/clip.md".into()),
                output_paths: Vec::new(),
                open_path: None,
            }),
        )));
        let e = app.status.log.visible().next().expect("entry");
        assert_eq!(e.severity, Severity::Error);
        assert_eq!(e.text, "File as kind failed: already exists: stack/clip.md");
        assert_eq!(e.source.as_deref(), Some("job j1"));
        // The display slot is unchanged.
        assert_eq!(app.status.error_text(), Some(e.text.as_str()));
    }

    #[test]
    fn status_bar_click_opens_or_focuses_the_log_tab() {
        let mut app = App::new(None, None);
        app.status.notify(Severity::Warning, "w");
        assert!(app.status.log.unread >= 1);
        // The status bar's button emits `LogMsg::Open`.
        let _ = app.update(Message::Log(LogMsg::Open));
        assert_eq!(log_tabs(&app), 1);
        assert!(app.active_tab().is_some_and(|t| t.is_log()));
        assert!(app.active_note().is_none(), "never a note");
        assert_eq!(app.status.log.unread, 0, "opening resets unread");
        // A second click (or the palette command) focuses the same tab.
        let m = crate::palette::ClientCmd::OpenLog.message(app.focus);
        let _ = app.update(m);
        assert_eq!(log_tabs(&app), 1);
        assert!(app.active_tab().is_some_and(|t| t.is_log()));
    }

    #[test]
    fn severity_filter_works() {
        let mut log = Log::default();
        log.push(Severity::Info, "i".into(), None);
        log.push(Severity::Warning, "w".into(), None);
        log.push(Severity::Error, "e".into(), Some("p.md".into()));
        let texts = |l: &Log| l.visible().map(|e| e.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&log), ["e", "w", "i"]);
        log.filter = Some(Severity::Warning);
        assert_eq!(texts(&log), ["e", "w"]);
        log.filter = Some(Severity::Error);
        assert_eq!(texts(&log), ["e"]);
        assert!(log.copy_text().contains("ERROR [p.md] e"));
        log.clear();
        assert!(log.is_empty());
    }
}
