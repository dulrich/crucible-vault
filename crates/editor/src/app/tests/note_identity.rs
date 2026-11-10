//! Plan note-identity, WP-4 (D3/D4, audit E1/E2): open documents are keyed by note id, a move is a
//! path-field update, and every notification and completion is matched by id.

use std::sync::mpsc::Receiver;
use std::time::Instant;

use super::rename_order::{Order, deliver};
use super::unsaved_dialog::{save, setup, type_x};
use super::*;
use crate::app::NoteId;
use crate::daemon::Topic;
use crate::message::{ExplorerMsg, SourceMsg};
use iced::widget::pane_grid::Pane;

fn file_changes(rx: &Receiver<Notification>) -> Vec<Notification> {
    rx.try_iter()
        .filter(|n| matches!(n, Notification::FileChanged { .. }))
        .collect()
}

fn feed(app: &mut App, ns: Vec<Notification>) {
    for n in ns {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
}

/// Audit E1: a daemon-side move delivers its full pair (`Renamed { from }`, then the source
/// `Deleted` carrying the same id). The `Deleted` half is not a delete of the moved document: it
/// stays live at the new path, keeps its id, and a later edit saves and lands there.
#[test]
fn the_rename_pairs_deleted_half_never_deletes_the_moved_document() {
    let (_tmp, d, mut app) = setup(&["ni1/a.md"]);
    let id = app.note_id("ni1/a.md").expect("bound");
    let pair = d.daemon_move("ni1/a.md", "ni1/b.md").expect("move");
    assert!(
        pair.iter().any(|n| matches!(
            n,
            Notification::FileChanged {
                kind: cv_protocol::ChangeKind::Deleted,
                ..
            }
        )),
        "the production pair"
    );
    feed(&mut app, pair);
    assert_eq!(app.open_paths(), ["ni1/b.md"]);
    assert_eq!(app.note_id("ni1/b.md"), Some(id));
    assert_eq!(app.status.error_text(), None, "no `file deleted on disk`");
    let doc = app.doc("ni1/b.md").expect("live");
    assert!(!doc.is_missing() && doc.conflict().is_none());
    type_x(&mut app);
    save(&mut app, &d, "ni1/b.md");
    assert_eq!(d.read_file("ni1/b.md").expect("b").0, b"xbody");
    assert!(d.read_file("ni1/a.md").is_err(), "nothing recreated at a");
}

/// Audit E1, every order: a title rename's reply landing before, after, or between the halves of
/// the pair leaves the document live at the new path, and a later edit lands on disk there.
fn title_rename_then_save_scenario(order: Order) {
    let (_tmp, d) = fake_arc();
    d.create_note("ni2/Old.md", "---\ntitle: Old\n---\nx\n")
        .expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(crate::message::ExplorerMsg::Open(
        "ni2/Old.md".into(),
    )));
    let id = app.note_id("ni2/Old.md").expect("bound");
    let doc = app.doc_mut("ni2/Old.md").expect("open");
    doc.select_range((1, 7), (1, 10));
    assert!(doc.paste("New".into(), Instant::now()));
    app.rpc_exec = crate::app::RpcExec::Pool; // hold the rename RPC
    let job = app
        .doc_mut("ni2/Old.md")
        .and_then(|t| t.begin_save())
        .expect("save starts");
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    let seq = app.doc("ni2/Old.md").expect("at from").edit_seq();
    let pair = d.daemon_move("ni2/Old.md", "ni2/New.md").expect("rename");
    deliver(&mut app, order, pair, |app| {
        let _ = app.send(Message::TitleRenamed {
            id,
            from: "ni2/Old.md".into(),
            to: "ni2/New.md".into(),
            seq,
            result: Ok("ni2/New.md".into()),
        });
    });
    app.queue_rpcs();
    assert_eq!(app.open_paths(), ["ni2/New.md"], "{order:?}");
    assert_eq!(app.note_id("ni2/New.md"), Some(id));
    let doc = app.doc_mut("ni2/New.md").expect("followed");
    assert!(!doc.is_renaming() && !doc.is_saving());
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("typed".into(), Instant::now()));
    save(&mut app, &d, "ni2/New.md");
    assert_eq!(
        d.read_file("ni2/New.md").expect("new").0,
        b"---\ntitle: New\n---\ntyped\n",
        "{order:?}"
    );
    assert!(d.read_file("ni2/Old.md").is_err(), "{order:?}");
}

