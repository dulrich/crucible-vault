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

// Plan note-gone-restore-edges WP-1.

/// An `id: None` notification at `path` (the daemon's id store failed to record it).
fn idless(app: &mut App, path: &str, kind: cv_protocol::ChangeKind) {
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            id: None,
            path: path.to_owned(),
            kind,
            hash: None,
            origin: cv_protocol::Origin::Daemon,
            from: None,
            replaced_by: None,
        },
    )));
}

/// Deletes `path` (fed: gone), recreates it with `body` and drops the recreate's notifications.
fn gone_then_recreated_silently(app: &mut App, d: &FakeDaemon, path: &str, body: &str) {
    let rx = gone_then_recreated(app, d, path, body);
    let _ = rx.try_iter().count();
}

/// Lands `job` (a `keep_mine` / `begin_save` result) through `update` only: completions queued.
fn land_unpumped(app: &mut App, d: &FakeDaemon, job: crate::source::SaveJob) {
    let outcome = job.run(d);
    let _ = app.update(job.done(outcome));
}

fn queued(app: &mut App) -> Vec<Message> {
    match &mut app.rpc_exec {
        crate::app::RpcExec::Queue(q) => q.drain(..).collect(),
        crate::app::RpcExec::Pool => Vec::new(),
    }
}

/// Case 1: an id-less event restores a clean and a dirty gone document; a later save lands on
/// the new id.
#[test]
fn an_idless_event_restores_a_gone_document() {
    for dirty in [false, true] {
        let (_tmp, d, mut app) = setup(&["ge1/a.md"]);
        let old = app.note_id("ge1/a.md").expect("bound");
        if dirty {
            type_x(&mut app);
        }
        gone_then_recreated_silently(&mut app, &d, "ge1/a.md", "restored");
        idless(&mut app, "ge1/a.md", cv_protocol::ChangeKind::Created);
        let new = d.bind_note("ge1/a.md").expect("new id");
        assert_ne!(new, old);
        assert_eq!(app.note_id("ge1/a.md"), Some(new));
        assert!(app.docs.by_id(old).is_none());
        assert_eq!(tab_notes(&app), [new]);
        let doc = app.doc("ge1/a.md").expect("open");
        assert!(!doc.is_missing());
        if dirty {
            assert_eq!(doc.text(), "xbody");
            assert!(matches!(doc.conflict(), Some(Conflict::Changed(_))));
            let job = app
                .doc_mut("ge1/a.md")
                .and_then(|b| b.keep_mine())
                .expect("keep");
            assert_eq!(job.id, new);
            let outcome = job.run(d.as_ref());
            let _ = app.send(job.done(outcome));
            assert_eq!(d.read_file("ge1/a.md").expect("a").0, b"xbody");
        } else {
            assert_eq!(doc.text(), "restored");
            assert!(doc.conflict().is_none());
            type_x(&mut app);
            let job = app
                .doc_mut("ge1/a.md")
                .and_then(|b| b.begin_save())
                .expect("save");
            assert_eq!(job.id, new);
            let outcome = job.run(d.as_ref());
            let _ = app.send(job.done(outcome));
            assert_eq!(d.read_file("ge1/a.md").expect("a").0, b"xrestored");
        }
        let doc = app.doc("ge1/a.md").expect("open");
        assert!(!doc.is_dirty() && doc.conflict().is_none());
    }
}

/// Case 1: an id-less event whose restore open fails (no file at the path) leaves it gone.
#[test]
fn an_idless_event_with_a_failed_restore_stays_gone() {
    let (_tmp, d, mut app) = setup(&["ge2/a.md"]);
    let old = app.note_id("ge2/a.md").expect("bound");
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete("ge2/a.md").expect("delete");
    feed(&mut app, &rx);
    idless(&mut app, "ge2/a.md", cv_protocol::ChangeKind::Modified);
    assert_eq!(app.note_id("ge2/a.md"), Some(old));
    assert!(app.doc("ge2/a.md").is_some_and(|b| b.is_missing()), "gone");
}

