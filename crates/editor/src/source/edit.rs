//! Editing: every text change goes through [`SourceBuffer::transact`] (or a recorded
//! [`Change`]) into `editor_model::undo` (iced 0.14 has no undo).

use std::sync::Arc;
use std::time::Instant;

use editor_model::editing::{self, ListEnter};
use editor_model::undo::{self, Change, EditKind};
use iced::widget::text_editor::{self, Action, Content, Edit, Motion};

use super::SourceBuffer;
use super::buffer::{Repr, to_iced, to_model};
use super::place::place;

/// Shift+Tab removes up to this many leading spaces when the line does not start with `\t`.
pub const OUTDENT_SPACES: usize = 4;

impl SourceBuffer {
    /// Tab / Shift+Tab (plan editor-feedback-round-2 WP-4) as ONE recorded edit. Indent with a
    /// selection (any, incl. within one line) or with only indentation + a list marker before the
    /// cursor prepends `\t` to every touched line; otherwise Tab types `\t` (round-4 WP-2). Outdent removes one leading `\t`, else up
    /// to [`OUTDENT_SPACES`] leading spaces, from every touched line (the cursor's line without
    /// a selection). A selection ending at column 0 of a later line does not touch that line.
    /// The selection keeps covering the same lines (columns shift by the bytes changed).
    pub fn indent(&mut self, outdent: bool, now: Instant) -> bool {
        let content = self.shaped();
        let before = content.cursor();
        let pos = before.position;
        let sel = before.selection.filter(|s| *s != pos);
        let key = |p: text_editor::Position| (p.line, p.column);
        let (lo, hi) = match sel {
            Some(a) if key(a) < key(pos) => (a, pos),
            Some(a) => (pos, a),
            None => (pos, pos),
        };
        // plan editor-round-4 WP-2: any selection indents every touched line; without one, Tab
        // indents the line only when the text before the cursor is indentation + a list marker.
        let list = sel.is_none()
            && content.line(pos.line).is_some_and(|l| {
                l.text
                    .get(..pos.column)
                    .is_some_and(editing::is_list_prefix)
            });
        if !outdent && sel.is_none() && !list {
            return self.perform(Action::Edit(Edit::Paste(Arc::new("\t".to_owned()))), now);
        }
        let last = if hi.line > lo.line && hi.column == 0 {
            hi.line - 1
        } else {
            hi.line
        };
        let first = lo.line;
        self.transact(before, EditKind::Other, now, |c| {
            let mut delta = Vec::with_capacity(last - first + 1);
            for l in first..=last {
                let n = if outdent {
                    let n = c.line(l).map_or(0, |x| outdent_width(&x.text));
                    if n > 0 {
                        c.move_to(text_editor::Cursor {
                            position: text_editor::Position { line: l, column: n },
                            selection: Some(text_editor::Position { line: l, column: 0 }),
                        });
                        c.perform(Action::Edit(Edit::Delete));
                    }
                    n
                } else {
                    place(
                        c,
                        text_editor::Cursor {
                            position: text_editor::Position { line: l, column: 0 },
                            selection: None,
                        },
                    );
                    c.perform(Action::Edit(Edit::Paste(Arc::new("\t".to_owned()))));
                    1
                };
                delta.push(n);
            }
            let adj = |p: text_editor::Position| {
                let column = match p.line.checked_sub(first).and_then(|i| delta.get(i)) {
                    Some(&n) if outdent => p.column.saturating_sub(n),
                    Some(&n) => p.column + n,
                    None => p.column,
                };
                text_editor::Position {
                    line: p.line,
                    column,
                }
            };
            place(
                c,
                text_editor::Cursor {
                    position: adj(pos),
                    selection: before.selection.map(adj),
                },
            );
        })
    }

