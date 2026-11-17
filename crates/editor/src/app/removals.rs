//! Plan `intake-removals-sources-rescan` WP-6: the editor side of the removal review — a
//! Delete / Keep / Delete all click on Reading's `[!removed]` fold, "Trim selection as removed"
//! and "Next note with removals". The daemon is the only writer: the editor's one write on this
//! path is the save before the request (the existing save path), so the request always carries
//! the content hash the daemon holds. The reply is shown as counts only; the note reloads
//! through the normal changed-on-disk follow, never from the reply.

use cv_protocol::{
    NextFoldNoteParams, NextFoldNoteResult, RemovalKind, RemovalOp, RemovalRefusal,
    RemovalsApplied, ResolveRemovalsParams, ResolveRemovalsResult,
};
use iced::Task;

use super::App;
use crate::daemon::DaemonError;
use crate::message::{AfterSave, Message, RemovalsMsg};

/// The status text of an applied resolve — counts only, no note text.
#[must_use]
pub fn applied_text(a: &RemovalsApplied) -> String {
    let mut s = format!(
        "Removals applied: {} groups resolved, {} lines labelled",
        a.groups, a.lines_labelled
    );
    let restored = a.restored_at_anchor + a.restored_at_end;
    if restored > 0 {
        s.push_str(&format!(", {restored} lines restored"));
    }
    if a.media_queued > 0 {
        s.push_str(&format!(", {} media queued for cleanup", a.media_queued));
    }
    if a.labels_failed {
        s.push_str("; the labels could not be recorded");
    }
    if a.cleanup_failed {
        s.push_str("; the media cleanup could not be queued");
    }
    s
}

/// The status text of a refusal (each reason has its own).
#[must_use]
pub fn refusal_text(r: RemovalRefusal) -> &'static str {
    match r {
        RemovalRefusal::StaleHash => "Removals: the note changed; try again",
        RemovalRefusal::UnknownGroup => "Removals: that group is no longer in the note",
        RemovalRefusal::BadRange => "Removals: the selected lines are not a valid range",
        RemovalRefusal::Generated => "Removals: the selection is inside a generated block",
        RemovalRefusal::UnknownNote => "Removals: the daemon does not know this note",
    }
}

/// The status text of a failed call; an older daemon that lacks the request answers
/// method-not-found.
#[must_use]
pub fn call_error_text(what: &str, e: &DaemonError) -> String {
    match e {
        DaemonError::Rpc(r) if r.code == cv_protocol::error_codes::METHOD_NOT_FOUND => {
            format!(
                "{what}: the running daemon does not support this — restart it after installing"
            )
        }
        e => format!("{what}: {e}"),
    }
}

impl App {
    /// A fold control click (or a trim, once its lines are known): saves first when the buffer
    /// has unsaved edits and continues only when the save landed clean; otherwise sends at once.
    pub(super) fn removals_click(&mut self, path: String, op: RemovalOp) -> Task<Message> {
        let Some(id) = self.docs.id_of(&path) else {
            self.status
                .set_error("Removals: the note is not open".into());
            return Task::none();
        };
        let Some(buf) = self.docs.buffer(Some(id)) else {
            return Task::none();
        };
        if buf.is_dirty() || buf.is_saving() {
            return self
                .save_then(id, Some(AfterSave::Removals(op)))
                .unwrap_or_else(|| {
                    self.status.set_error(
                        "Removals: not done (the note is saving or has a conflict); try again"
                            .into(),
                    );
                    Task::none()
                });
        }
        if buf.conflict().is_some() {
            self.status
                .set_error("Removals: not done (the note has a conflict)".into());
            return Task::none();
        }
        self.send_removals(&path, op)
    }

    /// Sends `resolve_removals` for the note at `path` with the hash the daemon last confirmed.
    pub(super) fn send_removals(&mut self, path: &str, op: RemovalOp) -> Task<Message> {
        let Some(id) = self.docs.id_of(path) else {
            return Task::none();
        };
        let Some(buf) = self.docs.buffer(Some(id)) else {
            return Task::none();
        };
        let p = ResolveRemovalsParams {
            id,
            base_hash: buf.base_hash().as_str().to_owned(),
            op,
        };
        self.rpc(
            move |d| {
                d.resolve_removals(&p)
                    .map_err(|e| call_error_text("Removals", &e))
            },
            |r| Message::Removals(RemovalsMsg::Resolved(r)),
        )
    }

