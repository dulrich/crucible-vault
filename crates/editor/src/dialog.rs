//! Modal dialogs (plan editor-data-safety WP-2).
//!
//! Two layers, kept apart so later dialogs reuse the shell (plan editor-ui-round-3 WP-3's
//! delete-confirm): the **modal frame** ([`Dialog`], [`DialogMsg`], [`frame`], [`key_event`] —
//! overlay card, title, OK / Cancel, Escape = Cancel, Enter = the default button; while a
//! dialog is open `App::update` gives it input priority, see `App::modal_filter`) and the
//! **content** ([`Content`] — here the unsaved-edits loss list).

use iced::keyboard::{self, Key};
use iced::widget::{Space, button, column, row, text};
use iced::{Element, Length};

use crate::message::Message;
use crate::theme::Tokens;

/// Frame messages (the content may add its own later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogMsg {
    Ok,
    Cancel,
    /// Enter: the dialog's default button.
    Default,
}

/// Escape / Enter for the open dialog (the global key listener; ignored with no dialog).
pub fn key_event(ev: &keyboard::Event) -> Option<Message> {
    match ev {
        keyboard::Event::KeyPressed { key, .. } => match key.as_ref() {
            Key::Named(keyboard::key::Named::Escape) => Some(Message::Dialog(DialogMsg::Cancel)),
            Key::Named(keyboard::key::Named::Enter) => Some(Message::Dialog(DialogMsg::Default)),
            _ => None,
        },
        _ => None,
    }
}

/// What OK does (the caller's continuation; Cancel always just closes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnOk {
    /// Exit without writing.
    Quit,
    /// Close these tabs (by [`crate::app::Tab::id`]) without writing.
    CloseTabs(Vec<u64>),
}

/// An open modal dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub ok_label: String,
    /// Enter presses OK (`true`) or Cancel (`false`; the safe default for destructive dialogs).
    pub enter_is_ok: bool,
    pub content: Content,
    pub on_ok: OnOk,
}

/// A dialog's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    UnsavedEdits(UnsavedEdits),
}

/// Why a note's edits cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LossReason {
    Conflict,
    Missing,
    Disconnected,
    SaveFailed(String),
}

impl LossReason {
    pub fn label(&self) -> String {
        match self {
            Self::Conflict => "conflict".into(),
            Self::Missing => "file missing".into(),
            Self::Disconnected => "daemon disconnected".into(),
            Self::SaveFailed(e) => format!("save failed: {e}"),
        }
    }
}

/// The unsaved-edits loss list: notes (vault path, reason) and settings files never written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnsavedEdits {
    pub notes: Vec<(String, LossReason)>,
    pub settings: Vec<String>,
}

impl UnsavedEdits {
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.settings.is_empty()
    }

    /// Records a note loss (the first reason per path wins).
    pub fn note(&mut self, path: &str, reason: LossReason) {
        if !self.notes.iter().any(|(p, _)| p == path) {
            self.notes.push((path.to_owned(), reason));
        }
    }

    pub fn setting(&mut self, path: &str) {
        if !self.settings.iter().any(|p| p == path) {
            self.settings.push(path.to_owned());
        }
    }
}

impl Dialog {
    /// The "unsaved edits will be lost" dialog.
    pub fn unsaved(losses: UnsavedEdits, on_ok: OnOk) -> Self {
        let ok_label = match on_ok {
            OnOk::Quit => "Quit without saving",
            OnOk::CloseTabs(_) => "Close without saving",
        };
        Self {
            title: "Unsaved edits will be lost".into(),
            ok_label: ok_label.into(),
            enter_is_ok: false,
            content: Content::UnsavedEdits(losses),
            on_ok,
        }
    }

    pub fn unsaved_edits(&self) -> Option<&UnsavedEdits> {
        match &self.content {
            Content::UnsavedEdits(u) => Some(u),
        }
    }
}

/// The modal frame: title, `body`, then Cancel / OK.
pub fn frame<'a>(d: &'a Dialog, body: Element<'a, Message>, t: Tokens) -> Element<'a, Message> {
    column![
        text(d.title.as_str())
            .size(16)
            .color(t.warn_text.to_color()),
        body,
        row![
            Space::new().width(Length::Fill),
            button(text("Cancel")).on_press(Message::Dialog(DialogMsg::Cancel)),
            button(text(d.ok_label.as_str())).on_press(Message::Dialog(DialogMsg::Ok)),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .width(Length::Fill)
    .into()
}

/// The dialog card (frame + content).
pub fn view(d: &Dialog, t: Tokens) -> Element<'_, Message> {
    let body: Element<'_, Message> = match &d.content {
        Content::UnsavedEdits(u) => unsaved_view(u),
    };
    frame(d, body, t)
}

fn unsaved_view(u: &UnsavedEdits) -> Element<'_, Message> {
    let mut col = column![].spacing(4);
    for (p, why) in &u.notes {
        col = col.push(text(format!("{p} — {}", why.label())).size(14));
    }
    for p in &u.settings {
        col = col.push(text(format!("{p} — settings change not saved")).size(14));
    }
    col.into()
}
