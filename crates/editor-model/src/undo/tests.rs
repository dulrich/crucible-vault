use super::*;

/// A tiny document driver: applies edits to a string the way the editor does (diff of the whole
/// text as one window starting at line 0) and records them.
struct Doc {
    text: String,
    cursor: Cursor,
    h: UndoHistory,
    t: Instant,
}

impl Doc {
    fn new(text: &str, cap: usize) -> Self {
        Self {
            text: text.to_owned(),
            cursor: Cursor::default(),
            h: UndoHistory::new(cap),
            t: Instant::now(),
        }
    }

    fn edit(&mut self, new_text: &str, after: Cursor, kind: EditKind, dt_ms: u64) {
        self.t += Duration::from_millis(dt_ms);
        let ch = Change::diff(0, &self.text, new_text).expect("a change");
        let before = self.cursor;
        self.text = new_text.to_owned();
        self.cursor = after;
        self.h.record(ch, kind, before, after, self.t);
    }

    fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            let off = offset_of(&self.text, self.cursor.pos).expect("off");
            let mut t = self.text.clone();
            t.insert(off, c);
            let after = Cursor::caret(self.cursor.pos.line, self.cursor.pos.col + c.len_utf8());
            self.edit(&t, after, EditKind::Type(c), 50);
        }
    }

    fn undo(&mut self) -> bool {
        match self.h.undo(&self.text).expect("undo") {
            Some((t, c)) => {
                self.text = t;
                self.cursor = c;
                true
            }
            None => false,
        }
    }

    fn redo(&mut self) -> bool {
        match self.h.redo(&self.text).expect("redo") {
            Some((t, c)) => {
                self.text = t;
                self.cursor = c;
                true
            }
            None => false,
        }
    }
}

#[test]
fn diff_is_minimal_and_multiline() {
    let c = Change::diff(3, "ab\ncdé\nx", "ab\ncZé\nx").expect("change");
    assert_eq!(c.at, Pos::new(4, 1));
    assert_eq!(c.removed, "d");
    assert_eq!(c.inserted, "Z");
    // Multi-byte boundary: replacing é with è shares a lead byte but must not split it.
    let c = Change::diff(0, "é", "è").expect("change");
    assert_eq!((c.removed.as_str(), c.inserted.as_str()), ("é", "è"));
    assert!(Change::diff(0, "same", "same").is_none());
}

#[test]
fn record_joined_merges_into_the_users_group_for_one_undo() {
    let mut d = Doc::new("---\na: 1\n---\nbody", DEFAULT_CAP_BYTES);
    d.cursor = Cursor::caret(3, 4);
    d.type_str("X");
    let (before_bytes, user_after) = (d.h.bytes(), d.cursor);
    assert_eq!(d.h.undo_len(), 1);
    assert!(d.h.can_join());
    // The follow-up edit: the frontmatter grows by a line and the cursor shifts down.
    let lint = Change::diff(0, "---\na: 1\n---\n", "---\na: 1\nb: 2\n---\n").unwrap();
    let mut text = d.text.clone();
    lint.apply(&mut text).unwrap();
    let after = Cursor::caret(user_after.pos.line + 1, user_after.pos.col);
    d.h.record_joined(lint, user_after, after, d.t);
    d.text = text;
    d.cursor = after;
    assert_eq!(d.h.undo_len(), 1, "joined, not a new group");
    assert!(d.h.bytes() > before_bytes);
    // One undo reverts both and restores the cursor from before the user's edit.
    assert!(d.undo());
    assert_eq!(d.text, "---\na: 1\n---\nbody");
    assert_eq!(d.cursor, Cursor::caret(3, 4));
    assert_eq!((d.h.undo_len(), d.h.redo_len()), (0, 1));
    // Redo re-applies both and restores the shifted cursor.
    assert!(d.redo());
    assert_eq!(d.text, "---\na: 1\nb: 2\n---\nbodyX");
    assert_eq!(d.cursor, after);
}

#[test]
fn record_joined_without_a_group_or_under_redo_is_its_own_group() {
    let mut d = Doc::new("a\nb", DEFAULT_CAP_BYTES);
    assert!(!d.h.can_join());
    let ch = Change::diff(0, "a\nb", "a\nb\nc").unwrap();
    d.h.record_joined(ch, Cursor::default(), Cursor::default(), d.t);
    assert_eq!(d.h.undo_len(), 1);
    // After an undo the redo stack is pending: no joining into the older group.
    d.text = "a\nb\nc".into();
    assert!(d.undo());
    assert!(!d.h.can_join());
}

#[test]
fn undo_redo_restore_text_cursor_and_selection_exactly() {
    let mut d = Doc::new("hello\nworld\n", DEFAULT_CAP_BYTES);
    let s0 = (d.text.clone(), d.cursor);
    // Replace a selection "llo\nwo" with "LL" (cursor carried a selection before).
    d.cursor = Cursor {
        pos: Pos::new(1, 2),
        anchor: Some(Pos::new(0, 2)),
    };
    let s1 = (d.text.clone(), d.cursor);
    d.edit("heLLrld\n", Cursor::caret(0, 4), EditKind::Other, 10);
    let s2 = (d.text.clone(), d.cursor);
    // Delete one char.
    d.edit("heLrld\n", Cursor::caret(0, 3), EditKind::Other, 2000);
    let s3 = (d.text.clone(), d.cursor);
    // Insert a paste spanning lines.
    d.edit("heL\nnew\nrld\n", Cursor::caret(2, 0), EditKind::Other, 10);
    let s4 = (d.text.clone(), d.cursor);

    assert!(d.undo());
    assert_eq!((d.text.clone(), d.cursor), s3);
    assert!(d.undo());
    assert_eq!((d.text.clone(), d.cursor), s2);
    assert!(d.undo());
    assert_eq!((d.text.clone(), d.cursor), s1, "selection restored");
    assert!(!d.undo());
    assert_eq!(d.text, s0.0);
    assert!(d.redo());
    assert_eq!((d.text.clone(), d.cursor), s2);
    assert!(d.redo());
    assert!(d.redo());
    assert_eq!((d.text.clone(), d.cursor), s4);
    assert!(!d.redo());
}