    /// Applies an iced action. Editing actions are recorded in the undo history (and dropped
    /// when read-only). Returns whether the text changed.
    pub fn perform(&mut self, action: Action, now: Instant) -> bool {
        if !action.is_edit() {
            let scroll = match action {
                Action::Scroll { lines } => Some(lines),
                _ => None,
            };
            let before = self.cursor().position.line;
            self.shaped().perform(action);
            self.track_top(scroll, before);
            return false;
        }
        let before = self.shaped().cursor();
        let kind = match (&action, before.selection) {
            (Action::Edit(Edit::Insert(c)), None) => EditKind::Type(*c),
            (Action::Edit(Edit::Backspace | Edit::Delete), None) => EditKind::Delete,
            _ => EditKind::Other,
        };
        self.transact(before, kind, now, |c| c.perform(action))
    }

    /// Runs `f` (any number of actions) as ONE recorded edit (plan editor-usability-round E1):
    /// one Undo reverts it and restores `before`. `f` may only change lines within one line of
    /// `before` or the current cursor/selection.
    fn transact(
        &mut self,
        before: text_editor::Cursor,
        kind: EditKind,
        now: Instant,
        f: impl FnOnce(&mut Content),
    ) -> bool {
        let content = self.shaped();
        let now_c = content.cursor();
        let lines = [
            Some(before.position.line),
            before.selection.map(|p| p.line),
            Some(now_c.position.line),
            now_c.selection.map(|p| p.line),
        ];
        let lo = lines.iter().flatten().min().copied().unwrap_or(0);
        let hi = lines.iter().flatten().max().copied().unwrap_or(0);
        let l0 = content.line_count();
        let a = lo.saturating_sub(1);
        let b = (hi + 1).min(l0.saturating_sub(1));
        let old = window(content, a, b);
        f(content);
        let l1 = content.line_count();
        let b1 = (b + l1).saturating_sub(l0);
        let new = window(content, a, b1.max(a));
        let Some(change) = Change::diff(a, &old, &new) else {
            return false;
        };
        let after = content.cursor();
        self.history
            .record(change, kind, to_model(before), to_model(after), now);
        self.mark_edited();
        true
    }

    /// Ctrl+X (plan editor-ui-round-3 WP-5): returns (clipboard text, text changed). With a
    /// selection: the selection, deleted (as iced's own cut; read-only copies only). Without
    /// one: the whole current line including its trailing newline, removed as ONE recorded
    /// edit; on the last line (no trailing newline) the line text is copied and the preceding
    /// newline is removed with it (same edit), so the line disappears. Read-only cuts nothing.
    pub fn cut(&mut self, now: Instant) -> (Option<String>, bool) {
        if let Some(sel) = self.selection().filter(|s| !s.is_empty()) {
            let changed = self.perform(Action::Edit(Edit::Delete), now);
            return (Some(sel), changed);
        }
        let content = self.shaped();
        let before = content.cursor();
        let l = before.position.line;
        let count = content.line_count();
        let len = |c: &Content, i: usize| c.line(i).map_or(0, |x| x.text.len());
        let pos = |line, column| text_editor::Position { line, column };
        let line_text = content
            .line(l)
            .map(|x| x.text.to_string())
            .unwrap_or_default();
        let (clip, from, to) = if l + 1 < count {
            (format!("{line_text}\n"), pos(l, 0), pos(l + 1, 0))
        } else if l > 0 {
            (
                line_text,
                pos(l - 1, len(content, l - 1)),
                pos(l, len(content, l)),
            )
        } else {
            (line_text, pos(0, 0), pos(0, len(content, 0)))
        };
        if from == to {
            return (Some(clip), false);
        }
        let changed = self.transact(before, EditKind::Other, now, |c| {
            c.move_to(text_editor::Cursor {
                position: to,
                selection: Some(from),
            });
            c.perform(Action::Edit(Edit::Delete));
        });
        (Some(clip), changed)
    }

    /// Enter with list continuation (one edit; a selection is replaced first).
    pub fn enter(&mut self, now: Instant) -> bool {
        let before = self.shaped().cursor();
        self.transact(before, EditKind::Other, now, |c| {
            if c.cursor().selection.is_some() {
                c.perform(Action::Edit(Edit::Delete));
            }
            let pos = c.cursor().position;
            let line = c.line(pos.line).map(|l| l.text.into_owned());
            match line.and_then(|l| editing::list_enter(&l, pos.column)) {
                Some(ListEnter::Continue(s)) => c.perform(Action::Edit(Edit::Paste(Arc::new(s)))),
                Some(ListEnter::Exit(r)) => {
                    c.move_to(text_editor::Cursor {
                        position: text_editor::Position {
                            line: pos.line,
                            column: r.end,
                        },
                        selection: Some(text_editor::Position {
                            line: pos.line,
                            column: r.start,
                        }),
                    });
                    c.perform(Action::Edit(Edit::Delete));
                }
                None => c.perform(Action::Edit(Edit::Enter)),
            }
        })
    }

