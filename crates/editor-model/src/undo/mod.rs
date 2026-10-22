//! Undo/redo model wrapping every `text_editor` action (plan `v0-editor.md` WP-2).
//!
//! iced 0.14's `text_editor` has no undo (S3), so the editor owns history. The model is
//! **operation records**: each edit is stored as a [`Change`] (`at` + removed text + inserted
//! text, in `(line, byte column)` coordinates of the document *before* the change) together with
//! the cursor/selection before and after. Undo applies the inverse change to the current text and
//! restores the "before" cursor; redo re-applies and restores the "after" cursor. Records cost
//! only the bytes that changed (a 1 MB note pays nothing per keystroke), which is what makes the
//! per-buffer byte cap meaningful.
//!
//! Coalescing: consecutive single-character typing (or consecutive deletions) merge into one
//! group unless a word boundary is crossed (a word character typed after whitespace/punctuation),
//! the caller's clock shows ≥ [`IDLE_BREAK`] since the last edit, or the cursor jumped (the new
//! change's "before" cursor is not the group's "after" cursor). Everything else (paste, enter,
//! indent, replace-selection) is its own group.
//!
//! The model is pure: no iced, no clock of its own (callers pass `Instant`s, tests inject them).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Default per-buffer history cap in bytes (8 MiB).
pub const DEFAULT_CAP_BYTES: usize = 8 * 1024 * 1024;

/// Idle time that closes a typing group.
pub const IDLE_BREAK: Duration = Duration::from_secs(1);

/// Fixed per-record overhead counted against the cap (struct + bookkeeping).
const RECORD_OVERHEAD: usize = 64;

/// A position: zero-based line and byte column within that line (iced/cosmic-text use byte
/// columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    /// Constructor.
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// Cursor plus optional selection anchor (mirrors iced's `Cursor { position, selection }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub pos: Pos,
    pub anchor: Option<Pos>,
}

impl Cursor {
    /// A caret without selection.
    pub const fn caret(line: usize, col: usize) -> Self {
        Self {
            pos: Pos::new(line, col),
            anchor: None,
        }
    }
}

/// One text change: at `at`, `removed` was replaced by `inserted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub at: Pos,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    /// Builds the minimal change between an old and a new window of text that both start at
    /// line `start_line`, column 0. Returns `None` if they are equal.
    pub fn diff(start_line: usize, old: &str, new: &str) -> Option<Self> {
        if old == new {
            return None;
        }
        let mut prefix = old
            .bytes()
            .zip(new.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let (o, n) = (&old[prefix..], &new[prefix..]);
        let mut suffix = o
            .bytes()
            .rev()
            .zip(n.bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !o.is_char_boundary(o.len() - suffix) || !n.is_char_boundary(n.len() - suffix) {
            suffix -= 1;
        }
        let head = &old[..prefix];
        let lines_in_head = head.matches('\n').count();
        let col = match head.rfind('\n') {
            Some(i) => prefix - i - 1,
            None => prefix,
        };
        Some(Self {
            at: Pos::new(start_line + lines_in_head, col),
            removed: o[..o.len() - suffix].to_owned(),
            inserted: n[..n.len() - suffix].to_owned(),
        })
    }

    /// Where the removed text ends (the change's span in the "before" document).
    pub fn removed_end(&self) -> Pos {
        end_of(self.at, &self.removed)
    }

    /// Where the inserted text ends (the change's span in the "after" document).
    pub fn inserted_end(&self) -> Pos {
        end_of(self.at, &self.inserted)
    }

    fn cost(&self) -> usize {
        self.removed.len() + self.inserted.len() + RECORD_OVERHEAD
    }

    /// The change that undoes this one (same `at`).
    pub fn inverse(&self) -> Self {
        Self {
            at: self.at,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }

    /// Applies the change to `text` (lines separated by `\n`; a `\r` before `\n` belongs to the
    /// line ending, not the column space).
    pub fn apply(&self, text: &mut String) -> Result<(), UndoError> {
        let off = offset_of(text, self.at).ok_or(UndoError::OutOfRange)?;
        let end = off + self.removed.len();
        if text.get(off..end) != Some(self.removed.as_str()) {
            return Err(UndoError::Mismatch);
        }
        text.replace_range(off..end, &self.inserted);
        Ok(())
    }
}

fn end_of(at: Pos, s: &str) -> Pos {
    match s.rfind('\n') {
        Some(i) => Pos::new(at.line + s.matches('\n').count(), s.len() - i - 1),
        None => Pos::new(at.line, at.col + s.len()),
    }
}

/// Byte offset of `pos` in `text`.
pub fn offset_of(text: &str, pos: Pos) -> Option<usize> {
    let mut start = 0usize;
    for _ in 0..pos.line {
        start += text.get(start..)?.find('\n')? + 1;
    }
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let off = start + pos.col;
    (off <= line_end && text.is_char_boundary(off)).then_some(off)
}

/// What kind of edit produced a change (drives coalescing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// One typed character.
    Type(char),
    /// Backspace/Delete of one character without a selection.
    Delete,
    /// Anything else: paste, enter, indent, replace-selection.
    Other,
}

/// History failures (the text no longer matches the record — the caller resets).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UndoError {
    #[error("history position out of range")]
    OutOfRange,
    #[error("history does not match the buffer text")]
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    Typing { last_boundary: bool },
    Deleting,
    Other,
}

