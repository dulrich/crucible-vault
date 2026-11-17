//! Plan note-gone-restore WP-1: a file restored at a gone document's path (a fresh note id)
//! clears the gone state — clean → the restored bytes, dirty → the changed-on-disk bar with the
//! text kept — through a `FileChanged` (trigger a); reconnect resync is in `reconnect.rs`.

use std::sync::mpsc::Receiver;

use super::shared_docs::{duplicated, views};
use super::unsaved_dialog::{setup, type_x};
use super::*;
use crate::app::NoteId;
use crate::daemon::Topic;
use crate::message::SourceMsg;
use crate::source::Conflict;

fn feed(app: &mut App, rx: &Receiver<Notification>) {
    let ns: Vec<_> = rx
        .try_iter()
        .filter(|n| matches!(n, Notification::FileChanged { .. }))
        .collect();
    for n in ns {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
}

/// Deletes `path` (fed: the document goes gone), then recreates it with `body`; returns the
/// subscription with the recreate's notifications still unfed.
fn gone_then_recreated(
    app: &mut App,
    d: &FakeDaemon,
    path: &str,
    body: &str,
) -> Receiver<Notification> {
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete(path).expect("delete");
    feed(app, &rx);
    assert!(app.doc(path).is_some_and(|b| b.is_missing()), "gone");
    d.create_note(path, body).expect("restore");
    rx
}

fn tab_notes(app: &App) -> Vec<NoteId> {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter().filter_map(|t| t.note()))
        .collect()
}

#[test]
fn a_clean_gone_note_switches_to_the_restored_file() {
    let (_tmp, d, mut app) = setup(&["gr1/a.md"]);
    let old = app.note_id("gr1/a.md").expect("bound");
    let rx = gone_then_recreated(&mut app, &d, "gr1/a.md", "restored");
    feed(&mut app, &rx);
    let new = d.bind_note("gr1/a.md").expect("new id");
    assert_ne!(new, old);
    assert_eq!(app.note_id("gr1/a.md"), Some(new));
    assert!(app.docs.by_id(old).is_none(), "retired document dropped");
    assert_eq!(tab_notes(&app), [new], "the tab shows the new note");
    let doc = app.doc("gr1/a.md").expect("open");
    assert!(!doc.is_missing() && doc.conflict().is_none() && !doc.is_dirty());
    assert_eq!(doc.text(), "restored");
}

#[test]
fn a_dirty_gone_note_gets_the_changed_bar_and_keep_mine_writes_over() {
    let (_tmp, d, mut app) = setup(&["gr2/a.md"]);
    type_x(&mut app);
    let rx = gone_then_recreated(&mut app, &d, "gr2/a.md", "restored");
    feed(&mut app, &rx);
    let new = d.bind_note("gr2/a.md").expect("new id");
    assert_eq!(tab_notes(&app), [new]);
    let disk = d.read_file("gr2/a.md").expect("restored").1;
    let doc = app.doc("gr2/a.md").expect("open");
    assert!(!doc.is_missing());
    assert_eq!(doc.conflict(), Some(&Conflict::Changed(disk)));
    assert!(doc.is_dirty());
    assert_eq!(doc.text(), "xbody", "unsaved text kept");
    let job = app
        .doc_mut("gr2/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep mine");
    assert_eq!(job.id, new);
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    assert_eq!(d.read_file("gr2/a.md").expect("a").0, b"xbody");
    assert_eq!(
        d.bind_note("gr2/a.md").ok(),
        Some(new),
        "written to the restored note"
    );
    let doc = app.doc("gr2/a.md").expect("open");
    assert!(!doc.is_dirty() && doc.conflict().is_none());
}

#[test]
fn a_dirty_gone_note_take_theirs_loads_the_restored_file() {
    let (_tmp, d, mut app) = setup(&["gr3/a.md"]);
    type_x(&mut app);
    let rx = gone_then_recreated(&mut app, &d, "gr3/a.md", "restored");
    feed(&mut app, &rx);
    let new = d.bind_note("gr3/a.md").expect("new id");
    let _ = app.send(Message::Source(SourceMsg::TakeTheirs(new)));
    app.pump();
    let doc = app.doc("gr3/a.md").expect("open");
    assert_eq!(doc.text(), "restored");
    assert!(!doc.is_dirty() && doc.conflict().is_none() && !doc.is_missing());
    assert_eq!(app.note_id("gr3/a.md"), Some(new));
}

