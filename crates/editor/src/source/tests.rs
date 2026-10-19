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
    let _ = app.send(Message::Tabs(TabsMsg::Open(p.clone())));
    (tmp, d, app, p)
}

fn buf(app: &mut App) -> &mut SourceBuffer {
    app.active_doc_mut().expect("active doc")
}

fn type_text(app: &mut App, s: &str) {
    for c in s.chars() {
        let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert(c),
        ))));
    }
}

/// Runs the save the app would spawn, synchronously, and feeds the result back.
fn save_now(app: &mut App, d: &FakeDaemon) -> SaveOutcome {
    let job = buf(app).begin_save(false).expect("a save job");
    let out = job.run(d);
    let _ = app.send(job.done(out.clone()));
    out
}

fn notify(app: &mut App, path: &str, hash: Option<String>) {
    let _ = app.send(Message::Status(StatusMsg::Notification(
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
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Enter,
    ))));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \n\nline three\n");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    let b = buf(&mut app);
    assert_eq!(b.text(), "line one\nlineXY two\nline three\n");
    assert_eq!(b.cursor(), sel, "selection restored exactly");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 1, column: 4 }
    );
    let _ = app.send(Message::Source(SourceMsg::Redo));
    let _ = app.send(Message::Source(SourceMsg::Redo));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \n\nline three\n");
    // Backspace across a line join, then undo.
    let b = buf(&mut app);
    b.move_to(Cursor {
        position: Position { line: 2, column: 0 },
        selection: None,
    });
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Backspace,
    ))));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \nline three\n");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nlineXY \n\nline three\n");
    // Navigation is not an edit.
    let seq = buf(&mut app).edit_seq();
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Move(
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
    let _ = app.send(job.done(out));
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
    let _ = app.send(job.done(out));
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
    let _ = app.send(Message::Source(SourceMsg::TakeTheirs(p.clone())));
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
    app.status.mode = Mode::Shadow;
    type_text(&mut app, "nope");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    let _ = app.send(Message::Source(SourceMsg::Save));
    let b = buf(&mut app);
    assert_eq!(b.text(), "line one\nline two\nline three\n");
    assert!(!b.is_dirty() && !b.is_saving());
    assert!(b.begin_save(true).is_none());
    // Navigation still works.
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Move(
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
        find: None,
        find_all: Arc::from([]),
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
    assert_eq!(
        full[1],
        vec![(
            0..2,
            Paint {
                color: s.colors.code,
                bold: false
            }
        )]
    );
    assert_eq!(
        full[3],
        vec![(
            0..2,
            Paint {
                color: s.colors.tag,
                bold: false
            }
        )]
    );
    h.update(&HighlightSettings {
        epoch: 1,
        ..s.clone()
    });
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
        let km = crate::palette::Keymap::default();
        let el = view(b, t, Mode::Live, crate::theme::BASE_TEXT_SIZE, &km, None);
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
    let _ = app.send(Message::Source(SourceMsg::Key(k)));
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
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "  - [x] done\n  - [ ] \n");
    let _ = app.send(Message::Source(SourceMsg::Undo));
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
    let _ = app.send(Message::Source(SourceMsg::Undo));
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
    let _ = app.send(Message::Source(SourceMsg::Undo));
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
    let _ = app.send(Message::Source(SourceMsg::PastePrimary {
        path: p,
        text: Some(" big".into()),
    }));
    assert_eq!(buf(&mut app).text(), "line big one\nline two\nline three\n");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
}

// ---- editor-usability-round WP-2: Reading-view task toggle ----

#[test]
fn toggle_frontmatter_bool_flips_only_the_scalar() {
    let src = "---\nread: false # x\ntags: [a]\n---\nbody\n";
    let mut b = SourceBuffer::new("t.md", src.as_bytes(), ContentHash::of(src.as_bytes()));
    let seq = b.edit_seq();
    assert!(b.toggle_frontmatter_bool("read", false, Instant::now()));
    assert_eq!(b.text(), "---\nread: true # x\ntags: [a]\n---\nbody\n");
    assert!(b.is_dirty());
    assert_eq!(b.edit_seq(), seq + 1);
    assert!(!b.toggle_frontmatter_bool("tags", false, Instant::now()));
    assert!(!b.toggle_frontmatter_bool("read", true, Instant::now()));
    assert!(b.undo());
    assert_eq!(b.text(), src);
}

