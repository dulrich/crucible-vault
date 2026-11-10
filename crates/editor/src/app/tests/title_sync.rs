//! `title` → filename sync.

use std::time::Instant;

use super::*;
use crate::message::ExplorerMsg;

const OLD: &str = "---\ntitle: Old\n---\nx\n";

/// Opens `path` and rewrites the `title` value `Old` (line 1, cols 7..10) to `new`.
fn open_and_retitle(d: &Arc<FakeDaemon>, path: &str, new: &str) -> App {
    d.create_note(path, OLD).expect("create");
    let mut app = app_over(d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(path.into())));
    let doc = app.doc_mut(path).expect("open");
    doc.select_range((1, 7), (1, 10));
    assert!(doc.paste(new.into(), Instant::now()));
    app
}

fn save(app: &mut App, d: &FakeDaemon, path: &str) {
    let job = app
        .doc_mut(path)
        .and_then(|t| t.begin_save())
        .expect("save starts");
    let outcome = job.run(d);
    let _ = app.send(job.done(outcome));
}

fn gone(d: &FakeDaemon, path: &str) -> bool {
    matches!(d.read_file(path), Err(DaemonError::NotFound(_)))
}

#[test]
fn title_edit_renames_a_note_named_after_its_title() {
    let (_tmp, d) = fake_arc();
    let mut app = open_and_retitle(&d, "ts/Old.md", "New? One");
    save(&mut app, &d, "ts/Old.md");
    assert!(gone(&d, "ts/Old.md"));
    let (bytes, _) = d.read_file("ts/New One.md").expect("renamed");
    assert_eq!(bytes, b"---\ntitle: New? One\n---\nx\n");
    assert_eq!(app.open_paths(), ["ts/New One.md"], "the tab follows");
    assert!(app.doc("ts/Old.md").is_none(), "doc key moved");
    assert!(app.doc("ts/New One.md").is_some_and(|t| !t.is_dirty()));
    assert!(app.explorer.contains("ts/New One.md"));
    // A further save with the same title does not rename again.
    let doc = app.doc_mut("ts/New One.md").expect("open");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("y".into(), Instant::now()));
    save(&mut app, &d, "ts/New One.md");
    assert_eq!(app.open_paths(), ["ts/New One.md"]);
}

/// Plan round-7-feedback-decisions WP-2: the title-sync rename of a (now clean) note keeps
/// cursor, selection, undo depth and top_line, with no conflict.
#[test]
fn title_sync_rename_keeps_editor_state() {
    let (_tmp, d) = fake_arc();
    let mut app = open_and_retitle(&d, "r7ts/Old.md", "Fresh");
    let doc = app.doc_mut("r7ts/Old.md").expect("open");
    doc.select_range((3, 0), (3, 1));
    let before = (doc.cursor(), doc.history().undo_len(), doc.top_line());
    assert!(before.1 > 0);
    save(&mut app, &d, "r7ts/Old.md");
    assert_eq!(app.open_paths(), ["r7ts/Fresh.md"]);
    let doc = app.doc("r7ts/Fresh.md").expect("followed");
    assert_eq!(
        (doc.cursor(), doc.history().undo_len(), doc.top_line()),
        before
    );
    assert!(!doc.is_dirty() && doc.conflict().is_none());
}

#[test]
fn title_edit_leaves_a_note_with_another_name_alone() {
    let (_tmp, d) = fake_arc();
    let mut app = open_and_retitle(&d, "ts2/Custom.md", "New");
    save(&mut app, &d, "ts2/Custom.md");
    assert_eq!(app.open_paths(), ["ts2/Custom.md"]);
    assert!(gone(&d, "ts2/New.md"));
}

#[test]
fn title_edit_onto_an_existing_file_does_not_rename() {
    let (_tmp, d) = fake_arc();
    d.create_note("ts3/New.md", "other").expect("create");
    let mut app = open_and_retitle(&d, "ts3/Old.md", "New");
    save(&mut app, &d, "ts3/Old.md");
    assert_eq!(app.open_paths(), ["ts3/Old.md"]);
    assert_eq!(d.read_file("ts3/New.md").expect("kept").0, b"other");
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("already exists")),
        "{:?}",
        app.status.notice
    );
}

