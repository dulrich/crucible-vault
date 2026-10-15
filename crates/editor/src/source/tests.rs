//! WP-2 source-buffer tests against `FakeDaemon` (synthetic vault only, I1).

use std::sync::Arc;
use std::time::Instant;

use cv_protocol::{ChangeKind, Mode, Notification, Origin};
use cv_testkit::{SyntheticVault, VaultSpec};
use iced::widget::text_editor::{Action, Cursor, Edit, Motion, Position};

use super::*;
use crate::app::App;
use crate::daemon::FakeDaemon;
use crate::message::{Message, StatusMsg, TabsMsg};

fn setup() -> (tempfile::TempDir, Arc<FakeDaemon>, App, String) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let p = "wp2/note.md".to_owned();
    d.create_note(&p, "line one\nline two\nline three\n")
        .expect("create");
    let dc: Arc<dyn DaemonClient> = d.clone();
    let mut app = App::new(Some(dc), None);
    let _ = app.update(Message::Tabs(TabsMsg::Open(p.clone())));
    (tmp, d, app, p)
}

fn buf(app: &mut App) -> &mut SourceBuffer {
    let ps = app.panes.get_mut(app.focus).expect("focused pane");
    &mut ps.tabs[ps.active.expect("active")].source
}

fn type_text(app: &mut App, s: &str) {
    for c in s.chars() {
        let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert(c),
        ))));
    }
}

/// Runs the save the app would spawn, synchronously, and feeds the result back.
fn save_now(app: &mut App, d: &FakeDaemon) -> SaveOutcome {
    let job = buf(app).begin_save(false).expect("a save job");
    let out = job.run(d);
    let _ = app.update(job.done(out.clone()));
    out
}

fn notify(app: &mut App, path: &str, hash: Option<String>) {
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: path.to_owned(),
            kind: ChangeKind::Modified,
            hash,
            origin: Origin::Daemon,
        },
    )));
}

#[test]
fn edits_route_through_undo_and_restore_cursor() {
    let (_t, _d, mut app, _p) = setup();
    let b = buf(&mut app);
    b.move_to(Cursor {
        position: Position { line: 1, column: 4 },
        selection: None,
    });
    type_text(&mut app, "XY");
    let b = buf(&mut app);
    assert_eq!(b.text(), "line one\nlineXY two\nline three\n");
    assert!(b.is_dirty());
    // Select "two" (anchor at col 7, cursor at col 10) and Enter over it.
    b.move_to(Cursor {
        position: Position {
            line: 1,
            column: 10,
        },
        selection: Some(Position { line: 1, column: 7 }),
    });
    let sel = b.cursor();
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Enter,
    ))));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \n\nline three\n");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    let b = buf(&mut app);
    assert_eq!(b.text(), "line one\nlineXY two\nline three\n");
    assert_eq!(b.cursor(), sel, "selection restored exactly");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 1, column: 4 }
    );
    let _ = app.update(Message::Source(SourceMsg::Redo));
    let _ = app.update(Message::Source(SourceMsg::Redo));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \n\nline three\n");
    // Backspace across a line join, then undo.
    let b = buf(&mut app);
    b.move_to(Cursor {
        position: Position { line: 2, column: 0 },
        selection: None,
    });
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Backspace,
    ))));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \nline three\n");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \n\nline three\n");
    // Navigation is not an edit.
    let seq = buf(&mut app).edit_seq();
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Move(
        Motion::Down,
    ))));
    assert_eq!(buf(&mut app).edit_seq(), seq);
}

#[test]
fn save_succeeds_and_updates_base_hash() {
    let (_t, d, mut app, p) = setup();
    type_text(&mut app, "A");
    let out = save_now(&mut app, &d);
    let b = buf(&mut app);
    let SaveOutcome::Saved(h) = out else {
        panic!("expected saved, got {out:?}");
    };
    assert_eq!(b.base_hash(), &h);
    assert!(!b.is_dirty() && !b.is_saving());
    assert_eq!(
        d.read_file(&p).expect("read").0,
        b"Aline one\nline two\nline three\n"
    );
    // The echo of our own save (origin is `daemon`, so only the hash identifies it).
    notify(&mut app, &p, Some(h.as_str().to_owned()));
    assert_eq!(buf(&mut app).text(), "Aline one\nline two\nline three\n");
    assert!(buf(&mut app).conflict().is_none());
}

