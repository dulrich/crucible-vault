//! Daemon reconnect (all with `FakeDaemon`).

use std::sync::atomic::{AtomicU32, Ordering};

use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::daemon::Connector;
use crate::message::{ExplorerMsg, SourceMsg};
use crate::source::SaveOutcome;

/// A connector over `d` that fails `fails` times first; returns it and its call counter.
fn flaky(d: &Arc<FakeDaemon>, fails: u32) -> (Connector, Arc<AtomicU32>) {
    let calls = Arc::new(AtomicU32::new(0));
    let (d, n) = (Arc::clone(d), Arc::clone(&calls));
    let c: Connector = Arc::new(move || {
        if n.fetch_add(1, Ordering::SeqCst) < fails {
            Err(DaemonError::Disconnected)
        } else {
            Ok(Arc::clone(&d) as Arc<dyn DaemonClient>)
        }
    });
    (c, calls)
}

fn setup(notes: &[&str], fails: u32) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    for p in notes {
        d.create_note(p, "body").expect("create");
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let (c, _) = flaky(&d, fails);
    let mut app = App::new(Some(dc), None).with_connector(c);
    app.queue_rpcs();
    for p in notes {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
    }
    (tmp, d, app)
}

fn type_x(app: &mut App) {
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('x'),
    ))));
}

fn reconnect(app: &mut App) {
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
}

fn write_disk(d: &FakeDaemon, path: &str, body: &str) {
    std::fs::write(d.vault_root().join(path), body).expect("write");
}

fn doc<'a>(app: &'a mut App, path: &str) -> &'a mut crate::source::SourceBuffer {
    app.doc_mut(path).expect("doc")
}

fn land(app: &mut App, path: &str, seq: u64, outcome: SaveOutcome) {
    let _ = app.send(Message::Source(SourceMsg::Saved {
        path: path.to_owned(),
        seq,
        outcome,
        after: None,
    }));
}

#[test]
fn backoff_is_capped_and_never_zero() {
    assert_eq!(crate::app::reconnect_delay(0), Duration::from_millis(500));
    assert_eq!(crate::app::reconnect_delay(1), Duration::from_secs(1));
    assert_eq!(crate::app::reconnect_delay(30), Duration::from_secs(5));
}

#[test]
fn failing_connector_then_success_installs_a_new_client_and_bumps_the_epoch() {
    let (_tmp, _d, mut app) = setup(&[], 2);
    assert_eq!(app.connection_epoch(), 0);
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    assert!(app.status.line().starts_with("reconnecting"));
    for _ in 0..2 {
        let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
        assert!(!app.status.connected);
        assert_eq!(app.connection_epoch(), 0);
    }
    let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
    assert!(app.status.connected);
    assert!(!app.status.reconnecting);
    assert_eq!(app.connection_epoch(), 1);
    assert!(app.status.line().starts_with("connected"));
}

#[test]
fn boot_without_a_daemon_connects_once_the_connector_succeeds() {
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    let (c, calls) = flaky(&d, 1);
    let mut app = App::new(None, None).with_connector(c);
    app.queue_rpcs();
    assert!(app.daemon.is_none());
    // The subscription reports "no connection" at boot.
    reconnect(&mut app);
    assert!(app.daemon.is_none() && app.status.reconnecting);
    let _ = app.send(Message::Status(StatusMsg::ReconnectDue));
    assert!(app.daemon.is_some());
    assert!(app.status.connected);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        !app.explorer.paths.is_empty(),
        "file tree loaded on connect"
    );
}

#[test]
fn dirty_document_whose_save_failed_is_saved_after_reconnect() {
    let (_tmp, _d, mut app) = setup(&["r1/a.md"], 0);
    type_x(&mut app);
    let _ = app.send(Message::Source(SourceMsg::Save));
    let seq = doc(&mut app, "r1/a.md").edit_seq();
    land(
        &mut app,
        "r1/a.md",
        seq,
        SaveOutcome::Failed("daemon disconnected".into()),
    );
    assert!(!doc(&mut app, "r1/a.md").is_saving());
    reconnect(&mut app);
    let t = doc(&mut app, "r1/a.md");
    assert!(t.is_saving(), "resync schedules the save");
    assert!(t.conflict().is_none());
}

#[test]
fn dirty_document_whose_disk_changed_gets_the_conflict_bar() {
    let (_tmp, d, mut app) = setup(&["r2/a.md"], 0);
    type_x(&mut app);
    write_disk(&d, "r2/a.md", "changed elsewhere");
    reconnect(&mut app);
    let t = doc(&mut app, "r2/a.md");
    assert!(t.conflict().is_some());
    assert!(!t.is_saving(), "no save over a changed file");
    assert!(t.is_dirty());
}

#[test]
fn clean_document_reloads_when_the_disk_differs() {
    let (_tmp, d, mut app) = setup(&["r3/a.md"], 0);
    write_disk(&d, "r3/a.md", "new text");
    reconnect(&mut app);
    let t = doc(&mut app, "r3/a.md");
    assert_eq!(t.text(), "new text");
    assert!(!t.is_dirty());
}

#[test]
fn dirty_document_whose_text_already_landed_becomes_clean() {
    let (_tmp, d, mut app) = setup(&["r4/a.md"], 0);
    type_x(&mut app);
    let text = doc(&mut app, "r4/a.md").text();
    write_disk(&d, "r4/a.md", &text);
    reconnect(&mut app);
    let t = doc(&mut app, "r4/a.md");
    assert!(!t.is_dirty() && !t.is_saving() && t.conflict().is_none());
}

/// Audit E1: the reconnect arrives before the old save's failure completion; the resync
/// waits for it, and the queued `save_again` is not stranded.
#[test]
fn reconnect_before_the_old_saves_failure_resyncs_after_it() {
    let (_tmp, _d, mut app) = setup(&["r5/a.md"], 0);
    type_x(&mut app);
    let _ = app.send(Message::Source(SourceMsg::Save));
    let seq = doc(&mut app, "r5/a.md").edit_seq();
    type_x(&mut app);
    // A second save request while the first is in flight queues `save_again`.
    let _ = app.send(Message::Source(SourceMsg::Save));
    reconnect(&mut app);
    assert_eq!(app.connection_epoch(), 1);
    assert!(doc(&mut app, "r5/a.md").is_saving(), "still the old save");
    land(
        &mut app,
        "r5/a.md",
        seq,
        SaveOutcome::Failed("daemon disconnected".into()),
    );
    let t = doc(&mut app, "r5/a.md");
    assert!(
        t.is_saving(),
        "resync launched a fresh save after the failure"
    );
    assert!(t.is_dirty());
    assert!(!t.take_resync_pending());
}

/// Plan note-path-continuity D3: a note moved while the editor was disconnected is followed on
/// reconnect (dirty text kept, saved at the new path), never marked missing.
#[test]
fn reconnect_follows_a_note_moved_while_disconnected() {
    let (_tmp, d, mut app) = setup(&["r9/a.md"], 0);
    type_x(&mut app);
    let _unfed = d.daemon_move("r9/a.md", "r9/b.md").expect("move");
    reconnect(&mut app);
    assert!(app.doc("r9/a.md").is_none(), "left the old path");
    let t = doc(&mut app, "r9/b.md");
    assert_eq!(t.text(), "xbody", "dirty text kept");
    assert!(!t.is_missing() && t.conflict().is_none());
    assert_eq!(app.status.error_text(), None);
}
