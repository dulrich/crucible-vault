//! Shared documents across duplicate tabs and the Quick Switch modes.

use std::collections::HashSet;

use iced::widget::pane_grid::Pane;
use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::header::QuickSwitchOpen;
use crate::message::{ExplorerMsg, PanesMsg, SourceMsg, SwitcherMsg};
use crate::reading::ViewMode;
use crate::source::SaveOutcome;

/// Opens `notes` into one pane (rpc inline).
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

fn quick(app: &mut App, mode: QuickSwitchOpen, path: &str) {
    app.prefs.quick_switch_open = mode;
    let _ = app.send(Message::Switcher(SwitcherMsg::Pick(path.to_owned())));
}

fn split(app: &mut App) -> Pane {
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    app.focus
}

/// Every `(pane, tab id)` showing `path`.
pub(super) fn views(app: &App, path: &str) -> Vec<(Pane, u64)> {
    app.panes
        .iter()
        .flat_map(|(p, ps)| {
            ps.tabs
                .iter()
                .filter(|t| t.shows(path))
                .map(move |t| (*p, t.id))
        })
        .collect()
}

fn tab_paths(app: &App, pane: Pane) -> Vec<String> {
    app.panes
        .get(pane)
        .map(|ps| {
            ps.tabs
                .iter()
                .filter_map(|t| t.path().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `w7/a.md` open in the left pane and duplicated into the (focused) right pane.
pub(super) fn duplicated() -> (tempfile::TempDir, Arc<FakeDaemon>, App, Pane, Pane) {
    let (tmp, d, mut app) = setup(&["w7/a.md"]);
    let left = app.focus;
    let right = split(&mut app);
    quick(&mut app, QuickSwitchOpen::Duplicate, "w7/a.md");
    assert_eq!(views(&app, "w7/a.md").len(), 2);
    (tmp, d, app, left, right)
}

pub(super) fn saved(app: &mut App, path: &str, seq: u64) {
    let _ = app.send(Message::Source(SourceMsg::Saved {
        path: path.to_owned(),
        seq,
        outcome: SaveOutcome::Saved(ContentHash::from_hex("h1")),
        after: None,
    }));
}

#[test]
fn image_pins_are_per_view() {
    use crate::image_cache::{Decoded, ImageCache, view_owner};
    let decoded = || Decoded {
        handle: iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
        width: 1,
        height: 1,
        bytes: 4,
    };
    let mut c = ImageCache::with_budget(1 << 20);
    let (v1, v2) = (view_owner("a.md", 1), view_owner("a.md", 2));
    assert!(c.show(&v1, "k"));
    assert!(!c.show(&v2, "k"), "the second view waits on the same load");
    assert!(c.loaded("k", Some(decoded())));
    // show / show / hide: the other view keeps the pin.
    c.hide(&v1, "k");
    assert!(c.contains("k"), "the other view still shows it");
    assert_eq!(c.pins("k"), 1);
    c.hide(&v2, "k");
    assert!(!c.contains("k"), "no view shows it: released");
    // A bare-path release (last tab closed) covers every view.
    assert!(c.show(&v1, "k"));
    assert!(c.loaded("k", Some(decoded())));
    c.show(&v2, "k");
    c.release_owner("a.md");
    assert!(c.is_empty());
    // ... and never another note whose path merely starts with it.
    assert!(c.show(&view_owner("a.md.md", 3), "j"));
    assert!(c.loaded("j", Some(decoded())));
    c.retain_owner("a.md", &HashSet::new());
    assert!(c.contains("j"));
}

#[test]
fn duplicate_tabs_share_one_document_and_its_edits() {
    let (_tmp, _d, mut app, left, right) = duplicated();
    assert_eq!(app.docs.len(), 1);
    type_x(&mut app);
    assert!(app.doc("w7/a.md").is_some_and(|d| d.is_dirty()));
    app.focus = left;
    let text = app.active_doc().expect("doc").text();
    assert!(text.starts_with('x'), "{text}");
    assert_eq!(tab_paths(&app, right), ["w7/a.md"]);
}

#[test]
fn save_completion_with_two_tabs_of_one_path() {
    let (_tmp, _d, mut app, _left, _right) = duplicated();
    type_x(&mut app);
    let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
    let seq = app.doc("w7/a.md").expect("doc").edit_seq();
    assert!(app.doc("w7/a.md").expect("doc").is_saving());
    saved(&mut app, "w7/a.md", seq);
    let doc = app.doc("w7/a.md").expect("doc");
    assert!(!doc.is_dirty() && !doc.is_saving());
    assert_eq!(views(&app, "w7/a.md").len(), 2, "both tabs stay");
    // The autosave timer is path-keyed too: the one (clean) document answers it.
    let _ = app.send(Message::Source(SourceMsg::AutosaveDue {
        path: "w7/a.md".into(),
        seq,
    }));
    assert!(!app.doc("w7/a.md").expect("doc").is_saving(), "clean");
}

#[test]
fn e2_shared_document_is_shaped_from_the_union_of_its_views() {
    let (_tmp, d, mut app, left, right) = duplicated();
    d.create_note("w7/b.md", "other").expect("create");
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: "w7/a.md".into(),
        mode: ViewMode::Source,
    }));
    assert!(app.doc("w7/a.md").expect("doc").is_shaped());
    // The right pane switches to another note: the left pane still shows a.md.
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("w7/b.md".into())));
    assert_eq!(app.focus, right);
    assert_eq!(tab_paths(&app, right), ["w7/a.md", "w7/b.md"]);
    assert!(
        app.doc("w7/a.md").expect("doc").is_shaped(),
        "still visible in the left pane"
    );
    // Hidden in both panes → plain text.
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("w7/b.md".into())));
    let _ = app.send(Message::Tabs(TabsMsg::Close(left, 0)));
    assert!(app.doc("w7/a.md").is_some_and(|d| !d.is_shaped()));
}

