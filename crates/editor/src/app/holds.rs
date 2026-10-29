//! Plan round-17 WP-2 (Decision 3): note holds. A running daemon job whose action declares
//! `resources.note = true` holds its note (`JobStatus.held_note`); while held, the note's tabs are
//! read-only, saves are refused with a notice, and after [`OVERLAY_DELAY`] a dimmed overlay with a
//! spinner and the action label covers the tab body. Holds clear on the job's terminal status and
//! on every (re)connect — the daemon's subscribe snapshot re-fills them.
//!
//! Crucible reference: `src/orchestration/NoteLockManager.ts:47-112` (per-note lock state and its
//! change event) and `src/noteLockOverlay.ts:29-72` (grayed overlay + spinner naming the holder),
//! under `https://github.com/dulrich/obsidian-crucible/blob/master/`. The Obsidian-induced part (plugin-side
//! lock bookkeeping around Obsidian's editor) is dropped: the daemon is the authority, and the
//! editor only mirrors its signal.

use std::time::{Duration, Instant};

use cv_protocol::{JobState, JobStatus};
use iced::widget::{Space, column, container, stack, text};
use iced::{Element, Length, Task};

use super::{App, Severity, buf};
use crate::message::{LinkKey, Message, SourceKey, SourceMsg, SourceTarget};

/// A hold shorter than this never shows the overlay (no flicker for fast on-edit actions).
pub const OVERLAY_DELAY: Duration = Duration::from_millis(400);
/// The spinner frame interval (the tick runs only while a hold exists).
pub const SPINNER_TICK: Duration = Duration::from_millis(100);
const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

/// One held note as the editor mirrors it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    /// Vault path (follows daemon moves).
    pub path: String,
    /// The holding action's label.
    pub label: String,
    /// When the editor first learned of the hold.
    pub since: Instant,
}

impl Hold {
    /// The overlay predicate: shown once the hold has lasted [`OVERLAY_DELAY`].
    pub fn overlay_shown(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= OVERLAY_DELAY
    }
}

/// What a held note does with a source message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gate {
    /// Not a mutation (navigation, selection, copy, find, scroll, reload).
    Pass,
    /// A buffer mutation or autosave: dropped silently.
    Drop,
    /// An explicit save: refused with a notice.
    Refuse,
}

/// Classifies a source-editor message for a held note.
pub(super) fn source_gate(m: &SourceMsg) -> Gate {
    match m {
        SourceMsg::Action(a) if a.is_edit() => Gate::Drop,
        SourceMsg::Key(
            SourceKey::Enter
            | SourceKey::DeleteWordBack
            | SourceKey::DeleteWordForward
            | SourceKey::Indent
            | SourceKey::Outdent,
        )
        | SourceMsg::Undo
        | SourceMsg::Redo
        | SourceMsg::Cut
        | SourceMsg::Paste
        | SourceMsg::PasteResolved { .. }
        | SourceMsg::PastePrimary { .. }
        | SourceMsg::AutosaveDue { .. }
        | SourceMsg::LinkKey(LinkKey::Enter | LinkKey::Tab)
        | SourceMsg::LinkPick(_) => Gate::Drop,
        SourceMsg::Save | SourceMsg::KeepMine(_) => Gate::Refuse,
        _ => Gate::Pass,
    }
}

impl App {
    /// The hold on `path`, if a running job holds it.
    pub(crate) fn held(&self, path: &str) -> Option<&Hold> {
        self.holds.values().find(|h| h.path == path)
    }