/// Unchanged: an id-less change on a live document reloads it in place (same id).
#[test]
fn an_idless_event_on_a_live_document_reloads_as_before() {
    let (_tmp, d, mut app) = setup(&["ge3/a.md"]);
    let id = app.note_id("ge3/a.md").expect("bound");
    std::fs::write(d.vault_root().join("ge3/a.md"), "edited").expect("write");
    idless(&mut app, "ge3/a.md", cv_protocol::ChangeKind::Modified);
    assert_eq!(app.note_id("ge3/a.md"), Some(id));
    assert_eq!(app.doc("ge3/a.md").expect("open").text(), "edited");
}

/// Case 2: Keep on a gone document whose path is occupied (no event fed) restores it onto the
/// new note behind `Changed`, text kept; Keep mine then writes the edits there.
#[test]
fn keep_on_an_occupied_path_restores_then_keep_writes() {
    let (_tmp, d, mut app) = setup(&["ge4/a.md"]);
    let old = app.note_id("ge4/a.md").expect("bound");
    type_x(&mut app);
    gone_then_recreated_silently(&mut app, &d, "ge4/a.md", "restored");
    let job = app
        .doc_mut("ge4/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep");
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    let new = d.bind_note("ge4/a.md").expect("new id");
    assert_ne!(new, old);
    assert_eq!(app.note_id("ge4/a.md"), Some(new));
    assert_eq!(d.read_file("ge4/a.md").expect("a").0, b"restored");
    let doc = app.doc("ge4/a.md").expect("open");
    assert_eq!(doc.text(), "xbody");
    assert!(matches!(doc.conflict(), Some(Conflict::Changed(_))));
    let job = app
        .doc_mut("ge4/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep");
    assert_eq!(job.id, new);
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    assert_eq!(d.read_file("ge4/a.md").expect("a").0, b"xbody");
    assert!(app.doc("ge4/a.md").is_some_and(|b| !b.is_dirty()));
}

/// Case 2: the restore event arrives while the Keep save is in flight.
#[test]
fn a_restore_event_during_keep_ends_on_the_new_note() {
    let (_tmp, d, mut app) = setup(&["ge5/a.md"]);
    type_x(&mut app);
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete("ge5/a.md").expect("delete");
    feed(&mut app, &rx);
    let job = app
        .doc_mut("ge5/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep");
    d.create_note("ge5/a.md", "restored").expect("restore");
    feed(&mut app, &rx);
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    let new = d.bind_note("ge5/a.md").expect("new id");
    assert_eq!(app.note_id("ge5/a.md"), Some(new));
    assert_eq!(tab_notes(&app), [new]);
    assert_eq!(d.read_file("ge5/a.md").expect("a").0, b"restored");
    let doc = app.doc("ge5/a.md").expect("open");
    assert_eq!(doc.text(), "xbody");
    assert!(matches!(doc.conflict(), Some(Conflict::Changed(_))));
}

/// Audit E2: after a Keep conflict the document stays gone until the restore open lands.
#[test]
fn keep_conflict_stays_gone_until_the_restore_lands() {
    let (_tmp, d, mut app) = setup(&["ge6/a.md"]);
    let old = app.note_id("ge6/a.md").expect("bound");
    type_x(&mut app);
    gone_then_recreated_silently(&mut app, &d, "ge6/a.md", "restored");
    let job = app
        .doc_mut("ge6/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep");
    land_unpumped(&mut app, &d, job);
    let pending = queued(&mut app);
    assert!(!pending.is_empty(), "restore open pending");
    assert_eq!(app.note_id("ge6/a.md"), Some(old));
    assert!(app.doc("ge6/a.md").is_some_and(|b| b.is_missing()), "gone");
    for m in pending {
        let _ = app.send(m);
    }
    let new = d.bind_note("ge6/a.md").expect("new id");
    assert_eq!(app.note_id("ge6/a.md"), Some(new));
    assert!(app.doc("ge6/a.md").is_some_and(|b| !b.is_missing()));
}

