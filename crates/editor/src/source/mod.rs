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

use editor_model::editing::{self, ListEnter};
use editor_model::undo::{self, Change, EditKind, UndoHistory};
use iced::keyboard::{self, key};
use iced::widget::text_editor::{self, Action, Binding, Content, Edit, KeyPress, Motion};
use iced::widget::{Space, button, column, container, row, text};
use iced::{Element, Length, Point, Task, clipboard};

use crate::daemon::{ContentHash, DaemonClient, DaemonError};
use crate::message::{Message, SourceKey, SourceMsg};
use crate::theme::Tokens;

pub use highlighter::{
    FindSpan, KindColors, MarkdownHighlighter, Paint, Settings as HighlightSettings,
};

/// Autosave debounce after the last edit.
pub const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);

/// The text editor's padding (middle-click points are mapped through it like iced's own clicks).
pub(crate) const EDITOR_PADDING: f32 = 5.0;

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
            after: None,
        })
    }
}

/// The buffer text: a shaped iced `Content` while the tab is visible in Source/Split, else the
/// plain text plus the saved cursor/selection (plan editor-cutover-blockers WP-2 — a shaped
/// `Content` costs ~22.5 MiB per tab). Switching never touches text, history or save state.
#[derive(Debug)]
enum Repr {
    Shaped(Content),
    Plain { text: String, cursor: undo::Cursor },
}

impl Clone for Repr {
    fn clone(&self) -> Self {
        match self {
            Self::Shaped(c) => Self::Shaped(c.clone()),
            Self::Plain { text, cursor } => Self::Plain {
                text: text.clone(),
                cursor: *cursor,
            },
        }
    }
}

/// The frontmatter-title cache: `((edit_seq, epoch), title)` of the last parse.
type TitleCache = std::cell::RefCell<Option<((u64, u64), Option<String>)>>;