/// Plan round-8-intake-views-anchor WP-3: missing / null keys and frontmatter-less notes upsert
/// to `true` as one undoable edit.
#[test]
fn toggle_frontmatter_bool_upserts_in_one_undo_step() {
    for (src, want) in [
        (
            "---\ntitle: x\n---\nbody\n",
            "---\ntitle: x\nread: true\n---\nbody\n",
        ),
        (
            "---\nread: null\n---\nbody\n",
            "---\nread: true\n---\nbody\n",
        ),
        ("body\n", "---\nread: true\n---\nbody\n"),
    ] {
        let mut b = SourceBuffer::new("t.md", src.as_bytes(), ContentHash::of(src.as_bytes()));
        let seq = b.edit_seq();
        assert!(b.toggle_frontmatter_bool("read", false, Instant::now()));
        assert_eq!(b.text(), want);
        assert_eq!(b.edit_seq(), seq + 1);
        assert!(b.undo());
        assert_eq!(b.text(), src);
    }
}

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

// ---- plan editor-feedback-round-2 WP-4: Tab / Shift+Tab, find highlight ----

fn sel(app: &mut App, from: (usize, usize), to: (usize, usize)) {
    buf(app).select_range(from, to);
}

#[test]
fn tab_without_selection_inserts_tab_and_undoes() {
    let (_t, _d, mut app, _p) = setup();
    buf(&mut app).materialise();
    sel(&mut app, (1, 4), (1, 4));
    key(&mut app, SourceKey::Indent);
    assert_eq!(buf(&mut app).text(), "line one\nline\t two\nline three\n");
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 1, column: 5 }
    );
    assert!(buf(&mut app).is_dirty());
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
}

// plan editor-round-4 WP-2: a within-line selection indents the line (was: replaced by `\t`).
#[test]
fn tab_within_line_selection_indents_the_line() {
    let (_t, _d, mut app, _p) = setup();
    buf(&mut app).materialise();
    sel(&mut app, (0, 4), (0, 8));
    let seq = buf(&mut app).edit_seq();
    key(&mut app, SourceKey::Indent);
    assert_eq!(buf(&mut app).text(), "\tline one\nline two\nline three\n");
    assert_eq!(buf(&mut app).edit_seq(), seq + 1);
    let c = buf(&mut app).cursor();
    assert_eq!(c.position, Position { line: 0, column: 9 });
    assert_eq!(c.selection, Some(Position { line: 0, column: 5 }));
    assert_eq!(buf(&mut app).selection().as_deref(), Some(" one"));
}

#[test]
fn tab_after_list_marker_indents_the_line() {
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "- item\n");
    sel(&mut app, (0, 2), (0, 2));
    key(&mut app, SourceKey::Indent);
    assert_eq!(buf(&mut app).text(), "\t- item\n");
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 0, column: 3 }
    );
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "- item\n");
}

#[test]
fn tab_after_indented_task_marker_indents_the_line() {
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "  - [ ] todo\n");
    sel(&mut app, (0, 8), (0, 8));
    key(&mut app, SourceKey::Indent);
    assert_eq!(buf(&mut app).text(), "\t  - [ ] todo\n");
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 0, column: 9 }
    );
}

#[test]
fn tab_mid_text_on_list_line_types_a_tab() {
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "- item\n");
    sel(&mut app, (0, 4), (0, 4));
    key(&mut app, SourceKey::Indent);
    assert_eq!(buf(&mut app).text(), "- it\tem\n");
}

#[test]
fn shift_tab_after_list_marker_outdents_the_line() {
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "\t- item\n");
    sel(&mut app, (0, 3), (0, 3));
    key(&mut app, SourceKey::Outdent);
    assert_eq!(buf(&mut app).text(), "- item\n");
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 0, column: 2 }
    );
}

#[test]
fn shift_click_maps_to_a_drag_only_with_shift() {
    let p = iced::Point::new(3.0, 4.0);
    let click = || SourceMsg::Action(Action::Click(p));
    assert!(matches!(
        shift_click(click(), true),
        SourceMsg::Action(Action::Drag(q)) if q == p
    ));
    assert!(matches!(
        shift_click(click(), false),
        SourceMsg::Action(Action::Click(_))
    ));
}