    /// Mirrors one `job_updated`: `running` with a held note inserts or retargets the hold (its
    /// `since` kept); any other status of a known job releases it. A released note with unsaved
    /// edits re-arms its autosave (whose save then meets the CAS conflict bar if the job wrote).
    pub(super) fn hold_job_updated(&mut self, j: &JobStatus, now: Instant) -> Task<Message> {
        if j.status == JobState::Running {
            if let Some(path) = &j.held_note {
                let label = j.action_label.clone().unwrap_or_else(|| j.job_id.clone());
                self.holds
                    .entry(j.job_id.clone())
                    .and_modify(|h| {
                        h.path.clone_from(path);
                        h.label.clone_from(&label);
                    })
                    .or_insert_with(|| Hold {
                        path: path.clone(),
                        label,
                        since: now,
                    });
            }
            return Task::none();
        }
        let Some(h) = self.holds.remove(&j.job_id) else {
            return Task::none();
        };
        if self.held(&h.path).is_some() {
            return Task::none();
        }
        match buf(&self.docs, &h.path).filter(|b| b.is_dirty()) {
            Some(b) => crate::source::autosave_timer(h.path, b.edit_seq()),
            None => Task::none(),
        }
    }

    /// Drops every hold (connection lost or re-established; the subscribe snapshot re-fills).
    pub(super) fn clear_holds(&mut self) {
        self.holds.clear();
    }

    /// A daemon move of `from` to `to` retargets its holds.
    pub(super) fn retarget_holds(&mut self, from: &str, to: &str) {
        for h in self.holds.values_mut().filter(|h| h.path == from) {
            to.clone_into(&mut h.path);
        }
    }

    /// `true` (with the one notice) when `path` is held and must not be saved now.
    pub(super) fn refuse_held_save(&mut self, path: &str) -> bool {
        let Some(label) = self.held(path).map(|h| h.label.clone()) else {
            return false;
        };
        self.status.notify(
            Severity::Warning,
            format!("{label} is updating this note — save held"),
        );
        true
    }

    /// The gate a source message meets: `Pass` unless its note is held; a refused save posts the
    /// notice.
    pub(super) fn held_source_gate(&mut self, m: &SourceMsg) -> Gate {
        let path = match m.target() {
            SourceTarget::Path(p) => Some(p.to_owned()),
            SourceTarget::Active => self.active_note().map(|t| t.path.to_owned()),
            // The `[[` list inserts into the active note.
            SourceTarget::App if matches!(m, SourceMsg::LinkKey(_) | SourceMsg::LinkPick(_)) => {
                self.active_note().map(|t| t.path.to_owned())
            }
            SourceTarget::App => None,
        };
        let Some(path) = path.filter(|p| self.held(p).is_some()) else {
            return Gate::Pass;
        };
        let gate = source_gate(m);
        if gate == Gate::Refuse {
            self.refuse_held_save(&path);
        }
        gate
    }

    /// Advances the spinner (the [`SPINNER_TICK`] subscription, live only while holds exist).
    pub(super) fn hold_tick(&mut self) {
        self.hold_frame = self.hold_frame.wrapping_add(1);
    }

    /// Wraps a note tab's body: always a `stack` (the widget tree shape never changes, so editor
    /// and scroll state survive), whose top layer is empty unless the overlay is shown.
    pub(super) fn hold_overlay<'a>(
        &'a self,
        path: &str,
        body: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let shown = self.held(path).filter(|h| h.overlay_shown(Instant::now()));
        let top: Element<'a, Message> = match shown {
            None => Space::new().into(),
            Some(h) => {
                let frame = FRAMES[self.hold_frame as usize % FRAMES.len()];
                let tokens = self.tokens;
                let card = column![text(frame).size(28), text(format!("{}…", h.label))]
                    .spacing(8)
                    .align_x(iced::Alignment::Center);
                // `opaque` swallows clicks and scrolls aimed at the held body.
                iced::widget::opaque(container(card).center(Length::Fill).style(move |_| {
                    container::Style {
                        background: Some(iced::Background::Color(iced::Color {
                            a: 0.6,
                            ..tokens.bg.to_color()
                        })),
                        text_color: Some(tokens.text.to_color()),
                        ..container::Style::default()
                    }
                }))
            }
        };
        stack![body, top].into()
    }
}
