//! Modal dialogs (plan editor-data-safety WP-2).
//!
//! Two layers, kept apart so later dialogs reuse the shell (plan editor-ui-round-3 WP-3's
//! delete-confirm): the **modal frame** ([`Dialog`], [`DialogMsg`], [`frame`], [`key_event`] —
//! overlay card, title, OK / Cancel, Escape = Cancel, Enter = the default button; while a
//! dialog is open `App::update` gives it input priority, see `App::modal_filter`) and the
//! **content** ([`Content`] — here the unsaved-edits loss list).

use iced::keyboard::{self, Key};
use iced::widget::{Space, button, checkbox, column, row, text};
use iced::{Element, Length};

use crate::daemon::{DeleteMode, DeletePreviewItem};
use crate::message::Message;
use crate::theme::Tokens;

/// Frame messages (the content may add its own later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogMsg {
    Ok,
    Cancel,
    /// Enter: the dialog's default button.
    Default,
    /// Delete dialog: toggle item `n`'s tick (referenced-elsewhere items ignore it).
    Toggle(usize),
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
    /// Delete the note in [`Content::Delete`] with its ticked items (plan editor-ui-round-3 WP-3).
    DeleteNote,
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
    Delete(DeleteConfirm),
}

/// The delete-note confirm (plan editor-ui-round-3 WP-3, Decisions "orphans only, ticked"):
/// orphans start ticked and toggle; items referenced elsewhere are listed but never tickable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteConfirm {
    pub path: String,
    pub mode: DeleteMode,
    /// `(item, ticked)`.
    pub items: Vec<(DeletePreviewItem, bool)>,
}

impl DeleteConfirm {
    pub fn new(path: String, mode: DeleteMode, items: Vec<DeletePreviewItem>) -> Self {
        let items = items
            .into_iter()
            .map(|i| {
                let tick = !i.referenced_elsewhere;
                (i, tick)
            })
            .collect();
        Self { path, mode, items }
    }

    /// Flips item `n`'s tick; referenced-elsewhere items stay unticked.
    pub fn toggle(&mut self, n: usize) {
        if let Some((item, t)) = self.items.get_mut(n)
            && !item.referenced_elsewhere
        {
            *t = !*t;
        }
    }

    /// The ticked attachment paths.
    pub fn ticked(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|(i, t)| *t && !i.referenced_elsewhere)
            .map(|(i, _)| i.path.clone())
            .collect()
    }
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
            OnOk::CloseTabs(_) | OnOk::DeleteNote => "Close without saving",
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
            Content::Delete(_) => None,
        }
    }

    /// The delete-note confirm (Enter = Cancel: destructive).
    pub fn delete(confirm: DeleteConfirm) -> Self {
        let ok_label = match confirm.mode {
            DeleteMode::Trash => "Move to .trash",
            DeleteMode::Permanent => "Delete permanently",
        };
        Self {
            title: format!("Delete {}?", confirm.path),
            ok_label: ok_label.into(),
            enter_is_ok: false,
            content: Content::Delete(confirm),
            on_ok: OnOk::DeleteNote,
        }
    }

    pub fn delete_confirm(&self) -> Option<&DeleteConfirm> {
        match &self.content {
            Content::Delete(c) => Some(c),
            Content::UnsavedEdits(_) => None,
        }
    }

    pub fn delete_confirm_mut(&mut self) -> Option<&mut DeleteConfirm> {
        match &mut self.content {
            Content::Delete(c) => Some(c),
            Content::UnsavedEdits(_) => None,
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
        Content::Delete(c) => delete_view(c, t),
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

fn delete_view(c: &DeleteConfirm, t: Tokens) -> Element<'_, Message> {
    let mut col = column![].spacing(4);
    if c.items.is_empty() {
        col = col.push(text("No embeds or attachments.").size(14));
    } else {
        col = col.push(text("Also delete (only files nothing else links to):").size(14));
    }
    for (n, (item, ticked)) in c.items.iter().enumerate() {
        if item.referenced_elsewhere {
            col = col.push(
                text(format!("{} — linked elsewhere, kept", item.path))
                    .size(14)
                    .color(t.text_faint.to_color()),
            );
        } else {
            col = col.push(
                checkbox(*ticked)
                    .label(item.path.as_str())
                    .on_toggle(move |_| Message::Dialog(DialogMsg::Toggle(n)))
                    .size(14)
                    .text_size(14),
            );
        }
    }
    col.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(p: &str, shared: bool) -> DeletePreviewItem {
        DeletePreviewItem {
            path: p.into(),
            referenced_elsewhere: shared,
        }
    }

    #[test]
    fn tick_rules_orphans_ticked_referenced_untickable() {
        let mut c = DeleteConfirm::new(
            "n.md".into(),
            DeleteMode::Trash,
            vec![item("own.png", false), item("shared.png", true)],
        );
        assert!(c.items[0].1);
        assert!(!c.items[1].1);
        assert_eq!(c.ticked(), ["own.png"]);
        c.toggle(1);
        assert!(!c.items[1].1, "referenced item cannot be ticked");
        c.toggle(0);
        assert!(c.ticked().is_empty());
        c.toggle(0);
        c.toggle(9);
        assert_eq!(c.ticked(), ["own.png"]);
    }
}