#[test]
fn shift_click_extends_selection_from_the_cursor() {
    use iced::keyboard::Modifiers;
    let (_t, _d, mut app, _p) = setup();
    buf(&mut app).materialise();
    sel(&mut app, (2, 3), (2, 3));
    let _ = app.send(Message::Modifiers(Modifiers::SHIFT));
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Click(
        iced::Point::new(0.0, 0.0),
    ))));
    let c = buf(&mut app).cursor();
    assert_eq!(c.position, Position { line: 0, column: 0 });
    assert_eq!(c.selection, Some(Position { line: 2, column: 3 }));
    // Shift released: a click just moves the cursor.
    let _ = app.send(Message::Modifiers(Modifiers::empty()));
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Click(
        iced::Point::new(0.0, 0.0),
    ))));
    assert_eq!(buf(&mut app).cursor().selection, None);
}

#[test]
fn go_to_bottom_moves_top_line_near_the_end() {
    let (_t, d, mut app, p) = setup();
    let text: String = (0..100).map(|i| format!("line {i}\n\n")).collect();
    set_text(&mut app, &d, &p, &text);
    buf(&mut app).go_to_edge(true);
    let last = 200;
    let top = buf(&mut app).top_line();
    assert_eq!(top, last + 1 - VISIBLE_LINES);
    buf(&mut app).go_to_edge(false);
    assert_eq!(buf(&mut app).top_line(), 0);
    // A cursor jump below the viewport (find) drags the top line with it.
    sel(&mut app, (150, 0), (150, 2));
    assert_eq!(buf(&mut app).top_line(), 151 - VISIBLE_LINES);
}

#[test]
fn tab_multi_line_indents_every_line_in_one_undo_step() {
    let (_t, _d, mut app, _p) = setup();
    buf(&mut app).materialise();
    sel(&mut app, (0, 2), (2, 3));
    let seq = buf(&mut app).edit_seq();
    key(&mut app, SourceKey::Indent);
    assert_eq!(
        buf(&mut app).text(),
        "\tline one\n\tline two\n\tline three\n"
    );
    assert_eq!(buf(&mut app).edit_seq(), seq + 1);
    let c = buf(&mut app).cursor();
    assert_eq!(c.position, Position { line: 2, column: 4 });
    assert_eq!(c.selection, Some(Position { line: 0, column: 3 }));
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
    let c = buf(&mut app).cursor();
    assert_eq!(c.position, Position { line: 2, column: 3 });
    assert_eq!(c.selection, Some(Position { line: 0, column: 2 }));
    // A selection ending at column 0 does not touch that line; a whole-line selection indents.
    sel(&mut app, (0, 0), (2, 0));
    key(&mut app, SourceKey::Indent);
    assert_eq!(buf(&mut app).text(), "\tline one\n\tline two\nline three\n");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    sel(&mut app, (1, 0), (1, 8));
    key(&mut app, SourceKey::Indent);
    assert_eq!(buf(&mut app).text(), "line one\n\tline two\nline three\n");
}

#[test]
fn shift_tab_removes_tab_or_spaces_or_nothing() {
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "\ta\n      b\n  c\nd\n");
    sel(&mut app, (0, 1), (3, 1));
    key(&mut app, SourceKey::Outdent);
    assert_eq!(buf(&mut app).text(), "a\n  b\nc\nd\n");
    let c = buf(&mut app).cursor();
    assert_eq!(c.position, Position { line: 3, column: 1 });
    assert_eq!(c.selection, Some(Position { line: 0, column: 0 }));
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "\ta\n      b\n  c\nd\n");
    // Nothing to remove: no edit recorded.
    sel(&mut app, (3, 1), (3, 1));
    let seq = buf(&mut app).edit_seq();
    key(&mut app, SourceKey::Outdent);
    assert_eq!(buf(&mut app).edit_seq(), seq);
}

