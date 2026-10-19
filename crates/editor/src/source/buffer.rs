//! [`SourceBuffer`]: the buffer state, its two text representations and the read accessors.

use editor_model::carry::Carry;
use editor_model::undo::{self, UndoHistory};
use iced::widget::text_editor::{self, Content};

use super::place::{VISIBLE_LINES, fresh_view};
use super::save::SaveState;
use crate::daemon::ContentHash;

/// The buffer text: a shaped iced `Content` while the tab is visible in Source/Split, else the
/// plain text plus the saved cursor/selection (plan editor-cutover-blockers WP-2 — a shaped
/// `Content` costs ~22.5 MiB per tab). Switching never touches text, history or save state.
#[derive(Debug, Clone)]
pub(super) enum Repr {
    Shaped(Content),
    Plain { text: String, cursor: undo::Cursor },
}

/// The frontmatter-title cache: `((edit_seq, epoch), title)` of the last parse.
type TitleCache = std::cell::RefCell<Option<((u64, u64), Option<String>)>>;

/// One open note's source buffer.
#[derive(Debug, Clone)]
pub struct SourceBuffer {
    pub(super) path: String,
    pub(super) content: Repr,
    pub(super) history: UndoHistory,
    /// Bumped on every edit; autosave ticks and save completions compare against it.
    pub(super) edit_seq: u64,
    /// Base hash, dirty flag, in-flight save, conflict and rename hold ([`SaveState`]).
    pub(super) save: SaveState,
    /// Highlighter epoch (bumped on wholesale content replacement).
    pub(super) epoch: u64,
    /// The selection last written to PRIMARY (not re-written while unchanged).
    pub(super) primary: Option<String>,
    /// plan editor-ui-round-3 WP-6: the top visible source line, tracked from the wheel's
    /// `Action::Scroll` and our own scrolls (iced 0.14's `text_editor` exposes no scroll
    /// offset); pulled up when the cursor moves above it. 0 for a fresh shaped buffer.
    pub(super) top_line: usize,
    /// Plan round-7 WP-4 / round-8 WP-4: a placement waiting for the editor's next layout
    /// ([`Carry`]; run by [`SourceBuffer::on_layout`]).
    pub(super) carry: Option<Carry>,
    /// Plan round-5 WP-1: the source lines the editor's viewport shows, measured from the laid
    /// out pane height (a `sensor` around the editor stack; [`super::viewport_lines`]); falls
    /// back to [`VISIBLE_LINES`] before the first measure.
    pub(super) visible_lines: usize,
    /// WP-R15 (SP2): the frontmatter `title` of the current text, keyed by `(edit_seq, epoch)`
    /// (an external reload does not bump `edit_seq`) — the `tab_title = "title"` source,
    /// recomputed only when the buffer changed, so drawing the tab bar never re-parses an
    /// unchanged note (plan editor-feedback-round-2 WP-6).
    title_cache: TitleCache,
}

pub(super) fn to_model(c: text_editor::Cursor) -> undo::Cursor {
    undo::Cursor {
        pos: undo::Pos::new(c.position.line, c.position.column),
        anchor: c.selection.map(|p| undo::Pos::new(p.line, p.column)),
    }
}

pub(super) fn to_iced(c: undo::Cursor) -> text_editor::Cursor {
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
            history: UndoHistory::new(cap),
            edit_seq: 0,
            save: SaveState::new(hash, title),
            epoch: 0,
            primary: None,
            top_line: 0,
            carry: None,
            visible_lines: VISIBLE_LINES,
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

    pub(super) fn shaped(&mut self) -> &mut Content {
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
    /// The number of source lines.
    pub(super) fn line_count(&self) -> usize {
        match &self.content {
            Repr::Shaped(c) => c.line_count(),
            Repr::Plain { text, .. } => text.matches('\n').count() + 1,
        }
    }
    /// The position just past the last character (the end of the last line).
    pub(super) fn end_pos(&self) -> text_editor::Position {
        let text = self.text();
        text_editor::Position {
            line: text.matches('\n').count(),
            column: text.rsplit('\n').next().map_or(0, str::len),
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

    /// Moves the cursor (navigation; not an edit).
    pub fn move_to(&mut self, c: text_editor::Cursor) {
        match &mut self.content {
            Repr::Shaped(content) => super::place::place(content, c),
            Repr::Plain { cursor, .. } => *cursor = to_model(c),
        }
    }

    /// The selection if it is non-empty and differs from what PRIMARY last received; records it.
    pub(super) fn take_new_primary(&mut self) -> Option<String> {
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
}