/// Audit E2: a Keep conflict whose restore open fails stays gone; a later Keep restores.
#[test]
fn keep_conflict_with_a_failed_restore_stays_gone_and_retries() {
    let (_tmp, d, mut app) = setup(&["ge7/a.md"]);
    let old = app.note_id("ge7/a.md").expect("bound");
    type_x(&mut app);
    gone_then_recreated_silently(&mut app, &d, "ge7/a.md", "restored");
    let job = app
        .doc_mut("ge7/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep");
    land_unpumped(&mut app, &d, job);
    for m in queued(&mut app) {
        let m = match m {
            Message::NoteOpened {
                path, req, purpose, ..
            } => Message::NoteOpened {
                path,
                req,
                purpose,
                result: Err(crate::app::settle::OpenFailed::Other("down".into())),
            },
            m => m,
        };
        let _ = app.send(m);
    }
    assert_eq!(app.note_id("ge7/a.md"), Some(old));
    assert!(app.doc("ge7/a.md").is_some_and(|b| b.is_missing()), "gone");
    let job = app
        .doc_mut("ge7/a.md")
        .and_then(|b| b.keep_mine())
        .expect("keep again");
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    let new = d.bind_note("ge7/a.md").expect("new id");
    assert_eq!(app.note_id("ge7/a.md"), Some(new));
    assert_eq!(d.read_file("ge7/a.md").expect("a").0, b"restored");
    assert_eq!(app.doc("ge7/a.md").expect("open").text(), "xbody");
}

/// Opens `a` and `b` in the left pane and `a` alone in a right pane (typed into when `dirty`),
/// deletes `a` and moves `b` onto its path (both fed).
fn gone_beside_open(prefix: &str, dirty: bool) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    use super::shared_docs::{quick, split};
    let (a, b) = (format!("{prefix}/a.md"), format!("{prefix}/b.md"));
    let (tmp, d, mut app) = setup(&[a.as_str(), b.as_str()]);
    let left = app.focus;
    split(&mut app);
    quick(&mut app, crate::header::QuickSwitchOpen::Duplicate, &a);
    if dirty {
        let _ = app.send(Message::Source(SourceMsg::Action(
            iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert('x')),
        )));
    }
    app.focus = left;
    let rx = d.subscribe(&[Topic::FileChanged]).expect("sub");
    d.delete(&a).expect("delete");
    feed(&mut app, &rx);
    assert!(app.doc(&a).is_some_and(|t| t.is_missing()));
    let _ = d.daemon_move(&b, &a).expect("move");
    feed(&mut app, &rx);
    (tmp, d, app)
}

/// Case 3 (audit E1: through the real `Renamed` of the open note): a clean gone document's
/// tabs move onto the open note in every pane, and no pane keeps two tabs of it.
#[test]
fn a_clean_gone_document_moves_onto_the_open_note_renamed_there() {
    let (_tmp, d, app) = gone_beside_open("ge8", false);
    let b = d.bind_note("ge8/a.md").expect("moved note");
    assert_eq!(app.note_id("ge8/a.md"), Some(b));
    assert_eq!(app.docs.iter().count(), 1, "the gone document dropped");
    for (_, ps) in app.panes.iter() {
        let notes: Vec<_> = ps.tabs.iter().filter_map(|t| t.note()).collect();
        assert_eq!(notes, [b], "one tab of the open note per pane");
    }
}

/// Case 3: a dirty gone document stays gone; Keep sets a status error and writes nothing.
#[test]
fn a_dirty_gone_document_beside_the_open_note_stays_gone_and_keep_refuses() {
    let (_tmp, d, mut app) = gone_beside_open("ge9", true);
    let b = d.bind_note("ge9/a.md").expect("moved note");
    let gone = app
        .docs
        .iter()
        .find(|(_, od)| od.buffer.is_missing())
        .map(|(_, od)| od.id)
        .expect("still gone");
    assert_ne!(gone, b);
    let _ = app.send(Message::Source(SourceMsg::KeepMine(gone)));
    assert!(app.status.error_text().is_some(), "status error");
    let od = app.docs.by_id(gone).expect("kept");
    assert!(od.buffer.is_missing() && od.buffer.is_dirty() && !od.buffer.is_saving());
    assert_eq!(d.read_file("ge9/a.md").expect("a").0, b"body");
    let live = app.docs.by_id(b).expect("open note");
    assert!(!live.buffer.is_dirty() && live.buffer.conflict().is_none());
    assert_eq!(live.buffer.text(), "body");
}
