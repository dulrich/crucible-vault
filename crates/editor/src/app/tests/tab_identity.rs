//! Plan WP-R5 (review E3): a note tab holds its note id and reads its path from the document
//! store, so a move updates no tab; a delete confirmed for an open note goes by its id.

use super::shared_docs::setup;
use super::*;
use crate::app::{Place, TabKind};
use crate::message::PanesMsg;

fn feed(app: &mut App, ns: Vec<Notification>) {
    for n in ns {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
    app.pump();
}

/// Every tab's runtime kind, with its id.
fn tab_kinds(app: &App) -> Vec<(u64, TabKind)> {
    let mut v: Vec<(u64, TabKind)> = app
        .panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter().map(|t| (t.id, t.kind.clone())))
        .collect();
    v.sort_by_key(|(id, _)| *id);
    v
}

#[test]
fn a_moved_document_moves_both_of_its_tabs_with_no_tab_mutation() {
    let (_tmp, d, mut app) = setup(&["r5c/a.md"]);
    app.pump();
    let id = app.note_id("r5c/a.md").expect("open");
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    let second = app.focus;
    app.open_into(second, "r5c/a.md", Place::Duplicate)
        .expect("duplicate");
    app.pump();
    let before = tab_kinds(&app);
    assert_eq!(before.len(), 2, "two tabs");
    assert!(before.iter().all(|(_, k)| *k == TabKind::Note(id)), "bound");
    assert_eq!(app.panes.iter().count(), 2, "two panes");

    let pair = d.daemon_move("r5c/a.md", "r5c/b.md").expect("move");
    feed(&mut app, pair);

    assert_eq!(tab_kinds(&app), before, "no tab changed");
    for (_, ps) in app.panes.iter() {
        let t = ps.active_tab().expect("tab");
        assert_eq!(t.path(&app.docs), Some("r5c/b.md"));
    }
    let _ = app.send(Message::PersistDue);
    let saved: Vec<crate::ui_state::TabKind> = app
        .ui
        .panes
        .as_ref()
        .expect("layout")
        .root
        .leaves()
        .into_iter()
        .flat_map(|(tabs, _)| tabs.to_vec())
        .collect();
    assert_eq!(
        saved,
        vec![crate::ui_state::TabKind::Note("r5c/b.md".into()); 2],
        "the session holds the new path"
    );
}

#[test]
fn a_delete_confirmed_before_a_move_deletes_the_note_at_its_new_path() {
    let (_tmp, d, mut app) = setup(&["r5c/a.md"]);
    app.pump();
    let id = app.note_id("r5c/a.md").expect("open");
    let _ = app.begin_delete("r5c/a.md".to_owned());
    app.pump();
    match app.dialog.as_ref() {
        Some(crate::dialog::Dialog::DeleteConfirm(c)) => {
            assert_eq!(c.path, "r5c/a.md");
            assert_eq!(c.note, Some(id), "opened for the open document");
        }
        other => panic!("delete confirm expected, got {other:?}"),
    }
    let pair = d.daemon_move("r5c/a.md", "r5c/b.md").expect("move");
    feed(&mut app, pair);
    assert_eq!(app.note_id("r5c/b.md"), Some(id), "followed");
    let _ = app.send(Message::Dialog(crate::dialog::DialogMsg::Ok));
    app.pump();
    assert!(
        matches!(d.read_file("r5c/b.md"), Err(DaemonError::NotFound(_))),
        "deleted where it lives now"
    );
}
