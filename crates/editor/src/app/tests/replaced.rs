//! Round 11 WP-1 (Decision 1): a deletion carrying `replaced_by` (auto-file absorbed the note)
//! lands the open tabs on the absorbing note instead of "file deleted on disk".

use super::unsaved_dialog::{setup, type_x};
use super::*;
use cv_protocol::{ChangeKind, Origin};

fn deleted_by(app: &mut App, path: &str, by: &str) {
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            id: None,
            path: path.into(),
            kind: ChangeKind::Deleted,
            hash: None,
            origin: Origin::Daemon,
            from: None,
            replaced_by: Some(by.into()),
        },
    )));
}

fn open_paths(app: &App) -> Vec<String> {
    app.panes
        .iter()
        .flat_map(|(_, ps)| {
            ps.tabs
                .iter()
                .filter_map(|t| t.path(&app.docs).map(str::to_owned))
        })
        .collect()
}

#[test]
fn replaced_clean_tab_lands_on_the_target_without_a_deleted_status() {
    let (_tmp, d, mut app) = setup(&["Clippings/b.md"]);
    d.create_note("Blog/t.md", "target").expect("create");
    std::fs::remove_file(d.vault_root().join("Clippings/b.md")).expect("rm");
    deleted_by(&mut app, "Clippings/b.md", "Blog/t.md");
    assert_eq!(open_paths(&app), vec!["Blog/t.md".to_owned()]);
    assert!(app.doc("Clippings/b.md").is_none());
    let doc = app.doc("Blog/t.md").expect("retargeted");
    assert!(!doc.is_missing() && doc.conflict().is_none());
    assert_eq!(doc.text(), "target");
    assert_eq!(app.status.error_text(), None, "no `file deleted on disk`");
    // Plan note-identity audit E2: the tab now shows the absorbing note's own document (its
    // id), so an edit saves and lands in the absorbing note.
    assert_eq!(app.note_id("Blog/t.md"), d.bind_note("Blog/t.md").ok());
    type_x(&mut app);
    super::unsaved_dialog::save(&mut app, &d, "Blog/t.md");
    assert_eq!(d.read_file("Blog/t.md").expect("t").0, b"xtarget");
    assert!(app.doc("Blog/t.md").is_some_and(|b| !b.is_dirty()));
}

#[test]
fn replaced_dirty_clipping_keeps_the_conflict_path() {
    let (_tmp, d, mut app) = setup(&["Clippings/b.md"]);
    d.create_note("Blog/t.md", "target").expect("create");
    type_x(&mut app);
    std::fs::remove_file(d.vault_root().join("Clippings/b.md")).expect("rm");
    deleted_by(&mut app, "Clippings/b.md", "Blog/t.md");
    assert_eq!(open_paths(&app), vec!["Clippings/b.md".to_owned()]);
    let doc = app.doc("Clippings/b.md").expect("kept");
    assert!(doc.is_dirty());
    assert!(doc.is_missing() || doc.conflict().is_some());
}

#[test]
fn replaced_onto_an_open_dirty_target_keeps_its_unsaved_text() {
    let (_tmp, d, mut app) = setup(&["Blog/t.md", "Clippings/b.md"]);
    let pane = app.focus;
    let _ = app.send(Message::Tabs(crate::message::TabsMsg::Select(pane, 0)));
    type_x(&mut app);
    let before = app.doc("Blog/t.md").expect("open").text();
    assert!(app.doc("Blog/t.md").expect("open").is_dirty());
    std::fs::remove_file(d.vault_root().join("Clippings/b.md")).expect("rm");
    deleted_by(&mut app, "Clippings/b.md", "Blog/t.md");
    assert_eq!(
        open_paths(&app),
        vec!["Blog/t.md".to_owned()],
        "clipping tab closed"
    );
    assert!(app.doc("Clippings/b.md").is_none());
    let doc = app.doc("Blog/t.md").expect("target");
    assert!(doc.is_dirty());
    assert_eq!(doc.text(), before, "unsaved target text survives");
    assert_eq!(app.status.error_text(), None);
}