#[test]
fn shift_tab_without_selection_outdents_cursor_line() {
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "x\n    - item\n");
    sel(&mut app, (1, 7), (1, 7));
    key(&mut app, SourceKey::Outdent);
    assert_eq!(buf(&mut app).text(), "x\n- item\n");
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 1, column: 3 }
    );
    assert_eq!(buf(&mut app).cursor().selection, None);
}

#[test]
fn tab_and_shift_tab_are_noops_when_read_only() {
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "\tx\n");
    let b = buf(&mut app);
    let now = Instant::now();
    assert!(!b.indent(false, true, now));
    assert!(!b.indent(true, true, now));
    assert_eq!(b.text(), "\tx\n");
    assert!(!b.is_dirty());
}

#[test]
fn tab_keys_bind_only_when_focused() {
    use iced::keyboard::key::{Named, Physical};
    use iced::keyboard::{Key, Modifiers};
    use iced::widget::text_editor::{Binding, KeyPress, Status};
    let kp = |m: Modifiers, status: Status| KeyPress {
        key: Key::Named(Named::Tab),
        modified_key: Key::Named(Named::Tab),
        physical_key: Physical::Unidentified(iced::keyboard::key::NativeCode::Unidentified),
        modifiers: m,
        text: None,
        status,
    };
    let focused = Status::Focused { is_hovered: false };
    let k = |b: Option<Binding<Message>>| match b {
        Some(Binding::Custom(Message::Source(SourceMsg::Key(k)))) => Some(k),
        _ => None,
    };
    assert_eq!(
        k(test_key_binding(kp(Modifiers::empty(), focused))),
        Some(SourceKey::Indent)
    );
    assert_eq!(
        k(test_key_binding(kp(Modifiers::SHIFT, focused))),
        Some(SourceKey::Outdent)
    );
    assert!(test_key_binding(kp(Modifiers::empty(), Status::Active)).is_none());
}

#[test]
fn find_highlight_tracks_current_match_and_clears_on_close() {
    use crate::message::FindMsg;
    let (_t, _d, mut app, p) = setup();
    // plan editor-round-4 WP-3: Reading steps visible occurrences (no source highlight), so
    // the source highlight is exercised in Source mode.
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: p.clone(),
        mode: crate::reading::ViewMode::Source,
    }));
    let _ = app.send(Message::Find(FindMsg::Open));
    let _ = app.send(Message::Find(FindMsg::Query("two".into())));
    assert_eq!(buf(&mut app).find_highlight(), Some(((1, 5), (1, 8))));
    assert_eq!(app.reading.find_query(&p), Some("two"));
    let _ = app.send(Message::Find(FindMsg::Query("zzz".into())));
    assert_eq!(buf(&mut app).find_highlight(), None);
    let _ = app.send(Message::Find(FindMsg::Query("line".into())));
    assert!(buf(&mut app).find_highlight().is_some());
    let text = buf(&mut app).text();
    let _ = app.send(Message::Find(FindMsg::Close));
    assert_eq!(buf(&mut app).find_highlight(), None);
    assert_eq!(app.reading.find_query(&p), None);
    assert_eq!(buf(&mut app).text(), text, "find never mutates the buffer");
}

#[test]
fn find_after_non_ascii_selects_and_paints_exact_bytes() {
    use crate::message::FindMsg;
    use iced::advanced::text::highlighter::Highlighter as _;
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "café bar\n");
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: p.clone(),
        mode: crate::reading::ViewMode::Source,
    }));
    buf(&mut app).move_to(Cursor {
        position: Position { line: 0, column: 0 },
        selection: None,
    });
    let _ = app.send(Message::Find(FindMsg::Open));
    let _ = app.send(Message::Find(FindMsg::Query("bar".into())));
    let c = buf(&mut app).cursor();
    assert_eq!(c.selection, Some(Position { line: 0, column: 6 }));
    assert_eq!(c.position, Position { line: 0, column: 9 });
    assert_eq!(&buf(&mut app).text()[6..9], "bar");
    let span = buf(&mut app).find_highlight();
    assert_eq!(span, Some(((0, 6), (0, 9))));
    let tokens = crate::theme::Surround::Med.tokens().expect("tokens");
    let s = HighlightSettings {
        colors: KindColors::from_tokens(&tokens),
        epoch: 0,
        find: span,
        find_all: Arc::from([]),
    };
    let mut h = MarkdownHighlighter::new(&s);
    let line = "café bar";
    let painted: Vec<_> = h.highlight_line(line).filter(|(_, p)| p.bold).collect();
    assert_eq!(painted.len(), 1);
    assert_eq!(&line[painted[0].0.clone()], "bar");
}

