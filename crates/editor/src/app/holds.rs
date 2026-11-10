//! Plan round-17 WP-2 (Decision 3): note holds. A running daemon job whose action declares
//! `resources.note = true` holds its note (`JobStatus.hold`); while held, the note's tabs are
//! read-only, saves are refused with a notice, and after [`OVERLAY_DELAY`] a dimmed overlay with a
//! spinner and the action label covers the tab body. Holds clear on the job's terminal status and
//! on every (re)connect — the daemon's subscribe snapshot re-fills them.
//!
//! Crucible reference: `src/orchestration/NoteLockManager.ts:47-112` (per-note lock state and its
//! change event) and `src/noteLockOverlay.ts:29-72` (grayed overlay + spinner naming the holder),
//! in the Crucible repository (`https://github.com/dulrich/obsidian-crucible/blob/master/`). The Obsidian-induced part (plugin-side
//! lock bookkeeping around Obsidian's editor) is dropped: the daemon is the authority, and the
//! editor only mirrors its signal.

use std::time::{Duration, Instant};

use cv_protocol::{JobState, JobStatus};
use iced::widget::{Space, column, container, stack, text};
use iced::{Element, Length, Task};

use super::{AfterSave, App, Severity, buf, panes};
use crate::explorer::FileOp;
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
    /// Plan round-18 WP-2 (Decision 4): Cut copies the selection and leaves the buffer alone.
    CopyOnly,
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
        | SourceMsg::Paste
        | SourceMsg::PasteResolved { .. }
        | SourceMsg::PastePrimary { .. }
        | SourceMsg::AutosaveDue { .. }
        | SourceMsg::LinkKey(LinkKey::Enter | LinkKey::Tab)
        | SourceMsg::LinkPick(_) => Gate::Drop,
        SourceMsg::Cut => Gate::CopyOnly,
        SourceMsg::Save | SourceMsg::KeepMine(_) => Gate::Refuse,
        // Every remaining variant passes; listed so a new `SourceMsg` fails to compile here.
        SourceMsg::Action(_)
        | SourceMsg::Key(SourceKey::Home { .. } | SourceKey::End { .. })
        | SourceMsg::Viewport { .. }
        | SourceMsg::Saved { .. }
        | SourceMsg::TakeTheirs(_)
        | SourceMsg::MiddleClick { .. }
        | SourceMsg::CommandClick { .. }
        | SourceMsg::LinkKey(LinkKey::Up | LinkKey::Down | LinkKey::Escape) => Gate::Pass,
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
            if let Some(cv_protocol::NoteHold { path, label }) = &j.hold {
                let label = if label.is_empty() {
                    j.job_id.clone()
                } else {
                    label.clone()
                };
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
        if self.has_hold_waits(&h.path) {
            return self.resume_hold_waits(&h.path);
        }
        match buf(&self.docs, &h.path).filter(|b| b.is_dirty()) {
            Some(b) => crate::source::autosave_timer(b.note_id(), b.edit_seq()),
            None => Task::none(),
        }
    }

    /// Drops every hold (connection lost or re-established; the subscribe snapshot re-fills).
    /// A missed release never arrives, so the waiters are reconciled here: after a disconnect a
    /// waiting close goes to the close dialog ("not connected") and a waiting quit entry becomes
    /// a `Disconnected` loss; on a live reconnect a waiter (there is none in practice, as the
    /// disconnect already drained them) proceeds to its save, which the daemon refuses with
    /// `HELD` if the snapshot re-holds the note — a failure dialog, never a hang.
    pub(super) fn clear_holds(&mut self) -> Task<Message> {
        self.holds.clear();
        let mut paths: Vec<String> = self.hold_closes.iter().map(|(p, _)| p.clone()).collect();
        if let Some(q) = &self.quit {
            paths.extend(q.held.iter().cloned());
        }
        paths.sort();
        paths.dedup();
        if self.status.connected {
            return Task::batch(paths.iter().map(|p| self.resume_hold_waits(p)));
        }
        for path in &paths {
            let ids: Vec<u64> = self
                .hold_closes
                .iter()
                .filter(|(p, _)| p == path)
                .map(|(_, id)| *id)
                .collect();
            self.hold_closes.retain(|(p, _)| p != path);
            let ids: Vec<u64> = ids
                .into_iter()
                .filter(|id| panes::find_id(&self.panes, *id).is_some())
                .collect();
            if !ids.is_empty()
                && self.dialog.is_none()
                && let Some(note) = self.docs.id_of(path)
            {
                self.close_dialog(note, ids, "not connected");
            }
            if let Some(q) = &mut self.quit
                && q.held.remove(path)
            {
                q.losses.note(path, crate::dialog::LossReason::Disconnected);
            }
        }
        self.quit_progress()
    }

    /// Whether a close or the quit waits for `path`'s hold to release.
    fn has_hold_waits(&self, path: &str) -> bool {
        self.hold_closes.iter().any(|(p, _)| p == path)
            || self.quit.as_ref().is_some_and(|q| q.held.contains(path))
    }

    /// Plan round-18 WP-2 (Decision 3): `path`'s hold released (or was dropped on a live
    /// reconnect) while a close or the quit waited on it. Drives the note's save with the
    /// matching follow-up — the first waiting tab id (or the quit) rides the save, other waiting
    /// tabs join `close_waiters` — so the existing landing paths close or finish the quit on a
    /// clean save, and a conflict or failure reaches the usual close dialog / loss list.
    fn resume_hold_waits(&mut self, path: &str) -> Task<Message> {
        let ids: Vec<u64> = self
            .hold_closes
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, id)| *id)
            .filter(|id| panes::find_id(&self.panes, *id).is_some())
            .collect();
        self.hold_closes.retain(|(p, _)| p != path);
        let quit = self.quit.as_mut().is_some_and(|q| q.held.remove(path));
        let dirty = self.doc_dirty(path);
        if !dirty {
            // Saved or reloaded meanwhile: nothing to lose.
            for id in ids {
                self.remove_tab(id);
            }
            return self.quit_progress();
        }
        let (after, rest) = if quit {
            (AfterSave::Quit, ids.clone())
        } else if let Some((first, rest)) = ids.split_first() {
            (AfterSave::CloseTab(*first), rest.to_vec())
        } else {
            return self.quit_progress();
        };
        // The holds are path-keyed: the document is resolved once here.
        let Some(note) = self.docs.id_of(path) else {
            return self.quit_progress();
        };
        let started = self.save_then(note, Some(after));
        let saving = buf(&self.docs, path).is_some_and(|d| d.is_saving());
        match started {
            Some(t) => {
                if quit && let Some(q) = &mut self.quit {
                    q.pending.insert(note);
                }
                self.add_close_waiters(path, &rest);
                t
            }
            None if saving => {
                // A save is in flight (the follow-up is queued): the landing resolves the waits.
                if quit && let Some(q) = &mut self.quit {
                    q.awaiting.insert(note);
                }
                self.add_close_waiters(path, &ids);
                Task::none()
            }
            None => {
                if quit {
                    let reason = self.loss_reason(note, "the save could not start");
                    if let Some(q) = &mut self.quit {
                        q.losses.note(path, reason);
                    }
                }
                if !ids.is_empty() && self.quit.is_none() {
                    self.close_dialog(note, ids, "the save could not start");
                }
                self.quit_progress()
            }
        }
    }

    fn add_close_waiters(&mut self, path: &str, ids: &[u64]) {
        if let Some(d) = self.docs.get_mut(path) {
            for id in ids {
                if !d.close_waiters.contains(id) {
                    d.close_waiters.push(*id);
                }
            }
        }
    }

    /// A quit asked again while the first still waits on held notes: with no saves of its own in
    /// flight, offers the usual unsaved-edits dialog naming each hold — OK is "quit anyway".
    pub(super) fn quit_anyway_dialog(&mut self) -> Task<Message> {
        let Some(q) = &self.quit else {
            return Task::none();
        };
        if q.held.is_empty() || !q.pending.is_empty() || !q.awaiting.is_empty() {
            return Task::none();
        }
        let mut losses = q.losses.clone();
        let mut held: Vec<String> = q.held.iter().cloned().collect();
        held.sort();
        for path in held {
            let label = self
                .held(&path)
                .map_or_else(String::new, |h| h.label.clone());
            losses.note(&path, crate::dialog::LossReason::Held(label));
        }
        self.quit = None;
        self.dialog = Some(crate::dialog::Dialog::Unsaved {
            losses,
            then: crate::dialog::Discard::Quit,
        });
        Task::none()
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
        self.notify_held(&label);
        true
    }

    /// The one hold notice (a local refusal and a daemon `HELD` reply both post it).
    pub(super) fn notify_held(&mut self, label: &str) {
        self.status.notify(
            Severity::Warning,
            format!("{label} is updating this note — save held"),
        );
    }

    /// Reports a stringified daemon error: a `HELD` refusal (see `daemon::rpc_err`) posts the
    /// hold notice, anything else is the usual error.
    pub(super) fn report_error(&mut self, e: String) {
        match crate::daemon::held_label(&e) {
            Some(label) => {
                let label = label.to_owned();
                self.notify_held(&label);
            }
            None => self.status.set_error(e),
        }
    }

    /// The holder label when `op` renames or deletes a held note (a delete also covers any held
    /// note under the path as a folder).
    pub(super) fn held_by_op(&self, op: &FileOp) -> Option<String> {
        let label = |h: &Hold| Some(h.label.clone());
        match op {
            FileOp::Create { .. } => None,
            FileOp::Rename { from, to } => {
                self.held(from).or_else(|| self.held(to)).and_then(label)
            }
            FileOp::Delete { path } => {
                let under = format!("{}/", path.trim_end_matches('/'));
                self.holds
                    .values()
                    .find(|h| h.path == *path || h.path.starts_with(&under))
                    .and_then(label)
            }
        }
    }

    /// Cut on a held note: the selection goes to the clipboard, the buffer is untouched.
    pub(super) fn held_cut_copy(&self) -> Task<Message> {
        match self
            .active_doc()
            .and_then(crate::source::SourceBuffer::selection)
            .filter(|s| !s.is_empty())
        {
            Some(sel) => iced::clipboard::write(sel),
            None => Task::none(),
        }
    }

    /// The gate a source message meets: `Pass` unless its note is held; a refused save posts the
    /// notice.
    pub(super) fn held_source_gate(&mut self, m: &SourceMsg) -> Gate {
        let path = match m.target() {
            SourceTarget::Note(id) => self.docs.path_of(id),
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
