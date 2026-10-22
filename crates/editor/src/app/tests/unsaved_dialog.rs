//! Unsaved-edits dialog; missing-file conflict (all `FakeDaemon`).

use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::dialog::{DialogMsg, LossReason};
use crate::message::{ExplorerMsg, PaletteMsg, SourceMsg, SwitcherMsg};

pub(super) fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    for p in notes {
        d.create_note(p, "body").expect("create");
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    for p in notes {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
    }
    (tmp, d, app)
}

pub(super) fn type_x(app: &mut App) {
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('x'),
    ))));
}

pub(super) fn dialog_notes(app: &App) -> Vec<(String, LossReason)> {
    app.dialog
        .as_ref()
        .and_then(|d| match d {
            crate::dialog::Dialog::Unsaved { losses, .. } => Some(losses),
            _ => None,
        })
        .map(|u| u.notes.clone())
        .unwrap_or_default()
}

fn deleted(app: &mut App, path: &str) {
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: path.to_owned(),
            kind: cv_protocol::ChangeKind::Deleted,
            hash: None,
            origin: cv_protocol::Origin::Daemon,
            from: None,
        },
    )));
}

/// Runs `job` (a `begin_save` / `keep_mine` result) on the daemon and lands it.
fn land(app: &mut App, d: &FakeDaemon, job: Option<crate::source::SaveJob>) {
    let job = job.expect("a save starts");
    let outcome = job.run(d);
    let _ = app.send(job.done(outcome));
}

pub(super) fn save(app: &mut App, d: &FakeDaemon, path: &str) {
    let job = app.doc_mut(path).and_then(|t| t.begin_save());
    land(app, d, job);
}

fn keep_mine(app: &mut App, d: &FakeDaemon, path: &str) {
    let job = app.doc_mut(path).and_then(|t| t.keep_mine());
    land(app, d, job);
}

fn missing(app: &App, path: &str) -> bool {
    app.doc(path)
        .is_some_and(crate::source::SourceBuffer::is_missing)
}

/// Audit E2 (a): a delete notification under a dirty buffer → missing-file state; Keep
/// mine recreates the file at its exact path.
#[test]
fn deleted_under_edits_enters_missing_state_and_keep_mine_recreates() {
    let (_tmp, d, mut app) = setup(&["d2m/a.md"]);
    type_x(&mut app);
    d.delete("d2m/a.md").expect("delete");
    deleted(&mut app, "d2m/a.md");
    assert!(missing(&app, "d2m/a.md"));
    keep_mine(&mut app, &d, "d2m/a.md");
    assert_eq!(d.read_file("d2m/a.md").expect("file").0, b"xbody");
    let t = app.doc("d2m/a.md").expect("doc");
    assert!(!t.is_dirty() && t.conflict().is_none());
}

/// Audit E2 (b): a save CAS conflict with an empty current hash → missing; Take theirs
/// discards the edits and closes the note's tabs (no re-read of a missing file).
#[test]
fn save_conflict_against_no_file_is_missing_and_take_theirs_closes_tabs() {
    let (_tmp, d, mut app) = setup(&["d2t/a.md"]);
    type_x(&mut app);
    std::fs::remove_file(d.vault_root().join("d2t/a.md")).expect("rm");
    save(&mut app, &d, "d2t/a.md");
    assert!(missing(&app, "d2t/a.md"));
    let _ = app.send(Message::Source(SourceMsg::TakeTheirs("d2t/a.md".into())));
    assert!(app.open_paths().is_empty());
    assert!(app.doc("d2t/a.md").is_none());
    assert!(d.read_file("d2t/a.md").is_err(), "nothing written");
}

/// WP-1 hand-off: a reconnect resync whose read is NotFound enters the missing state.
#[test]
fn reconnect_resync_of_a_vanished_file_enters_missing_state() {
    let (_tmp, d, app) = setup(&["d2r/a.md"]);
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = app.with_connector(Arc::new(move || Ok(Arc::clone(&dc))));
    type_x(&mut app);
    std::fs::remove_file(d.vault_root().join("d2r/a.md")).expect("rm");
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
    assert!(missing(&app, "d2r/a.md"));
}

/// Audit B1: Keep mine when another file took the path meanwhile → the normal conflict,
/// and no alternate (suffixed) file is created.
#[test]
fn keep_mine_when_the_path_was_taken_conflicts_without_an_alternate_file() {
    let (_tmp, d, mut app) = setup(&["d2b/a.md"]);
    type_x(&mut app);
    d.delete("d2b/a.md").expect("delete");
    deleted(&mut app, "d2b/a.md");
    assert!(missing(&app, "d2b/a.md"));
    std::fs::write(d.vault_root().join("d2b/a.md"), "other").expect("write");
    keep_mine(&mut app, &d, "d2b/a.md");
    let t = app.doc("d2b/a.md").expect("doc");
    assert!(
        t.conflict().is_some() && !t.is_missing(),
        "a normal conflict"
    );
    assert_eq!(d.read_file("d2b/a.md").expect("file").0, b"other");
    let names: Vec<_> = std::fs::read_dir(d.vault_root().join("d2b"))
        .expect("dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names, ["a.md"], "no alternate file");
}

/// Audit E3: the open dialog takes input priority; Escape = Cancel; a repeated close
/// request is ignored.
#[test]
fn dialog_is_modal_and_escape_cancels() {
    let (_tmp, _d, mut app) = setup(&["d2e/a.md", "d2e/b.md"]);
    type_x(&mut app);
    app.status.connected = false;
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    let before = app.dialog.clone();
    assert!(before.is_some());
    let text = app.active_doc().map(crate::source::SourceBuffer::text);
    // Global chords, editor input, focus-loss flush, tab close, repeated close request.
    let ctrl_w = crate::palette::chord::parse("Ctrl+W").expect("chord");
    let _ = app.send(Message::Palette(PaletteMsg::Key(ctrl_w)));
    type_x(&mut app);
    let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    assert_eq!(app.open_paths().len(), 2, "no tab closed");
    assert_eq!(
        app.active_doc().map(crate::source::SourceBuffer::text),
        text
    );
    assert_eq!(app.dialog, before, "repeated close ignored");
    assert!(app.quit.is_none() && !app.exiting);
    // Escape: the dialog key listener maps it to Cancel (the switcher's Close too).
    let esc = iced::keyboard::Event::KeyPressed {
        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        physical_key: iced::keyboard::key::Physical::Unidentified(
            iced::keyboard::key::NativeCode::Unidentified,
        ),
        location: iced::keyboard::Location::Standard,
        modifiers: iced::keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    };
    assert!(matches!(
        crate::dialog::key_event(&esc),
        Some(Message::Dialog(DialogMsg::Cancel))
    ));
    let _ = app.send(Message::Switcher(SwitcherMsg::Close));
    assert!(app.dialog.is_none() && !app.exiting);
    // Enter on the unsaved-edits dialog is Cancel (the safe default), never OK.
    let _ = app.send(Message::Tabs(TabsMsg::Quit));
    let _ = app.send(Message::Dialog(DialogMsg::Default));
    assert!(app.dialog.is_none() && !app.exiting);
}