// ---- plan editor-feedback-round-2 WP-9: image paste ----

#[test]
fn image_paste_stores_attachment_and_inserts_embed() {
    use crate::clipboard_image::{ClipboardReader, PasteOutcome, resolve};
    struct Png;
    impl ClipboardReader for Png {
        fn read(&self, target: &str) -> std::result::Result<Vec<u8>, String> {
            match target {
                "TARGETS" => Ok(b"TARGETS\nimage/png\nimage/jpeg\n".to_vec()),
                "image/png" => Ok(b"\x89PNG fake".to_vec()),
                t => Err(format!("no {t}")),
            }
        }
    }
    let (_t, d, mut app, p) = setup();
    let outcome = resolve(&Png, Some(d.as_ref() as &dyn DaemonClient), &p);
    let PasteOutcome::Insert(ref s) = outcome else {
        panic!("expected an insert, got {outcome:?}");
    };
    let name = s
        .strip_prefix("![[")
        .and_then(|r| r.strip_suffix("]]"))
        .expect("wiki embed")
        .to_owned();
    assert!(name.ends_with(".png"), "{name}");
    let stored = d
        .read_file(&format!("wp2/_attachments/note/{name}"))
        .expect("attachment written");
    assert_eq!(stored.0, b"\x89PNG fake");
    // Identical bytes resolve to the same file.
    assert_eq!(
        resolve(&Png, Some(d.as_ref() as &dyn DaemonClient), &p),
        outcome
    );
    at(&mut app, 0, 4, None);
    let _ = app.send(Message::Source(SourceMsg::PasteResolved {
        path: p,
        outcome,
    }));
    assert_eq!(
        buf(&mut app).text(),
        format!("line![[{name}]] one\nline two\nline three\n")
    );
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
}

#[test]
fn ctrl_v_routes_to_image_aware_paste() {
    let kp = KeyPress {
        key: keyboard::Key::Character("v".into()),
        modified_key: keyboard::Key::Character("v".into()),
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        modifiers: keyboard::Modifiers::CTRL,
        text: None,
        status: text_editor::Status::Focused { is_hovered: false },
    };
    assert!(matches!(
        test_key_binding(kp),
        Some(Binding::Custom(Message::Source(SourceMsg::Paste)))
    ));
}

// ---- plan editor-ui-round-3 WP-5 ----

#[test]
fn ctrl_x_without_selection_cuts_the_line_as_one_undoable_edit() {
    let (_t, _d, mut app, _p) = setup();
    buf(&mut app).materialise();
    at(&mut app, 1, 3, None);
    let (clip, changed) = buf(&mut app).cut(false, Instant::now());
    assert!(changed);
    assert_eq!(clip.as_deref(), Some("line two\n"));
    assert_eq!(buf(&mut app).text(), "line one\nline three\n");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "line one\nline two\nline three\n");
    // Last line without a trailing newline: the line text; the preceding newline goes too.
    let (_t, d, mut app, p) = setup();
    set_text(&mut app, &d, &p, "a\nlast");
    at(&mut app, 1, 2, None);
    let (clip, _) = buf(&mut app).cut(false, Instant::now());
    assert_eq!(clip.as_deref(), Some("last"));
    assert_eq!(buf(&mut app).text(), "a");
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(buf(&mut app).text(), "a\nlast");
    // With a selection: only the selection (as before).
    at(&mut app, 0, 1, Some((0, 0)));
    let (clip, _) = buf(&mut app).cut(false, Instant::now());
    assert_eq!(clip.as_deref(), Some("a"));
    assert_eq!(buf(&mut app).text(), "\nlast");
    // Read-only: nothing is cut.
    at(&mut app, 1, 0, None);
    assert_eq!(buf(&mut app).cut(true, Instant::now()), (None, false));
}