#[test]
fn move_carries_unsaved_edits_and_undo() {
    let (_tmp, _d, mut app) = setup(&["w7m/a.md"]);
    let left = app.focus;
    let id = app.active_note().expect("tab").id;
    type_x(&mut app);
    let right = split(&mut app);
    quick(&mut app, QuickSwitchOpen::Move, "w7m/a.md");
    assert!(tab_paths(&app, left).is_empty());
    assert_eq!(views(&app, "w7m/a.md"), [(right, id)], "the same tab moved");
    assert_eq!(app.focus, right);
    let doc = app.active_doc().expect("doc");
    assert!(doc.is_dirty() && doc.text().starts_with('x'));
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(app.active_doc().expect("doc").text(), "body");
}

#[test]
fn quick_switch_modes_cover_other_pane_active_pane_and_nowhere() {
    for mode in QuickSwitchOpen::ALL {
        // Open in another pane.
        let (_tmp, d, mut app) = setup(&["w7q/a.md"]);
        d.create_note("w7q/n.md", "new").expect("create");
        let left = app.focus;
        let right = split(&mut app);
        quick(&mut app, mode, "w7q/a.md");
        let (want_left, want_right, focus): (&[&str], &[&str], Pane) = match mode {
            QuickSwitchOpen::Switch => (&["w7q/a.md"], &[], left),
            QuickSwitchOpen::Move => (&[], &["w7q/a.md"], right),
            QuickSwitchOpen::Duplicate => (&["w7q/a.md"], &["w7q/a.md"], right),
        };
        assert_eq!(tab_paths(&app, left), want_left, "{mode:?}");
        assert_eq!(tab_paths(&app, right), want_right, "{mode:?}");
        assert_eq!(app.focus, focus, "{mode:?}");
        assert_eq!(app.docs.len(), 1, "{mode:?}");

        // Open nowhere: opens in the active pane.
        quick(&mut app, mode, "w7q/n.md");
        assert_eq!(
            app.active_note().map(|t| t.path),
            Some("w7q/n.md"),
            "{mode:?}"
        );
        assert_eq!(app.focus, focus, "{mode:?}");

        // Already in the active pane: just focused.
        let (_tmp2, _d2, mut app2) = setup(&["w7q/a.md", "w7q/b.md"]);
        let pane = app2.focus;
        quick(&mut app2, mode, "w7q/a.md");
        assert_eq!(tab_paths(&app2, pane), ["w7q/a.md", "w7q/b.md"], "{mode:?}");
        assert_eq!(
            app2.active_note().map(|t| t.path),
            Some("w7q/a.md"),
            "{mode:?}"
        );
    }
}

#[test]
fn restore_keeps_the_same_path_in_two_panes() {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    d.create_note("w7r/a.md", "body").expect("create");
    let state = tmp.path().join("editor.json");
    {
        let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
        let mut app = App::new(Some(dc), Some(state.clone()));
        app.queue_rpcs();
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("w7r/a.md".into())));
        split(&mut app);
        quick(&mut app, QuickSwitchOpen::Duplicate, "w7r/a.md");
        assert_eq!(views(&app, "w7r/a.md").len(), 2);
        let _ = app.send(Message::PersistDue);
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let app = App::new(Some(dc), Some(state));
    assert_eq!(views(&app, "w7r/a.md").len(), 2, "both tabs restore");
    assert_eq!(app.docs.len(), 1, "one document");
}

#[test]
fn rename_retargets_every_tab_and_the_document_key() {
    let (_tmp, d, mut app, _left, _right) = duplicated();
    let ids: Vec<u64> = views(&app, "w7/a.md").iter().map(|v| v.1).collect();
    d.rename("w7/a.md", "w7/z.md").expect("rename");
    let _ = app.send(Message::Explorer(ExplorerMsg::Done(
        crate::explorer::FileOpDone {
            op: crate::explorer::FileOp::Rename {
                from: "w7/a.md".into(),
                to: "w7/z.md".into(),
            },
            result: Ok("w7/z.md".into()),
        },
    )));
    assert!(views(&app, "w7/a.md").is_empty());
    let now: Vec<u64> = views(&app, "w7/z.md").iter().map(|v| v.1).collect();
    assert_eq!(now, ids, "every tab follows, ids kept");
    assert!(app.doc("w7/a.md").is_none());
    assert_eq!(app.doc("w7/z.md").map(|d| d.path()), Some("w7/z.md"));
    assert_eq!(app.docs.len(), 1);
    assert!(app.closed().is_empty(), "SR13: a rename closes no tab");
}
