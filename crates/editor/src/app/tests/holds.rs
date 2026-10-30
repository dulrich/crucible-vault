//! Plan round-17 WP-2: a daemon job's note hold makes the note read-only, refuses its saves,
//! follows daemon moves, clears on release / reconnect, and shows the overlay after 400 ms.

use std::time::Instant;

use cv_protocol::JobStatus;

use super::unsaved_dialog::{setup, type_x};
use super::*;
use crate::app::holds::{Hold, OVERLAY_DELAY};
use crate::message::SourceMsg;

fn job(id: &str, status: JobState, held: Option<&str>) -> Notification {
    Notification::JobUpdated(JobStatus {
        job_id: id.into(),
        status,
        message: None,
        output_paths: Vec::new(),
        open_path: None,
        hold: held.map(|p| cv_protocol::NoteHold {
            path: p.to_owned(),
            label: "Refine".to_owned(),
        }),
    })
}

fn feed(app: &mut App, n: Notification) {
    let _ = app.send(Message::Status(StatusMsg::Notification(n)));
}

fn notice(app: &App) -> Option<&str> {
    app.status.notice.as_ref().map(|n| n.text.as_str())
}

#[test]
fn held_note_drops_edits_refuses_saves_and_releases_on_terminal() {
    let (_tmp, _d, mut app) = setup(&["h/a.md"]);
    feed(&mut app, job("j1", JobState::Running, Some("h/a.md")));
    assert_eq!(app.held("h/a.md").map(|h| h.label.as_str()), Some("Refine"));
    let before = app.doc("h/a.md").expect("open").text();
    type_x(&mut app);
    let _ = app.send(Message::Source(SourceMsg::Undo));
    let doc = app.doc("h/a.md").expect("open");
    assert_eq!(doc.text(), before, "edit dropped");
    assert!(!doc.is_dirty());
    // An explicit save and `save_then` are refused with the one notice.
    let _ = app.send(Message::Source(SourceMsg::Save));
    assert_eq!(
        notice(&app),
        Some("Refine is updating this note — save held")
    );
    app.doc_mut("h/a.md").expect("open").perform(
        iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert('y')),
        Instant::now(),
    );
    assert!(app.save_then("h/a.md", None).is_none());
    assert!(!app.doc("h/a.md").expect("open").is_saving());
    // The terminal status releases it; edits flow again.
    feed(&mut app, job("j1", JobState::Done, Some("h/a.md")));
    assert!(app.held("h/a.md").is_none());
    type_x(&mut app);
    assert!(app.doc("h/a.md").expect("open").text().contains('x'));
}

#[test]
fn holds_follow_daemon_moves_and_clear_on_disconnect() {
    let (_tmp, d, mut app) = setup(&["Clippings/m.md"]);
    feed(
        &mut app,
        job("j2", JobState::Running, Some("Clippings/m.md")),
    );
    for n in d
        .daemon_move("Clippings/m.md", "fanfic/m.md")
        .expect("move")
    {
        feed(&mut app, n);
    }
    assert!(app.held("Clippings/m.md").is_none());
    assert!(
        app.held("fanfic/m.md").is_some(),
        "the hold followed the move"
    );
    // A republished running status with the new path keeps the hold (and its start time).
    let since = app.held("fanfic/m.md").expect("held").since;
    feed(&mut app, job("j2", JobState::Running, Some("fanfic/m.md")));
    assert_eq!(app.held("fanfic/m.md").expect("held").since, since);
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    assert!(
        app.holds.is_empty(),
        "reconnect clears; the snapshot re-fills"
    );
}

#[test]
fn overlay_shows_only_after_the_delay() {
    let t0 = Instant::now();
    let h = Hold {
        path: "a.md".into(),
        label: "Refine".into(),
        since: t0,
    };
    assert!(!h.overlay_shown(t0));
    assert!(!h.overlay_shown(t0 + OVERLAY_DELAY / 2));
    assert!(h.overlay_shown(t0 + OVERLAY_DELAY));
}
