//! Close and quit: save-first closes, in-flight saves, duplicate tabs, the unsaved-edits dialog at quit, pending writes and the state flush.

mod save_first {

    use super::super::*;
    use crate::message::TabsMsg;

    use super::super::commands::{app_with, open, type_char};
    use crate::reading::ViewMode;

    #[test]
    fn close_tab_saves_a_dirty_buffer_first_and_cancels_on_conflict() {
        let (_tmp, d, mut app) = app_with(&[("wp4c/a.md", "a"), ("wp4c/b.md", "b")]);
        open(&mut app, "wp4c/a.md");
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4c/a.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'x');
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        assert!(app.open_paths().is_empty(), "closed after the save");
        assert_eq!(d.read_file("wp4c/a.md").expect("file").0, b"xa");

        // Conflict: the file changes on disk under a dirty buffer → the close is cancelled.
        open(&mut app, "wp4c/b.md");
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4c/b.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'y');
        let (_, h) = d.read_file("wp4c/b.md").expect("file");
        d.write_note("wp4c/b.md", "theirs", &h)
            .expect("external write");
        let _ = app.send(Message::Tabs(TabsMsg::Close(app.focus, 0)));
        assert_eq!(app.open_paths(), ["wp4c/b.md"], "close waits on the dialog");
        assert!(app.dialog.is_some(), "plan editor-data-safety WP-2: dialog");
        assert!(!app.exiting);
    }

    #[test]
    fn quit_flushes_dirty_buffers_then_exits_and_cancels_on_failure() {
        let (_tmp, d, mut app) = app_with(&[("wp4q/a.md", "a")]);
        open(&mut app, "wp4q/a.md");
        let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4q/a.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app, 'x');
        let (_, h) = d.read_file("wp4q/a.md").expect("file");
        d.write_note("wp4q/a.md", "theirs", &h)
            .expect("external write");
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting, "a failed save stops the quit");
        assert!(app.dialog.is_some(), "plan editor-data-safety WP-2: dialog");

        let (_tmp2, d2, mut app2) = app_with(&[("wp4q/b.md", "b")]);
        open(&mut app2, "wp4q/b.md");
        let _ = app2.send(Message::Reading(crate::message::ReadingMsg::SetMode {
            path: "wp4q/b.md".into(),
            mode: ViewMode::Source,
        }));
        type_char(&mut app2, 'y');
        let _ = app2.send(Message::Tabs(TabsMsg::Quit));
        assert!(app2.exiting);
        assert_eq!(d2.read_file("wp4q/b.md").expect("file").0, b"yb");
    }
}

mod in_flight {

    use super::super::*;

    use super::super::autosave::{in_flight, land, setup};
    use crate::panes;
    use crate::source::SaveOutcome;

    #[test]
    fn quit_waits_for_an_in_flight_save_then_exits() {
        let (_tmp, _d, mut app) = setup(&["w3q/a.md"]);
        let seq = in_flight(&mut app, "w3q/a.md");
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting, "waits instead of refusing");
        assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
        land(
            &mut app,
            "w3q/a.md",
            seq,
            SaveOutcome::Saved(ContentHash::from_hex("h1")),
        );
        assert!(app.exiting, "exits once the save lands clean");
    }

    #[test]
    fn quit_while_saving_then_conflict_cancels() {
        let (_tmp, _d, mut app) = setup(&["w3qc/a.md"]);
        let seq = in_flight(&mut app, "w3qc/a.md");
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        land(
            &mut app,
            "w3qc/a.md",
            seq,
            SaveOutcome::Conflict(ContentHash::from_hex("abc")),
        );
        assert!(!app.exiting);
        assert!(app.quit.is_none(), "quit state cleared, not hanging");
        assert!(app.dialog.is_some(), "plan editor-data-safety WP-2: dialog");
    }

    #[test]
    fn close_while_saving_closes_after_it_lands() {
        let (_tmp, _d, mut app) = setup(&["w3x/a.md", "w3x/b.md"]);
        let seq = in_flight(&mut app, "w3x/a.md");
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        assert!(panes::find(&app.panes, "w3x/a.md").is_some(), "still open");
        assert!(app.status.error_text().is_none(), "{:?}", app.status.notice);
        land(
            &mut app,
            "w3x/a.md",
            seq,
            SaveOutcome::Saved(ContentHash::from_hex("h1")),
        );
        assert!(panes::find(&app.panes, "w3x/a.md").is_none(), "closed");
        assert!(panes::find(&app.panes, "w3x/b.md").is_some());
    }
}