/// Audit E1: a gone document shown in two panes keeps both tabs, repointed to the new note.
#[test]
fn a_restore_keeps_every_tab_of_the_gone_document() {
    let (_tmp, d, mut app, _left, _right) = duplicated();
    let old = app.note_id("w7/a.md").expect("bound");
    let rx = gone_then_recreated(&mut app, &d, "w7/a.md", "restored");
    feed(&mut app, &rx);
    let new = d.bind_note("w7/a.md").expect("new id");
    assert_ne!(new, old);
    assert_eq!(views(&app, "w7/a.md").len(), 2, "both tabs survive");
    assert_eq!(tab_notes(&app), [new, new]);
    assert!(!app.doc("w7/a.md").expect("open").is_missing());
}

/// A note created at an unrelated path, or a new note where the open document is not gone,
/// leaves the document alone.
#[test]
fn a_new_note_elsewhere_or_at_a_live_document_changes_nothing() {
    let (_tmp, d, mut app) = setup(&["gr4/a.md"]);
    let id = app.note_id("gr4/a.md").expect("bound");
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete("gr4/a.md").expect("delete");
    feed(&mut app, &rx);
    d.create_note("gr4/b.md", "other").expect("elsewhere");
    feed(&mut app, &rx);
    assert!(
        app.doc("gr4/a.md").is_some_and(|b| b.is_missing()),
        "still gone"
    );
    assert_eq!(app.note_id("gr4/a.md"), Some(id));
    assert_eq!(tab_notes(&app), [id]);

    let (_tmp2, d2, mut app2) = setup(&["gr5/a.md"]);
    let id2 = app2.note_id("gr5/a.md").expect("bound");
    let rx2 = d2.subscribe(&[Topic::FileChanged]).expect("sub");
    let other = d2.bind_note("gr5/a.md").expect("same note");
    let _ = app2.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            id: Some(NoteId(other.0 + 1000)),
            path: "gr5/a.md".to_owned(),
            kind: cv_protocol::ChangeKind::Created,
            hash: None,
            origin: cv_protocol::Origin::Daemon,
            from: None,
            replaced_by: None,
        },
    )));
    feed(&mut app2, &rx2);
    assert_eq!(app2.note_id("gr5/a.md"), Some(id2));
    assert_eq!(tab_notes(&app2), [id2]);
    assert!(app2.doc("gr5/a.md").is_some_and(|b| !b.is_missing()));
}

/// Audit E2: the gone document's save is in flight when its file is restored. The restore waits
/// for the save's completion (which lands as missing on the retired id, writing nothing), then
/// runs: the edits are kept behind the changed bar, the restored file is untouched.
#[test]
fn a_restore_waits_for_the_gone_documents_in_flight_save() {
    let (_tmp, d, mut app) = setup(&["gr6/a.md"]);
    let old = app.note_id("gr6/a.md").expect("bound");
    type_x(&mut app);
    let job = app
        .doc_mut("gr6/a.md")
        .and_then(|b| b.begin_save())
        .expect("save in flight");
    let rx = gone_then_recreated(&mut app, &d, "gr6/a.md", "restored");
    feed(&mut app, &rx);
    assert_eq!(
        app.note_id("gr6/a.md"),
        Some(old),
        "deferred behind the save"
    );
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    let new = d.bind_note("gr6/a.md").expect("new id");
    assert_ne!(new, old);
    assert_eq!(app.note_id("gr6/a.md"), Some(new));
    assert_eq!(tab_notes(&app), [new]);
    assert_eq!(
        d.read_file("gr6/a.md").expect("a").0,
        b"restored",
        "no write"
    );
    let doc = app.doc("gr6/a.md").expect("open");
    assert_eq!(doc.text(), "xbody");
    assert!(doc.is_dirty() && matches!(doc.conflict(), Some(Conflict::Changed(_))));
}