#[test]
fn echo_arriving_before_save_result_is_ignored() {
    let (_t, d, mut app, p) = setup();
    type_text(&mut app, "Q");
    let job = buf(&mut app).begin_save(false).expect("job");
    let out = job.run(d.as_ref());
    let SaveOutcome::Saved(h) = out.clone() else {
        panic!("saved")
    };
    notify(&mut app, &p, Some(h.as_str().to_owned()));
    assert!(buf(&mut app).conflict().is_none(), "pending-save echo");
    let _ = app.update(job.done(out));
    assert!(!buf(&mut app).is_dirty());
}

#[test]
fn stale_base_conflicts_then_keep_mine_resaves() {
    let (_t, d, mut app, p) = setup();
    type_text(&mut app, "mine ");
    // Someone else writes first.
    let base = buf(&mut app).base_hash().clone();
    let theirs = d.write_note(&p, "theirs\n", &base).expect("write");
    let out = save_now(&mut app, &d);
    assert_eq!(out, SaveOutcome::Conflict(theirs.clone()));
    assert_eq!(buf(&mut app).conflict(), Some(&theirs));
    assert!(
        buf(&mut app).begin_save(false).is_none(),
        "no save while conflicted"
    );
    // Keep mine: re-save against the new hash.
    let job = buf(&mut app).keep_mine(false).expect("job");
    let out = job.run(d.as_ref());
    let _ = app.update(job.done(out));
    assert!(buf(&mut app).conflict().is_none());
    assert!(!buf(&mut app).is_dirty());
    assert_eq!(
        d.read_file(&p).expect("read").0,
        b"mine line one\nline two\nline three\n"
    );
}

#[test]
fn dirty_buffer_external_change_shows_bar_and_take_theirs_reloads() {
    let (_t, d, mut app, p) = setup();
    type_text(&mut app, "x");
    let base = buf(&mut app).base_hash().clone();
    let theirs = d.write_note(&p, "external\n", &base).expect("write");
    notify(&mut app, &p, Some(theirs.as_str().to_owned()));
    assert_eq!(buf(&mut app).conflict(), Some(&theirs));
    assert!(buf(&mut app).text().starts_with('x'), "buffer kept");
    let _ = app.update(Message::Source(SourceMsg::TakeTheirs(p.clone())));
    let b = buf(&mut app);
    assert_eq!(b.text(), "external\n");
    assert!(b.conflict().is_none() && !b.is_dirty());
    assert_eq!(b.history().undo_len(), 0, "undo reset");
    assert_eq!(b.base_hash(), &theirs);
}

#[test]
fn clean_buffer_reloads_and_keeps_cursor_line() {
    let (_t, d, mut app, p) = setup();
    buf(&mut app).move_to(Cursor {
        position: Position { line: 2, column: 6 },
        selection: None,
    });
    let base = buf(&mut app).base_hash().clone();
    let h = d
        .write_note(&p, "a\nb\nc changed here\nd\n", &base)
        .expect("write");
    notify(&mut app, &p, Some(h.as_str().to_owned()));
    let b = buf(&mut app);
    assert_eq!(b.text(), "a\nb\nc changed here\nd\n");
    assert_eq!(b.cursor().position, Position { line: 2, column: 6 });
    assert_eq!(b.base_hash(), &h);
    // Shorter file: line clamps.
    let h2 = d.write_note(&p, "only\n", &h).expect("write");
    notify(&mut app, &p, None);
    let b = buf(&mut app);
    assert_eq!(b.base_hash(), &h2);
    assert_eq!(b.cursor().position.line, 1);
}

#[test]
fn shadow_mode_drops_edits_and_never_saves() {
    let (_t, d, mut app, p) = setup();
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::ModeChanged { mode: Mode::Shadow },
    )));
    type_text(&mut app, "nope");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    let _ = app.update(Message::Source(SourceMsg::Save));
    let b = buf(&mut app);
    assert_eq!(b.text(), "line one\nline two\nline three\n");
    assert!(!b.is_dirty() && !b.is_saving());
    assert!(b.begin_save(true).is_none());
    // Navigation still works.
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Move(
        Motion::Down,
    ))));
    assert_eq!(buf(&mut app).cursor().position.line, 1);
    assert_eq!(
        d.read_file(&p).expect("read").0,
        b"line one\nline two\nline three\n"
    );
}