/// One open note's source buffer.
#[derive(Debug)]
pub struct SourceBuffer {
    path: String,
    content: Repr,
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
    /// The selection last written to PRIMARY (not re-written while unchanged).
    primary: Option<String>,
    /// The find bar's current match ((line, byte column) from..to), painted by the highlighter
    /// while the find input holds focus (plan editor-feedback-round-2 WP-4).
    find_hl: Option<FindSpan>,
    /// plan editor-ui-round-3 WP-5: every find match, painted light.
    find_all: std::sync::Arc<[FindSpan]>,
    /// plan editor-data-safety WP-1 (audit E1): a reconnect arrived while a save on the old
    /// connection was in flight; resync once its completion has been processed.
    resync_pending: bool,
    /// plan editor-ui-round-3 WP-6: the top visible source line, tracked from the wheel's
    /// `Action::Scroll` and our own [`Self::scroll_to_line`] (iced 0.14's `text_editor` exposes
    /// no scroll offset); pulled up when the cursor moves above it. 0 for a fresh shaped buffer.
    top_line: usize,
    /// Plan round-7 WP-4: an edge scroll (`true` = the end) waiting for the editor's next
    /// layout. A scroll performed before the content is laid out at its size is lost (cosmic
    /// clamps an unsized buffer's scroll to the top), so it is applied on the sensor report.
    pending_edge: Option<bool>,
    /// Plan round-7 WP-4 (attempt 2): a Reading → Source line carry waiting for layout — the
    /// target source line, and whether pass 1 (caret to the document end) has run. Pass 2
    /// moves the caret to the line's start, so cosmic scrolls back up with it at the top.
    pending_line: Option<(usize, bool)>,
    /// Plan round-8-intake-views-anchor WP-4: the `scroll_anchor` fraction of the pending line
    /// carry (0 = the line lands at the top; `pending_line` then holds the unclamped target).
    pending_anchor: f32,
    /// Plan round-8 WP-4: pass 3 of an anchored carry — after pass 2's layout put the line at
    /// the top, scroll up this many visual rows (the caret-follow of that layout would
    /// otherwise undo a scroll performed with the caret move).
    pending_shift: Option<usize>,
    /// Plan round-5 WP-1: the source lines the editor's viewport shows, measured from the laid
    /// out pane height (a `sensor` around the editor stack; [`viewport_lines`]); falls back to
    /// [`VISIBLE_LINES`] before the first measure.
    visible_lines: usize,
    /// Plan usability-sprint-captures-links WP-5: the frontmatter `title` of the text last known
    /// on disk (`new`/`reload`/landed saves).
    disk_title: Option<String>,
    /// WP-5: the title the current filename was derived from — the pre-change disk title, kept
    /// until a title rename happens (fresh buffer) or the title stops matching the filename.
    name_title: Option<String>,
    /// WP-5: the title of the text in the in-flight save.
    pending_title: Option<String>,
    /// WP-5 (audit E2): a title rename of this note is in flight; saves are held (queued as
    /// `save_again`) so the old path is never written or recreated.
    renaming: bool,
    /// WP-R15 (SP2): the frontmatter `title` of the current text, keyed by `(edit_seq, epoch)`
    /// (an external reload does not bump `edit_seq`) — the `tab_title = "title"` source,
    /// recomputed only when the buffer changed, so drawing the tab bar never re-parses an
    /// unchanged note (plan editor-feedback-round-2 WP-6).
    title_cache: TitleCache,
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
            primary: self.primary.clone(),
            find_hl: self.find_hl,
            find_all: self.find_all.clone(),
            resync_pending: self.resync_pending,
            top_line: self.top_line,
            pending_edge: self.pending_edge,
            pending_line: self.pending_line,
            pending_anchor: self.pending_anchor,
            pending_shift: self.pending_shift,
            visible_lines: self.visible_lines,
            disk_title: self.disk_title.clone(),
            name_title: self.name_title.clone(),
            pending_title: self.pending_title.clone(),
            renaming: self.renaming,
            title_cache: self.title_cache.clone(),
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
        let text = String::from_utf8_lossy(bytes).into_owned();
        let title = crate::header::frontmatter_title(&text);
        Self {
            path: path.to_owned(),
            content: Repr::Plain {
                text,
                cursor: undo::Cursor::default(),
            },
            base_hash: hash,
            history: UndoHistory::new(cap),
            dirty: false,
            edit_seq: 0,
            saving: false,
            pending_hash: None,
            save_again: false,
            conflict: None,
            epoch: 0,
            primary: None,
            find_hl: None,
            find_all: std::sync::Arc::from([]),
            resync_pending: false,
            top_line: 0,
            pending_edge: None,
            pending_line: None,
            pending_anchor: 0.0,
            pending_shift: None,
            visible_lines: VISIBLE_LINES,
            disk_title: title.clone(),
            name_title: title,
            pending_title: None,
            renaming: false,
            title_cache: std::cell::RefCell::default(),
        }
    }

    /// The vault-relative path.
    pub fn path(&self) -> &str {
        &self.path
    }
    /// The full text.
    pub fn text(&self) -> String {
        match &self.content {
            Repr::Shaped(c) => c.text(),
            Repr::Plain { text, .. } => text.clone(),
        }
    }
    /// The shaped iced buffer exists (the tab is visible in Source/Split).
    pub fn is_shaped(&self) -> bool {
        matches!(self.content, Repr::Shaped(_))
    }
    /// Builds the shaped buffer from the plain text, restoring cursor and selection.
    pub fn materialise(&mut self) {
        let _ = self.shaped();
    }
    /// Drops the shaped buffer, keeping the text and cursor/selection.
    pub fn dematerialise(&mut self) {
        if let Repr::Shaped(c) = &self.content {
            self.content = Repr::Plain {
                text: c.text(),
                cursor: to_model(c.cursor()),
            };
        }
    }
    /// Materialises iff `visible` (no-op when already in that state).
    pub fn set_shaped(&mut self, visible: bool) {
        if visible {
            self.materialise();
        } else {
            self.dematerialise();
        }
    }
    /// Plan round-7 WP-4 test hook: the shaped editor content (measuring the laid-out view).
    #[cfg(test)]
    pub(crate) fn content_mut(&mut self) -> &mut Content {
        self.shaped()
    }

    fn shaped(&mut self) -> &mut Content {
        if let Repr::Plain { text, cursor } = &self.content {
            let mut c = Content::with_text(text);
            // Plan round-5 WP-1: a fresh shaped buffer starts scrolled to the top; re-apply the
            // tracked top line (kept across dematerialise — tab blur/resume keeps its place).
            fresh_view(&mut c, to_iced(*cursor), self.top_line);
            self.content = Repr::Shaped(c);
        }
        match &mut self.content {
            Repr::Shaped(c) => c,
            Repr::Plain { .. } => unreachable!("materialised above"),
        }
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

    /// plan editor-data-safety WP-2: the conflict is "the file is gone" (the disk hash is the
    /// empty hash — a save CAS conflict against no file, or a NotFound read under edits).
    pub fn is_missing(&self) -> bool {
        self.conflict
            .as_ref()
            .is_some_and(|h| h.as_str().is_empty())
    }

    /// A dirty buffer whose file is gone enters the missing-file state (no-op when clean).
    pub fn mark_missing(&mut self) {
        if self.dirty {
            self.conflict = Some(ContentHash::from_hex(""));
        }
    }
    /// The edit counter.
    pub fn edit_seq(&self) -> u64 {
        self.edit_seq
    }
    /// The wholesale-replacement counter (bumped by [`SourceBuffer::reload`]).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// The text's frontmatter `title` (cached per edit seq and reload epoch).
    pub fn frontmatter_title(&self) -> Option<String> {
        let key = (self.edit_seq, self.epoch);
        let mut cache = self.title_cache.borrow_mut();
        if let Some((k, t)) = cache.as_ref()
            && *k == key
        {
            return t.clone();
        }
        let t = crate::header::frontmatter_title(&self.text());
        *cache = Some((key, t.clone()));
        t
    }
    /// The undo history (read-only).
    pub fn history(&self) -> &UndoHistory {
        &self.history
    }
    /// Source line `line`'s text without its newline (plan round-5 WP-9; one line, no full
    /// buffer copy when shaped).
    pub fn line_text(&self, line: usize) -> Option<String> {
        match &self.content {
            Repr::Shaped(c) => c.line(line).map(|l| l.text.to_string()),
            Repr::Plain { text, .. } => text.split('\n').nth(line).map(str::to_owned),
        }
    }
    /// The iced cursor.
    pub fn cursor(&self) -> text_editor::Cursor {
        match &self.content {
            Repr::Shaped(c) => c.cursor(),
            Repr::Plain { cursor, .. } => to_iced(*cursor),
        }
    }
    /// The selected text, if any (WP-5 action context).
    pub fn selection(&self) -> Option<String> {
        match &self.content {
            Repr::Shaped(c) => c.selection(),
            Repr::Plain { text, cursor } => {
                let anchor = cursor.anchor?;
                let a = undo::offset_of(text, anchor)?;
                let b = undo::offset_of(text, cursor.pos)?;
                text.get(a.min(b)..a.max(b)).map(str::to_owned)
            }
        }
    }
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
    fn track_top(&mut self, scroll: Option<i32>, before: usize) {
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

    /// Plan round-5 WP-1: the measured viewport line count ([`Self::visible_lines`]).
    pub fn visible_lines(&self) -> usize {
        self.visible_lines
    }

    /// Plan round-5 WP-1: records the viewport's measured line count (at least 1).
    /// Plan round-7 WP-4: returns `true` when a pending line carry needs one more layout pass
    /// — the caller re-sends the report after the next layout
    /// ([`crate::reading::ScrollIntent::SourceRelayout`]).
    pub fn set_visible_lines(&mut self, lines: usize) -> bool {
        self.visible_lines = lines.max(1);
        // Plan round-7 WP-4: the editor is laid out now; a pending edge scroll lands exactly
        // (and wins over a line carry).
        if let Some(bottom) = self.pending_edge.take() {
            self.pending_line = None;
            self.pending_anchor = 0.0;
            self.pending_shift = None;
            let lines = if bottom { 1_000_000 } else { -1_000_000 };
            self.shaped().perform(text_editor::Action::Scroll { lines });
            return false;
        }
        match self.pending_line {
            // Pass 1: the caret at the end scrolls the editor to the end on the next layout.
            Some((line, false)) => {
                let text = self.text();
                let end = text_editor::Position {
                    line: text.matches('\n').count(),
                    column: text.rsplit('\n').next().map_or(0, str::len),
                };
                self.move_to(text_editor::Cursor {
                    position: end,
                    selection: None,
                });
                self.pending_line = Some((line, true));
                true
            }
            // Pass 2: the caret moving up to the line scrolls it to the viewport top.
            Some((line, true)) => {
                self.pending_line = None;
                self.move_to(text_editor::Cursor {
                    position: text_editor::Position { line, column: 0 },
                    selection: None,
                });
                // Plan round-8 WP-4 (audit M2): the line sits at the top (or the view at the
                // end when it is past the last top); move it down to the anchor height by
                // scrolling up `rows` visual rows from the unclamped target, clamped once here.
                let max = self.max_top();
                let at = line.min(max);
                let target = line.saturating_sub(self.anchor_rows()).min(max);
                self.pending_anchor = 0.0;
                self.top_line = target;
                if target == at {
                    return false;
                }
                // Pass 3, after this layout has scrolled the caret line to the top.
                self.pending_shift = Some(at - target);
                true
            }
            None => {
                if let Some(up) = self.pending_shift.take() {
                    let up = i32::try_from(up).unwrap_or(i32::MAX);
                    self.shaped()
                        .perform(text_editor::Action::Scroll { lines: -up });
                }
                false
            }
        }
    }

    /// Plan round-7 WP-4: the source line at the laid-out editor's viewport top, measured by
    /// hit-testing the viewport's top-left corner (exact with wrapped lines, unlike the
    /// [`Self::top_line`] estimate); the cursor and selection are put back. Records it as the
    /// tracked top. Only meaningful while the editor is shown.
    ///
    /// Plan round-7 WP-4 (attempt 2): restoring a caret that is off-screen (the view was
    /// wheel-scrolled away from it) would make the next layout scroll back to it, moving the
    /// still-shown view (Source → Split). Then the caret stays at the viewport's top-left.
    pub fn measure_top_line(&mut self) -> usize {
        self.measure_anchor_line(0.0, 0.0)
    }

    /// Plan round-8-intake-views-anchor WP-4: [`Self::measure_top_line`], also hit-testing
    /// the row at `anchor` (fraction) of the viewport height (`visible_lines` rows at
    /// `font_size`); returns the source line there (the top line at anchor 0). The tracked
    /// top and the caret restore are unchanged.
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
            c.perform(text_editor::Action::Click(iced::Point::ORIGIN));
            let top = c.cursor().position;
            if dy > 0.0 {
                c.perform(text_editor::Action::Click(iced::Point::new(0.0, dy)));
                anchored = Some(c.cursor().position.line);
            }
            // Past the last visible row: the hit clamps to the viewport's last position.
            c.perform(text_editor::Action::Click(iced::Point::new(1.0e6, 1.0e6)));
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

    /// Plan round-8 WP-4: the visual rows between the viewport top and the anchor height of
    /// the pending carry (at most one less than the viewport, so the caret stays visible).
    fn anchor_rows(&self) -> usize {
        rows_at(self.pending_anchor, self.visible_lines)
    }

    /// Plan round-5 WP-1: the largest top line — the editor never scrolls past the last line
    /// at the viewport's bottom, so a top nearer the end than one viewport is unreachable.
    fn max_top(&self) -> usize {
        let lines = match &self.content {
            Repr::Shaped(c) => c.line_count(),
            Repr::Plain { text, .. } => text.matches('\n').count() + 1,
        };
        lines.saturating_sub(self.visible_lines)
    }

    /// plan editor-ui-round-3 WP-6: scrolls the editor so source `line` is at the top
    /// (materialising it): scroll fully up, then down by `line` lines. Wrapped lines above
    /// `line` count once each, so a wrapped paragraph above it lands the view slightly early.
    /// Plan round-5 WP-1: clamped to the last reachable top ([`Self::max_top`]), so a carry
    /// to a block near the end shows the document's last viewport, never a past-the-end view.
    pub fn scroll_to_line(&mut self, line: usize) {
        self.scroll_to_line_anchored(line, 0.0);
    }

    /// Plan round-8-intake-views-anchor WP-4: [`Self::scroll_to_line`] with source `line` at
    /// `anchor` (fraction) of the viewport height instead of the top. Anchor 0 is exactly
    /// `scroll_to_line`; otherwise the deferred carry keeps the **unclamped** line, and the
    /// anchor shift + viewport clamp happen once in pass 2 (audit M2).
    pub fn scroll_to_line_anchored(&mut self, line: usize, anchor: f32) {
        self.pending_anchor = if anchor.is_finite() {
            anchor.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let target = line.saturating_sub(self.anchor_rows()).min(self.max_top());
        // Plan round-6 WP-1: one delta from the tracked (pending) top — `shaped()` of a plain
        // buffer has already queued the scroll to `top_line`.
        let from = self.top_line;
        scroll_content(self.shaped(), from, target);
        self.top_line = target;
        // Plan round-7 WP-4 (attempt 2): the scroll above is lost when the editor is not laid
        // out yet (a Reading → Source switch); the two-pass caret carry places it exactly.
        self.pending_edge = None;
        self.pending_shift = None;
        let carried = if self.pending_anchor > 0.0 {
            line
        } else {
            target
        };
        self.pending_line = Some((carried, false));
    }

    /// Plan round-6 WP-1: scrolls to the document's start / end without moving the cursor (a
    /// mode switch after Ctrl+PgUp/PgDn). The end overshoots by the line count: shaping clamps
    /// the view to the last full viewport, wrapped lines included.
    pub fn scroll_to_edge(&mut self, bottom: bool) {
        // Plan round-7 WP-4: deferred to the next layout ([`Self::set_visible_lines`]).
        self.materialise();
        self.pending_edge = Some(bottom);
        self.pending_line = None;
        self.top_line = if bottom { self.max_top() } else { 0 };
    }

    /// plan editor-ui-round-3 WP-5: moves the cursor to the buffer's start (`false`) or end
    /// (`true`), dropping the selection; materialises a plain buffer so the editor scrolls.
    pub fn go_to_edge(&mut self, bottom: bool) {
        let text = self.text();
        let at = if bottom {
            let line = text.matches('\n').count();
            let column = text.rsplit('\n').next().map_or(0, str::len);
            text_editor::Position { line, column }
        } else {
            text_editor::Position { line: 0, column: 0 }
        };
        self.materialise();
        // Plan round-7 WP-4: a caret already at the edge moves nothing, so the editor would
        // not scroll (after a wheel scroll away); scroll explicitly then.
        let still = self.cursor().position == at && self.cursor().selection.is_none();
        self.move_to(text_editor::Cursor {
            position: at,
            selection: None,
        });
        if still {
            let lines = if bottom { 1_000_000 } else { -1_000_000 };
            self.shaped().perform(text_editor::Action::Scroll { lines });
        }
        // plan editor-round-4 WP-2: the editor scrolls to the edge; place-keeping follows.
        self.top_line = if bottom {
            at.line.saturating_sub(self.visible_lines - 1)
        } else {
            0
        };
    }

    /// The painted find match, if any.
    pub fn find_highlight(&self) -> Option<FindSpan> {
        self.find_hl
    }

    /// Sets (or clears) the painted find match; never touches the text.
    pub fn set_find_highlight(&mut self, span: Option<FindSpan>) {
        self.find_hl = span;
    }

    /// Every painted find match (plan editor-ui-round-3 WP-5).
    pub fn find_matches(&self) -> &[FindSpan] {
        &self.find_all
    }

    /// Sets (or clears, with an empty list) every painted find match; never touches the text.
    pub fn set_find_matches(&mut self, all: Vec<FindSpan>) {
        self.find_all = all.into();
    }

    /// Tab / Shift+Tab (plan editor-feedback-round-2 WP-4) as ONE recorded edit. Indent with a
    /// selection (any, incl. within one line) or with only indentation + a list marker before the
    /// cursor prepends `\t` to every touched line; otherwise Tab types `\t` (round-4 WP-2). Outdent removes one leading `\t`, else up
    /// to [`OUTDENT_SPACES`] leading spaces, from every touched line (the cursor's line without
    /// a selection). A selection ending at column 0 of a later line does not touch that line.
    /// The selection keeps covering the same lines (columns shift by the bytes changed).
    pub fn indent(&mut self, outdent: bool, read_only: bool, now: Instant) -> bool {
        if read_only {
            return false;
        }
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
            return self.perform(
                Action::Edit(Edit::Paste(Arc::new("\t".to_owned()))),
                false,
                now,
            );
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

    /// Moves the cursor (navigation; not an edit).
    pub fn move_to(&mut self, c: text_editor::Cursor) {
        match &mut self.content {
            Repr::Shaped(content) => place(content, c),
            Repr::Plain { cursor, .. } => *cursor = to_model(c),
        }
    }

    /// Applies an iced action. Editing actions are recorded in the undo history (and dropped
    /// when `read_only`). Returns whether the text changed.
    pub fn perform(&mut self, action: Action, read_only: bool, now: Instant) -> bool {
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
        if read_only {
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
    pub fn cut(&mut self, read_only: bool, now: Instant) -> (Option<String>, bool) {
        if let Some(sel) = self.selection().filter(|s| !s.is_empty()) {
            let changed = self.perform(Action::Edit(Edit::Delete), read_only, now);
            return (Some(sel), changed);
        }
        if read_only {
            return (None, false);
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
    pub fn enter(&mut self, read_only: bool, now: Instant) -> bool {
        if read_only {
            return false;
        }
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
    pub fn delete_word(&mut self, forward: bool, read_only: bool, now: Instant) -> bool {
        if read_only {
            return false;
        }
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
        read_only: bool,
        now: Instant,
    ) -> bool {
        if read_only {
            return false;
        }
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

    /// Toggles the task checkbox at source `line` (see `editor_model::render::task_toggle`).
    pub fn toggle_task(&mut self, line: usize, read_only: bool, now: Instant) -> bool {
        let Some((cols, next)) = editor_model::render::task_toggle(&self.text(), line) else {
            return false;
        };
        self.replace_in_line(line, cols, &next.to_string(), read_only, now)
    }

    /// plan usability-sprint-captures-links WP-4: flips top-level frontmatter boolean `key`
    /// as one edit that rewrites only its scalar (see
    /// `editor_model::render::frontmatter_bool_edit`). Plan round-8-intake-views-anchor WP-3:
    /// upserts — a null / missing key or a frontmatter-less note becomes `key: true`.
    pub fn toggle_frontmatter_bool(&mut self, key: &str, read_only: bool, now: Instant) -> bool {
        self.set_frontmatter_bool(key, None, read_only, now)
    }

    /// Plan round-8-intake-views-anchor WP-3: sets frontmatter boolean `key` to `to` (`None` =
    /// toggle) as one undoable edit (see `editor_model::render::frontmatter_bool_edit`).
    pub fn set_frontmatter_bool(
        &mut self,
        key: &str,
        to: Option<bool>,
        read_only: bool,
        now: Instant,
    ) -> bool {
        let text = self.text();
        let Some((range, with)) = editor_model::render::frontmatter_bool_edit(&text, key, to)
        else {
            return false;
        };
        // The splice is one line's scalar (or an inserted line): line + byte columns.
        let head = &text[..range.start];
        let line = head.matches('\n').count();
        let col = range.start - head.rfind('\n').map_or(0, |i| i + 1);
        self.replace_in_line(line, col..col + range.len(), &with, read_only, now)
    }

    /// Sets the task status at source `line` to `to` as one edit (plan editor-ui-round-3 WP-4;
    /// see `editor_model::render::task_set_status`).
    pub fn set_task_status(
        &mut self,
        line: usize,
        to: char,
        read_only: bool,
        now: Instant,
    ) -> bool {
        let Some((cols, next)) = editor_model::render::task_set_status(&self.text(), line, to)
        else {
            return false;
        };
        self.replace_in_line(line, cols, &next.to_string(), read_only, now)
    }

    /// Pastes `text` at the cursor (replacing any selection) as one edit.
    pub fn paste(&mut self, text: String, read_only: bool, now: Instant) -> bool {
        self.perform(Action::Edit(Edit::Paste(Arc::new(text))), read_only, now)
    }

    /// The selection if it is non-empty and differs from what PRIMARY last received; records it.
    fn take_new_primary(&mut self) -> Option<String> {
        if !self.is_shaped() {
            return None;
        }
        let sel = self.selection().filter(|s| !s.is_empty())?;
        if self.primary.as_deref() == Some(sel.as_str()) {
            return None;
        }
        self.primary = Some(sel.clone());
        Some(sel)
    }

    fn mark_edited(&mut self) {
        self.find_hl = None;
        self.find_all = std::sync::Arc::from([]);
        self.dirty = true;
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

    /// Replaces the text with what is on disk, keeping the cursor line (clamped);
    /// history is reset, the buffer becomes clean, and any conflict clears.
    pub fn reload(&mut self, bytes: &[u8], hash: ContentHash) {
        let old = self.cursor().position;
        let text = String::from_utf8_lossy(bytes).into_owned();
        let line_count = text.split('\n').count();
        let line = old.line.min(line_count.saturating_sub(1));
        let line_text = text
            .split('\n')
            .nth(line)
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .unwrap_or_default();
        let mut column = old.column.min(line_text.len());
        while !line_text.is_char_boundary(column) {
            column -= 1;
        }
        let cursor = undo::Cursor {
            pos: undo::Pos::new(line, column),
            anchor: None,
        };
        let shaped = self.is_shaped();
        self.disk_title = crate::header::frontmatter_title(&text);
        self.name_title.clone_from(&self.disk_title);
        self.content = Repr::Plain { text, cursor };
        if shaped {
            self.materialise();
        }
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
        if self.saving || self.renaming {
            self.save_again = true;
            return None;
        }
        let text = self.text();
        self.pending_title = crate::header::frontmatter_title(&text);
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
        let title = self.pending_title.take();
        match outcome {
            SaveOutcome::Saved(h) => {
                self.base_hash = h;
                self.disk_title = title;
                if self.title_rename().is_none() {
                    self.name_title.clone_from(&self.disk_title);
                }
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
                    // WP-2 (audit E2a): deleted / moved away under edits → missing-file state.
                    Err(DaemonError::NotFound(_)) => ContentHash::from_hex(""),
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

    /// WP-5: the title-sync rename target for the text on disk (the disk title changed from the
    /// one the filename was derived from), per [`crate::title_sync::title_sync_target`].
    pub fn title_rename(&self) -> Option<String> {
        crate::title_sync::title_sync_target(
            &self.path,
            self.name_title.as_deref(),
            self.disk_title.as_deref(),
        )
    }

    /// WP-5: a title rename is in flight.
    pub fn is_renaming(&self) -> bool {
        self.renaming
    }

    /// WP-5: marks a title rename in flight (saves are held) or finished (returns whether a held
    /// save is waiting).
    pub fn set_renaming(&mut self, on: bool) -> bool {
        self.renaming = on;
        !on && self.dirty && self.save_again
    }

    /// WP-5: stop syncing the current title change (rename refused or failed): the filename is
    /// no longer derived from a pending title.
    pub fn settle_title(&mut self) {
        self.name_title.clone_from(&self.disk_title);
    }

    /// The note moved to `to` (whose on-disk hash is `disk`) while this buffer had edits the move
    /// did not carry: follow the path, keep the text, and raise the conflict bar (Keep mine
    /// saves it at `to`; Take theirs discards it). editor-usability-round WP-5, audit M2.
    pub fn follow_move(&mut self, to: &str, disk: ContentHash) {
        to.clone_into(&mut self.path);
        self.dirty = true;
        self.conflict = Some(disk);
    }

    /// Plan round-7-feedback-decisions WP-2: a clean buffer whose file was renamed to `to` with
    /// unchanged bytes is re-keyed in place, so cursor, selection, undo history and `top_line`
    /// survive. Unlike [`Self::follow_move`] it neither dirties nor raises a conflict.
    pub fn rename_clean(&mut self, to: &str) {
        to.clone_into(&mut self.path);
    }

    /// Keep mine: re-save against the on-disk hash.
    pub fn keep_mine(&mut self, read_only: bool) -> Option<SaveJob> {
        let current = self.conflict.take()?;
        self.base_hash = current;
        self.begin_save(read_only)
    }

    /// plan editor-data-safety WP-1 (audit E1): mark a reconnect resync as waiting on the
    /// in-flight save's completion.
    pub fn defer_resync(&mut self) {
        self.resync_pending = true;
    }

    /// Takes the deferred-resync mark (set while a save was in flight across a reconnect).
    pub fn take_resync_pending(&mut self) -> bool {
        std::mem::take(&mut self.resync_pending)
    }

    /// Reconnect resync against the file now on disk (`bytes`, `disk`). Clean → reload when
    /// the disk differs. Dirty → if the disk already holds this exact text (an old save landed
    /// but its reply was lost) adopt it as clean; else conflict bar when the disk differs from
    /// the base; else a fresh save (any queued `save_again` is subsumed: the save carries the
    /// latest text). Never call while a save is in flight.
    pub fn resync(&mut self, bytes: &[u8], disk: ContentHash, read_only: bool) -> Option<SaveJob> {
        if !self.dirty {
            if disk != self.base_hash {
                self.reload(bytes, disk);
            }
            return None;
        }
        if ContentHash::of(self.text().as_bytes()) == disk {
            self.base_hash = disk;
            self.dirty = false;
            self.conflict = None;
            self.save_again = false;
            return None;
        }
        if self.conflict.is_some() || disk != self.base_hash {
            self.conflict = Some(disk);
            return None;
        }
        self.begin_save(read_only)
    }
}

/// `Content::move_to` that also clears a stale selection: iced's `move_to` leaves the old
/// selection in place when `c.selection` is `None` (WP-4); a motion over an empty selection
/// collapses it without moving the cursor.
fn place(content: &mut impl EditorOps, c: text_editor::Cursor) {
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

/// Shift+Tab removes up to this many leading spaces when the line does not start with `\t`.
pub const OUTDENT_SPACES: usize = 4;

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

/// What [`update`] asks the app to do (WP-R15 SP5: the one effect shape `App` drains, as
/// `views::Effect`).
pub enum Effect {
    /// Run this task (saves, clipboard, timers, follow-up messages).
    Task(Task<Message>),
    /// An error notice for the status bar.
    Error(String),
}

/// Plan editor-feedback-round-2 WP-3: flush `buf` now (tab switch away, window unfocus) through
/// the one save machine — `begin_save` is a no-op for clean, read-only and conflicted
/// buffers and re-queues (`save_again`) behind an in-flight save.
pub(crate) fn flush(
    buf: &mut SourceBuffer,
    daemon: Option<&Arc<dyn DaemonClient>>,
    read_only: bool,
) -> Task<Message> {
    if daemon.is_none() {
        return Task::none();
    }
    spawn_save(daemon, buf.begin_save(read_only))
}

pub(crate) fn spawn_save(
    daemon: Option<&Arc<dyn DaemonClient>>,
    job: Option<SaveJob>,
) -> Task<Message> {
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

pub(crate) fn autosave_timer(path: String, seq: u64) -> Task<Message> {
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

/// Handles a [`SourceMsg`] for `buf`, the document [`SourceMsg::target`] names (one
/// document per path, plan editor-feedback-round-2 WP-7).
pub fn update(
    buf: &mut SourceBuffer,
    msg: SourceMsg,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Vec<Effect> {
    // Shadow mode (the only read-only source) was retired in plan tn-review-baseline-remediation
    // WP-R6; the buffer API keeps its `read_only` gate.
    let read_only = false;
    let mut fx = Vec::new();
    let edited = |buf: &mut SourceBuffer, changed: bool| {
        let primary = buf
            .take_new_primary()
            .map_or_else(Task::none, clipboard::write_primary);
        if changed {
            Task::batch([primary, autosave_timer(buf.path.clone(), buf.edit_seq)])
        } else {
            primary
        }
    };
    let task = match msg {
        SourceMsg::Action(a) => {
            let changed = buf.perform(a, read_only, Instant::now());
            edited(buf, changed)
        }
        SourceMsg::Key(k) => {
            let now = Instant::now();
            let changed = match k {
                SourceKey::Enter => buf.enter(read_only, now),
                SourceKey::DeleteWordBack => buf.delete_word(false, read_only, now),
                SourceKey::DeleteWordForward => buf.delete_word(true, read_only, now),
                SourceKey::Home { select } => {
                    buf.smart_home_end(false, select);
                    false
                }
                SourceKey::End { select } => {
                    buf.smart_home_end(true, select);
                    false
                }
                SourceKey::Indent => buf.indent(false, read_only, now),
                SourceKey::Outdent => buf.indent(true, read_only, now),
            };
            edited(buf, changed)
        }
        SourceMsg::MiddleClick { path, at: p } => {
            buf.perform(
                Action::Click(Point::new(p.x - EDITOR_PADDING, p.y - EDITOR_PADDING)),
                read_only,
                Instant::now(),
            );
            clipboard::read_primary().map(move |text| {
                Message::Source(SourceMsg::PastePrimary {
                    path: path.clone(),
                    text,
                })
            })
        }
        SourceMsg::CommandClick { path, at: p } => {
            buf.perform(
                Action::Click(Point::new(p.x - EDITOR_PADDING, p.y - EDITOR_PADDING)),
                read_only,
                Instant::now(),
            );
            let pos = buf.cursor().position;
            let line = buf.shaped().line(pos.line).map(|l| l.text.into_owned());
            match line.and_then(|l| command_click_msg(&path, &l, pos.column)) {
                Some(m) => Task::done(m),
                None => Task::none(),
            }
        }
        SourceMsg::PastePrimary { text, .. } => {
            let changed =
                text.is_some_and(|t| !t.is_empty() && buf.paste(t, read_only, Instant::now()));
            edited(buf, changed)
        }
        SourceMsg::Cut => {
            let (clip, changed) = buf.cut(read_only, Instant::now());
            let write = clip.map_or_else(Task::none, clipboard::write);
            Task::batch([write, edited(buf, changed)])
        }
        SourceMsg::Paste => {
            let path = buf.path.clone();
            let daemon = daemon.map(Arc::clone);
            let p = path.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        crate::clipboard_image::resolve(
                            &crate::clipboard_image::Xclip,
                            daemon.as_deref(),
                            &p,
                        )
                    })
                    .await
                    .unwrap_or_else(|e| {
                        crate::clipboard_image::PasteOutcome::Text {
                            notice: Some(format!("image paste failed: {e}")),
                        }
                    })
                },
                move |outcome| {
                    Message::Source(SourceMsg::PasteResolved {
                        path: path.clone(),
                        outcome,
                    })
                },
            )
        }
        SourceMsg::PasteResolved { path, outcome } => {
            use crate::clipboard_image::PasteOutcome;
            match outcome {
                PasteOutcome::Insert(s) => {
                    let changed = buf.paste(s, read_only, Instant::now());
                    edited(buf, changed)
                }
                PasteOutcome::Failed(e) => {
                    fx.push(Effect::Error(e));
                    Task::none()
                }
                PasteOutcome::Text { notice } => {
                    fx.extend(notice.map(Effect::Error));
                    clipboard::read().map(move |text| {
                        Message::Source(SourceMsg::PastePrimary {
                            path: path.clone(),
                            text,
                        })
                    })
                }
            }
        }
        SourceMsg::Undo | SourceMsg::Redo => {
            let changed = !read_only
                && if matches!(msg, SourceMsg::Undo) {
                    buf.undo()
                } else {
                    buf.redo()
                };
            if changed {
                autosave_timer(buf.path.clone(), buf.edit_seq)
            } else {
                Task::none()
            }
        }
        SourceMsg::Save => spawn_save(daemon, buf.begin_save(read_only)),
        SourceMsg::AutosaveDue { seq, .. } => {
            if seq == buf.edit_seq {
                spawn_save(daemon, buf.begin_save(read_only))
            } else {
                Task::none()
            }
        }
        SourceMsg::KeepMine(_) => spawn_save(daemon, buf.keep_mine(read_only)),
        // Routed by the app (`App::update_inner`); never reaches here.
        SourceMsg::Viewport { .. }
        | SourceMsg::Saved { .. }
        | SourceMsg::LinkKey(_)
        | SourceMsg::LinkPick(_) => Task::none(),
        SourceMsg::TakeTheirs(path) => {
            if let Some(d) = daemon {
                match d.read_file(&path) {
                    Ok((bytes, h)) => buf.reload(&bytes, h),
                    Err(e) => fx.push(Effect::Error(format!("reload failed: {e}"))),
                }
            }
            Task::none()
        }
    };
    fx.push(Effect::Task(task));
    fx
}

/// plan editor-round-4 WP-2: iced maps a single click to `Action::Click` whatever the
/// modifiers; with Shift held it becomes a drag to the click point, which extends the
/// selection from its anchor (or the cursor when there is none).
pub fn shift_click(msg: SourceMsg, shift: bool) -> SourceMsg {
    match msg {
        SourceMsg::Action(Action::Click(p)) if shift => SourceMsg::Action(Action::Drag(p)),
        m => m,
    }
}

/// Plan round-8-intake-views-anchor WP-4: the visual rows from the viewport top to the anchor
/// height `anchor` (fraction) of a `visible`-row viewport — rounded, and at most one less than
/// the viewport so a caret on that row stays visible.
pub fn rows_at(anchor: f32, visible: usize) -> usize {
    let a = if anchor.is_finite() {
        anchor.clamp(0.0, 1.0)
    } else {
        0.0
    };
    #[allow(clippy::cast_precision_loss)]
    let rows = (a * visible as f32).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rows = rows.max(0.0) as usize;
    rows.min(visible.saturating_sub(1))
}

/// plan editor-round-4 WP-2: the visible source line count `top_line` tracking assumes before
/// the editor's viewport was first measured (plan round-5 WP-1: then [`viewport_lines`]).
pub const VISIBLE_LINES: usize = 30;

/// Plan round-5 WP-1: the source lines a laid-out editor `height` (px) shows at text size
/// `font_size` — iced's default relative line height (1.3), minus the editor padding.
pub fn viewport_lines(height: f32, font_size: f32) -> usize {
    let line = (font_size * 1.3).max(1.0);
    let lines = ((height - 2.0 * EDITOR_PADDING).max(0.0) / line).floor();
    // Truncation is the intent: a partly visible last line does not count.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = lines as usize;
    n.max(1)
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

/// Plan round-6 WP-1 (spike `docs/spikes/source-scroll/FINDINGS.md`): the single scroll that
/// moves a view whose (pending) top source line is `from` to `to`. cosmic-text =0.15.0 sums
/// every `Scroll` into one pixel offset that is only resolved at the next shaping (where a
/// net-negative offset clamps to line 0), so the old "fully up, then down `line`" pair landed
/// at the top; one relative delta from the tracked top lands on `to` (wrapped lines above
/// count once each). `None` when already there.
pub(crate) fn scroll_action(from: usize, to: usize) -> Option<Action> {
    let delta = i64::try_from(to).unwrap_or(i64::MAX) - i64::try_from(from).unwrap_or(i64::MAX);
    let lines = i32::try_from(delta).unwrap_or(if delta < 0 { i32::MIN } else { i32::MAX });
    (lines != 0).then_some(Action::Scroll { lines })
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

/// Handles `file_changed` for any open tab on `path`.
pub fn file_changed(
    buf: &mut SourceBuffer,
    hash: Option<&str>,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Option<String> {
    let d = daemon?;
    let path = buf.path.clone();
    buf.external_change(hash, || d.read_file(&path))
}

/// Ctrl+S / Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, then iced's defaults.
fn key_binding(
    kp: KeyPress,
    km: &crate::palette::Keymap,
    link_open: bool,
) -> Option<Binding<Message>> {
    if !matches!(kp.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    // Plan round-5 WP-9: the open `[[` list takes Up/Down/Enter/Tab/Esc (never the buffer).
    if link_open
        && kp.modifiers.is_empty()
        && let keyboard::Key::Named(named) = kp.key.as_ref()
    {
        use crate::message::LinkKey;
        let k = match named {
            key::Named::ArrowUp => Some(LinkKey::Up),
            key::Named::ArrowDown => Some(LinkKey::Down),
            key::Named::Enter => Some(LinkKey::Enter),
            key::Named::Tab => Some(LinkKey::Tab),
            key::Named::Escape => Some(LinkKey::Escape),
            _ => None,
        };
        if let Some(k) = k {
            return Some(Binding::Custom(Message::Source(SourceMsg::LinkKey(k))));
        }
    }
    if let keyboard::Key::Named(named) = kp.key.as_ref() {
        let m = kp.modifiers;
        let plain = !m.alt() && !m.logo();
        // editor-usability-round WP-3: Ctrl+Alt+Left/Right is pane back/forward, not word motion.
        if m.command() && m.alt() {
            let nav = match named {
                key::Named::ArrowLeft => Some(crate::message::PanesMsg::BackFocused),
                key::Named::ArrowRight => Some(crate::message::PanesMsg::ForwardFocused),
                _ => None,
            };
            if let Some(n) = nav {
                return Some(Binding::Custom(Message::Panes(n)));
            }
        }
        let k = match named {
            key::Named::Enter if plain && !m.command() && !m.shift() => Some(SourceKey::Enter),
            key::Named::Backspace if plain && m.command() => Some(SourceKey::DeleteWordBack),
            key::Named::Delete if plain && m.command() => Some(SourceKey::DeleteWordForward),
            key::Named::Home if plain && !m.command() => {
                Some(SourceKey::Home { select: m.shift() })
            }
            key::Named::End if plain && !m.command() => Some(SourceKey::End { select: m.shift() }),
            // plan editor-feedback-round-2 WP-4: Tab stays in the editor (indent / outdent).
            key::Named::Tab if plain && !m.command() => Some(if m.shift() {
                SourceKey::Outdent
            } else {
                SourceKey::Indent
            }),
            _ => None,
        };
        if let Some(k) = k {
            return Some(Binding::Custom(Message::Source(SourceMsg::Key(k))));
        }
    }
    if kp.modifiers.command()
        && let keyboard::Key::Character(c) = kp.key.as_ref()
    {
        let msg = match c.to_ascii_lowercase().as_str() {
            "z" if kp.modifiers.shift() => Some(SourceMsg::Redo),
            "z" => Some(SourceMsg::Undo),
            "y" => Some(SourceMsg::Redo),
            "s" => Some(SourceMsg::Save),
            // plan editor-ui-round-3 WP-5: line cut without a selection.
            "x" if !kp.modifiers.shift() && !kp.modifiers.alt() => Some(SourceMsg::Cut),
            // Plan editor-feedback-round-2 WP-9: image-aware paste (was iced's text Paste).
            "v" if !kp.modifiers.shift() && !kp.modifiers.alt() => Some(SourceMsg::Paste),
            _ => None,
        };
        if let Some(m) = msg {
            return Some(Binding::Custom(Message::Source(m)));
        }
    }
    // editor-usability-round WP-4: client chords (Ctrl+W/Q/F/P, Alt+1..8, …) reach the app even
    // while the editor is focused — `text_editor` would swallow some (Alt+digit inserts).
    // plan editor-settings-tab WP-4 (audit E1): decided from the effective keymap, so `[keys]`
    // overrides route here too (and a chord moved away no longer does).
    if (kp.modifiers.command() || kp.modifiers.alt())
        && let Some(c) = crate::palette::chord::from_event(&kp.key, kp.modifiers)
        && km.routed(&c)
    {
        return Some(Binding::Custom(Message::Palette(
            crate::message::PaletteMsg::Key(c),
        )));
    }
    Binding::from_key_press(kp)
}

/// `key_binding` for tests.
#[cfg(test)]
pub(crate) fn test_key_binding(kp: KeyPress) -> Option<Binding<Message>> {
    key_binding(kp, &crate::palette::Keymap::default(), false)
}

/// `key_binding` with the `[[` list open, for tests.
#[cfg(test)]
pub(crate) fn test_key_binding_link(kp: KeyPress) -> Option<Binding<Message>> {
    key_binding(kp, &crate::palette::Keymap::default(), true)
}

/// `key_binding` under a given keymap, for tests.
#[cfg(test)]
pub(crate) fn test_key_binding_with(
    kp: KeyPress,
    km: &crate::palette::Keymap,
) -> Option<Binding<Message>> {
    key_binding(kp, km, false)
}

/// The source editor's widget id for `path` (find bar Escape refocuses it).
pub fn editor_id(path: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("source-editor:{path}"))
}

/// Plan usability-sprint-captures-links WP-3: what a Ctrl/Cmd+click at byte `col` of `line` in
/// note `path` does — a remote URL (bare or md-link) opens in the browser, a wikilink or vault
/// md-link opens the note exactly as Reading's `OpenLink` does; elsewhere nothing.
pub fn command_click_msg(path: &str, line: &str, col: usize) -> Option<Message> {
    use editor_model::link_at::{LinkAt, link_at};
    Some(match link_at(line, col)? {
        LinkAt::Url(u) => Message::OpenUrl(u),
        LinkAt::Md(u) if editor_model::render::is_remote(&u) => Message::OpenUrl(u),
        LinkAt::Md(target) | LinkAt::Wiki(target) => {
            Message::Reading(crate::message::ReadingMsg::OpenLink {
                src: path.to_owned(),
                target,
            })
        }
    })
}

/// Renders a tab's source buffer with its conflict bar; `font_size` is the
/// `editor.toml` base text size (live, plan editor-settings-tab WP-2); `keymap` is the effective
/// keymap its key bindings route by (WP-4). `link` is the open `[[` note list overlay (plan
/// round-5 WP-9), already positioned; while it is `Some` its keys are intercepted.
pub fn view<'a>(
    buf: &'a SourceBuffer,
    tokens: Tokens,
    font_size: f32,
    keymap: &'a crate::palette::Keymap,
    link: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let link_open = link.is_some();
    let editor: Element<'a, Message> = match &buf.content {
        Repr::Shaped(content) => crate::widget::middle_click::middle_click(
            text_editor::TextEditor::new(content)
                .id(editor_id(&buf.path))
                .size(font_size)
                .height(Length::Fill)
                .padding(EDITOR_PADDING)
                .on_action(|a| Message::Source(SourceMsg::Action(a)))
                .key_binding(move |kp| key_binding(kp, keymap, link_open))
                .highlight_with::<MarkdownHighlighter>(
                    HighlightSettings {
                        colors: KindColors::from_tokens(&tokens),
                        epoch: buf.epoch,
                        find: buf.find_hl,
                        find_all: buf.find_all.clone(),
                    },
                    highlighter::to_format,
                ),
            move |at| {
                Message::Source(SourceMsg::MiddleClick {
                    path: buf.path.clone(),
                    at,
                })
            },
        )
        .on_command_left(move |at| {
            Message::Source(SourceMsg::CommandClick {
                path: buf.path.clone(),
                at,
            })
        })
        .into(),
        // Never visible in practice (the app materialises visible tabs before drawing).
        Repr::Plain { text: t, .. } => text(t.as_str()).size(font_size).into(),
    };
    // The editor's widget state (focus, cursor scroll) is matched by tree position, so it is
    // always layer 0 of a stack; the conflict slot floats in layer 1 (an empty `Space` when
    // unused). Plan editor-feedback-round-2 WP-6: it is out of the editor's vertical flow, so a
    // conflict bar never pushes the editor down (dirty / saving live on the tab header, see
    // `crate::header`).
    let conflict: Element<'a, Message> = if buf.conflict.is_some() {
        let msg = if buf.is_missing() {
            "The file is gone from disk (deleted or moved away). Keep mine recreates it; Take theirs closes it."
        } else {
            "File changed on disk while you were editing."
        };
        let bar = row![
            text(msg).color(tokens.warn_text.to_color()),
            button(text("Keep mine"))
                .on_press(Message::Source(SourceMsg::KeepMine(buf.path.clone()))),
            button(text("Take theirs"))
                .on_press(Message::Source(SourceMsg::TakeTheirs(buf.path.clone()))),
        ]
        .spacing(8);
        container(bar)
            .padding(6)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(tokens.panel_2.to_color().into()),
                ..container::Style::default()
            })
            .into()
    } else {
        Space::new().into()
    };
    let overlay = column![conflict].spacing(4).padding(4).width(Length::Fill);
    // Plan round-5 WP-1: the stack is always wrapped (stable tree shape); the sensor reports
    // the laid-out height so `top_line` tracking uses the real viewport line count.
    let path = buf.path.as_str();
    let report = move |s: iced::Size| {
        Message::Source(SourceMsg::Viewport {
            path: path.to_owned(),
            lines: viewport_lines(s.height, font_size),
        })
    };
    let link: Element<'a, Message> = link.unwrap_or_else(|| Space::new().into());
    iced::widget::sensor(iced::widget::stack![editor, overlay, link])
        .on_show(report)
        .on_resize(report)
        .into()
}

/// The source stack's slots, in order: the editor is always layer 0, whatever the dirty /
/// conflict state; the conflict bar floats above it (a fixed slot).
pub const SLOTS: [&str; 2] = ["editor", "conflict"];

#[cfg(test)]
mod tests;