mod duplicates {

    use super::super::*;
    use crate::app::Tab;

    use super::super::shared_docs::{duplicated, saved, setup, type_x, views};

    #[test]
    fn closing_one_duplicate_keeps_the_others_document_and_reading_state() {
        let (_tmp, _d, mut app, left, right) = duplicated();
        assert!(app.reading.doc(&app.docs, "w7/a.md").is_some(), "rendered");
        let _ = app.send(Message::Tabs(TabsMsg::Close(right, 0)));
        let v = views(&app, "w7/a.md");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, left);
        assert!(app.doc("w7/a.md").is_some());
        assert!(
            app.reading.doc(&app.docs, "w7/a.md").is_some(),
            "reading state kept"
        );
        let _ = app.send(Message::Tabs(TabsMsg::Close(left, 0)));
        assert!(app.doc("w7/a.md").is_none(), "dropped with the last tab");
        assert!(app.reading.doc(&app.docs, "w7/a.md").is_none());
    }

    #[test]
    fn e1_closing_either_dirty_duplicate_closes_exactly_that_tab() {
        for close_left in [true, false] {
            // Different panes (the save-before-close runs inline).
            let (_tmp, d, mut app, left, right) = duplicated();
            type_x(&mut app);
            let target = if close_left { left } else { right };
            let all = views(&app, "w7/a.md");
            let gone = all.iter().find(|v| v.0 == target).expect("view").1;
            let _ = app.send(Message::Tabs(TabsMsg::Close(target, 0)));
            let now = views(&app, "w7/a.md");
            assert_eq!(now.len(), 1, "close_left={close_left}");
            assert_ne!(now[0].1, gone, "close_left={close_left}");
            assert_ne!(now[0].0, target);
            assert!(app.doc("w7/a.md").is_some_and(|d| !d.is_dirty()), "saved");
            assert!(d.read_file("w7/a.md").expect("file").0.starts_with(b"x"));

            // Same pane, closed while a save is in flight (the close waits on it).
            let (_tmp, _d, mut app) = setup(&["w7s/a.md"]);
            let pane = app.focus;
            let extra = Tab::bound("w7s/a.md", app.note_id("w7s/a.md").expect("bound"));
            let extra_id = extra.id;
            let first_id = app.active_note().expect("tab").id;
            if let Some(ps) = app.panes.get_mut(pane) {
                ps.tabs.push(extra);
            }
            type_x(&mut app);
            let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
            let seq = app.doc("w7s/a.md").expect("doc").edit_seq();
            let i = usize::from(!close_left);
            let _ = app.send(Message::Tabs(TabsMsg::Close(pane, i)));
            assert_eq!(views(&app, "w7s/a.md").len(), 2, "waits on the save");
            saved(&mut app, "w7s/a.md", seq);
            let kept = if close_left { extra_id } else { first_id };
            assert_eq!(
                views(&app, "w7s/a.md"),
                [(pane, kept)],
                "close_left={close_left}"
            );
            assert!(app.doc("w7s/a.md").is_some(), "shared document intact");
        }
    }

    #[test]
    fn quit_with_duplicates_awaits_the_document_once() {
        let (_tmp, _d, mut app, _left, _right) = duplicated();
        type_x(&mut app);
        let _ = app.send(Message::Tabs(TabsMsg::FlushAll));
        let seq = app.doc("w7/a.md").expect("doc").edit_seq();
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert_eq!(app.quit_waits(), Some(1));
        saved(&mut app, "w7/a.md", seq);
        assert!(app.exiting);
    }
}

mod loss_dialog {

    use super::super::unsaved_dialog::{dialog_notes, save, setup, type_x};
    use super::super::*;
    use crate::dialog::{Dialog, DialogMsg, Discard, LossReason};
    use crate::header::QuickSwitchOpen;
    use crate::message::{PanesMsg, SourceMsg, SwitcherMsg};
    use crate::source::SaveOutcome;

