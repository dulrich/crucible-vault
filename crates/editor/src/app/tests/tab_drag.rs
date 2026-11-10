//! Plan round-10 WP-8: dragging tabs between panes (message level; the pure state machine has
//! its own unit tests in `app/tab_drag.rs`).

use iced::Point;
use iced::widget::pane_grid::Pane;

use super::shared_docs::{setup, type_x};
use super::*;
use crate::app::tab_drag::DropTarget;
use crate::message::PanesMsg;

fn tab(app: &mut App, m: TabsMsg) {
    let _ = app.send(Message::Tabs(m));
}

fn paths(app: &App, pane: Pane) -> Vec<String> {
    app.panes
        .get(pane)
        .map(|ps| {
            ps.tabs
                .iter()
                .filter_map(|t| t.path(&app.docs).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Three notes `a b c` in the left pane (b active after opening order a,b,c → c) and an empty,
/// focused right pane.
fn two_panes() -> (tempfile::TempDir, Arc<FakeDaemon>, App, Pane, Pane) {
    let (tmp, d, mut app) = setup(&["td/a.md", "td/b.md", "td/c.md"]);
    let left = app.focus;
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    let right = app.focus;
    assert_ne!(left, right);
    (tmp, d, app, left, right)
}

/// Press `(pane, i)`, move past the threshold, hover `target`, release.
fn drag(app: &mut App, from: (Pane, usize), target: Option<DropTarget>) {
    tab(app, TabsMsg::DragPress(from.0, from.1));
    tab(app, TabsMsg::DragMove(Point::new(0.0, 0.0)));
    tab(app, TabsMsg::DragMove(Point::new(40.0, 0.0)));
    if let Some(t) = target {
        tab(app, TabsMsg::DragEnter(t));
    }
    tab(app, TabsMsg::DragRelease);
}

#[test]
fn drop_on_other_strip_inserts_at_slot_and_focuses() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    drag(&mut app, (left, 0), Some(DropTarget::End(right)));
    assert_eq!(paths(&app, left), ["td/b.md", "td/c.md"]);
    assert_eq!(paths(&app, right), ["td/a.md"]);
    drag(&mut app, (left, 1), Some(DropTarget::Slot(right, 0)));
    assert_eq!(paths(&app, right), ["td/c.md", "td/a.md"]);
    assert_eq!(app.focus, right);
    assert_eq!(app.panes.get(right).expect("pane").active, Some(0));
    assert_eq!(paths(&app, left), ["td/b.md"]);
    assert_eq!(
        app.panes.get(left).expect("pane").active,
        Some(0),
        "source active index fixed"
    );
}

#[test]
fn drop_on_other_body_appends() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    drag(&mut app, (left, 0), Some(DropTarget::End(right)));
    drag(&mut app, (left, 0), Some(DropTarget::Body(right)));
    assert_eq!(paths(&app, right), ["td/a.md", "td/b.md"]);
    assert_eq!(app.panes.get(right).expect("pane").active, Some(1));
}

#[test]
fn dirty_buffer_and_undo_travel_with_the_tab() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    tab(&mut app, TabsMsg::Select(left, 2));
    type_x(&mut app);
    assert!(app.docs.get("td/c.md").expect("doc").buffer.is_dirty());
    drag(&mut app, (left, 2), Some(DropTarget::End(right)));
    assert_eq!(paths(&app, right), ["td/c.md"]);
    assert!(
        app.docs.get("td/c.md").expect("doc").buffer.is_dirty(),
        "still dirty after the move"
    );
}

#[test]
fn reorder_within_own_strip() {
    let (_tmp, _d, mut app, left, _right) = two_panes();
    drag(&mut app, (left, 0), Some(DropTarget::Slot(left, 2)));
    assert_eq!(paths(&app, left), ["td/b.md", "td/c.md", "td/a.md"]);
    assert_eq!(app.panes.get(left).expect("pane").active, Some(2));
    drag(&mut app, (left, 2), Some(DropTarget::End(left)));
    assert_eq!(paths(&app, left), ["td/b.md", "td/c.md", "td/a.md"]);
}

#[test]
fn cancel_by_esc_release_elsewhere_or_own_body_changes_nothing() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    tab(&mut app, TabsMsg::DragPress(left, 0));
    tab(&mut app, TabsMsg::DragMove(Point::new(0.0, 0.0)));
    tab(&mut app, TabsMsg::DragMove(Point::new(40.0, 0.0)));
    tab(&mut app, TabsMsg::DragEnter(DropTarget::End(right)));
    tab(&mut app, TabsMsg::DragCancel);
    tab(&mut app, TabsMsg::DragRelease);
    drag(&mut app, (left, 0), None);
    drag(&mut app, (left, 0), Some(DropTarget::Body(left)));
    assert_eq!(paths(&app, left), ["td/a.md", "td/b.md", "td/c.md"]);
    assert!(paths(&app, right).is_empty());
}