#[test]
fn ctrl_x_routes_to_cut() {
    let kp = KeyPress {
        key: keyboard::Key::Character("x".into()),
        modified_key: keyboard::Key::Character("x".into()),
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        modifiers: keyboard::Modifiers::CTRL,
        text: None,
        status: text_editor::Status::Focused { is_hovered: false },
    };
    assert!(matches!(
        test_key_binding(kp),
        Some(Binding::Custom(Message::Source(SourceMsg::Cut)))
    ));
}

#[test]
fn go_to_top_and_bottom_move_the_source_cursor() {
    use crate::palette::ClientCmd;
    let (_t, _d, mut app, p) = setup();
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: p.clone(),
        mode: crate::reading::ViewMode::Source,
    }));
    at(&mut app, 1, 3, Some((1, 0)));
    let focus = app.focus;
    let _ = app.send(ClientCmd::GoToBottom.message(focus));
    let c = buf(&mut app).cursor();
    assert_eq!(c.position, Position { line: 3, column: 0 });
    assert_eq!(c.selection, None);
    let _ = app.send(ClientCmd::GoToTop.message(focus));
    assert_eq!(
        buf(&mut app).cursor().position,
        Position { line: 0, column: 0 }
    );
    assert!(!buf(&mut app).is_dirty(), "motion only");
}

#[test]
fn highlighter_paints_every_match_light_and_the_current_strong_in_bytes() {
    use iced::advanced::text::highlighter::Highlighter as _;
    let tokens = crate::theme::Surround::Med.tokens().expect("tokens");
    let line = "été bar été";
    // Byte columns: "été" = 0..5 and 10..15 (é is two bytes).
    let all: Arc<[FindSpan]> = Arc::from([((0, 0), (0, 5)), ((0, 10), (0, 15))]);
    let s = HighlightSettings {
        colors: KindColors::from_tokens(&tokens),
        epoch: 0,
        find: Some(((0, 10), (0, 15))),
        find_all: all,
    };
    let mut h = MarkdownHighlighter::new(&s);
    let painted: Vec<_> = h.highlight_line(line).collect();
    let light: Vec<_> = painted
        .iter()
        .filter(|(_, p)| p.color == s.colors.find_all && !p.bold)
        .map(|(r, _)| r.clone())
        .collect();
    assert_eq!(light, vec![0..5, 10..15]);
    let strong: Vec<_> = painted.iter().filter(|(_, p)| p.bold).collect();
    assert_eq!(strong.len(), 1);
    assert_eq!(strong[0].0, 10..15);
    assert_eq!(strong[0].1.color, s.colors.find);
    assert_eq!(&line[strong[0].0.clone()], "été");
}

/// Plan usability-sprint-captures-links WP-3: Ctrl+click routing (bare URL, md-link, wikilink).
#[test]
fn command_click_routes_links() {
    use crate::message::{Message, ReadingMsg};
    let line = "x https://a.b/c#frag [[Note|n]] [l](sub/n.md) [w](http://w.x)";
    let at = |needle: &str| line.find(needle).expect("needle");
    assert!(matches!(
        super::command_click_msg("a.md", line, at("#frag")),
        Some(Message::OpenUrl(u)) if u == "https://a.b/c#frag"
    ));
    assert!(matches!(
        super::command_click_msg("a.md", line, at("Note")),
        Some(Message::Reading(ReadingMsg::OpenLink { src, target })) if src == "a.md" && target == "Note"
    ));
    assert!(matches!(
        super::command_click_msg("a.md", line, at("[l]")),
        Some(Message::Reading(ReadingMsg::OpenLink { target, .. })) if target == "sub/n.md"
    ));
    assert!(matches!(
        super::command_click_msg("a.md", line, at("[w]")),
        Some(Message::OpenUrl(u)) if u == "http://w.x"
    ));
    assert!(super::command_click_msg("a.md", line, 0).is_none());
}