    #[test]
    fn quit_with_a_disconnected_dirty_note_shows_the_dialog_ok_exits_cancel_returns() {
        let (_tmp, d, mut app) = setup(&["d2q/a.md"]);
        type_x(&mut app);
        app.status.connected = false;
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting);
        assert_eq!(
            dialog_notes(&app),
            [("d2q/a.md".to_owned(), LossReason::Disconnected)]
        );
        let _ = app.send(Message::Dialog(DialogMsg::Cancel));
        assert!(app.dialog.is_none() && !app.exiting);
        assert!(app.doc("d2q/a.md").is_some_and(|t| t.is_dirty()));
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(app.dialog.is_some());
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert!(app.exiting);
        assert_eq!(
            d.read_file("d2q/a.md").expect("file").0,
            b"body",
            "nothing written"
        );
    }

    #[test]
    fn quit_with_every_flush_succeeding_exits_without_a_dialog() {
        let (_tmp, d, mut app) = setup(&["d2s/a.md"]);
        type_x(&mut app);
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(app.dialog.is_none());
        assert!(app.exiting);
        assert_eq!(d.read_file("d2s/a.md").expect("file").0, b"xbody");
    }

    /// WP-1 hand-off: a quit awaiting a save that failed during an outage lands in the dialog.
    #[test]
    fn quit_awaiting_a_save_that_failed_shows_the_dialog() {
        let (_tmp, _d, mut app) = setup(&["d2f/a.md"]);
        type_x(&mut app);
        let job = app.doc_mut("d2f/a.md").and_then(|t| t.begin_save());
        let seq = job.expect("job").seq;
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert_eq!(app.quit_waits(), Some(1));
        let _ = app.send(Message::Source(SourceMsg::Saved {
            id: app.note_id("d2f/a.md"),
            seq,
            outcome: SaveOutcome::Failed("daemon disconnected".into()),
            at: None,
            after: None,
        }));
        assert!(!app.exiting);
        assert_eq!(dialog_notes(&app).len(), 1);
        assert!(matches!(
            app.dialog,
            Some(Dialog::Unsaved {
                then: Discard::Quit,
                ..
            })
        ));
    }

    #[test]
    fn quit_with_only_a_validation_blocked_settings_edit_shows_the_dialog() {
        let (_tmp, _d, mut app) = setup(&[]);
        let mut f = crate::settings::ConfigFile::new("_custom/crucible-vault/x.toml", None);
        f.test_mark_dirty(); // `doc: None` → `wants_save()` is false: the flush can never write it.
        app.settings.files.insert(f.path().to_owned(), f);
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting);
        let u = app
            .dialog
            .as_ref()
            .and_then(|d| match d {
                crate::dialog::Dialog::Unsaved { losses, .. } => Some(losses),
                _ => None,
            })
            .expect("dialog");
        assert_eq!(u.settings, ["_custom/crucible-vault/x.toml"]);
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert!(app.exiting);
    }

    #[test]
    fn tab_close_of_a_conflicted_duplicate_closes_only_that_tab_on_ok() {
        let (_tmp, d, mut app) = setup(&["d2c/a.md"]);
        let _ = app.send(Message::Panes(PanesMsg::SplitRight));
        app.prefs.quick_switch_open = QuickSwitchOpen::Duplicate;
        let _ = app.send(Message::Switcher(SwitcherMsg::Pick("d2c/a.md".into())));
        let count = |app: &App| {
            app.panes
                .iter()
                .flat_map(|(_, ps)| ps.tabs.iter())
                .filter(|t| t.shows("d2c/a.md"))
                .count()
        };
        assert_eq!(count(&app), 2);
        type_x(&mut app);
        let (_, h) = d.read_file("d2c/a.md").expect("file");
        d.write_note("d2c/a.md", "theirs", &h).expect("external");
        save(&mut app, &d, "d2c/a.md");
        assert!(app.doc("d2c/a.md").is_some_and(|t| t.conflict().is_some()));
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        assert_eq!(
            dialog_notes(&app),
            [("d2c/a.md".to_owned(), LossReason::Conflict)]
        );
        let _ = app.send(Message::Dialog(DialogMsg::Cancel));
        assert_eq!(count(&app), 2, "Cancel keeps it");
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        let _ = app.send(Message::Dialog(DialogMsg::Ok));
        assert_eq!(count(&app), 1, "only that tab closed");
        assert!(
            app.doc("d2c/a.md").is_some_and(|t| t.is_dirty()),
            "the other tab keeps the shared document"
        );
        assert_eq!(d.read_file("d2c/a.md").expect("file").0, b"theirs");
    }
}

