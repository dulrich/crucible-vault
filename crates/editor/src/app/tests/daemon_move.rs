//! Round 9 WP-3 (Decision 4): a daemon-side move retargets the open tabs; MRU tab close.

use super::unsaved_dialog::{setup, type_x};
use super::*;
use crate::message::{ExplorerMsg, TabsMsg};

fn feed(app: &mut App, ns: Vec<Notification>) {
    for n in ns {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
}

fn active_path(app: &App) -> Option<String> {
    app.active_tab().and_then(|t| t.path().map(str::to_owned))
}

fn open_paths(app: &App) -> Vec<String> {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter().filter_map(|t| t.path().map(str::to_owned)))
        .collect()
}

#[test]
fn daemon_move_retargets_a_clean_tab_without_a_deleted_status() {
    let (_tmp, d, mut app) = setup(&["Clippings/a.md"]);
    let pair = d
        .daemon_move("Clippings/a.md", "fanfic/a.md")
        .expect("move");
    feed(&mut app, pair);
    assert_eq!(open_paths(&app), vec!["fanfic/a.md".to_owned()]);
    assert_eq!(active_path(&app).as_deref(), Some("fanfic/a.md"));
    assert!(app.doc("Clippings/a.md").is_none());
    let doc = app.doc("fanfic/a.md").expect("re-keyed");
    assert!(!doc.is_missing() && doc.conflict().is_none());
    assert_eq!(app.status.error_text(), None, "no `file deleted on disk`");
    assert!(!app.explorer.contains("Clippings/a.md"));
    assert!(app.explorer.contains("fanfic/a.md"));
}

#[test]
fn daemon_move_retargets_a_dirty_tab_keeping_its_edits() {
    let (_tmp, d, mut app) = setup(&["Clippings/b.md"]);
    type_x(&mut app);
    let before = app.doc("Clippings/b.md").expect("open").text();
    assert!(app.doc("Clippings/b.md").expect("open").is_dirty());
    let pair = d
        .daemon_move("Clippings/b.md", "fanfic/b.md")
        .expect("move");
    feed(&mut app, pair);
    assert_eq!(open_paths(&app), vec!["fanfic/b.md".to_owned()]);
    let doc = app.doc("fanfic/b.md").expect("re-keyed");
    assert!(doc.is_dirty());
    assert!(!doc.is_missing() && doc.conflict().is_none());
    assert_eq!(doc.text(), before, "dirty text intact");
    assert_eq!(app.status.error_text(), None);
}

#[test]
fn closing_the_active_tab_focuses_the_most_recently_used_one() {
    let (_tmp, _d, mut app) = setup(&["mru/a.md", "mru/b.md", "mru/c.md"]);
    let pane = app.focus;
    assert_eq!(active_path(&app).as_deref(), Some("mru/c.md"));
    let _ = app.send(Message::Tabs(TabsMsg::Select(pane, 0)));
    let _ = app.send(Message::Tabs(TabsMsg::Select(pane, 2)));
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(active_path(&app).as_deref(), Some("mru/a.md"));
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("mru/b.md".into())));
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(active_path(&app).as_deref(), Some("mru/a.md"));
}

/// Round 14 WP-4: a restored session has no visit history, so only the persisted active tab
/// is "recent"; closing it falls back to the right neighbour, not the right-most tab.
#[test]
fn closing_the_active_tab_after_a_restore_does_not_jump_to_the_rightmost() {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    let notes = ["r14m/a.md", "r14m/b.md", "r14m/c.md", "r14m/d.md"];
    for p in notes {
        d.create_note(p, "body").expect("create");
    }
    let state = tmp.path().join("editor.json");
    {
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), Some(state.clone()));
        app.queue_rpcs();
        for p in notes {
            let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.to_owned())));
        }
        let pane = app.focus;
        let _ = app.send(Message::Tabs(TabsMsg::Select(pane, 1)));
        let _ = app.send(Message::PersistDue);
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), Some(state));
    assert_eq!(active_path(&app).as_deref(), Some("r14m/b.md"));
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(active_path(&app).as_deref(), Some("r14m/c.md"));
}