/// Plan round-11 WP-6: the strip `pane` as previewed mid-drag, as note paths (the dragged tab by
/// its home path).
fn preview(app: &App, pane: Pane) -> Vec<String> {
    use crate::app::tab_drag::Cell;
    let from = app.tab_drag.dragged();
    let own = |p: Pane, i: usize| -> String {
        app.panes
            .get(p)
            .and_then(|ps| ps.tabs.get(i))
            .and_then(|t| t.path(&app.docs).map(str::to_owned))
            .unwrap_or_default()
    };
    let len = app.panes.get(pane).map_or(0, |ps| ps.tabs.len());
    app.tab_drag
        .layout(pane, len)
        .into_iter()
        .map(|c| match c {
            Cell::Tab(i) => own(pane, i),
            Cell::Dragged => {
                let (fp, fi) = from.expect("dragged cell needs a drag");
                own(fp, fi)
            }
        })
        .collect()
}

/// Starts a drag of `from` and hovers `target`, leaving it live.
fn hold(app: &mut App, from: (Pane, usize), target: DropTarget) {
    tab(app, TabsMsg::DragPress(from.0, from.1));
    tab(app, TabsMsg::DragMove(Point::new(0.0, 0.0)));
    tab(app, TabsMsg::DragMove(Point::new(40.0, 0.0)));
    tab(app, TabsMsg::DragEnter(target));
}

#[test]
fn rightward_move_lands_on_the_previewed_slot() {
    let (_tmp, _d, mut app, left, _right) = two_panes();
    hold(&mut app, (left, 0), DropTarget::Slot(left, 2));
    let shown = preview(&app, left);
    assert_eq!(shown, ["td/b.md", "td/c.md", "td/a.md"]);
    tab(&mut app, TabsMsg::DragRelease);
    assert_eq!(paths(&app, left), shown, "release commits the preview");
}

#[test]
fn leftward_move_lands_on_the_previewed_slot() {
    let (_tmp, _d, mut app, left, _right) = two_panes();
    hold(&mut app, (left, 2), DropTarget::Slot(left, 0));
    let shown = preview(&app, left);
    assert_eq!(shown, ["td/c.md", "td/a.md", "td/b.md"]);
    tab(&mut app, TabsMsg::DragRelease);
    assert_eq!(paths(&app, left), shown);
}

#[test]
fn cross_pane_insert_in_the_middle_closes_the_origin_gap() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    drag(&mut app, (left, 0), Some(DropTarget::End(right)));
    drag(&mut app, (left, 0), Some(DropTarget::End(right)));
    // left = [c], right = [a, b]; drag c into the middle of right.
    hold(&mut app, (left, 0), DropTarget::Slot(right, 1));
    let shown = preview(&app, right);
    assert_eq!(shown, ["td/a.md", "td/c.md", "td/b.md"]);
    assert!(
        preview(&app, left).is_empty(),
        "origin strip closes the gap"
    );
    tab(&mut app, TabsMsg::DragRelease);
    assert_eq!(paths(&app, right), shown);
}

#[test]
fn esc_restores_the_original_order() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    hold(&mut app, (left, 0), DropTarget::Slot(left, 2));
    assert_ne!(preview(&app, left), paths(&app, left));
    tab(&mut app, TabsMsg::DragCancel);
    assert_eq!(preview(&app, left), paths(&app, left));
    tab(&mut app, TabsMsg::DragRelease);
    assert_eq!(paths(&app, left), ["td/a.md", "td/b.md", "td/c.md"]);
    assert!(paths(&app, right).is_empty());
}

#[test]
fn press_release_under_threshold_selects_without_moving() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    tab(&mut app, TabsMsg::DragPress(left, 0));
    tab(&mut app, TabsMsg::DragMove(Point::new(10.0, 10.0)));
    tab(&mut app, TabsMsg::DragMove(Point::new(12.0, 11.0)));
    tab(&mut app, TabsMsg::DragEnter(DropTarget::End(right)));
    tab(&mut app, TabsMsg::DragRelease);
    assert_eq!(app.focus, left);
    assert_eq!(app.panes.get(left).expect("pane").active, Some(0));
    assert_eq!(paths(&app, left).len(), 3);
    assert!(paths(&app, right).is_empty());
}

#[test]
fn moving_the_last_tab_leaves_an_empty_pane_like_a_close() {
    let (_tmp, _d, mut app, left, right) = two_panes();
    drag(&mut app, (left, 0), Some(DropTarget::End(right)));
    drag(&mut app, (left, 0), Some(DropTarget::End(right)));
    drag(&mut app, (left, 0), Some(DropTarget::End(right)));
    assert!(paths(&app, left).is_empty());
    assert_eq!(app.panes.get(left).expect("pane").active, None);
    assert_eq!(app.panes.len(), 2, "no pane collapse, as after a close");
    assert_eq!(paths(&app, right).len(), 3);
}
