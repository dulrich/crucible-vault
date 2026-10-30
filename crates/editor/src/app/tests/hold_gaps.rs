//! Plan round-18 WP-2: the hold gaps — explorer local gate, daemon `HELD` replies mapped to the
//! one notice, close / quit waiting for a release, and Cut as copy-only.

use cv_protocol::JobStatus;
use iced::widget::text_editor::Action;

use super::unsaved_dialog::{dialog_notes, setup, type_x};
use super::*;
use crate::dialog::{Dialog, DialogMsg, Discard, LossReason};
use crate::explorer::FileOp;
use crate::message::{SourceMsg, ViewsMsg};

const HELD_NOTICE: &str = "Refine is updating this note — save held";

fn job(id: &str, status: JobState, held: &str) -> Notification {
    Notification::JobUpdated(JobStatus {
        job_id: id.into(),
        status,
        message: None,
        output_paths: Vec::new(),
        open_path: None,
        held_note: Some(held.to_owned()),
        action_label: Some("Refine".to_owned()),
    })
}

fn feed(app: &mut App, n: Notification) {
    let _ = app.send(Message::Status(StatusMsg::Notification(n)));
}

fn notice(app: &App) -> Option<&str> {
    app.status.notice.as_ref().map(|n| n.text.as_str())
}

fn clear_notice(app: &mut App) {
    app.status.notice = None;
}

#[test]
fn explorer_rename_and_delete_of_held_notes_send_nothing() {
    let (_tmp, d, mut app) = setup(&["hg/a.md", "hg/sub/b.md"]);
    feed(&mut app, job("j1", JobState::Running, "hg/sub/b.md"));
    let before = d.mutation_calls().expect("calls");
    let ops = [
        FileOp::Rename {
            from: "hg/sub/b.md".into(),
            to: "hg/c.md".into(),
        },
        FileOp::Rename {
            from: "hg/a.md".into(),
            to: "hg/sub/b.md".into(),
        },
        FileOp::Delete {
            path: "hg/sub/b.md".into(),
        },
        FileOp::Delete {
            path: "hg/sub".into(),
        },
    ];
    for op in ops {
        clear_notice(&mut app);
        let _ = app.dispatch_op(op.clone());
        app.pump();
        assert_eq!(notice(&app), Some(HELD_NOTICE), "{op:?}");
    }
    assert_eq!(d.mutation_calls().expect("calls"), before, "nothing sent");
    // An unrelated rename still goes out.
    let _ = app.dispatch_op(FileOp::Rename {
        from: "hg/a.md".into(),
        to: "hg/a2.md".into(),
    });
    app.pump();
    assert!(
        d.mutation_calls()
            .expect("calls")
            .contains(&"rename hg/a.md".to_owned())
    );
}

#[test]
fn held_replies_post_the_hold_notice_for_save_rename_delete_and_set_bool() {
    let (_tmp, d, mut app) = setup(&["hr/a.md", "hr/b.md"]);
    d.create_note("hr/closed.md", "x").expect("create");
    d.set_held_reply(Some(("hr/a.md", "Refine"))).expect("set");
    // Save: the buffer stays dirty with no conflict, and the hold notice (not a failure).
    type_x(&mut app);
    let job = app
        .doc_mut("hr/b.md")
        .and_then(|t| t.begin_save())
        .expect("save starts");
    let path = job.path.clone();
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    assert_eq!(notice(&app), Some(HELD_NOTICE));
    let doc = app.doc(&path).expect("open");
    assert!(doc.is_dirty() && doc.conflict().is_none() && !doc.is_saving());
    // Rename / delete through the explorer.
    for op in [
        FileOp::Rename {
            from: "hr/closed.md".into(),
            to: "hr/z.md".into(),
        },
        FileOp::Delete {
            path: "hr/closed.md".into(),
        },
    ] {
        clear_notice(&mut app);
        let _ = app.dispatch_op(op.clone());
        app.pump();
        assert_eq!(notice(&app), Some(HELD_NOTICE), "{op:?}");
    }
    // Delete-note.
    clear_notice(&mut app);
    let _ = app.send(Message::Explorer(crate::message::ExplorerMsg::NoteDeleted(
        Err(crate::daemon::rpc_err(
            &d.delete_note("hr/closed.md", &[], crate::daemon::DeleteMode::Trash)
                .expect_err("held"),
        )),
    )));
    assert_eq!(notice(&app), Some(HELD_NOTICE));
    // note_set_bool on a closed note.
    clear_notice(&mut app);
    let _ = app.send(Message::Views(ViewsMsg::SetBool {
        path: "hr/closed.md".into(),
        key: "done".into(),
        value: true,
    }));
    assert_eq!(notice(&app), Some(HELD_NOTICE));
}