#[test]
fn typing_coalesces_on_word_boundaries() {
    let mut d = Doc::new("", DEFAULT_CAP_BYTES);
    d.type_str("hello world");
    assert_eq!(d.h.undo_len(), 2, "'hello ' and 'world'");
    assert!(d.undo());
    assert_eq!(d.text, "hello ");
    assert!(d.undo());
    assert_eq!(d.text, "");
}

#[test]
fn idle_gap_breaks_group() {
    let mut d = Doc::new("", DEFAULT_CAP_BYTES);
    d.type_str("ab");
    d.t += Duration::from_millis(1100);
    d.type_str("cd");
    assert_eq!(d.h.undo_len(), 2);
    assert!(d.undo());
    assert_eq!(d.text, "ab");
}

#[test]
fn cursor_jump_breaks_group() {
    let mut d = Doc::new("xy", DEFAULT_CAP_BYTES);
    d.cursor = Cursor::caret(0, 2);
    d.type_str("ab");
    d.cursor = Cursor::caret(0, 0);
    d.type_str("cd");
    assert_eq!(d.text, "cdxyab");
    assert_eq!(d.h.undo_len(), 2);
    assert!(d.undo());
    assert_eq!(d.text, "xyab");
    assert_eq!(d.cursor, Cursor::caret(0, 0));
}

#[test]
fn deletions_coalesce_but_not_with_typing() {
    let mut d = Doc::new("abcd", DEFAULT_CAP_BYTES);
    d.cursor = Cursor::caret(0, 4);
    d.edit("abc", Cursor::caret(0, 3), EditKind::Delete, 10);
    d.edit("ab", Cursor::caret(0, 2), EditKind::Delete, 10);
    d.type_str("Z");
    assert_eq!(d.h.undo_len(), 2);
    assert!(d.undo());
    assert!(d.undo());
    assert_eq!((d.text.as_str(), d.cursor), ("abcd", Cursor::caret(0, 4)));
}

#[test]
fn byte_cap_evicts_oldest_and_stays_under() {
    let cap = 1000;
    let mut d = Doc::new("", cap);
    for i in 0..50 {
        let mut t = d.text.clone();
        t.push_str(&"x".repeat(40));
        let after = Cursor::caret(0, t.len());
        d.edit(&t, after, EditKind::Other, 10);
        assert!(d.h.bytes() <= cap, "step {i}: {} > {cap}", d.h.bytes());
    }
    let n = d.h.undo_len();
    assert!(n > 0 && n < 50);
    while d.undo() {}
    // Oldest groups were dropped: undo stops short of the empty document.
    assert_eq!(d.text.len(), (50 - n) * 40);
    // A single over-cap change leaves nothing (and still respects the cap).
    let big = "y".repeat(2000);
    d.edit(&big, Cursor::caret(0, 2000), EditKind::Other, 10);
    assert!(d.h.bytes() <= cap);
}

#[test]
fn new_edit_clears_redo_and_reset_clears_all() {
    let mut d = Doc::new("", DEFAULT_CAP_BYTES);
    d.type_str("ab ");
    d.type_str("cd");
    assert!(d.undo());
    assert_eq!(d.h.redo_len(), 1);
    d.type_str("Q");
    assert_eq!(d.h.redo_len(), 0);
    assert!(!d.redo());
    d.h.reset();
    assert_eq!((d.h.undo_len(), d.h.redo_len(), d.h.bytes()), (0, 0, 0));
    assert!(!d.undo());
}

#[test]
fn mismatch_resets_history() {
    let mut d = Doc::new("", DEFAULT_CAP_BYTES);
    d.type_str("abc");
    assert_eq!(d.h.undo("totally different"), Err(UndoError::Mismatch));
    assert_eq!(d.h.undo_len(), 0);
}

#[test]
fn crlf_offsets_skip_line_ending() {
    assert_eq!(offset_of("ab\r\ncd", Pos::new(1, 1)), Some(5));
    assert_eq!(offset_of("ab", Pos::new(1, 0)), None);
    assert_eq!(offset_of("ab", Pos::new(0, 3)), None);
}

#[test]
fn ops_variants_match_string_variants() {
    let mut d = Doc::new("ab\ncd", DEFAULT_CAP_BYTES);
    d.cursor = Cursor::caret(1, 1);
    d.edit("ab\nc\nXY\nd", Cursor::caret(2, 2), EditKind::Other, 10);
    let mut h2 = d.h.clone();
    let (ops, cur) = h2.undo_ops().expect("ops");
    let mut t = d.text.clone();
    for c in &ops {
        assert_eq!(c.removed_end(), Pos::new(3, 0));
        c.apply(&mut t).expect("apply");
    }
    assert!(d.undo());
    assert_eq!((t.as_str(), cur), (d.text.as_str(), d.cursor));
    let (ops, cur) = h2.redo_ops().expect("redo ops");
    assert_eq!(ops[0].inserted_end(), Pos::new(3, 0));
    assert_eq!(cur, Cursor::caret(2, 2));
}