    /// Ctrl+Backspace / Ctrl+Delete: delete to the previous / next word boundary (iced
    /// `WordLeft`/`WordRight`), or the selection, as one edit.
    pub fn delete_word(&mut self, forward: bool, now: Instant) -> bool {
        let content = self.shaped();
        let before = content.cursor();
        if before.selection.is_none() {
            content.perform(Action::Select(if forward {
                Motion::WordRight
            } else {
                Motion::WordLeft
            }));
            if content.cursor().selection.is_none() {
                return false;
            }
        }
        let changed = self.transact(before, EditKind::Other, now, |c| {
            c.perform(Action::Edit(Edit::Delete));
        });
        if !changed {
            self.shaped().move_to(before);
        }
        changed
    }

    /// Smart Home / End (see `editor_model::editing`); `select` extends the selection.
    pub fn smart_home_end(&mut self, end: bool, select: bool) {
        let content = self.shaped();
        let before = content.cursor();
        let pos = before.position;
        let Some(line) = content.line(pos.line).map(|l| l.text.into_owned()) else {
            return;
        };
        content.move_to(text_editor::Cursor {
            position: pos,
            selection: None,
        });
        content.perform(Action::Move(if end { Motion::End } else { Motion::Home }));
        let moved = content.cursor().position;
        let visual = if moved.line == pos.line {
            moved.column
        } else if end {
            line.len()
        } else {
            0
        };
        let column = if end {
            editing::end_target(pos.column, visual, line.len())
        } else {
            editing::home_target(pos.column, editing::indent_width(&line), visual)
        };
        let position = text_editor::Position {
            line: pos.line,
            column,
        };
        let anchor = before.selection.unwrap_or(pos);
        content.move_to(text_editor::Cursor {
            position,
            selection: (select && anchor != position).then_some(anchor),
        });
    }

    /// Replaces byte columns `cols` of `line` with `with` as ONE recorded edit, on either
    /// representation (a Reading-mode buffer stays plain — no shaping). The cursor is kept.
    /// Returns whether the text changed.
    pub fn replace_in_line(
        &mut self,
        line: usize,
        cols: std::ops::Range<usize>,
        with: &str,
        now: Instant,
    ) -> bool {
        let text = self.text();
        let (Some(a), Some(b)) = (
            undo::offset_of(&text, undo::Pos::new(line, cols.start)),
            undo::offset_of(&text, undo::Pos::new(line, cols.end)),
        ) else {
            return false;
        };
        if text[a..b] == *with {
            return false;
        }
        let change = Change {
            at: undo::Pos::new(line, cols.start),
            removed: text[a..b].to_owned(),
            inserted: with.to_owned(),
        };
        let cursor = to_model(self.cursor());
        match &mut self.content {
            Repr::Shaped(c) => {
                if !apply_change(c, &change) {
                    c.move_to(to_iced(cursor));
                    return false;
                }
                c.move_to(to_iced(cursor));
            }
            Repr::Plain { text: t, .. } => t.replace_range(a..b, with),
        }
        self.history
            .record(change, EditKind::Other, cursor, cursor, now);
        self.mark_edited();
        true
    }

    /// Toggles the task checkbox at source `line` (see `editor_model::edits::task_toggle`).
    pub fn toggle_task(&mut self, line: usize, now: Instant) -> bool {
        let Some((cols, next)) = editor_model::edits::task_toggle(&self.text(), line) else {
            return false;
        };
        self.replace_in_line(line, cols, &next.to_string(), now)
    }