/// Plain-session control: a visit to a tab is remembered across a later close.
#[test]
fn plain_session_close_follows_visit_order() {
    let (_tmp, _d, mut app) = setup(&["r14c/a.md", "r14c/b.md", "r14c/c.md", "r14c/d.md"]);
    let pane = app.focus;
    let _ = app.send(Message::Tabs(TabsMsg::Select(pane, 1)));
    let _ = app.send(Message::Tabs(TabsMsg::Select(pane, 0)));
    let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
    assert_eq!(active_path(&app).as_deref(), Some("r14c/b.md"));
}

// Plan note-path-continuity WP-3 (D3): the editor resolves lineage before it reports "deleted".

fn deleted_event(path: &str, seq: Option<u64>) -> Notification {
    Notification::FileChanged {
        id: None,
        path: path.to_owned(),
        kind: ChangeKind::Deleted,
        hash: None,
        origin: Origin::Daemon,
        from: None,
        replaced_by: None,
        seq,
    }
}

/// Saves `path`'s buffer on the daemon and lands the outcome (the retry, if any, is pumped).
fn save_now(app: &mut App, d: &FakeDaemon, path: &str) {
    let job = app
        .doc_mut(path)
        .and_then(|b| b.begin_save())
        .expect("a save starts");
    let outcome = job.run(d);
    let _ = app.send(job.done(outcome));
}

#[test]
fn chained_moves_end_on_the_final_path() {
    let (_tmp, d, mut app) = setup(&["wp3a/a.md"]);
    let id = app.note_id("wp3a/a.md").expect("bound");
    let mut ns = d.daemon_move("wp3a/a.md", "wp3a/b.md").expect("a to b");
    ns.extend(d.daemon_move("wp3a/b.md", "wp3a/c.md").expect("b to c"));
    feed(&mut app, ns);
    assert_eq!(open_paths(&app), vec!["wp3a/c.md".to_owned()]);
    assert_eq!(app.status.error_text(), None);
    let doc = app.doc("wp3a/c.md").expect("at c");
    assert!(!doc.is_missing() && doc.conflict().is_none());
    // Plan note-identity D4: the document is bound to the note id, not a lineage cursor; the
    // same document (same id) followed both moves.
    assert_eq!(app.note_id("wp3a/c.md"), Some(id), "same note id");
}

#[test]
fn degraded_deleted_plus_created_is_a_delete_under_note_ids() {
    // Plan note-identity D1 (kept residual): an unpaired move retires the id and mints a new one
    // for the file at `to`. The open document keeps its id (D3), so it is not silently rebound
    // to the other id: it takes today's deleted handling, and `c` is a different note.
    let (_tmp, d, mut app) = setup(&["wp3b/a.md"]);
    let id = app.note_id("wp3b/a.md").expect("bound");
    let ev = d
        .daemon_move_degraded("wp3b/a.md", "wp3b/c.md")
        .expect("move");
    feed(&mut app, ev);
    assert_eq!(open_paths(&app), vec!["wp3b/a.md".to_owned()]);
    assert_eq!(app.status.error_text(), Some("file deleted on disk"));
    assert_eq!(app.note_id("wp3b/a.md"), Some(id));
    assert_ne!(d.bind_note("wp3b/c.md").ok(), Some(id), "c is another note");
}

