//! Place-keeping: the tracked top line, the measured viewport, and the deferred carry run on
//! the editor's layout report ([`editor_model::carry`]).

use editor_model::carry::{self, Carry, Step};
use iced::widget::text_editor::{self, Action, Content, Motion};

use super::SourceBuffer;
use super::buffer::Repr;
use super::view::EDITOR_PADDING;

pub use editor_model::carry::rows_at;

/// plan editor-round-4 WP-2: the visible source line count `top_line` tracking assumes before
/// the editor's viewport was first measured (plan round-5 WP-1: then [`viewport_lines`]).
pub const VISIBLE_LINES: usize = 30;

/// The overshooting scroll (lines) that reaches a document edge; shaping clamps it to the last
/// full viewport, wrapped lines included.
pub const EDGE_SCROLL: i32 = 1_000_000;

/// What the caller does after [`SourceBuffer::on_layout`].
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relayout {
    /// Placement is done (or there was none).
    Settled,
    /// A carry needs one more layout pass: re-send the report after the next layout
    /// ([`crate::reading::ScrollIntent::SourceRelayout`]).
    Again,
}

impl SourceBuffer {
    /// Selects `from..to` ((line, byte column) each; the cursor ends at `to`), materialising a
    /// plain buffer first so the shaped editor scrolls to it (find in note).
    pub fn select_range(&mut self, from: (usize, usize), to: (usize, usize)) {
        self.materialise();
        let pos = |(line, column)| text_editor::Position { line, column };
        self.move_to(text_editor::Cursor {
            position: pos(to),
            selection: (from != to).then(|| pos(from)),
        });
        self.follow_cursor();
    }

    /// plan editor-ui-round-3 WP-6: the top visible source line (block-granular place-keeping
    /// estimate; see the `top_line` field).
    pub fn top_line(&self) -> usize {
        self.top_line
    }

    /// Updates the tracked top line after a non-edit action: a wheel scroll of `lines` (visual
    /// lines; equal to source lines when nothing wraps), then — when the cursor moved from
    /// line `before` to above the top — the editor scrolled up to it.
    pub(super) fn track_top(&mut self, scroll: Option<i32>, before: usize) {
        let last = self.shaped().line_count().saturating_sub(1);
        if let Some(lines) = scroll {
            let top = i64::try_from(self.top_line).unwrap_or(i64::MAX) + i64::from(lines);
            self.top_line = usize::try_from(top.max(0))
                .unwrap_or(0)
                .min(last)
                .min(self.max_top());
        }
        if self.cursor().position.line != before {
            self.follow_cursor();
        }
    }

    /// plan editor-round-4 WP-2: keeps the cursor inside the tracked viewport — the editor
    /// scrolls to a cursor that moved above the top or below the last visible line
    /// ([`VISIBLE_LINES`] estimate), so the top line follows it either way.
    fn follow_cursor(&mut self) {
        let cursor = self.cursor().position.line;
        if cursor < self.top_line {
            self.top_line = cursor;
        } else if cursor >= self.top_line + self.visible_lines {
            self.top_line = cursor + 1 - self.visible_lines;
        }
    }

    /// Plan round-5 WP-1: the measured viewport line count.
    pub fn visible_lines(&self) -> usize {
        self.visible_lines
    }

    /// Plan round-5 WP-1: records the viewport's measured line count (at least 1).
    pub fn set_visible_lines(&mut self, lines: usize) {
        self.visible_lines = lines.max(1);
    }

    /// Plan round-7 WP-4: the editor's layout report (its measured line count): records it, then
    /// runs one pass of the pending carry, which can be performed exactly now that the editor
    /// is laid out ([`carry::layout_pass`]).
    pub fn on_layout(&mut self, lines: usize) -> Relayout {
        self.set_visible_lines(lines);
        let pass = carry::layout_pass(self.carry.take(), self.max_top(), self.visible_lines);
        match pass.step {
            None => {}
            Some(Step::Edge { bottom }) => self.scroll_edge(bottom),
            // Pass 1: the caret at the end scrolls the editor to the end on the next layout.
            Some(Step::CaretToEnd) => self.caret_at(self.end_pos()),
            // Pass 2: the caret moving up to the line scrolls it to the viewport top.
            Some(Step::CaretToLine { line, top }) => {
                self.caret_at(text_editor::Position { line, column: 0 });
                self.top_line = top;
            }
            // Pass 3, after the last layout has scrolled the caret line to the top.
            Some(Step::ScrollUp { rows }) => {
                let up = i32::try_from(rows).unwrap_or(i32::MAX);
                self.shaped().perform(Action::Scroll { lines: -up });
            }
        }
        self.carry = pass.next;
        if pass.relayout() {
            Relayout::Again
        } else {
            Relayout::Settled
        }
    }

