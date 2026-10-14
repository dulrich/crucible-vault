//! Source editor: highlighting, own undo/redo, save/conflict (plan `v0-editor.md` WP-2).
//!
//! [`SourceBuffer`] wraps an iced `text_editor::Content` and routes every editing `Action`
//! through `editor_model::undo` (iced 0.14 has no undo). Saves go through
//! [`DaemonClient::write_note`] only (D1), CAS against the buffer's base hash, off the UI thread.
//!
//! External changes replace Crucible's `ChainManager.reconcileOpenEditor`
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/chains.ts:212`, inventory §3): there the plugin had to diff an
//! editor it didn't own after its own chains wrote the file; here the daemon notifies the editor
//! (`file_changed`) and the editor decides — clean buffer reloads silently keeping the cursor
//! line, dirty buffer shows a conflict bar. The Obsidian-induced part (polling the open editor
//! after a plugin write) is dropped.

pub mod highlighter;

use std::sync::Arc;
use std::time::{Duration, Instant};

use cv_protocol::Mode;
use editor_model::undo::{self, Change, EditKind, UndoHistory};
use iced::keyboard;
use iced::widget::text_editor::{self, Action, Binding, Content, Edit, KeyPress};
use iced::widget::{button, column, container, row, text};
use iced::{Element, Length, Task};

use crate::app::Tab;
use crate::daemon::{ContentHash, DaemonClient, DaemonError};
use crate::message::{Message, SourceMsg};
use crate::theme::Tokens;

pub use highlighter::{KindColors, MarkdownHighlighter, Settings as HighlightSettings};

/// Autosave debounce after the last edit.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(2);

/// Result of one save attempt (cloneable for the message bus; never carries note text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved(ContentHash),
    Conflict(ContentHash),
    Failed(String),
}

/// A save ready to run off the UI thread.
#[derive(Debug, Clone)]
pub struct SaveJob {
    pub path: String,
    pub seq: u64,
    text: String,
    base: ContentHash,
}

impl SaveJob {
    /// Runs the CAS write (blocking; call from `spawn_blocking` or a test).
    pub fn run(&self, d: &dyn DaemonClient) -> SaveOutcome {
        match d.write_note(&self.path, &self.text, &self.base) {
            Ok(h) => SaveOutcome::Saved(h),
            Err(DaemonError::Conflict { current_hash }) => SaveOutcome::Conflict(current_hash),
            Err(e) => SaveOutcome::Failed(e.to_string()),
        }
    }

    /// The message that reports `outcome` back.
    pub fn done(&self, outcome: SaveOutcome) -> Message {
        Message::Source(SourceMsg::Saved {
            path: self.path.clone(),
            seq: self.seq,
            outcome,
        })
    }
}

/// One open note's source buffer.
#[derive(Debug)]
pub struct SourceBuffer {
    path: String,
    content: Content,
    base_hash: ContentHash,
    history: UndoHistory,
    dirty: bool,
    /// Bumped on every edit; autosave ticks and save completions compare against it.
    edit_seq: u64,
    /// A save is in flight (one per buffer).
    saving: bool,
    /// Hash of the text currently being saved (its `file_changed` echo is ignored).
    pending_hash: Option<ContentHash>,
    /// Another save was requested while one was in flight.
    save_again: bool,
    /// Dirty buffer vs a newer file on disk: the disk hash, for the conflict bar.
    conflict: Option<ContentHash>,
    /// Highlighter epoch (bumped on wholesale content replacement).
    epoch: u64,
}

impl Clone for SourceBuffer {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            content: self.content.clone(),
            base_hash: self.base_hash.clone(),
            history: self.history.clone(),
            dirty: self.dirty,
            edit_seq: self.edit_seq,
            saving: self.saving,
            pending_hash: self.pending_hash.clone(),
            save_again: self.save_again,
            conflict: self.conflict.clone(),
            epoch: self.epoch,
        }
    }
}

fn to_model(c: text_editor::Cursor) -> undo::Cursor {
    undo::Cursor {
        pos: undo::Pos::new(c.position.line, c.position.column),
        anchor: c.selection.map(|p| undo::Pos::new(p.line, p.column)),
    }
}

fn to_iced(c: undo::Cursor) -> text_editor::Cursor {
    text_editor::Cursor {
        position: text_editor::Position {
            line: c.pos.line,
            column: c.pos.col,
        },
        selection: c.anchor.map(|p| text_editor::Position {
            line: p.line,
            column: p.col,
        }),
    }
}

impl SourceBuffer {
    /// A buffer over bytes read from disk with their daemon content hash.
    pub fn new(path: &str, bytes: &[u8], hash: ContentHash) -> Self {
        Self::with_cap(path, bytes, hash, undo::DEFAULT_CAP_BYTES)
    }

    /// As [`Self::new`] with an explicit undo byte cap.
    pub fn with_cap(path: &str, bytes: &[u8], hash: ContentHash, cap: usize) -> Self {
        Self {
            path: path.to_owned(),
            content: Content::with_text(&String::from_utf8_lossy(bytes)),
            base_hash: hash,
            history: UndoHistory::new(cap),
            dirty: false,
            edit_seq: 0,
            saving: false,
            pending_hash: None,
            save_again: false,
            conflict: None,
            epoch: 0,
        }
    }

    /// The vault-relative path.
    pub fn path(&self) -> &str {
        &self.path
    }
    /// The full text.
    pub fn text(&self) -> String {
        self.content.text()
    }
    /// The hash the next save is CAS'd against.
    pub fn base_hash(&self) -> &ContentHash {
        &self.base_hash
    }
    /// Unsaved edits exist.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    /// A save is in flight.
    pub fn is_saving(&self) -> bool {
        self.saving
    }
    /// The on-disk hash when the conflict bar is up.
    pub fn conflict(&self) -> Option<&ContentHash> {
        self.conflict.as_ref()
    }
    /// The edit counter.
    pub fn edit_seq(&self) -> u64 {
        self.edit_seq
    }
    /// The undo history (read-only).
    pub fn history(&self) -> &UndoHistory {
        &self.history
    }
    /// The iced cursor.
    pub fn cursor(&self) -> text_editor::Cursor {
        self.content.cursor()
    }
    /// The selected text, if any (WP-5 action context).
    pub fn selection(&self) -> Option<String> {
        self.content.selection()
    }
    /// Moves the cursor (navigation; not an edit).
    pub fn move_to(&mut self, c: text_editor::Cursor) {
        self.content.move_to(c);
    }

    /// Text of lines `a..=b` joined with their own line endings (as `Content::text` does).
    fn window(&self, a: usize, b: usize) -> String {
        let mut s = String::new();
        for i in a..=b {
            let Some(l) = self.content.line(i) else { break };
            s.push_str(&l.text);
            if i < b && self.content.line(i + 1).is_some() {
                s.push_str(match l.ending {
                    text_editor::LineEnding::None => "\n",
                    e => e.as_str(),
                });
            }
        }
        s
    }

    /// Applies an iced action. Editing actions are recorded in the undo history (and dropped
    /// when `read_only`). Returns whether the text changed.
    pub fn perform(&mut self, action: Action, read_only: bool, now: Instant) -> bool {
        if !action.is_edit() {
            self.content.perform(action);
            return false;
        }
        if read_only {
            return false;
        }
        let before = self.content.cursor();
        let kind = match (&action, before.selection) {
            (Action::Edit(Edit::Insert(c)), None) => EditKind::Type(*c),
            (Action::Edit(Edit::Backspace | Edit::Delete), None) => EditKind::Delete,
            _ => EditKind::Other,
        };
        let l0 = self.content.line_count();
        let (lo, hi) = match before.selection {
            Some(s) => (
                before.position.line.min(s.line),
                before.position.line.max(s.line),
            ),
            None => (before.position.line, before.position.line),
        };
        let a = lo.saturating_sub(1);
        let b = (hi + 1).min(l0.saturating_sub(1));
        let old = self.window(a, b);
        self.content.perform(action);
        let l1 = self.content.line_count();
        let b1 = (b + l1).saturating_sub(l0);
        let new = self.window(a, b1.max(a));
        let Some(change) = Change::diff(a, &old, &new) else {
            return false;
        };
        let after = self.content.cursor();
        self.history
            .record(change, kind, to_model(before), to_model(after), now);
        self.mark_edited();
        true
    }

    fn mark_edited(&mut self) {
        self.dirty = true;
        self.edit_seq += 1;
    }

    /// The text between two positions (`None` if out of range).
    fn span(&self, from: undo::Pos, to: undo::Pos) -> Option<String> {
        let w = self.window(from.line, to.line);
        let a = undo::offset_of(&w, undo::Pos::new(0, from.col))?;
        let b = undo::offset_of(&w, undo::Pos::new(to.line - from.line, to.col))?;
        w.get(a..b).map(str::to_owned)
    }

    /// Applies one recorded change in place (select the removed span, paste/delete). In-place
    /// application costs microseconds; `Content::with_text` on a 1 MB buffer measured ~650 ms
    /// (report). Returns `false` if the buffer no longer matches the record.
    fn apply_change(&mut self, c: &Change) -> bool {
        let end = c.removed_end();
        if self.span(c.at, end).as_deref() != Some(c.removed.as_str()) {
            return false;
        }
        self.content.move_to(to_iced(undo::Cursor {
            pos: end,
            anchor: Some(c.at),
        }));
        if !c.inserted.is_empty() {
            self.content
                .perform(Action::Edit(Edit::Paste(Arc::new(c.inserted.clone()))));
        } else if !c.removed.is_empty() {
            self.content.perform(Action::Edit(Edit::Delete));
        }
        self.span(c.at, c.inserted_end()).as_deref() == Some(c.inserted.as_str())
    }

    /// Undo (`true` if something was undone).
    pub fn undo(&mut self) -> bool {
        let ops = self.history.undo_ops();
        self.apply_ops(ops)
    }

    /// Redo.
    pub fn redo(&mut self) -> bool {
        let ops = self.history.redo_ops();
        self.apply_ops(ops)
    }

    fn apply_ops(&mut self, ops: Option<(Vec<Change>, undo::Cursor)>) -> bool {
        let Some((changes, cursor)) = ops else {
            return false;
        };
        for c in &changes {
            if !self.apply_change(c) {
                tracing::warn!("undo history out of sync with the buffer; reset");
                self.history.reset();
                self.mark_edited();
                return false;
            }
        }
        self.content.move_to(to_iced(cursor));
        self.mark_edited();
        true
    }

    /// Replaces the text with what is on disk, keeping the cursor line (clamped);
    /// history is reset, the buffer becomes clean, and any conflict clears.
    pub fn reload(&mut self, bytes: &[u8], hash: ContentHash) {
        let old = self.content.cursor().position;
        let text = String::from_utf8_lossy(bytes);
        self.content = Content::with_text(&text);
        let line = old.line.min(self.content.line_count().saturating_sub(1));
        let line_text = self
            .content
            .line(line)
            .map(|l| l.text.into_owned())
            .unwrap_or_default();
        let mut column = old.column.min(line_text.len());
        while !line_text.is_char_boundary(column) {
            column -= 1;
        }
        self.content.move_to(text_editor::Cursor {
            position: text_editor::Position { line, column },
            selection: None,
        });
        self.epoch += 1;
        self.base_hash = hash;
        self.history.reset();
        self.dirty = false;
        self.conflict = None;
        self.save_again = false;
    }

    /// Prepares a save, or `None` (clean, read-only, conflicted or already in flight — in the
    /// last case a follow-up save is queued).
    pub fn begin_save(&mut self, read_only: bool) -> Option<SaveJob> {
        if read_only || !self.dirty || self.conflict.is_some() {
            return None;
        }
        if self.saving {
            self.save_again = true;
            return None;
        }
        let text = self.content.text();
        self.saving = true;
        self.save_again = false;
        self.pending_hash = Some(ContentHash::of(text.as_bytes()));
        Some(SaveJob {
            path: self.path.clone(),
            seq: self.edit_seq,
            text,
            base: self.base_hash.clone(),
        })
    }

    /// Applies a save result. Returns `true` if a queued follow-up save should start.
    pub fn finish_save(&mut self, seq: u64, outcome: SaveOutcome) -> Result<bool, String> {
        self.saving = false;
        self.pending_hash = None;
        match outcome {
            SaveOutcome::Saved(h) => {
                self.base_hash = h;
                if seq == self.edit_seq {
                    self.dirty = false;
                }
                Ok(self.dirty && std::mem::take(&mut self.save_again))
            }
            SaveOutcome::Conflict(current) => {
                self.conflict = Some(current);
                Ok(false)
            }
            SaveOutcome::Failed(e) => Err(e),
        }
    }

    /// A `file_changed` for this buffer's path. `hash` is the notification's hash;
    /// `read` re-reads the file. Returns an error string worth surfacing, if any.
    pub fn external_change(
        &mut self,
        hash: Option<&str>,
        read: impl FnOnce() -> Result<(Vec<u8>, ContentHash), DaemonError>,
    ) -> Option<String> {
        if let Some(h) = hash
            && (h == self.base_hash.as_str()
                || self.pending_hash.as_ref().is_some_and(|p| p.as_str() == h))
        {
            return None; // Our own save's echo (origin is always `daemon`).
        }
        if self.dirty {
            let current = match hash {
                Some(h) => ContentHash::from_hex(h),
                None => match read() {
                    Ok((_, h)) => h,
                    Err(e) => return Some(format!("file changed: {e}")),
                },
            };
            if current != self.base_hash {
                self.conflict = Some(current);
            }
            return None;
        }
        match read() {
            Ok((bytes, h)) => {
                if h != self.base_hash {
                    self.reload(&bytes, h);
                }
                None
            }
            Err(DaemonError::NotFound(_)) => Some("file deleted on disk".into()),
            Err(e) => Some(format!("reload failed: {e}")),
        }
    }

    /// Keep mine: re-save against the on-disk hash.
    pub fn keep_mine(&mut self, read_only: bool) -> Option<SaveJob> {
        let current = self.conflict.take()?;
        self.base_hash = current;
        self.begin_save(read_only)
    }
}

fn find<'a>(tabs: &'a mut [Tab], path: &str) -> Option<&'a mut SourceBuffer> {
    tabs.iter_mut()
        .find(|t| t.path == path)
        .map(|t| &mut t.source)
}

/// What `update` needs from the app.
pub struct Ctx<'a> {
    pub daemon: Option<&'a Arc<dyn DaemonClient>>,
    pub mode: Mode,
    pub error: &'a mut Option<String>,
}

fn spawn_save(daemon: Option<&Arc<dyn DaemonClient>>, job: Option<SaveJob>) -> Task<Message> {
    let (Some(d), Some(job)) = (daemon, job) else {
        return Task::none();
    };
    let d = Arc::clone(d);
    let done = job.clone();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || job.run(d.as_ref()))
                .await
                .unwrap_or_else(|e| SaveOutcome::Failed(e.to_string()))
        },
        move |o| done.done(o),
    )
}

fn autosave_timer(path: String, seq: u64) -> Task<Message> {
    // The sleep future is built lazily inside the async block (it needs the executor's reactor).
    Task::perform(
        async { tokio::time::sleep(AUTOSAVE_DELAY).await },
        move |()| {
            Message::Source(SourceMsg::AutosaveDue {
                path: path.clone(),
                seq,
            })
        },
    )
}

/// Handles a [`SourceMsg`] (`Action`/`Undo`/`Redo`/`Save` target the active tab).
pub fn update(
    tabs: &mut [Tab],
    active: Option<usize>,
    msg: SourceMsg,
    ctx: Ctx<'_>,
) -> Task<Message> {
    let read_only = ctx.mode == Mode::Shadow;
    let active_buf = active.and_then(|i| tabs.get(i)).map(|t| t.path.clone());
    let buf = match &msg {
        SourceMsg::AutosaveDue { path, .. }
        | SourceMsg::Saved { path, .. }
        | SourceMsg::KeepMine(path)
        | SourceMsg::TakeTheirs(path) => find(tabs, path),
        _ => active_buf.and_then(|p| find(tabs, &p)),
    };
    let Some(buf) = buf else {
        return Task::none();
    };
    match msg {
        SourceMsg::Action(a) => {
            if buf.perform(a, read_only, Instant::now()) {
                return autosave_timer(buf.path.clone(), buf.edit_seq);
            }
            Task::none()
        }
        SourceMsg::Undo | SourceMsg::Redo => {
            let changed = !read_only
                && if matches!(msg, SourceMsg::Undo) {
                    buf.undo()
                } else {
                    buf.redo()
                };
            if changed {
                return autosave_timer(buf.path.clone(), buf.edit_seq);
            }
            Task::none()
        }
        SourceMsg::Save => spawn_save(ctx.daemon, buf.begin_save(read_only)),
        SourceMsg::AutosaveDue { seq, .. } => {
            if seq == buf.edit_seq {
                spawn_save(ctx.daemon, buf.begin_save(read_only))
            } else {
                Task::none()
            }
        }
        SourceMsg::Saved { seq, outcome, .. } => match buf.finish_save(seq, outcome) {
            Ok(true) => spawn_save(ctx.daemon, buf.begin_save(read_only)),
            Ok(false) => Task::none(),
            Err(e) => {
                *ctx.error = Some(format!("save failed: {e}"));
                Task::none()
            }
        },
        SourceMsg::KeepMine(_) => spawn_save(ctx.daemon, buf.keep_mine(read_only)),
        SourceMsg::TakeTheirs(path) => {
            if let Some(d) = ctx.daemon {
                match d.read_file(&path) {
                    Ok((bytes, h)) => buf.reload(&bytes, h),
                    Err(e) => *ctx.error = Some(format!("reload failed: {e}")),
                }
            }
            Task::none()
        }
    }
}

/// Handles `file_changed` for any open tab on `path`.
pub fn file_changed(
    tabs: &mut [Tab],
    path: &str,
    hash: Option<&str>,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Option<String> {
    let d = daemon?;
    let buf = find(tabs, path)?;
    buf.external_change(hash, || d.read_file(path))
}

/// Ctrl+S / Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, then iced's defaults.
fn key_binding(kp: KeyPress) -> Option<Binding<Message>> {
    if kp.modifiers.command()
        && let keyboard::Key::Character(c) = kp.key.as_ref()
    {
        let msg = match c.to_ascii_lowercase().as_str() {
            "z" if kp.modifiers.shift() => Some(SourceMsg::Redo),
            "z" => Some(SourceMsg::Undo),
            "y" => Some(SourceMsg::Redo),
            "s" => Some(SourceMsg::Save),
            _ => None,
        };
        if let Some(m) = msg {
            return Some(Binding::Custom(Message::Source(m)));
        }
    }
    Binding::from_key_press(kp)
}

/// Renders a tab's source buffer with its conflict bar / read-only marker.
pub fn view<'a>(buf: &'a SourceBuffer, tokens: Tokens, mode: Mode) -> Element<'a, Message> {
    let editor = text_editor::TextEditor::new(&buf.content)
        .height(Length::Fill)
        .on_action(|a| Message::Source(SourceMsg::Action(a)))
        .key_binding(key_binding)
        .highlight_with::<MarkdownHighlighter>(
            HighlightSettings {
                colors: KindColors::from_tokens(&tokens),
                epoch: buf.epoch,
            },
            highlighter::to_format,
        );
    let mut col = column![].spacing(4);
    if mode == Mode::Shadow {
        col = col.push(
            text("read-only (shadow)")
                .size(14)
                .color(tokens.warn_text.to_color()),
        );
    } else if buf.dirty {
        let state = if buf.saving { "saving…" } else { "unsaved" };
        col = col.push(text(state).size(14).color(tokens.text_faint.to_color()));
    }
    if buf.conflict.is_some() {
        let bar = row![
            text("File changed on disk while you were editing.").color(tokens.warn_text.to_color()),
            button(text("Keep mine"))
                .on_press(Message::Source(SourceMsg::KeepMine(buf.path.clone()))),
            button(text("Take theirs"))
                .on_press(Message::Source(SourceMsg::TakeTheirs(buf.path.clone()))),
        ]
        .spacing(8);
        col = col.push(
            container(bar)
                .padding(6)
                .width(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(tokens.panel_2.to_color().into()),
                    ..container::Style::default()
                }),
        );
    }
    col.push(editor).into()
}

#[cfg(test)]
mod tests;
