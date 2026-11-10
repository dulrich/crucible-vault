//! Plan WP-R4 (review E1, E2): note opens, reloads and reconnect resyncs settle off the UI
//! thread, and a stale open or settle completion is dropped.

use super::*;
use crate::app::{NoteId, RpcExec};
use crate::message::{ExplorerMsg, PanesMsg};

fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let (tmp, d) = fake_arc();
    for p in notes {
        d.create_note(p, "body").expect("create");
    }
    let app = app_over(&d, None);
    (tmp, d, app)
}

/// `update` only (no pump): its RPC completions stay queued.
fn open_unpumped(app: &mut App, path: &str) {
    let _ = app.update(Message::Explorer(ExplorerMsg::Open(path.to_owned())));
}

fn queued(app: &mut App) -> Vec<Message> {
    match &mut app.rpc_exec {
        RpcExec::Queue(q) => q.drain(..).collect(),
        RpcExec::Pool => Vec::new(),
    }
}

fn tabs_of(app: &App, path: &str) -> usize {
    app.panes
        .iter()
        .map(|(_, ps)| {
            ps.tabs
                .iter()
                .filter(|t| t.path(&app.docs) == Some(path))
                .count()
        })
        .sum()
}

/// A `file_changed` for note `id` at `path` (not where its document is: it is settled).
fn changed(id: NoteId, path: &str, kind: ChangeKind) -> Message {
    Message::Status(StatusMsg::Notification(Notification::FileChanged {
        path: path.to_owned(),
        kind,
        hash: None,
        origin: Origin::External,
        from: None,
        replaced_by: None,
        id: Some(id),
    }))
}

/// The plan's blocking-fake test: with `bind_note` / `locate_note` panicking on this (the UI)
/// thread and RPCs on the pool (never run here), `update` returns from an open, a reload and a
/// reconnect — none of them asks the daemon for a note's identity inline.
#[test]
fn update_never_asks_for_identity_on_the_ui_thread() {
    let (_tmp, d, mut app) = setup(&["r4b/a.md", "r4b/b.md"]);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("r4b/a.md".into())));
    let id = app.note_id("r4b/a.md").expect("bound");
    app.rpc_exec = RpcExec::Pool;
    d.forbid_identity_calls_on(Some(std::thread::current().id()))
        .expect("gate");
    // Open: the tab waits for its completion.
    open_unpumped(&mut app, "r4b/b.md");
    assert_eq!(
        tabs_of(&app, "r4b/b.md"),
        0,
        "the tab appears when the open completes"
    );
    // Reload: the file is gone under the open document; the settle goes to the pool.
    std::fs::remove_file(d.vault_root().join("r4b/a.md")).expect("rm");
    let _ = app.update(changed(id, "r4b/a.md", ChangeKind::Modified));
    let _ = app.update(changed(id, "r4b/a.md", ChangeKind::Deleted));
    // Reconnect: every open document's resync settles first.
    let dc = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let c: crate::daemon::Connector = Arc::new(move || Ok(Arc::clone(&dc)));
    let conn = crate::daemon::connect(&c).expect("connect");
    let mut app = app.with_connector(c);
    let _ = app.update(Message::Status(StatusMsg::Disconnected));
    let _ = app.update(Message::Status(StatusMsg::Connected(Ok(conn))));
    assert!(app.status.connected, "the reconnect installed");
    // The gate is live: the same call on this thread panics.
    let probe = Arc::clone(&d);
    let r = std::panic::catch_unwind(move || probe.locate_note(id));
    assert!(r.is_err(), "the gate panics on the UI thread");
}

/// A pane closed before its open completes gets nothing: no tab, no document.
#[test]
fn closing_the_target_pane_before_the_open_completes_opens_nothing() {
    let (_tmp, _d, mut app) = setup(&["r4c/a.md", "r4c/b.md"]);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("r4c/a.md".into())));
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    let target = app.focus;
    open_unpumped(&mut app, "r4c/b.md");
    let _ = app.update(Message::Panes(PanesMsg::Close(target)));
    app.pump();
    assert_eq!(tabs_of(&app, "r4c/b.md"), 0);
    assert!(app.doc("r4c/b.md").is_none(), "no document either");
}

/// Close, then reopen the same path before the first open completes: exactly one tab, bound to
/// the note's id (the first completion is superseded).
#[test]
fn reopening_before_the_first_open_completes_yields_one_bound_tab() {
    let (_tmp, d, mut app) = setup(&["r4r/a.md", "r4r/b.md"]);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("r4r/a.md".into())));
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    let target = app.focus;
    open_unpumped(&mut app, "r4r/b.md");
    let _ = app.update(Message::Panes(PanesMsg::Close(target)));
    open_unpumped(&mut app, "r4r/b.md");
    let first = queued(&mut app);
    assert_eq!(first.len(), 2, "two opens in flight");
    for m in first {
        let _ = app.send(m);
    }
    assert_eq!(tabs_of(&app, "r4r/b.md"), 1, "exactly one tab");
    assert_eq!(app.note_id("r4r/b.md"), d.bind_note("r4r/b.md").ok());
}

/// A file-changed settle that lands after its document closed is dropped.
#[test]
fn a_settle_landing_after_its_document_closed_is_dropped() {
    let (_tmp, d, mut app) = setup(&["r4s/a.md", "r4s/b.md"]);
    for p in ["r4s/a.md", "r4s/b.md"] {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
    }
    let id = app.note_id("r4s/a.md").expect("bound");
    let _ = d.daemon_move("r4s/a.md", "r4s/c.md").expect("move");
    let _ = app.update(changed(id, "r4s/elsewhere.md", ChangeKind::Modified));
    let pane = app.focus;
    let _ = app.update(Message::Tabs(TabsMsg::Close(pane, 0)));
    assert!(app.doc("r4s/a.md").is_none(), "closed");
    app.pump();
    assert_eq!(
        app.open_paths(),
        ["r4s/b.md"],
        "the moved note is not resurrected"
    );
    assert_eq!(tabs_of(&app, "r4s/c.md"), 0);
}

/// Two settles of one document completing out of order: the newer applies, the older is
/// dropped (it starts nothing).
#[test]
fn out_of_order_settles_apply_only_the_newest() {
    let (_tmp, d, mut app) = setup(&["r4o/a.md"]);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("r4o/a.md".into())));
    let id = app.note_id("r4o/a.md").expect("bound");
    let _ = d.daemon_move("r4o/a.md", "r4o/c.md").expect("move");
    let _ = app.update(changed(id, "r4o/x1.md", ChangeKind::Modified)); // answers c
    let _ = d.daemon_move("r4o/c.md", "r4o/e.md").expect("move");
    let _ = app.update(changed(id, "r4o/x2.md", ChangeKind::Modified)); // answers e
    let mut settles = queued(&mut app);
    assert_eq!(settles.len(), 2);
    let older = settles.remove(0);
    let newer = settles.remove(0);
    let _ = app.send(newer);
    assert_eq!(
        app.note_id("r4o/e.md"),
        Some(id),
        "the newest answer applied"
    );
    let _ = app.update(older);
    assert!(
        queued(&mut app).is_empty(),
        "the stale answer started nothing"
    );
    assert_eq!(app.open_paths(), ["r4o/e.md"]);
}