#[test]
fn save_in_flight_queues_one_follow_up() {
    let (_t, d, mut app, _p) = setup();
    type_text(&mut app, "a");
    let job = buf(&mut app).begin_save(false).expect("job");
    type_text(&mut app, "b");
    assert!(buf(&mut app).begin_save(false).is_none(), "one in flight");
    let out = job.run(d.as_ref());
    // Still dirty after the first save lands (edit `b` came later); a follow-up is due.
    assert_eq!(buf(&mut app).finish_save(job.seq, out), Ok(true));
    assert!(buf(&mut app).is_dirty());
    let _ = save_now(&mut app, &d);
    assert!(!buf(&mut app).is_dirty());
}

/// Undo applies via `Content::with_text` + `move_to`: measure on a 1 MB buffer.
#[test]
fn undo_on_one_megabyte_buffer_timing() {
    let line = "lorem ipsum dolor sit amet [[link]] #tag consectetur adipiscing\n";
    let text = line.repeat(1_000_000 / line.len() + 1);
    let mut b = SourceBuffer::new("big.md", text.as_bytes(), ContentHash::of(text.as_bytes()));
    b.move_to(Cursor {
        position: Position {
            line: 7000,
            column: 5,
        },
        selection: None,
    });
    let t = Instant::now();
    assert!(b.perform(Action::Edit(Edit::Insert('Z')), false, Instant::now()));
    eprintln!("type one char {:?}", t.elapsed());
    let t = Instant::now();
    assert!(b.undo());
    let undo_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    assert!(b.redo());
    let redo_ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "1MB ({} bytes) undo {undo_ms:.1} ms, redo {redo_ms:.1} ms",
        text.len()
    );
    assert_eq!(
        b.cursor().position,
        Position {
            line: 7000,
            column: 6
        }
    );
}

#[test]
fn highlighter_change_line_reentry() {
    use iced::advanced::text::highlighter::Highlighter as _;
    let tokens = crate::theme::Surround::Med.tokens().expect("tokens");
    let s = HighlightSettings {
        colors: KindColors::from_tokens(&tokens),
        epoch: 0,
    };
    let lines = ["```", "#a", "```", "#b"];
    let mut h = MarkdownHighlighter::new(&s);
    let full: Vec<Vec<_>> = lines
        .iter()
        .map(|l| h.highlight_line(l).collect())
        .collect();
    assert_eq!(h.current_line(), 4);
    h.change_line(1);
    assert_eq!(h.current_line(), 1);
    let again: Vec<Vec<_>> = lines[1..]
        .iter()
        .map(|l| h.highlight_line(l).collect())
        .collect();
    assert_eq!(&full[1..], again.as_slice());
    assert_eq!(full[1], vec![(0..2, s.colors.code)]);
    assert_eq!(full[3], vec![(0..2, s.colors.tag)]);
    h.update(&HighlightSettings { epoch: 1, ..s });
    assert_eq!(h.current_line(), 0);
}

#[test]
fn materialise_round_trip_keeps_text_cursor_undo_and_save_state() {
    let mut b = SourceBuffer::new("n.md", b"alpha\nbeta\r\ngamma\n", ContentHash::of(b"x"));
    assert!(!b.is_shaped(), "new buffers start plain");
    let now = Instant::now();
    b.move_to(Cursor {
        position: Position { line: 1, column: 4 },
        selection: None,
    });
    for c in "XY".chars() {
        assert!(b.perform(Action::Edit(Edit::Insert(c)), false, now));
    }
    assert!(b.is_shaped(), "editing materialises");
    b.move_to(Cursor {
        position: Position { line: 0, column: 5 },
        selection: Some(Position { line: 0, column: 1 }),
    });
    // A dirty buffer meets a newer file on disk: conflict bar.
    assert_eq!(b.external_change(Some("ffff"), || unreachable!()), None);
    let reference = b.clone();
    let snap = |b: &SourceBuffer| {
        (
            b.text(),
            b.cursor(),
            b.selection(),
            b.is_dirty(),
            b.edit_seq(),
            b.base_hash().clone(),
            b.conflict().cloned(),
        )
    };
    let before = snap(&b);
    assert_eq!(before.2.as_deref(), Some("lpha"));
    assert!(before.6.is_some());
    b.dematerialise();
    assert!(!b.is_shaped());
    assert_eq!(snap(&b), before, "plain keeps everything");
    b.materialise();
    assert!(b.is_shaped());
    assert_eq!(snap(&b), before, "re-shaped restores cursor and selection");
    b.dematerialise();
    // Undo from the plain form matches undo on a never-dropped buffer.
    let mut r = reference;
    assert!(r.undo());
    assert!(b.undo());
    assert_eq!(b.text(), r.text());
    assert_eq!(b.text(), "alpha\nbeta\r\ngamma\n");
    assert_eq!(b.cursor(), r.cursor());
    assert_eq!(b.edit_seq(), r.edit_seq());
    assert!(b.redo());
    assert!(r.redo());
    assert_eq!(b.text(), r.text());
    assert_eq!(b.cursor(), r.cursor());
    // Reload on a plain buffer clamps the cursor without shaping.
    b.dematerialise();
    b.reload(b"one\n", ContentHash::of(b"one\n"));
    assert!(!b.is_shaped());
    assert_eq!(b.text(), "one\n");
    assert_eq!(b.cursor().position, Position { line: 1, column: 0 });
    assert!(!b.is_dirty());
    assert!(b.conflict().is_none());
}