    pub(super) fn update_removals(&mut self, m: RemovalsMsg) -> Task<Message> {
        match m {
            RemovalsMsg::Resolved(Ok(ResolveRemovalsResult::Applied(a))) => {
                self.status.notify(super::Severity::Info, applied_text(&a));
            }
            RemovalsMsg::Resolved(Ok(ResolveRemovalsResult::Refused { reason, .. })) => {
                self.status.set_error(refusal_text(reason).into());
            }
            RemovalsMsg::Resolved(Err(e)) => self.status.set_error(e),
            RemovalsMsg::Trim(kind) => return self.trim_selection(kind),
            RemovalsMsg::Next => return self.next_note_with_removals(),
            RemovalsMsg::NextFound(Ok(NextFoldNoteResult { path: Some(p) })) => self.open(&p),
            RemovalsMsg::NextFound(Ok(NextFoldNoteResult { path: None })) => {
                self.status
                    .notify(super::Severity::Info, "no other note has removals");
            }
            RemovalsMsg::NextFound(Err(e)) => self.status.set_error(e),
        }
        Task::none()
    }

    /// "Next note with removals": the daemon names the next note after the active one that still
    /// has a fold (the active note's id when known, else its path; none active → neither).
    fn next_note_with_removals(&mut self) -> Task<Message> {
        let active = self.active_note().map(|t| t.path.to_owned());
        let id = active.as_deref().and_then(|p| self.docs.id_of(p));
        let p = NextFoldNoteParams {
            id,
            path: if id.is_some() { None } else { active },
        };
        self.rpc(
            move |d| {
                d.next_fold_note(&p)
                    .map_err(|e| call_error_text("Next note with removals", &e))
            },
            |r| Message::Removals(RemovalsMsg::NextFound(r)),
        )
    }

    /// "Trim selection as removed": the selection (Source: the buffer's; Reading: the drawn
    /// selection) expanded to whole lines, as 0-based inclusive body lines, sent as the `trim`
    /// form with `kind`.
    fn trim_selection(&mut self, kind: RemovalKind) -> Task<Message> {
        let Some(path) = self.active_note().map(|t| t.path.to_owned()) else {
            self.status
                .set_error("Trim selection as removed: no active note".into());
            return Task::none();
        };
        match self.selected_body_lines(&path) {
            Ok((first, last)) => self.removals_click(
                path,
                RemovalOp::Trim {
                    first,
                    last,
                    kind: Some(kind),
                },
            ),
            Err(e) => {
                self.status.set_error(e.text().into());
                Task::none()
            }
        }
    }

    /// The active note's selected lines as body lines. The side that selected last is asked
    /// first (Split), then the other; `NoSelection` only when neither has one.
    fn selected_body_lines(
        &self,
        path: &str,
    ) -> Result<(usize, usize), editor_model::removals::TrimError> {
        use editor_model::removals::TrimError;
        let Some(tab) = self.active_note() else {
            return Err(TrimError::NoSelection);
        };
        let mode = self.reading.mode(&self.docs, path);
        let Some(buf) = self.docs.get(path).map(|od| &od.buffer) else {
            return Err(TrimError::NoSelection);
        };
        let text = buf.text();
        let source = || {
            let c = buf.cursor();
            let (f, l) = editor_model::removals::source_lines(
                (c.position.line, c.position.column),
                c.selection.map(|s| (s.line, s.column)),
            )?;
            editor_model::removals::body_range(&text, f, l)
        };
        let reading = || {
            let sel = self.reading.selection(&self.docs, path, tab.id);
            let doc = self.reading.doc(&self.docs, path);
            let (Some(sel), Some(doc)) = (sel, doc) else {
                return Err(TrimError::NoSelection);
            };
            let sel_text = self
                .reading
                .selection_text(&self.docs, path, tab.id)
                .unwrap_or_default();
            let (lo, hi) = sel.ordered();
            let (f, l) = editor_model::removals::reading_lines(
                &doc.model, &doc.runs, lo.run, hi.run, &sel_text, &text,
            )?;
            editor_model::removals::body_range(&text, f, l)
        };
        let reading_first = mode.shows_reading()
            && (!mode.shows_source()
                || !self.reading.source_selected_last(&self.docs, path, tab.id));
        let (a, b): (&dyn Fn() -> _, &dyn Fn() -> _) = if reading_first {
            (&reading, &source)
        } else {
            (&source, &reading)
        };
        match a() {
            Err(TrimError::NoSelection) if mode.shows_source() && mode.shows_reading() => b(),
            r => r,
        }
    }
}