/// Plan round-5 WP-1 (items 3C, 4b): the tracked top line survives dematerialise (tab blur)
/// and is re-applied on materialise; the measured viewport drives go-to-bottom and the clamp.
#[test]
fn top_line_survives_blur_and_uses_the_measured_viewport() {
    let (_t, d, mut app, p) = setup();
    let text: String = (0..100).map(|i| format!("line {i}\n\n")).collect();
    set_text(&mut app, &d, &p, &text);
    assert_eq!(viewport_lines(5.0 * 2.0 + 20.8 * 12.5, 16.0), 12);
    buf(&mut app).set_visible_lines(12);
    buf(&mut app).scroll_to_line(50);
    buf(&mut app).set_shaped(false);
    buf(&mut app).set_shaped(true);
    assert_eq!(buf(&mut app).top_line(), 50, "kept across blur/resume");
    // 201 lines, 12 visible: the last reachable top is 189.
    buf(&mut app).scroll_to_line(199);
    assert_eq!(buf(&mut app).top_line(), 189);
    buf(&mut app).go_to_edge(true);
    assert_eq!(buf(&mut app).top_line(), 200 - 11);
}

/// Plan round-6 WP-1 (audit E1): a layout-backed harness — an `iced_graphics` text editor whose
/// cosmic buffer is reachable (iced's `Content` hides it), driven by the production sequences.
mod layout {
    use iced::advanced::graphics::text::Editor as GEditor;
    use iced::advanced::text::editor::Editor as _;
    use iced::advanced::text::highlighter::PlainText;
    use iced::advanced::text::{LineHeight, Wrapping};
    use iced::widget::text_editor::{Action, Cursor, Position};
    use iced::{Font, Pixels, Size};

    use super::super::{EditorOps, fresh_view, scroll_content};

    /// 30 lines of 18.2 px visible, as in the spike.
    const LINE: f32 = 18.2;
    const VISIBLE: usize = 30;

    struct Harness(GEditor);

    impl EditorOps for Harness {
        fn act(&mut self, action: Action) {
            self.0.perform(action);
        }
        fn move_cursor(&mut self, cursor: Cursor) {
            self.0.move_to(cursor);
        }
    }

    impl Harness {
        fn new(lines: usize) -> Self {
            let text: String = (0..lines).map(|i| format!("line {i}\n")).collect();
            let mut h = Self(GEditor::with_text(&text));
            h.layout();
            h
        }
        /// One layout pass (iced's `update` ends with `shape_as_needed`).
        fn layout(&mut self) {
            #[allow(clippy::cast_precision_loss)]
            let height = LINE * VISIBLE as f32;
            self.0.update(
                Size::new(600.0, height),
                Font::MONOSPACE,
                Pixels(14.0),
                LineHeight::Absolute(Pixels(LINE)),
                Wrapping::None,
                &mut PlainText,
            );
        }
        /// The real top line after shaping.
        fn top(&mut self) -> usize {
            self.layout();
            self.0.buffer().scroll().line
        }
    }

    fn at(line: usize) -> Cursor {
        Cursor {
            position: Position { line, column: 0 },
            selection: None,
        }
    }

    #[test]
    fn mid_document_carry_lands_within_one_line() {
        let mut h = Harness::new(200);
        fresh_view(&mut h, at(0), 0);
        scroll_content(&mut h, 0, 50);
        assert!(h.top().abs_diff(50) <= 1, "landed on {}", h.top());
        // A second carry from the tracked top (back up) also lands.
        scroll_content(&mut h, 50, 20);
        assert!(h.top().abs_diff(20) <= 1, "landed on {}", h.top());
    }

    #[test]
    fn fresh_view_restores_the_tracked_top() {
        // Tab resume: a fresh editor with the kept cursor inside the kept view.
        let mut h = Harness::new(200);
        fresh_view(&mut h, at(60), 50);
        assert!(h.top().abs_diff(50) <= 1, "landed on {}", h.top());
        // The kept cursor out of the kept view (wheel-scrolled away) does not pull it back.
        let mut h = Harness::new(200);
        fresh_view(&mut h, at(5), 50);
        assert!(h.top().abs_diff(50) <= 1, "landed on {}", h.top());
    }

    #[test]
    fn go_to_end_carries_to_the_end() {
        let mut h = Harness::new(200);
        fresh_view(&mut h, at(0), 0);
        // `scroll_to_edge(true)`: overshoot by the line count, clamped by shaping.
        scroll_content(&mut h, 0, 201);
        let top = h.top();
        assert!(top.abs_diff(201 - VISIBLE) <= 1, "landed on {top}");
    }
}