/// The text editor keeps its child index and widget tag across clean → dirty → saving → clean:
/// iced matches widget state (focus) by tree position, so a sibling inserted before the editor on
/// the first dirtying keypress dropped focus on every keystroke after each autosave (cut-over E10).
#[test]
fn view_tree_position_stable_across_dirty_states() {
    use iced::advanced::widget::Tree;

    fn shape(b: &SourceBuffer) -> Vec<iced::advanced::widget::tree::Tag> {
        let t = crate::theme::Surround::default().tokens().expect("tokens");
        let el = view(b, t, Mode::Live);
        Tree::new(&el).children.iter().map(|c| c.tag).collect()
    }
    let mut b = SourceBuffer::new("n.md", b"text\n", ContentHash::of(b"text\n"));
    b.set_shaped(true);
    let clean = shape(&b);
    assert!(b.perform(Action::Edit(Edit::Insert('x')), false, Instant::now()));
    assert!(b.is_dirty());
    let dirty = shape(&b);
    let _job = b.begin_save(false).expect("save job");
    assert!(b.is_saving());
    let saving = shape(&b);
    assert_eq!(clean.len(), dirty.len());
    assert_eq!(clean.len(), saving.len());
    let last = clean.len() - 1;
    assert_eq!(clean[last], dirty[last], "editor tag moved");
    assert_eq!(clean[last], saving[last], "editor tag moved");
}

fn key(app: &mut App, k: crate::message::SourceKey) {
    let _ = app.update(Message::Source(SourceMsg::Key(k)));
}

fn at(app: &mut App, line: usize, column: usize, sel: Option<(usize, usize)>) {
    buf(app).move_to(Cursor {
        position: Position { line, column },
        selection: sel.map(|(line, column)| Position { line, column }),
    });
}

fn set_text(app: &mut App, d: &FakeDaemon, p: &str, text: &str) {
    let h = d.write_note(p, text, buf(app).base_hash()).expect("write");
    buf(app).reload(text.as_bytes(), h);
    buf(app).materialise();
}

#[test]
fn enter_continues_list_and_undoes_in_one_step() {
    use crate::message::SourceKey;
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "  - [x] done\n");
    at(&mut app, 0, 12, None);
    key(&mut app, SourceKey::Enter);
    let b = buf(&mut app);
    assert_eq!(b.text(), "  - [x] done\n  - [ ] \n");
    assert_eq!(b.cursor().position, Position { line: 1, column: 8 });
    assert!(b.is_dirty());
    assert_eq!(b.edit_seq(), 1);
    // Enter on the empty item removes the marker instead.
    key(&mut app, SourceKey::Enter);
    assert_eq!(buf(&mut app).text(), "  - [x] done\n  \n");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "  - [x] done\n  - [ ] \n");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    let b = buf(&mut app);
    assert_eq!(b.text(), "  - [x] done\n");
    assert_eq!(
        b.cursor().position,
        Position {
            line: 0,
            column: 12
        }
    );
}

#[test]
fn enter_numbered_replaces_selection_and_plain_lines_split() {
    use crate::message::SourceKey;
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "> 3. abcdef\nplain\n");
    // Select "cd" and Enter: selection replaced, then the list rule applies.
    at(&mut app, 0, 9, Some((0, 7)));
    key(&mut app, SourceKey::Enter);
    assert_eq!(buf(&mut app).text(), "> 3. ab\n> 4. ef\nplain\n");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    let b = buf(&mut app);
    assert_eq!(b.text(), "> 3. abcdef\nplain\n");
    assert_eq!(b.cursor().selection, Some(Position { line: 0, column: 7 }));
    at(&mut app, 1, 2, None);
    key(&mut app, SourceKey::Enter);
    assert_eq!(buf(&mut app).text(), "> 3. abcdef\npl\nain\n");
}

