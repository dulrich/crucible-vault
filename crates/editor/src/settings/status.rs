//! The Status page: the daemon's existing `status` RPC (mode, generation, queue counts, config
//! errors, service health), read-only. Refreshed when the page opens and on every
//! `ConfigReloaded`; the RPC runs off the UI thread like every daemon call. Config errors the
//! settings router could not attribute to a file page are listed here too.

use cv_protocol::{Mode, StatusResult};
use iced::widget::{button, column, row, text};
use iced::{Element, Length};

use super::SettingsMsg;
use crate::message::Message;
use crate::theme::Tokens;

/// The Status page model.
#[derive(Debug, Default)]
pub struct StatusPage {
    /// The last `status` result (`None` = not fetched yet).
    pub result: Option<StatusResult>,
    /// The last fetch error.
    pub error: Option<String>,
    /// A fetch is in flight.
    pub loading: bool,
    /// `ConfigReloaded` errors naming no known file.
    pub unrouted: Vec<String>,
}

impl StatusPage {
    /// Lands a `status` result.
    pub fn loaded(&mut self, r: Result<StatusResult, String>) {
        self.loading = false;
        match r {
            Ok(s) => {
                self.result = Some(s);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

fn kv<'a>(k: &'a str, v: String, t: Tokens) -> Element<'a, Message> {
    row![
        text(k)
            .size(14)
            .color(t.text_dim.to_color())
            .width(Length::Fixed(200.0)),
        text(v).size(14),
    ]
    .spacing(6)
    .into()
}

/// The page.
pub fn view(s: &StatusPage, t: Tokens) -> Element<'_, Message> {
    let refresh = button(crate::theme::control_label(
        if s.loading {
            "Refreshing…"
        } else {
            "Refresh"
        },
        14.0,
    ))
    .height(crate::theme::CONTROL)
    .padding([0, 14])
    .style(crate::theme::icon_button_style(t, false))
    .on_press_maybe((!s.loading).then_some(Message::Settings(SettingsMsg::RefreshStatus)));
    let mut col = column![refresh].spacing(8);
    if let Some(e) = &s.error {
        col = col.push(
            text(format!("status failed: {e}"))
                .size(13)
                .color(t.error_text.to_color()),
        );
    }
    match &s.result {
        Some(r) => {
            let mode = match r.mode {
                Mode::Shadow => "shadow",
                Mode::Live => "live",
            };
            col = col.push(kv("Mode", mode.to_owned(), t));
            col = col.push(kv("Index generation", r.generation.to_string(), t));
            let queue = if r.queue.is_empty() {
                "empty".to_owned()
            } else {
                r.queue
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            col = col.push(kv("Queue", queue, t));
            let services = if r.services.is_empty() {
                "none reported".to_owned()
            } else {
                r.services
                    .iter()
                    .map(|h| format!("{}: {}", h.service, h.state))
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            col = col.push(kv("Services", services, t));
            col = col.push(kv("Config errors", r.config_errors.len().to_string(), t));
            for e in &r.config_errors {
                col = col.push(text(e.as_str()).size(13).color(t.warn_text.to_color()));
            }
        }
        None if s.loading => {
            col = col.push(text("loading…").size(13).color(t.text_faint.to_color()));
        }
        None => {}
    }
    if !s.unrouted.is_empty() {
        col = col.push(
            text("Reload errors not tied to a settings page")
                .size(14)
                .color(t.heading.to_color()),
        );
        for e in &s.unrouted {
            col = col.push(text(e.as_str()).size(13).color(t.warn_text.to_color()));
        }
    }
    col.into()
}