#[derive(Debug, Clone)]
struct Group {
    changes: Vec<Change>,
    before: Cursor,
    after: Cursor,
    kind: GroupKind,
    last: Instant,
    bytes: usize,
}

/// The undo/redo history of one buffer.
#[derive(Debug, Clone)]
pub struct UndoHistory {
    cap: usize,
    undo: VecDeque<Group>,
    redo: Vec<Group>,
    bytes: usize,
    open: bool,
}

impl Default for UndoHistory {
    fn default() -> Self {
        Self::new(DEFAULT_CAP_BYTES)
    }
}

fn is_boundary(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_')
}

impl UndoHistory {
    /// An empty history capped at `cap_bytes`.
    pub fn new(cap_bytes: usize) -> Self {
        Self {
            cap: cap_bytes,
            undo: VecDeque::new(),
            redo: Vec::new(),
            bytes: 0,
            open: false,
        }
    }

    /// Bytes currently held (undo + redo), always ≤ the cap.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The cap.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Undo groups available.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Redo groups available.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Drops all history (reload from disk).
    pub fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
        self.open = false;
    }

    /// Closes the current group so the next edit starts a new one.
    pub fn break_group(&mut self) {
        self.open = false;
    }

    /// Records one change. Clears redo.
    pub fn record(
        &mut self,
        change: Change,
        kind: EditKind,
        before: Cursor,
        after: Cursor,
        now: Instant,
    ) {
        for g in self.redo.drain(..) {
            self.bytes -= g.bytes;
        }
        let cost = change.cost();
        let merge = self.open
            && self.undo.back().is_some_and(|g| {
                g.after == before
                    && now.saturating_duration_since(g.last) < IDLE_BREAK
                    && match (g.kind, kind) {
                        (GroupKind::Typing { last_boundary }, EditKind::Type(c)) => {
                            !(last_boundary && !is_boundary(c))
                        }
                        (GroupKind::Deleting, EditKind::Delete) => true,
                        _ => false,
                    }
            });
        let new_kind = match kind {
            EditKind::Type(c) => GroupKind::Typing {
                last_boundary: is_boundary(c),
            },
            EditKind::Delete => GroupKind::Deleting,
            EditKind::Other => GroupKind::Other,
        };
        match self.undo.back_mut() {
            Some(g) if merge => {
                g.changes.push(change);
                g.after = after;
                g.kind = new_kind;
                g.last = now;
                g.bytes += cost;
            }
            _ => self.undo.push_back(Group {
                changes: vec![change],
                before,
                after,
                kind: new_kind,
                last: now,
                bytes: cost,
            }),
        }
        self.bytes += cost;
        self.open = !matches!(new_kind, GroupKind::Other);
        self.enforce_cap();
    }

    /// Whether [`Self::record_joined`] can merge into the latest group: there is one, and
    /// nothing is undone (joining an older group under a pending redo would reorder history).
    pub fn can_join(&self) -> bool {
        !self.undo.is_empty() && self.redo.is_empty()
    }

    /// Records a programmatic follow-up edit (auto-lint, plan round-9 WP-6 / audit E2) into the
    /// latest group, so ONE undo reverts the user's edit together with it: the group's `before`
    /// cursor is kept, its `after` becomes `after`, and it never coalesces further typing.
    /// When [`Self::can_join`] is false this records `change` as its own group instead.
    pub fn record_joined(&mut self, change: Change, before: Cursor, after: Cursor, now: Instant) {
        if !self.can_join() {
            self.record(change, EditKind::Other, before, after, now);
            return;
        }
        let cost = change.cost();
        if let Some(g) = self.undo.back_mut() {
            g.changes.push(change);
            g.after = after;
            g.kind = GroupKind::Other;
            g.last = now;
            g.bytes += cost;
        }
        self.bytes += cost;
        self.open = false;
        self.enforce_cap();
    }

    fn enforce_cap(&mut self) {
        while self.bytes > self.cap {
            match self.undo.pop_front() {
                Some(g) => self.bytes -= g.bytes,
                None => break,
            }
        }
        if self.bytes > self.cap {
            // Only redo remains over the cap (cannot happen after `record`, which clears redo).
            for g in self.redo.drain(..) {
                self.bytes -= g.bytes;
            }
        }
        if self.undo.is_empty() {
            self.open = false;
        }
    }

    /// Pops the latest group for an in-place undo: the changes to apply **in order** (already
    /// inverted and reversed) and the cursor to restore. Callers that cannot apply them must
    /// [`Self::reset`].
    pub fn undo_ops(&mut self) -> Option<(Vec<Change>, Cursor)> {
        self.open = false;
        let g = self.undo.pop_back()?;
        let ops = g.changes.iter().rev().map(Change::inverse).collect();
        let cursor = g.before;
        self.redo.push(g);
        Some((ops, cursor))
    }

    /// Pops the latest undone group for an in-place redo (changes in order, cursor to restore).
    pub fn redo_ops(&mut self) -> Option<(Vec<Change>, Cursor)> {
        self.open = false;
        let g = self.redo.pop()?;
        let ops = g.changes.clone();
        let cursor = g.after;
        self.undo.push_back(g);
        Some((ops, cursor))
    }

    /// Undoes the latest group against `text`; returns the new text and the cursor to restore.
    /// On error the history is reset (it no longer describes the buffer).
    pub fn undo(&mut self, text: &str) -> Result<Option<(String, Cursor)>, UndoError> {
        self.open = false;
        let Some(g) = self.undo.pop_back() else {
            return Ok(None);
        };
        let mut out = text.to_owned();
        for c in g.changes.iter().rev() {
            if let Err(e) = c.inverse().apply(&mut out) {
                self.reset();
                return Err(e);
            }
        }
        let cursor = g.before;
        self.redo.push(g);
        Ok(Some((out, cursor)))
    }

    /// Redoes the latest undone group against `text`.
    pub fn redo(&mut self, text: &str) -> Result<Option<(String, Cursor)>, UndoError> {
        self.open = false;
        let Some(g) = self.redo.pop() else {
            return Ok(None);
        };
        let mut out = text.to_owned();
        for c in &g.changes {
            if let Err(e) = c.apply(&mut out) {
                self.reset();
                return Err(e);
            }
        }
        let cursor = g.after;
        self.undo.push_back(g);
        Ok(Some((out, cursor)))
    }
}

#[cfg(test)]
mod tests;