#[test]
fn title_rename_then_save_reply_first() {
    title_rename_then_save_scenario(Order::ReplyFirst);
}

#[test]
fn title_rename_then_save_notification_first() {
    title_rename_then_save_scenario(Order::NotificationFirst);
}

#[test]
fn title_rename_then_save_reply_between_the_pair() {
    title_rename_then_save_scenario(Order::ReplyBetween);
}

/// Audit E2: a replaced note (production shape, with ids): its tab moves to the absorbing note's
/// document (bound to the absorber's id, never the retired one), and an edit there saves and
/// lands in the absorbing note.
#[test]
fn a_replaced_notes_tab_moves_to_the_absorbing_document_and_saves_there() {
    let (_tmp, d, mut app) = setup(&["ni3/b.md"]);
    d.create_note("ni3/t.md", "target").expect("target");
    let retired = app.note_id("ni3/b.md").expect("bound");
    let absorber = d.bind_note("ni3/t.md").expect("t id");
    let ns = d
        .daemon_delete_replaced("ni3/b.md", "ni3/t.md")
        .expect("replaced");
    feed(&mut app, ns);
    assert_eq!(app.open_paths(), ["ni3/t.md"]);
    assert_eq!(app.note_id("ni3/t.md"), Some(absorber));
    assert!(
        app.docs.by_id(retired).is_none(),
        "retired document dropped"
    );
    assert_eq!(app.status.error_text(), None);
    type_x(&mut app);
    save(&mut app, &d, "ni3/t.md");
    assert_eq!(d.read_file("ni3/t.md").expect("t").0, b"xtarget");
}

/// D3: "Keep mine" on a deleted note writes by id with the "no file" base, reviving the same id
/// at the document's path.
#[test]
fn keep_mine_on_a_deleted_note_keeps_its_id() {
    let (_tmp, d, mut app) = setup(&["ni4/a.md"]);
    let id = app.note_id("ni4/a.md").expect("bound");
    type_x(&mut app);
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete("ni4/a.md").expect("delete");
    feed(&mut app, file_changes(&rx));
    assert!(app.doc("ni4/a.md").is_some_and(|b| b.is_missing()));
    let job = app
        .doc_mut("ni4/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep mine");
    assert_eq!(job.id, id, "addressed to the same note");
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    assert_eq!(d.read_file("ni4/a.md").expect("revived").0, b"xbody");
    assert_eq!(d.bind_note("ni4/a.md").ok(), Some(id), "same id revived");
    assert_eq!(app.note_id("ni4/a.md"), Some(id));
    assert!(app.doc("ni4/a.md").is_some_and(|b| !b.is_dirty()));
}

/// Path reuse: a document whose note was deleted is not confused with a new note created at the
/// same path (another id): the new note's notifications leave it alone and its save never lands
/// on the new note.
#[test]
fn a_new_note_at_a_deleted_notes_path_is_another_note() {
    let (_tmp, d, mut app) = setup(&["ni6/a.md"]);
    let id = app.note_id("ni6/a.md").expect("bound");
    type_x(&mut app);
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete("ni6/a.md").expect("delete");
    d.create_note("ni6/a.md", "new note").expect("reuse");
    feed(&mut app, file_changes(&rx));
    let other = d.bind_note("ni6/a.md").expect("new id");
    assert_ne!(other, id);
    let doc = app.doc("ni6/a.md").expect("kept");
    assert_eq!(
        doc.text(),
        "xbody",
        "edits kept, not replaced by the new note"
    );
    assert!(doc.is_missing(), "its own note is gone");
    assert_eq!(app.note_id("ni6/a.md"), Some(id));
    // Keep mine revives the retired id at the path, which the new note now holds: refused.
    let job = app
        .doc_mut("ni6/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep mine");
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    assert_eq!(d.read_file("ni6/a.md").expect("new").0, b"new note");
}