#[test]
fn word_delete_is_one_undoable_edit() {
    use crate::message::SourceKey;
    let (_t, _d, mut app, _p) = setup();
    at(&mut app, 1, 8, None);
    key(&mut app, SourceKey::DeleteWordBack);
    assert_eq!(buf(&mut app).text(), "line one\nline \nline three\n");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    let b = buf(&mut app);
    assert_eq!(b.text(), "line one\nline two\nline three\n");
    assert_eq!(b.cursor().position, Position { line: 1, column: 8 });
    assert_eq!(b.cursor().selection, None);
    at(&mut app, 2, 0, None);
    key(&mut app, SourceKey::DeleteWordForward);
    assert_eq!(buf(&mut app).text(), "line one\nline two\n three\n");
    // With a selection, only the selection goes.
    at(&mut app, 0, 3, Some((0, 1)));
    key(&mut app, SourceKey::DeleteWordBack);
    assert_eq!(buf(&mut app).text(), "le one\nline two\n three\n");
}

#[test]
fn smart_home_end_on_unwrapped_lines() {
    use crate::message::SourceKey;
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "    indented text\n");
    at(&mut app, 0, 10, None);
    key(&mut app, SourceKey::Home { select: false });
    assert_eq!(buf(&mut app).cursor().position.column, 4);
    key(&mut app, SourceKey::Home { select: false });
    assert_eq!(buf(&mut app).cursor().position.column, 0);
    key(&mut app, SourceKey::Home { select: false });
    assert_eq!(buf(&mut app).cursor().position.column, 4);
    key(&mut app, SourceKey::End { select: true });
    let c = buf(&mut app).cursor();
    assert_eq!(c.position.column, 17);
    assert_eq!(c.selection, Some(Position { line: 0, column: 4 }));
    assert_eq!(buf(&mut app).selection().as_deref(), Some("indented text"));
}

#[test]
fn primary_paste_inserts_as_one_edit() {
    let (_t, _d, mut app, p) = setup();
    at(&mut app, 0, 4, None);
    let _ = app.update(Message::Source(SourceMsg::PastePrimary {
        path: p,
        text: Some(" big".into()),
    }));
    assert_eq!(buf(&mut app).text(), "line big one\nline two\nline three\n");
    let _ = app.update(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
}

// ---- editor-usability-round WP-2: Reading-view task toggle ----

#[test]
fn toggle_task_on_plain_buffer_is_one_undoable_edit() {
    let src = "---\na: 1\n---\n- [ ] one\n- [/] two\n";
    let mut b = SourceBuffer::new("t.md", src.as_bytes(), ContentHash::of(src.as_bytes()));
    assert!(!b.is_shaped(), "a fresh buffer stays plain (Reading mode)");
    let seq = b.edit_seq();
    assert!(b.toggle_task(3, false, Instant::now()));
    assert!(!b.is_shaped(), "the toggle does not shape the buffer");
    assert_eq!(b.text(), "---\na: 1\n---\n- [x] one\n- [/] two\n");
    assert!(b.is_dirty());
    assert_eq!(b.edit_seq(), seq + 1);
    assert!(b.toggle_task(4, false, Instant::now()));
    assert_eq!(b.text(), "---\na: 1\n---\n- [x] one\n- [x] two\n");
    assert!(!b.toggle_task(1, false, Instant::now()), "not a task line");
    assert!(
        !b.toggle_task(3, true, Instant::now()),
        "read-only drops it"
    );
    assert!(b.undo());
    assert_eq!(b.text(), "---\na: 1\n---\n- [x] one\n- [/] two\n");
    assert!(b.undo());
    assert_eq!(b.text(), src);
    assert!(b.redo());
    assert_eq!(b.text(), "---\na: 1\n---\n- [x] one\n- [/] two\n");
}

#[test]
fn toggle_task_on_shaped_buffer_keeps_cursor_and_undoes() {
    let src = "intro\n- [x] done\n";
    let mut b = SourceBuffer::new("t.md", src.as_bytes(), ContentHash::of(src.as_bytes()));
    b.materialise();
    let at = Cursor {
        position: Position { line: 0, column: 2 },
        selection: None,
    };
    b.move_to(at);
    assert!(b.toggle_task(1, false, Instant::now()));
    assert!(b.is_shaped());
    assert_eq!(b.text(), "intro\n- [ ] done\n");
    assert_eq!(b.cursor().position, at.position, "cursor kept");
    assert!(b.undo());
    assert_eq!(b.text(), src);
}
