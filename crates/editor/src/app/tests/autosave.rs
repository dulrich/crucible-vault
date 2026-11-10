//! File auto-save timing and flush points.

use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::message::{ExplorerMsg, SourceMsg};
use crate::panes;
use crate::source::SaveOutcome;

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

fn tab<'a>(app: &'a mut App, path: &str) -> &'a mut crate::source::SourceBuffer {
    app.doc_mut(path).expect("open")
}

fn type_x(app: &mut App) {
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('x'),
    ))));
}

fn dirty_in_place(app: &mut App, path: &str) {
    let _ = tab(app, path).perform(Action::Edit(Edit::Insert('y')), std::time::Instant::now());
}

fn select(app: &mut App, path: &str) {
    let (p, i) = panes::find(&app.panes, path).expect("open");
    let _ = app.send(Message::Tabs(TabsMsg::Select(p, i)));
}

#[test]
fn autosave_delay_is_one_second() {
    assert_eq!(crate::source::AUTOSAVE_DELAY, Duration::from_secs(1));
}

#[test]
fn switching_tabs_away_from_a_dirty_note_starts_its_save() {
    let (_tmp, _d, mut app) = setup(&["w3s/a.md", "w3s/b.md"]);
    select(&mut app, "w3s/a.md");
    type_x(&mut app);
    assert!(tab(&mut app, "w3s/a.md").is_dirty());
    assert!(!tab(&mut app, "w3s/a.md").is_saving());
    select(&mut app, "w3s/b.md");
    assert!(
        tab(&mut app, "w3s/a.md").is_saving(),
        "flush started on switch"
    );
    // Alt+N back: leaving the clean tab starts nothing.
    let _ = app.send(Message::Tabs(TabsMsg::GoTo(1)));
    assert!(!tab(&mut app, "w3s/b.md").is_saving());
}

#[test]
fn unfocus_flushes_every_dirty_note() {
    let (_tmp, _d, mut app) = setup(&["w3u/a.md", "w3u/b.md", "w3u/c.md"]);
    select(&mut app, "w3u/a.md");
    type_x(&mut app);
    dirty_in_place(&mut app, "w3u/b.md");
    let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
    assert!(tab(&mut app, "w3u/a.md").is_saving());
    assert!(tab(&mut app, "w3u/b.md").is_saving());
    assert!(!tab(&mut app, "w3u/c.md").is_saving(), "clean: no-op");
}

#[test]
fn flush_skips_conflicted_buffers() {
    let (_tmp, _d, mut app) = setup(&["w3c/a.md", "w3c/b.md"]);
    select(&mut app, "w3c/a.md");
    type_x(&mut app);
    let t = tab(&mut app, "w3c/a.md");
    let job = t.begin_save().expect("job");
    let _ = t.finish_save(job.seq, SaveOutcome::Conflict(ContentHash::from_hex("abc")));
    assert!(t.conflict().is_some());
    select(&mut app, "w3c/b.md");
    let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
    let t = tab(&mut app, "w3c/a.md");
    assert!(!t.is_saving(), "a conflicted buffer is never auto-saved");
    assert!(t.is_dirty());
}

#[test]
fn edit_during_a_flush_started_save_requeues() {
    let (_tmp, _d, mut app) = setup(&["w3r/a.md", "w3r/b.md"]);
    select(&mut app, "w3r/a.md");
    type_x(&mut app);
    let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
    let t = tab(&mut app, "w3r/a.md");
    assert!(t.is_saving());
    let seq = t.edit_seq();
    type_x(&mut app);
    // A second flush while saving only queues the follow-up.
    let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
    let t = tab(&mut app, "w3r/a.md");
    let again = t
        .finish_save(seq, SaveOutcome::Saved(ContentHash::from_hex("h1")))
        .expect("saved");
    assert!(again, "the newer edit re-queues a save");
    assert!(t.is_dirty());
}

/// Starts a flush (async task, left in flight) and returns its seq.
pub(super) fn in_flight(app: &mut App, path: &str) -> u64 {
    select(app, path);
    type_x(app);
    let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
    let t = tab(app, path);
    assert!(t.is_saving());
    t.edit_seq()
}

pub(super) fn land(app: &mut App, path: &str, seq: u64, outcome: SaveOutcome) {
    let _ = app.send(Message::Source(SourceMsg::Saved {
        id: app.note_id(path),
        seq,
        outcome,
        after: None,
    }));
}