mod pending_writes {

    use super::super::*;
    use crate::bookmarks::BookmarkMsg;
    use crate::message::PanesMsg;

    use super::super::save_continue::setup;

    /// SR1: a bookmark write still in flight when quit starts lets the quit finish.
    #[test]
    fn bookmark_write_in_flight_at_quit_completes_the_quit() {
        let (_tmp, _d, mut app) = setup(&["r2bm/a.md"]);
        let _ = app.update(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
        let _ = app.update(Message::Tabs(TabsMsg::Quit));
        assert!(!app.exiting, "waits for the bookmark write");
        app.pump();
        assert!(app.exiting, "the landed bookmark write ends the quit");
    }
}

mod state_flush {
    use super::super::*;

    use crate::message::{ExplorerMsg, PanesMsg, ReadingMsg};

    /// SP2: closing and renaming a document leave no per-document state behind (the Reading
    /// edge / source-selection / place entries leaked on close).
    #[test]
    fn close_and_rename_leave_no_residual_doc_state() {
        let (_tmp, d) = fake_arc();
        let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
        d.create_note("r15/one.md", &body).expect("create");
        let mut app = app_over(&d, None);
        let _ = app.send(Message::Explorer(ExplorerMsg::Open("r15/one.md".into())));
        let view = app.active_note().expect("tab").id;
        let _ = app.send(Message::Reading(ReadingMsg::GoToEdge { bottom: true }));
        app.reading
            .note_source_selection(&mut app.docs, "r15/one.md", view);
        app.reading
            .set_top_block(&mut app.docs, "r15/one.md", view, 3);
        assert_eq!(app.reading.doc_entries(&app.docs), 1);
        // Rename: one re-key, nothing left under the old path.
        let _ = app.send(Message::Explorer(ExplorerMsg::Select("r15/one.md".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::BeginRename));
        let _ = app.send(Message::Explorer(ExplorerMsg::Input("two.md".into())));
        let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
        assert_eq!(app.open_paths(), ["r15/two.md"]);
        assert_eq!(
            app.reading.doc_entries(&app.docs),
            1,
            "re-keyed, not duplicated"
        );
        assert!(app.doc("r15/one.md").is_none());
        assert!(
            !app.reading
                .source_selected_last(&app.docs, "r15/one.md", view)
        );
        assert!(app.reading.edge(&app.docs, "r15/one.md", view).is_none());
        // Close: one remove each.
        let _ = app.send(Message::Tabs(TabsMsg::CloseActive));
        assert!(app.open_paths().is_empty());
        assert!(app.docs.is_empty());
        assert_eq!(
            app.reading.doc_entries(&app.docs),
            0,
            "no leaked reading state"
        );
        assert!(!app.reading.is_restoring(&app.docs, view));
    }

    /// SM12 (b): quit writes the latest state immediately, with no debounce pending.
    #[test]
    fn quit_writes_the_latest_state_immediately() {
        let (tmp, d) = fake_arc();
        let state = tmp.path().join("state/editor.json");
        let mut app = app_over(&d, Some(state.clone()));
        let _ = app.send(Message::Panes(PanesMsg::ToggleSide));
        assert_eq!(app.ui_writes, 0);
        let _ = app.send(Message::Tabs(TabsMsg::Quit));
        assert!(app.exiting);
        assert_eq!(app.ui_writes, 1, "written at exit");
        assert!(
            crate::ui_state::load(&state).side.0.is_none(),
            "the latest state"
        );
        // A debounce tick arriving after the exit write has nothing left to write.
        let _ = app.send(Message::PersistDue);
        assert_eq!(app.ui_writes, 1);
    }
}