#[test]
fn dirty_tab_moved_before_its_save_keeps_its_text_and_saves_at_the_new_path() {
    let (_tmp, d, mut app) = setup(&["wp3c/a.md"]);
    type_x(&mut app);
    let _unfed = d.daemon_move("wp3c/a.md", "wp3c/c.md").expect("move");
    save_now(&mut app, &d, "wp3c/a.md");
    assert_eq!(open_paths(&app), vec!["wp3c/c.md".to_owned()]);
    let doc = app.doc("wp3c/c.md").expect("at c");
    assert_eq!(doc.text(), "xbody");
    assert!(!doc.is_dirty() && doc.conflict().is_none() && !doc.is_missing());
    assert_eq!(d.read_file("wp3c/c.md").expect("c").0, b"xbody");
    assert!(d.read_file("wp3c/a.md").is_err(), "nothing recreated at a");
}

#[test]
fn dirty_tab_moved_before_its_save_hits_the_conflict_bar_when_c_changed() {
    let (_tmp, d, mut app) = setup(&["wp3c2/a.md"]);
    type_x(&mut app);
    let _unfed = d.daemon_move("wp3c2/a.md", "wp3c2/c.md").expect("move");
    d.write_note("wp3c2/c.md", "other", &ContentHash::of(b"body"))
        .expect("c edited elsewhere");
    save_now(&mut app, &d, "wp3c2/a.md");
    let doc = app.doc("wp3c2/c.md").expect("at c");
    assert_eq!(doc.text(), "xbody", "dirty text kept");
    assert!(doc.is_dirty() && doc.conflict().is_some() && !doc.is_missing());
    assert_eq!(
        d.read_file("wp3c2/c.md").expect("c").0,
        b"other",
        "no overwrite"
    );
}

#[test]
fn tab_follows_its_own_note_when_a_new_note_took_the_old_path() {
    let (_tmp, d, mut app) = setup(&["wp3d/a.md"]);
    let pair = d.daemon_move("wp3d/a.md", "wp3d/b.md").expect("move");
    d.create_note("wp3d/a.md", "new").expect("new note at a");
    feed(&mut app, pair);
    assert_eq!(open_paths(&app), vec!["wp3d/b.md".to_owned()]);
    assert_eq!(app.doc("wp3d/b.md").expect("own note").text(), "body");
    assert!(app.doc("wp3d/a.md").is_none());
    assert_eq!(app.status.error_text(), None);
}

#[test]
fn a_delayed_move_notification_leaves_a_tab_opened_after_it_alone() {
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note("wp3e/a.md", "x").expect("x");
    let delayed = d.daemon_move("wp3e/a.md", "wp3e/b.md").expect("move");
    d.create_note("wp3e/a.md", "y").expect("y at a");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp3e/a.md".into())));
    feed(&mut app, delayed);
    assert_eq!(open_paths(&app), vec!["wp3e/a.md".to_owned()]);
    assert_eq!(app.doc("wp3e/a.md").expect("y").text(), "y");
    assert!(app.doc("wp3e/b.md").is_none());
    assert_eq!(app.status.error_text(), None);
}

#[test]
fn a_real_delete_still_shows_file_deleted_on_disk() {
    let (_tmp, d, mut app) = setup(&["wp3f/a.md"]);
    d.delete("wp3f/a.md").expect("delete");
    feed(
        &mut app,
        vec![deleted_event("wp3f/a.md", d.lineage_head().ok())],
    );
    assert_eq!(app.status.error_text(), Some("file deleted on disk"));
}

#[test]
fn a_delayed_replaced_notification_leaves_a_tab_opened_after_it_alone() {
    let (_tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note("wp3g/a.md", "x").expect("x");
    d.create_note("wp3g/t.md", "target").expect("target");
    let delayed = d
        .daemon_delete_replaced("wp3g/a.md", "wp3g/t.md")
        .expect("replaced");
    d.create_note("wp3g/a.md", "y").expect("y at a");
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp3g/a.md".into())));
    feed(&mut app, delayed);
    assert_eq!(open_paths(&app), vec!["wp3g/a.md".to_owned()]);
    assert_eq!(app.doc("wp3g/a.md").expect("y").text(), "y");
    assert_eq!(app.status.error_text(), None);
}