    /// Plan round-8-intake-views-anchor WP-3: sets frontmatter boolean `key` to `to` (`None` =
    /// toggle) as one undoable edit that rewrites only its scalar (see
    /// `editor_model::edits::frontmatter_bool_edit`; plan usability-sprint-captures-links
    /// WP-4). Upserts — a null / missing key or a frontmatter-less note becomes `key: true`.
    pub fn set_frontmatter_bool(&mut self, key: &str, to: Option<bool>, now: Instant) -> bool {
        let text = self.text();
        let Some((range, with)) = editor_model::edits::frontmatter_bool_edit(&text, key, to) else {
            return false;
        };
        // The splice is one line's scalar (or an inserted line): line + byte columns.
        let head = &text[..range.start];
        let line = head.matches('\n').count();
        let col = range.start - head.rfind('\n').map_or(0, |i| i + 1);
        self.replace_in_line(line, col..col + range.len(), &with, now)
    }

    /// Sets the task status at source `line` to `to` as one edit (plan editor-ui-round-3 WP-4;
    /// see `editor_model::edits::task_set_status`).
    pub fn set_task_status(&mut self, line: usize, to: char, now: Instant) -> bool {
        let Some((cols, next)) = editor_model::edits::task_set_status(&self.text(), line, to)
        else {
            return false;
        };
        self.replace_in_line(line, cols, &next.to_string(), now)
    }

    /// Pastes `text` at the cursor (replacing any selection) as one edit.
    pub fn paste(&mut self, text: String, now: Instant) -> bool {
        self.perform(Action::Edit(Edit::Paste(Arc::new(text))), now)
    }

    fn mark_edited(&mut self) {
        self.save.dirty = true;
        self.edit_seq += 1;
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
        let content = self.shaped();
        let ok = changes.iter().all(|c| apply_change(content, c));
        if ok {
            content.move_to(to_iced(cursor));
        }
        if !ok {
            tracing::warn!("undo history out of sync with the buffer; reset");
            self.history.reset();
            self.mark_edited();
            return false;
        }
        self.mark_edited();
        true
    }
}

/// The leading bytes Shift+Tab removes from `line`: one `\t`, else up to [`OUTDENT_SPACES`]
/// spaces.
pub fn outdent_width(line: &str) -> usize {
    if line.starts_with('\t') {
        1
    } else {
        line.bytes()
            .take(OUTDENT_SPACES)
            .take_while(|&b| b == b' ')
            .count()
    }
}

/// The text between two positions (`None` if out of range).
fn span(content: &Content, from: undo::Pos, to: undo::Pos) -> Option<String> {
    let w = window(content, from.line, to.line);
    let a = undo::offset_of(&w, undo::Pos::new(0, from.col))?;
    let b = undo::offset_of(&w, undo::Pos::new(to.line - from.line, to.col))?;
    w.get(a..b).map(str::to_owned)
}

/// Applies one recorded change in place (select the removed span, paste/delete). In-place
/// application costs microseconds; `Content::with_text` on a 1 MB buffer measured ~650 ms
/// (report). Returns `false` if the buffer no longer matches the record.
fn apply_change(content: &mut Content, c: &Change) -> bool {
    let end = c.removed_end();
    if span(content, c.at, end).as_deref() != Some(c.removed.as_str()) {
        return false;
    }
    content.move_to(to_iced(undo::Cursor {
        pos: end,
        anchor: Some(c.at),
    }));
    if !c.inserted.is_empty() {
        content.perform(Action::Edit(Edit::Paste(Arc::new(c.inserted.clone()))));
    } else if !c.removed.is_empty() {
        content.perform(Action::Edit(Edit::Delete));
    }
    span(content, c.at, c.inserted_end()).as_deref() == Some(c.inserted.as_str())
}

/// Text of lines `a..=b` joined with their own line endings (as `Content::text` does).
fn window(content: &Content, a: usize, b: usize) -> String {
    let mut s = String::new();
    for i in a..=b {
        let Some(l) = content.line(i) else { break };
        s.push_str(&l.text);
        if i < b && content.line(i + 1).is_some() {
            s.push_str(match l.ending {
                text_editor::LineEnding::None => "\n",
                e => e.as_str(),
            });
        }
    }
    s
}