    fn caret_at(&mut self, position: text_editor::Position) {
        self.move_to(text_editor::Cursor {
            position,
            selection: None,
        });
    }

    fn scroll_edge(&mut self, bottom: bool) {
        let lines = if bottom { EDGE_SCROLL } else { -EDGE_SCROLL };
        self.shaped().perform(Action::Scroll { lines });
    }

    /// Plan round-7 WP-4: the source line at the laid-out editor's viewport top, measured by
    /// hit-testing the viewport's top-left corner (exact with wrapped lines, unlike the
    /// [`Self::top_line`] estimate), recorded as the tracked top. Plan round-8-intake-views-anchor
    /// WP-4: also hit-tests the row at `anchor` (fraction) of the viewport height
    /// (`visible_lines` rows at `font_size`) and returns the source line there (the top line at
    /// anchor 0). Only meaningful while the editor is shown.
    ///
    /// The cursor and selection are put back — unless the caret is off-screen (the view was
    /// wheel-scrolled away from it): restoring it would make the next layout scroll back to it,
    /// moving the still-shown view (Source → Split), so the caret then stays at the viewport's
    /// top-left (plan round-7 WP-4, attempt 2).
    pub fn measure_anchor_line(&mut self, anchor: f32, font_size: f32) -> usize {
        // The middle of the anchor row (the row a carry here would place a line on).
        let rows = rows_at(anchor, self.visible_lines);
        #[allow(clippy::cast_precision_loss)]
        let dy = if rows == 0 {
            0.0
        } else {
            (rows as f32 + 0.5) * (font_size * 1.3).max(0.0)
        };
        let mut anchored = None;
        if let Repr::Shaped(c) = &mut self.content {
            let saved = c.cursor();
            c.perform(Action::Click(iced::Point::ORIGIN));
            let top = c.cursor().position;
            if dy > 0.0 {
                c.perform(Action::Click(iced::Point::new(0.0, dy)));
                anchored = Some(c.cursor().position.line);
            }
            // Past the last visible row: the hit clamps to the viewport's last position.
            c.perform(Action::Click(iced::Point::new(1.0e6, 1.0e6)));
            let bottom = c.cursor().position;
            let key = |p: text_editor::Position| (p.line, p.column);
            let at = key(saved.position);
            if key(top) <= at && at <= key(bottom) {
                c.move_to(saved);
            } else {
                c.move_to(text_editor::Cursor {
                    position: top,
                    selection: None,
                });
            }
            self.top_line = top.line;
        }
        anchored.unwrap_or(self.top_line)
    }

    /// Plan round-5 WP-1: the largest top line — the editor never scrolls past the last line
    /// at the viewport's bottom, so a top nearer the end than one viewport is unreachable.
    pub(super) fn max_top(&self) -> usize {
        self.line_count().saturating_sub(self.visible_lines)
    }

    /// plan editor-ui-round-3 WP-6 / round-8-intake-views-anchor WP-4: scrolls the editor so
    /// source `line` sits at `anchor` (fraction) of the viewport height (0 = the top),
    /// materialising it. Wrapped lines above `line` count once each, so a wrapped paragraph
    /// above it lands the view slightly early. Plan round-5 WP-1: clamped to the last reachable
    /// top ([`Self::max_top`]), so a carry to a block near the end shows the document's last
    /// viewport, never a past-the-end view. The immediate scroll is lost when the editor is not
    /// laid out yet (a Reading → Source switch), so the multi-pass [`Carry`] places it exactly
    /// on the next layouts.
    pub fn scroll_to_line_anchored(&mut self, line: usize, anchor: f32) {
        let rows = rows_at(anchor, self.visible_lines);
        let target = line.saturating_sub(rows).min(self.max_top());
        // Plan round-6 WP-1: one delta from the tracked (pending) top — `shaped()` of a plain
        // buffer has already queued the scroll to `top_line`.
        let from = self.top_line;
        scroll_content(self.shaped(), from, target);
        self.top_line = target;
        self.carry = Some(carry::line_carry(line, target, anchor));
    }