/// The tab (pane, index) whose document is `id`.
fn tab_of(app: &App, id: NoteId) -> (Pane, usize) {
    app.panes
        .iter()
        .find_map(|(p, ps)| {
            ps.tabs
                .iter()
                .position(|t| t.note() == Some(id))
                .map(|i| (*p, i))
        })
        .expect("a tab of the document")
}

/// Focuses the tab of document `id`.
fn focus_doc(app: &mut App, id: NoteId) {
    let (p, i) = tab_of(app, id);
    app.focus = p;
    app.panes.get_mut(p).expect("pane").activate(i);
}

/// Two open documents at one path: a dirty document whose note was deleted (missing state),
/// then another open note moved onto its path. Returns (missing id, moved id).
fn two_docs_at_one_path(prefix: &str) -> (tempfile::TempDir, Arc<FakeDaemon>, App, NoteId, NoteId) {
    let a = format!("{prefix}/a.md");
    let b = format!("{prefix}/b.md");
    let (tmp, d, mut app) = setup(&[a.as_str()]);
    let gone = app.note_id(&a).expect("a bound");
    type_x(&mut app);
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete(&a).expect("delete");
    feed(&mut app, file_changes(&rx));
    assert!(app.doc(&a).is_some_and(|b| b.is_missing()));
    d.create_note(&b, "other").expect("b");
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(b.clone())));
    let moved = app.note_id(&b).expect("b bound");
    let pair = d.daemon_move(&b, &a).expect("move onto a");
    feed(&mut app, pair);
    assert_eq!(app.docs.len(), 2, "both documents open");
    assert_eq!(app.docs.path_of(gone).as_deref(), Some(a.as_str()));
    assert_eq!(app.docs.path_of(moved).as_deref(), Some(a.as_str()));
    (tmp, d, app, gone, moved)
}

fn text_of(app: &App, id: NoteId) -> String {
    app.docs.by_id(id).expect("open").buffer.text()
}

/// Plan note-identity, WP-4: with two documents at one path each tab shows and edits its own
/// document, and the path lookups take the live one (the same-path rule).
#[test]
fn two_documents_at_one_path_each_tab_edits_its_own() {
    let (_tmp, _d, mut app, gone, moved) = two_docs_at_one_path("ni7");
    assert_eq!(app.note_id("ni7/a.md"), Some(moved), "the live one wins");
    focus_doc(&mut app, gone);
    assert_eq!(app.active_doc().map(|b| b.text()), Some("xbody".into()));
    type_x(&mut app);
    assert_eq!(text_of(&app, gone), "xxbody");
    assert_eq!(text_of(&app, moved), "other");
    focus_doc(&mut app, moved);
    assert_eq!(app.active_doc().map(|b| b.text()), Some("other".into()));
    type_x(&mut app);
    assert_eq!(text_of(&app, moved), "xother");
    assert_eq!(text_of(&app, gone), "xxbody");
}

/// Each document's save addresses its own id: the moved note saves normally, and "Keep mine" on
/// the missing one (whose path another note now holds) is refused (D3), never landing on it.
#[test]
fn two_documents_at_one_path_save_by_their_own_ids() {
    let (_tmp, d, mut app, gone, moved) = two_docs_at_one_path("ni8");
    focus_doc(&mut app, moved);
    type_x(&mut app);
    let job = app
        .active_doc_mut()
        .and_then(|b| b.begin_save())
        .expect("save");
    assert_eq!(job.id, moved);
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    assert_eq!(d.read_file("ni8/a.md").expect("a").0, b"xother");
    focus_doc(&mut app, gone);
    let _ = app.send(Message::Source(SourceMsg::KeepMine(gone)));
    app.pump();
    assert_eq!(d.read_file("ni8/a.md").expect("a").0, b"xother", "refused");
    assert_eq!(text_of(&app, gone), "xbody", "edits kept");
    assert!(!app.docs.by_id(moved).expect("open").buffer.is_dirty());
}