#[test]
fn closing_a_dirty_held_tab_waits_for_the_release_then_saves_and_closes() {
    let (_tmp, d, mut app) = setup(&["hc/a.md"]);
    type_x(&mut app);
    feed(&mut app, job("j1", JobState::Running, "hc/a.md"));
    let before = d.mutation_calls().expect("calls");
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(app.open_paths(), ["hc/a.md"], "still open while held");
    assert!(app.dialog.is_none(), "no failure dialog");
    assert_eq!(d.mutation_calls().expect("calls"), before);
    feed(&mut app, job("j1", JobState::Done, "hc/a.md"));
    assert!(app.open_paths().is_empty(), "closed after release + save");
    assert_eq!(d.read_file("hc/a.md").expect("file").0, b"xbody");
    assert!(app.hold_closes.is_empty());
}

#[test]
fn a_disconnect_while_a_close_waits_routes_to_the_close_dialog() {
    let (_tmp, _d, mut app) = setup(&["hd/a.md"]);
    type_x(&mut app);
    feed(&mut app, job("j1", JobState::Running, "hd/a.md"));
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(app.hold_closes.len(), 1);
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    assert!(app.hold_closes.is_empty());
    assert_eq!(app.open_paths(), ["hd/a.md"], "nothing lost");
    assert!(matches!(
        &app.dialog,
        Some(Dialog::Unsaved {
            then: Discard::CloseTabs(_),
            ..
        })
    ));
    assert_eq!(
        dialog_notes(&app),
        [("hd/a.md".to_owned(), LossReason::Disconnected)]
    );
}

#[test]
fn quit_with_a_dirty_held_note_waits_then_saves_and_exits() {
    let (_tmp, d, mut app) = setup(&["hq/a.md"]);
    type_x(&mut app);
    feed(&mut app, job("j1", JobState::Running, "hq/a.md"));
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    assert!(!app.exiting && app.dialog.is_none() && app.quit.is_some());
    feed(&mut app, job("j1", JobState::Done, "hq/a.md"));
    assert!(app.exiting, "the released note saved and the quit finished");
    assert_eq!(d.read_file("hq/a.md").expect("file").0, b"xbody");
}

#[test]
fn quit_anyway_works_while_waiting_and_names_the_hold() {
    let (_tmp, d, mut app) = setup(&["hy/a.md"]);
    type_x(&mut app);
    feed(&mut app, job("j1", JobState::Running, "hy/a.md"));
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    assert!(!app.exiting);
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    assert_eq!(
        dialog_notes(&app),
        [("hy/a.md".to_owned(), LossReason::Held("Refine".to_owned()))]
    );
    let _ = app.send(Message::Dialog(DialogMsg::Ok));
    assert!(app.exiting, "quit anyway exits");
    assert_eq!(d.read_file("hy/a.md").expect("file").0, b"body");
}

#[test]
fn a_disconnect_while_quit_waits_records_a_disconnected_loss() {
    let (_tmp, _d, mut app) = setup(&["hz/a.md"]);
    type_x(&mut app);
    feed(&mut app, job("j1", JobState::Running, "hz/a.md"));
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    assert!(app.quit.is_none() && !app.exiting);
    assert_eq!(
        dialog_notes(&app),
        [("hz/a.md".to_owned(), LossReason::Disconnected)]
    );
}

#[test]
fn cut_on_a_held_note_copies_and_leaves_the_buffer_alone() {
    let (_tmp, _d, mut app) = setup(&["hx/a.md"]);
    type_x(&mut app);
    let _ = app.send(Message::Source(SourceMsg::Action(Action::SelectAll)));
    feed(&mut app, job("j1", JobState::Running, "hx/a.md"));
    let text = app.doc("hx/a.md").expect("open").text();
    let seq = app.doc("hx/a.md").expect("open").edit_seq();
    let task = app.send(Message::Source(SourceMsg::Cut));
    let doc = app.doc("hx/a.md").expect("open");
    assert_eq!(doc.text(), text);
    assert_eq!(doc.edit_seq(), seq);
    assert!(doc.is_dirty());
    assert!(task.units() > 0, "the selection went to the clipboard");
    assert_eq!(
        crate::app::holds::source_gate(&SourceMsg::Cut),
        crate::app::holds::Gate::CopyOnly
    );
}