    /// Plan round-6 WP-1: scrolls to the document's start / end without moving the cursor (a
    /// mode switch after Ctrl+PgUp/PgDn), deferred to the next layout ([`Self::on_layout`]).
    pub fn scroll_to_edge(&mut self, bottom: bool) {
        self.materialise();
        self.carry = Some(Carry::Edge { bottom });
        self.top_line = if bottom { self.max_top() } else { 0 };
    }

    /// plan editor-ui-round-3 WP-5: moves the cursor to the buffer's start (`false`) or end
    /// (`true`), dropping the selection; materialises a plain buffer so the editor scrolls.
    pub fn go_to_edge(&mut self, bottom: bool) {
        let at = if bottom {
            self.end_pos()
        } else {
            text_editor::Position { line: 0, column: 0 }
        };
        self.materialise();
        // Plan round-7 WP-4: a caret already at the edge moves nothing, so the editor would
        // not scroll (after a wheel scroll away); scroll explicitly then.
        let still = self.cursor().position == at && self.cursor().selection.is_none();
        self.caret_at(at);
        if still {
            self.scroll_edge(bottom);
        }
        // plan editor-round-4 WP-2: the editor scrolls to the edge; place-keeping follows.
        self.top_line = if bottom {
            at.line.saturating_sub(self.visible_lines - 1)
        } else {
            0
        };
    }
}

/// Plan round-5 WP-1: the source lines a laid-out editor `height` (px) shows at text size
/// `font_size` (the pure math is [`editor_model::place::viewport_lines`]).
pub fn viewport_lines(height: f32, font_size: f32) -> usize {
    editor_model::place::viewport_lines(height, font_size, EDITOR_PADDING)
}

/// Plan round-6 WP-1: the two editor operations the place-keeping sequences issue, so the
/// layout-backed tests drive an `iced_graphics` text editor (whose cosmic buffer is
/// reachable) with exactly the production sequence ([`fresh_view`], [`scroll_content`]).
pub(crate) trait EditorOps {
    fn act(&mut self, action: Action);
    /// The editor's raw `move_to`.
    fn move_cursor(&mut self, cursor: text_editor::Cursor);
}

impl EditorOps for Content {
    fn act(&mut self, action: Action) {
        self.perform(action);
    }
    fn move_cursor(&mut self, cursor: text_editor::Cursor) {
        self.move_to(cursor);
    }
}

/// `Content::move_to` that also clears a stale selection: iced's `move_to` leaves the old
/// selection in place when `c.selection` is `None` (WP-4); a motion over an empty selection
/// collapses it without moving the cursor.
pub(super) fn place(content: &mut impl EditorOps, c: text_editor::Cursor) {
    if c.selection.is_some() {
        content.move_cursor(c);
    } else {
        content.move_cursor(text_editor::Cursor {
            position: c.position,
            selection: Some(c.position),
        });
        content.act(Action::Move(Motion::Left));
    }
}

/// The iced adapter of [`editor_model::place::scroll_lines`].
fn scroll_action(from: usize, to: usize) -> Option<Action> {
    editor_model::place::scroll_lines(from, to).map(|lines| Action::Scroll { lines })
}

/// Scrolls an editor whose pending top source line is `from` so `to` is at the top.
pub(crate) fn scroll_content(e: &mut impl EditorOps, from: usize, to: usize) {
    if let Some(a) = scroll_action(from, to) {
        e.act(a);
    }
}

/// Plan round-6 WP-1: the sequence a freshly shaped editor (scroll at line 0) gets — the kept
/// cursor, then the tracked top line (measured: a kept cursor outside the kept view does not
/// pull the view back to it at the next shaping).
pub(crate) fn fresh_view(e: &mut impl EditorOps, cursor: text_editor::Cursor, top: usize) {
    place(e, cursor);
    scroll_content(e, 0, top);
}