/// Audit E2: edits typed after the save landed but before the rename completes follow the
/// tab to `to`, are kept (conflict bar), and nothing is ever written at `from`.
#[test]
fn edit_during_title_rename_is_kept_and_from_is_not_recreated() {
    let (_tmp, d) = fake_arc();
    let mut app = open_and_retitle(&d, "ts5/Old.md", "New");
    app.rpc_exec = crate::app::RpcExec::Pool; // hold the rename RPC; complete it by hand below.
    save(&mut app, &d, "ts5/Old.md");
    let doc = app.doc_mut("ts5/Old.md").expect("still at from");
    assert!(doc.is_renaming());
    let seq = doc.edit_seq();
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("typed".into(), Instant::now()));
    assert!(doc.begin_save().is_none(), "saves held during the rename");
    let typed = doc.edit_seq();
    d.rename("ts5/Old.md", "ts5/New.md").expect("rename");
    let id = app.note_id("ts5/Old.md").expect("open");
    let _ = app.send(Message::TitleRenamed {
        id,
        from: "ts5/Old.md".into(),
        to: "ts5/New.md".into(),
        seq,
        result: Ok("ts5/New.md".into()),
    });
    // The typed edit's autosave (plan note-identity D4: addressed to the note id) starts no
    // save: the followed edits sit behind the conflict bar.
    let _ = app.send(Message::Source(crate::message::SourceMsg::AutosaveDue {
        id,
        seq: typed,
    }));
    assert_eq!(app.open_paths(), ["ts5/New.md"]);
    let doc = app.doc("ts5/New.md").expect("followed");
    assert_eq!(doc.text(), "---\ntitle: New\n---\ntyped\n");
    assert!(doc.is_dirty());
    assert!(gone(&d, "ts5/Old.md"));
    // Keep mine saves the text at `to`.
    let job = app
        .doc_mut("ts5/New.md")
        .and_then(|t| t.keep_mine())
        .expect("keep mine");
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    assert_eq!(
        d.read_file("ts5/New.md").expect("to").0,
        b"---\ntitle: New\n---\ntyped\n"
    );
    assert!(gone(&d, "ts5/Old.md"));
}

/// A follow-up save in flight defers the rename until it lands.
#[test]
fn title_rename_waits_for_an_in_flight_follow_up_save() {
    let (_tmp, d) = fake_arc();
    let mut app = open_and_retitle(&d, "ts6/Old.md", "New");
    let first = app
        .doc_mut("ts6/Old.md")
        .and_then(|t| t.begin_save())
        .expect("save");
    let doc = app.doc_mut("ts6/Old.md").expect("open");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("z".into(), Instant::now()));
    assert!(doc.begin_save().is_none(), "queued as save_again");
    let outcome = first.run(d.as_ref());
    let _ = app.send(first.done(outcome));
    assert_eq!(
        app.open_paths(),
        ["ts6/Old.md"],
        "deferred while the follow-up saves"
    );
    assert!(app.doc("ts6/Old.md").is_some_and(|t| t.is_saving()));
    // The follow-up's task is not run by the harness: land it by hand.
    let doc = app.doc("ts6/Old.md").expect("open");
    let (seq, text) = (doc.edit_seq(), doc.text());
    let base = d.read_file("ts6/Old.md").expect("disk").1;
    let h = d.write_note("ts6/Old.md", &text, &base).expect("write");
    let _ = app.send(Message::Source(crate::message::SourceMsg::Saved {
        id: app.note_id("ts6/Old.md"),
        seq,
        outcome: crate::source::SaveOutcome::Saved(h),
        after: None,
    }));
    assert_eq!(app.open_paths(), ["ts6/New.md"]);
    assert_eq!(
        d.read_file("ts6/New.md").expect("to").0,
        b"---\ntitle: New\n---\nz\n"
    );
}