/// Closing one tab of two same-path documents drops that document only; the other stays open
/// and intact.
#[test]
fn closing_one_same_path_tab_leaves_the_other_document() {
    let (_tmp, d, mut app, gone, moved) = two_docs_at_one_path("ni9");
    // Take theirs on the missing one closes its tabs and drops it — not the moved note.
    focus_doc(&mut app, gone);
    let _ = app.send(Message::Source(SourceMsg::TakeTheirs(gone)));
    assert!(app.docs.by_id(gone).is_none(), "missing document dropped");
    assert_eq!(text_of(&app, moved), "other", "the other intact");
    assert_eq!(app.open_paths(), ["ni9/a.md"]);
    let (p, i) = tab_of(&app, moved);
    let tab = app.panes.get(p).expect("pane").tabs[i].id;
    let _ = app.close_tab(tab);
    assert!(app.docs.is_empty());
    assert_eq!(d.read_file("ni9/a.md").expect("a").0, b"other");
    // And the reverse: closing the moved one's tab keeps the dirty missing document.
    let (_tmp, _d, mut app, gone, moved) = two_docs_at_one_path("ni10");
    let (p, i) = tab_of(&app, moved);
    let tab = app.panes.get(p).expect("pane").tabs[i].id;
    let _ = app.close_tab(tab);
    assert!(app.docs.by_id(moved).is_none(), "the clean one closed");
    assert_eq!(text_of(&app, gone), "xbody", "the missing one kept");
}

/// A tab moved to another pane keeps its document (by id), and a restored session binds its
/// note tabs.
#[test]
fn a_moved_tab_keeps_its_document_and_restore_binds_tabs() {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note("ni11/a.md", "body").expect("create");
    let state = tmp.path().join("editor.json");
    let id = {
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), Some(state.clone()));
        app.queue_rpcs();
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("ni11/a.md".into())));
        let id = app.note_id("ni11/a.md").expect("bound");
        let from = tab_of(&app, id);
        let (to, _) = app
            .panes
            .split(
                crate::panes::axis_of(crate::ui_state::SplitDir::Right),
                from.0,
                crate::panes::PaneState::default(),
            )
            .expect("split");
        app.move_tab(from, to, None);
        assert_eq!(tab_of(&app, id).0, to, "moved");
        app.focus = to;
        type_x(&mut app);
        assert_eq!(text_of(&app, id), "xbody");
        let _ = app.send(Message::PersistDue);
        id
    };
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let app = App::new(Some(dc), Some(state));
    assert_eq!(app.note_id("ni11/a.md"), Some(id));
    let (p, i) = tab_of(&app, id);
    assert_eq!(
        app.panes.get(p).expect("pane").tabs[i].path(&app.docs),
        Some("ni11/a.md")
    );
}

/// Plan WP-R5 (review E3): the delete of an open document goes by its id (WP-R2), so a note the
/// daemon moved before the editor heard of it is previewed and deleted at its current path.
#[test]
fn a_by_id_delete_of_a_moved_document_acts_on_its_current_path() {
    let (_tmp, d, mut app) = setup(&["ni11/a.md"]);
    let _pair = d.daemon_move("ni11/a.md", "ni11/b.md").expect("move");
    // The move's notifications are not delivered: the document still says `a.md`.
    assert!(app.note_id("ni11/a.md").is_some());
    let _ = app.begin_delete("ni11/a.md".to_owned());
    app.pump();
    let confirm = match app.dialog.as_ref() {
        Some(crate::dialog::Dialog::DeleteConfirm(c)) => c.path.clone(),
        other => panic!("delete confirm expected, got {other:?}"),
    };
    assert_eq!(
        confirm, "ni11/b.md",
        "the preview names the note's current path"
    );
    let _ = app.send(Message::Dialog(crate::dialog::DialogMsg::Ok));
    app.pump();
    assert!(
        matches!(d.read_file("ni11/b.md"), Err(DaemonError::NotFound(_))),
        "deleted where it lives"
    );
}
