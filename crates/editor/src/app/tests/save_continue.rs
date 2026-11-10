//! One save-then-continue path, one settings-save completion and modal classification. In-flight races use `update` (no pump), inject the racing message, then `pump`.

use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::message::{ExplorerMsg, SourceMsg, SwitcherMsg};
use crate::panes;

pub(super) fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    for p in notes {
        d.create_note(p, "body").expect("create");
    }
    super::workspace::register_create(&d);
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let inner = Arc::clone(&d);
    let c: crate::daemon::Connector =
        Arc::new(move || Ok(Arc::clone(&inner) as Arc<dyn DaemonClient>));
    let mut app = App::new(Some(dc), None).with_connector(c);
    app.queue_rpcs();
    for p in notes {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
    }
    (tmp, d, app)
}

fn type_x() -> Message {
    Message::Source(SourceMsg::Action(Action::Edit(Edit::Insert('x'))))
}

/// Takes every queued RPC completion (to deliver after a racing message).
fn hold(app: &mut App) -> Vec<Message> {
    match &mut app.rpc_exec {
        crate::app::RpcExec::Queue(q) => q.drain(..).collect(),
        crate::app::RpcExec::Pool => Vec::new(),
    }
}

fn some_dialog() -> crate::dialog::Dialog {
    crate::dialog::Dialog::Unsaved {
        losses: crate::dialog::UnsavedEdits::default(),
        then: crate::dialog::Discard::Quit,
    }
}

/// SR2: an edit typed (and its save requested) while a save-before-close is in flight is
/// saved by the follow-up, and the close then completes.
#[test]
fn edit_typed_during_a_save_first_keeps_its_follow_up_save() {
    let (_tmp, d, mut app) = setup(&["r2fu/a.md", "r2fu/b.md"]);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("r2fu/a.md".into())));
    let _ = app.send(type_x());
    let _ = app.update(Message::Tabs(TabsMsg::CloseActive));
    let _ = app.update(type_x());
    let _ = app.update(Message::Source(SourceMsg::Save));
    app.pump();
    assert_eq!(d.read_file("r2fu/a.md").expect("file").0, b"xxbody");
    assert!(
        panes::find(&app.panes, "r2fu/a.md", &app.docs).is_none(),
        "closed"
    );
    assert!(app.dialog.is_none(), "{:?}", app.status.notice);
}

/// SR2: a reconnect resync deferred behind a save-before-delete runs when it lands.
#[test]
fn resync_deferred_behind_a_save_first_resolves() {
    use crate::dialog::DialogMsg;
    let (_tmp, _d, mut app) = setup(&["r2rs/a.md"]);
    let _ = app.send(type_x());
    let _ = app.send(Message::Explorer(ExplorerMsg::DeleteActive));
    let _ = app.update(Message::Dialog(DialogMsg::Ok));
    let held = hold(&mut app);
    assert_eq!(held.len(), 1, "the save before the delete");
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
    assert!(app.doc("r2rs/a.md").is_some_and(|t| t.is_saving()));
    for m in held {
        let _ = app.send(m);
    }
    let t = app.doc_mut("r2rs/a.md").expect("open until confirmed");
    assert!(!t.take_resync_pending(), "the deferred resync ran");
    assert!(
        app.dialog
            .as_ref()
            .and_then(|d| match d {
                crate::dialog::Dialog::DeleteConfirm(c) => Some(c),
                _ => None,
            })
            .is_some(),
        "the delete confirm follows"
    );
}

/// SR3: an async completion (`Created`) arriving while a dialog is open is applied.
#[test]
fn created_arriving_under_a_dialog_is_applied() {
    let (_tmp, d, mut app) = setup(&[]);
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    let _ = app.send(Message::Switcher(SwitcherMsg::Query("r2cr new".into())));
    let _ = app.update(Message::Switcher(SwitcherMsg::Submit));
    app.dialog = Some(some_dialog());
    app.pump();
    assert!(d.read_file("r2cr new.md").is_ok());
    assert_eq!(
        app.active_tab().and_then(|t| t.path(&app.docs)),
        Some("r2cr new.md"),
        "the created note opened"
    );
}

/// SR3: Shift released while a dialog is open does not stay held.
#[test]
fn shift_released_under_a_dialog_does_not_stick() {
    let (_tmp, d, mut app) = setup(&["r2sh/Works.md"]);
    let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::SHIFT));
    app.dialog = Some(some_dialog());
    let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::empty()));
    app.dialog = None;
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    let _ = app.send(Message::Switcher(SwitcherMsg::Query("works".into())));
    let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
    assert!(
        d.read_file("works.md").is_err(),
        "plain Enter opens the match"
    );
    assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
    assert_eq!(
        app.active_tab().and_then(|t| t.path(&app.docs)),
        Some("r2sh/Works.md")
    );
}
