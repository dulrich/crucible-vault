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
