//! Panes, explorer, switcher, bookmarks and nav (against `FakeDaemon`, RPC run inline).

use std::sync::Mutex;

use chrono::NaiveDate;
use editor_model::period::{NavTarget, Period};

use super::*;
use crate::bookmarks::{BOOKMARKS_PATH, BookmarkMsg};
use crate::message::{ExplorerMsg, NavMsg, PanesMsg, SwitcherMsg};
use crate::panes;

fn changed(app: &mut App, path: &str, kind: ChangeKind) {
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: path.to_owned(),
            kind,
            hash: None,
            origin: Origin::Daemon,
        },
    )));
}

fn row_paths(app: &App) -> Vec<String> {
    app.explorer.rows.iter().map(|r| r.path.clone()).collect()
}

#[test]
fn explorer_tree_build_and_incremental() {
    let (_tmp, d) = fake_arc();
    for p in [
        "wp4tree/Zeta/b.md",
        "wp4tree/alpha/c.md",
        "wp4tree/alpha/A.md",
        "wp4tree/B.md",
        "wp4tree/a.md",
    ] {
        d.create_note(p, "x").expect("create");
    }
    let mut app = app_over(&d, None);
    assert!(app.explorer.contains("wp4tree/alpha/A.md"));
    assert!(row_paths(&app).contains(&"wp4tree".to_owned()));
    assert!(
        !row_paths(&app).contains(&"wp4tree/a.md".to_owned()),
        "collapsed"
    );
    let _ = app.send(Message::Explorer(ExplorerMsg::Toggle("wp4tree".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Toggle(
        "wp4tree/alpha".into(),
    )));
    let rows = row_paths(&app);
    let start = rows
        .iter()
        .position(|p| p == "wp4tree")
        .expect("folder row");
    assert_eq!(
        &rows[start + 1..start + 7],
        [
            "wp4tree/alpha",
            "wp4tree/alpha/A.md",
            "wp4tree/alpha/c.md",
            "wp4tree/Zeta",
            "wp4tree/a.md",
            "wp4tree/B.md",
        ],
        "folders first, case-insensitive"
    );
    assert!(app.ui.expanded.contains("wp4tree/alpha"));

    d.create_note("wp4tree/alpha/new.md", "n").expect("create");
    changed(&mut app, "wp4tree/alpha/new.md", ChangeKind::Created);
    assert!(row_paths(&app).contains(&"wp4tree/alpha/new.md".to_owned()));
    d.delete("wp4tree/a.md").expect("delete");
    changed(&mut app, "wp4tree/a.md", ChangeKind::Deleted);
    assert!(!app.explorer.contains("wp4tree/a.md"));
    assert!(!app.explorer.stale);

    // An external rename names only its destination: stale until the next generation.
    d.rename("wp4tree/B.md", "wp4tree/C.md").expect("rename");
    changed(&mut app, "wp4tree/C.md", ChangeKind::Renamed);
    assert!(app.explorer.stale);
    assert!(app.explorer.contains("wp4tree/B.md"));
    let g = d.generation().expect("gen");
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::IndexGeneration { n: g },
    )));
    assert!(!app.explorer.stale);
    assert!(!app.explorer.contains("wp4tree/B.md"));
    assert!(app.explorer.contains("wp4tree/C.md"));
}

#[test]
fn explorer_rename_move_new_trash_via_rpc() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp4ops/one.md", "body").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp4ops/one.md".into())));
    assert_eq!(app.open_paths(), ["wp4ops/one.md"]);

    let _ = app.send(Message::Explorer(ExplorerMsg::BeginRename));
    assert_eq!(
        app.explorer.edit.as_ref().map(|e| e.input.as_str()),
        Some("one.md")
    );
    let _ = app.send(Message::Explorer(ExplorerMsg::Input("two.md".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
    assert!(
        d.read_file("wp4ops/two.md").is_ok(),
        "renamed through the daemon"
    );
    assert!(app.explorer.contains("wp4ops/two.md"));
    assert!(!app.explorer.contains("wp4ops/one.md"));
    assert_eq!(app.open_paths(), ["wp4ops/two.md"], "the tab follows");
    changed(&mut app, "wp4ops/two.md", ChangeKind::Renamed);
    assert!(!app.explorer.stale, "own rename is matched");

    let _ = app.send(Message::Explorer(ExplorerMsg::BeginMove));
    let _ = app.send(Message::Explorer(ExplorerMsg::Input("wp4moved".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
    assert!(app.explorer.contains("wp4moved/two.md"));
    assert_eq!(app.explorer.selected.as_deref(), Some("wp4moved/two.md"));

    let _ = app.send(Message::Explorer(ExplorerMsg::BeginNew));
    let _ = app.send(Message::Explorer(ExplorerMsg::Input("fresh".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
    assert!(app.explorer.contains("wp4moved/fresh.md"));
    assert_eq!(
        app.active_tab().and_then(|t| t.path()),
        Some("wp4moved/fresh.md")
    );

    let _ = app.send(Message::Explorer(ExplorerMsg::Select(
        "wp4moved/two.md".into(),
    )));
    let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
    let _ = app.send(Message::Dialog(crate::dialog::DialogMsg::Ok));
    assert!(matches!(
        d.read_file("wp4moved/two.md"),
        Err(DaemonError::NotFound(_))
    ));
    assert!(!app.explorer.contains("wp4moved/two.md"));
    assert!(!app.open_paths().contains(&"wp4moved/two.md".to_owned()));
    assert!(app.closed().contains(&"wp4moved/two.md".to_owned()));
}

#[test]
fn rename_and_trash_refuse_a_dirty_open_note() {
    use iced::widget::text_editor::{Action, Edit};
    let (_tmp, d) = fake_arc();
    d.create_note("wp4dirty/one.md", "body").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(
        "wp4dirty/one.md".into(),
    )));
    let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
        Action::Edit(Edit::Insert('x')),
    )));
    assert!(app.active_doc().is_some_and(|t| t.is_dirty()));

    let _ = app.send(Message::Explorer(ExplorerMsg::BeginRename));
    let _ = app.send(Message::Explorer(ExplorerMsg::Input("two.md".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
    assert!(d.read_file("wp4dirty/one.md").is_ok(), "not renamed");
    assert!(d.read_file("wp4dirty/two.md").is_err());
    assert!(app.active_doc().is_some_and(|t| t.is_dirty()), "edits kept");

    let _ = app.send(Message::Explorer(ExplorerMsg::Select(
        "wp4dirty/one.md".into(),
    )));
    let _ = app.send(Message::Explorer(ExplorerMsg::Trash));
    assert!(d.read_file("wp4dirty/one.md").is_ok(), "not trashed");
    assert!(app.status.error_text().is_some());
}

/// Plan round-7-feedback-decisions WP-2: a dirty note's delete opens the unsaved-edits
/// dialog; Cancel keeps everything.
fn dirty_delete_app(d: &Arc<FakeDaemon>, path: &str) -> App {
    use iced::widget::text_editor::{Action, Edit};
    d.create_note(path, "body").expect("create");
    let mut app = app_over(d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(path.into())));
    let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
        Action::Edit(Edit::Insert('x')),
    )));
    let _ = app.send(Message::Explorer(ExplorerMsg::DeleteActive));
    assert!(matches!(
        app.dialog.as_ref(),
        Some(crate::dialog::Dialog::DeleteUnsaved(p)) if p == path
    ));
    app
}

fn confirm_shown(app: &App) -> bool {
    app.dialog
        .as_ref()
        .and_then(|d| match d {
            crate::dialog::Dialog::DeleteConfirm(c) => Some(c),
            _ => None,
        })
        .is_some()
}

#[test]
fn dirty_delete_save_then_delete_writes_then_deletes() {
    use crate::dialog::DialogMsg;
    let (_tmp, d) = fake_arc();
    let mut app = dirty_delete_app(&d, "r7del/s.md");
    let _ = app.send(Message::Dialog(DialogMsg::Ok));
    assert_eq!(
        d.read_file("r7del/s.md").expect("file").0,
        b"xbody",
        "saved first"
    );
    assert!(confirm_shown(&app), "the delete confirm follows");
    let _ = app.send(Message::Dialog(DialogMsg::Ok));
    assert!(matches!(
        d.read_file("r7del/s.md"),
        Err(DaemonError::NotFound(_))
    ));
    assert!(app.open_paths().is_empty());
}

#[test]
fn dirty_delete_discard_then_delete() {
    use crate::dialog::DialogMsg;
    let (_tmp, d) = fake_arc();
    let mut app = dirty_delete_app(&d, "r7del/d.md");
    let _ = app.send(Message::Dialog(DialogMsg::Alt));
    assert!(
        app.doc("r7del/d.md").is_some_and(|t| !t.is_dirty()),
        "edits dropped"
    );
    assert!(confirm_shown(&app));
    let _ = app.send(Message::Dialog(DialogMsg::Ok));
    assert!(matches!(
        d.read_file("r7del/d.md"),
        Err(DaemonError::NotFound(_))
    ));
}

#[test]
fn dirty_delete_cancel_keeps_note_and_edits() {
    use crate::dialog::DialogMsg;
    let (_tmp, d) = fake_arc();
    let mut app = dirty_delete_app(&d, "r7del/c.md");
    let _ = app.send(Message::Dialog(DialogMsg::Cancel));
    assert!(app.dialog.is_none());
    assert_eq!(d.read_file("r7del/c.md").expect("file").0, b"body");
    assert!(
        app.doc("r7del/c.md")
            .is_some_and(|t| t.is_dirty() && t.text() == "xbody")
    );
}

/// WP-2 (round 7): an explorer rename of a clean open note keeps cursor, selection, undo
/// depth and top_line, with no conflict.
#[test]
fn explorer_rename_of_a_clean_note_keeps_editor_state() {
    let (_tmp, d) = fake_arc();
    let body: String = (0..80).map(|i| format!("line {i}\n")).collect();
    d.create_note("r7ren/one.md", &body).expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("r7ren/one.md".into())));
    let doc = app.doc_mut("r7ren/one.md").expect("open");
    doc.select_range((2, 0), (2, 4));
    assert!(doc.paste("LINE".into(), std::time::Instant::now()));
    let job = doc.begin_save().expect("save");
    let outcome = job.run(&*d);
    let _ = app.send(job.done(outcome));
    let doc = app.doc_mut("r7ren/one.md").expect("open");
    doc.scroll_to_line_anchored(30, 0.0);
    doc.select_range((5, 1), (5, 3));
    let before = (doc.cursor(), doc.history().undo_len(), doc.top_line());
    assert!(!doc.is_dirty() && before.1 > 0);
    let _ = app.send(Message::Explorer(ExplorerMsg::Select(
        "r7ren/one.md".into(),
    )));
    let _ = app.send(Message::Explorer(ExplorerMsg::BeginRename));
    let _ = app.send(Message::Explorer(ExplorerMsg::Input("two.md".into())));
    let _ = app.send(Message::Explorer(ExplorerMsg::Submit));
    assert_eq!(app.open_paths(), ["r7ren/two.md"]);
    let doc = app.doc("r7ren/two.md").expect("followed");
    assert_eq!(
        (doc.cursor(), doc.history().undo_len(), doc.top_line()),
        before
    );
    assert_eq!(doc.path(), "r7ren/two.md");
    assert!(!doc.is_dirty() && doc.conflict().is_none());
}

pub(super) fn register_create(d: &Arc<FakeDaemon>) {
    let inner = Arc::clone(d);
    d.register_action(
        ActionInfo {
            id: "note.create".into(),
            title: "Create note".into(),
            group: None,
            keys: vec![],
            params: vec![],
            needs: vec![],
            durable: false,
        },
        Arc::new(move |p: &RunActionParams| {
            let path = p
                .params
                .get("path")
                .and_then(Value::as_str)
                .ok_or("missing path")?;
            inner.create_note(path, "").map_err(|e| e.to_string())?;
            Ok(Some(path.to_owned()))
        }),
    )
    .expect("register");
}

#[test]
fn switcher_ranks_opens_and_creates_on_miss() {
    let (_tmp, d) = fake_arc();
    register_create(&d);
    d.create_note("wp4sw/Works.md", "w").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    assert!(app.switcher.open);
    let _ = app.send(Message::Switcher(SwitcherMsg::Query("works".into())));
    assert_eq!(app.switcher.current(), Some("wp4sw/Works.md"));
    let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
    assert!(!app.switcher.open);
    assert_eq!(
        app.active_tab().and_then(|t| t.path()),
        Some("wp4sw/Works.md")
    );

    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    let _ = app.send(Message::Switcher(SwitcherMsg::Query(
        "qqzz brand idea".into(),
    )));
    assert!(app.switcher.results.is_empty());
    let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
    assert!(d.read_file("qqzz brand idea.md").is_ok(), "note.create ran");
    assert_eq!(
        app.active_tab().and_then(|t| t.path()),
        Some("qqzz brand idea.md")
    );

    // Shift+Enter creates even when something matches.
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    let _ = app.send(Message::Switcher(SwitcherMsg::Query("wp4sw/Works2".into())));
    let _ = app.send(Message::Modifiers(iced::keyboard::Modifiers::SHIFT));
    let _ = app.send(Message::Switcher(SwitcherMsg::Submit));
    assert!(d.read_file("wp4sw/Works2.md").is_ok());
}

#[test]
fn bookmarks_load_add_and_conflict() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp4bm/a.md", "a").expect("create");
    let mut app = app_over(&d, None);
    assert!(
        app.bookmarks.items.is_empty()
            && app
                .settings
                .file(BOOKMARKS_PATH)
                .is_some_and(|f| f.base().is_none())
    );
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp4bm/a.md".into())));
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::AddActive)));
    let (bytes, h1) = d.read_file(BOOKMARKS_PATH).expect("created");
    assert_eq!(
        crate::bookmarks::parse(&bytes).expect("parse"),
        [crate::bookmarks::Bookmark {
            name: "a".into(),
            path: "wp4bm/a.md".into(),
            keys: vec![],
        }]
    );
    assert_eq!(
        app.settings.file(BOOKMARKS_PATH).and_then(|f| f.base()),
        Some(&h1)
    );

    // Loading from an existing file.
    let fresh = app_over(&d, None);
    assert_eq!(fresh.bookmarks.items.len(), 1);

    // Someone else rewrites the file: the next edit hits a CAS conflict and reloads.
    let other = "[[bookmark]]\nname = \"x\"\npath = \"x.md\"\n\n[[bookmark]]\nname = \"y\"\npath = \"y.md\"\n";
    d.write_note(BOOKMARKS_PATH, other, &h1).expect("external");
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Remove(0))));
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("changed on disk"))
    );
    let names: Vec<&str> = app
        .bookmarks
        .items
        .iter()
        .map(|b| b.name.as_str())
        .collect();
    assert_eq!(names, ["x", "y"]);
    assert!(!app.bookmarks.writing);
    let _ = app.send(Message::Panes(PanesMsg::Bookmark(BookmarkMsg::Down(0))));
    let names: Vec<String> = crate::bookmarks::parse(&d.read_file(BOOKMARKS_PATH).expect("r").0)
        .expect("parse")
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(names, ["y", "x"]);
}

fn register_period(d: &Arc<FakeDaemon>, materialize: bool) -> Arc<Mutex<Vec<(String, String)>>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let inner = Arc::clone(d);
    d.register_action(
        ActionInfo {
            id: "period.open".into(),
            title: "Open period".into(),
            group: None,
            keys: vec![],
            params: vec![],
            needs: vec![],
            durable: false,
        },
        Arc::new(move |p: &RunActionParams| {
            let get = |k: &str| {
                p.params
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            let (period, date) = (get("period"), get("date"));
            if let Ok(mut g) = seen.lock() {
                g.push((period.clone(), date.clone()));
            }
            let path = if period == "month" {
                format!("daily/month/{}.md", &date[..7])
            } else {
                format!("daily/day/{date}.md")
            };
            if materialize && inner.read_file(&path).is_err() {
                inner.create_note(&path, "").map_err(|e| e.to_string())?;
            }
            Ok(Some(path))
        }),
    )
    .expect("register");
    calls
}

#[test]
fn nav_runs_period_open_and_opens_in_the_same_pane() {
    let (_tmp, d) = fake_arc();
    let calls = register_period(&d, true);
    d.create_note("daily/day/2026-12-31.md", "d")
        .expect("create");
    d.create_note("wp4nav/other.md", "o").expect("create");
    let mut app = app_over(&d, None);
    app.today = NaiveDate::from_ymd_opt(2026, 10, 13);
    let _ = app.send(Message::Tabs(TabsMsg::Open(
        "daily/day/2026-12-31.md".into(),
    )));
    let day_pane = app.focus;
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    assert_ne!(app.focus, day_pane);
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp4nav/other.md".into())));
    let other_pane = app.focus;

    let tomorrow = NaiveDate::from_ymd_opt(2027, 1, 1).expect("date");
    let _ = app.send(Message::Nav(NavMsg::Go {
        pane: day_pane,
        target: NavTarget::Period(Period::Day, tomorrow),
    }));
    assert_eq!(
        calls.lock().expect("calls").last(),
        Some(&("day".to_owned(), "2027-01-01".to_owned()))
    );
    let tabs_of = |app: &App, p| -> Vec<String> {
        app.panes
            .get(p)
            .map(|s| {
                s.tabs
                    .iter()
                    .filter_map(|t| t.path().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        tabs_of(&app, day_pane),
        ["daily/day/2027-01-01.md"],
        "replaced in its own pane"
    );
    assert_eq!(tabs_of(&app, other_pane), ["wp4nav/other.md"]);
    assert!(app.closed().contains(&"daily/day/2026-12-31.md".to_owned()));

    let _ = app.send(Message::Nav(NavMsg::Go {
        pane: day_pane,
        target: NavTarget::Period(
            Period::Month,
            NaiveDate::from_ymd_opt(2027, 1, 1).expect("d"),
        ),
    }));
    assert_eq!(
        calls.lock().expect("calls").last(),
        Some(&("month".to_owned(), "2027-01-01".to_owned()))
    );
    assert_eq!(tabs_of(&app, day_pane), ["daily/month/2027-01.md"]);
}

#[test]
fn panes_split_open_close_and_persist() {
    let (tmp, d) = fake_arc();
    for p in ["wp4p/a.md", "wp4p/b.md", "wp4p/c.md"] {
        d.create_note(p, "x").expect("create");
    }
    let state = tmp.path().join("state/crucible-vault/editor.json");
    let mut app = app_over(&d, Some(state.clone()));
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/a.md".into())));
    let first = app.focus;
    let _ = app.send(Message::Panes(PanesMsg::SplitDown));
    let second = app.focus;
    assert_eq!(app.panes.len(), 2);
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/b.md".into())));
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/c.md".into())));
    assert_eq!(
        app.panes.get(second).map(|s| s.tabs.len()),
        Some(2),
        "opens into focus"
    );
    assert_eq!(app.panes.get(first).map(|s| s.tabs.len()), Some(1));
    // Re-opening a tab open in another pane focuses it.
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp4p/a.md".into())));
    assert_eq!(app.focus, first);
    let _ = app.send(Message::Panes(PanesMsg::Focus(second)));

    // Persist and reload the layout.
    let _ = app.send(Message::PersistDue);
    let restored = app_over(&d, Some(state.clone()));
    assert_eq!(restored.panes.len(), 2);
    assert_eq!(
        restored.open_paths(),
        ["wp4p/a.md", "wp4p/b.md", "wp4p/c.md"]
    );
    assert_eq!(
        restored.active_tab().and_then(|t| t.path()),
        Some("wp4p/c.md")
    );
    assert!(matches!(
        restored.ui.panes.as_ref().map(|l| &l.root),
        Some(crate::ui_state::PaneNode::Split {
            dir: crate::ui_state::SplitDir::Down,
            ..
        })
    ));

    // Closing a pane reports each of its tabs.
    app.clear_closed();
    let _ = app.send(Message::Panes(PanesMsg::Close(second)));
    assert_eq!(app.panes.len(), 1);
    assert_eq!(app.closed(), ["wp4p/b.md", "wp4p/c.md"]);
    assert_eq!(app.focus, first);
    let _ = app.send(Message::Tabs(TabsMsg::Close(first, 0)));
    assert_eq!(app.closed().last().map(String::as_str), Some("wp4p/a.md"));
    assert!(app.open_paths().is_empty());
}

#[test]
fn wp1_shaped_editor_json_loads_into_one_pane() {
    let (tmp, d) = fake_arc();
    d.create_note("wp4old/a.md", "x").expect("create");
    d.create_note("wp4old/b.md", "y").expect("create");
    let state = tmp.path().join("state/crucible-vault/editor.json");
    std::fs::create_dir_all(state.parent().expect("dir")).expect("mkdir");
    std::fs::write(
        &state,
        br#"{"tabs":["wp4old/a.md","wp4old/b.md"],"active_tab":0,"layout":"source","surround":"dark","recent":[]}"#,
    )
    .expect("write");
    let app = app_over(&d, Some(state));
    assert_eq!(app.panes.len(), 1);
    assert_eq!(app.open_paths(), ["wp4old/a.md", "wp4old/b.md"]);
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("wp4old/a.md"));
    let _ = panes::order(&app.panes);
}

// ---- editor-usability-round WP-2: Reading-view filename rename + task toggle ----

use crate::message::ReadingMsg;

pub(super) fn title(app: &mut App, path: &str, value: &str) {
    let _ = app.send(Message::Reading(ReadingMsg::TitleInput {
        path: path.into(),
        value: value.into(),
    }));
}

#[test]
fn filename_rename_dispatches_explorer_flow_and_retargets() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp2ren/one.md", "body").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp2ren/one.md".into())));
    assert_eq!(app.reading.title("wp2ren/one.md"), "one");
    title(&mut app, "wp2ren/one.md", "two");
    assert_eq!(app.reading.title("wp2ren/one.md"), "two");
    let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
        path: "wp2ren/one.md".into(),
    }));
    assert!(
        d.read_file("wp2ren/two.md").is_ok(),
        "renamed via the daemon"
    );
    assert!(app.explorer.contains("wp2ren/two.md"));
    assert_eq!(app.open_paths(), ["wp2ren/two.md"], "the tab follows");
    assert_eq!(app.reading.title("wp2ren/two.md"), "two");
    changed(&mut app, "wp2ren/two.md", ChangeKind::Renamed);
    assert!(!app.explorer.stale, "matched via expect_rename");

    // Blur (focus-loss poll) submits; Escape reverts.
    title(&mut app, "wp2ren/two.md", "three");
    let _ = app.send(Message::Reading(ReadingMsg::TitleFocus {
        path: "wp2ren/two.md".into(),
        focused: false,
    }));
    assert_eq!(app.open_paths(), ["wp2ren/three.md"]);
    title(&mut app, "wp2ren/three.md", "four");
    let _ = app.send(Message::Reading(ReadingMsg::TitleRevert));
    assert_eq!(app.reading.title("wp2ren/three.md"), "three");
    let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
        path: "wp2ren/three.md".into(),
    }));
    assert_eq!(app.open_paths(), ["wp2ren/three.md"], "reverted: no rename");
}

#[test]
fn filename_rename_daemon_error_surfaces_and_keeps_tab() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp2err/one.md", "body").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp2err/one.md".into())));
    // The file vanished behind the editor's back: the daemon's rename fails (NotFound).
    std::fs::remove_file(d.vault_root().join("wp2err/one.md")).expect("rm");
    title(&mut app, "wp2err/one.md", "two");
    let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
        path: "wp2err/one.md".into(),
    }));
    assert!(app.status.error_text().is_some(), "daemon error surfaced");
    assert_eq!(app.open_paths(), ["wp2err/one.md"], "tab unchanged");
    assert_eq!(app.reading.title("wp2err/one.md"), "one");
    // A slash is refused before any RPC.
    app.status.notice = None;
    title(&mut app, "wp2err/one.md", "a/b");
    let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
        path: "wp2err/one.md".into(),
    }));
    assert_eq!(app.status.error_text(), Some("invalid name"));
}

#[test]
fn filename_rename_flushes_a_dirty_buffer_first() {
    use iced::widget::text_editor::{Action, Edit};
    let (_tmp, d) = fake_arc();
    d.create_note("wp2dirty/one.md", "body").expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(
        "wp2dirty/one.md".into(),
    )));
    let _ = app.send(Message::Source(crate::message::SourceMsg::Action(
        Action::Edit(Edit::Insert('x')),
    )));
    assert!(app.active_doc().is_some_and(|t| t.is_dirty()));
    title(&mut app, "wp2dirty/one.md", "two");
    let _ = app.send(Message::Reading(ReadingMsg::TitleSubmit {
        path: "wp2dirty/one.md".into(),
    }));
    assert_eq!(app.open_paths(), ["wp2dirty/two.md"]);
    assert_eq!(d.read_file("wp2dirty/two.md").expect("renamed").0, b"xbody");
    assert!(app.active_doc().is_some_and(|t| !t.is_dirty()));
}

#[test]
fn reading_prop_toggle_edits_the_buffer_and_rerenders() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp4prop/t.md", "---\nread: false\n---\nx\n")
        .expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp4prop/t.md".into())));
    let _ = app.send(Message::Reading(ReadingMsg::ToggleProp {
        path: "wp4prop/t.md".into(),
        key: "read".into(),
    }));
    let tab = app.active_doc().expect("tab");
    assert_eq!(tab.text(), "---\nread: true\n---\nx\n");
    assert!(tab.is_dirty(), "saved by the normal autosave path");
    let doc = app.reading.doc("wp4prop/t.md").expect("rendered");
    assert_eq!(doc.seq, tab.edit_seq(), "re-rendered from the buffer");
    assert_eq!(
        doc.model.properties[0].values,
        [editor_model::props::PropValue::Bool(true)]
    );
}

/// Plan round-6-feedback-todos WP-6: a sidebar boolean property toggles through Reading's
/// frontmatter path and the panel re-renders it.
#[test]
fn sidebar_boolean_toggles_through_the_frontmatter_path() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp6prop/t.md", "---\nread: false\n---\nx\n")
        .expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp6prop/t.md".into())));
    app.panels.visible = true;
    let _ = app.send(Message::Panels(crate::message::PanelsMsg::Tick));
    let check = |app: &App| {
        app.panels
            .props
            .iter()
            .find(|p| p.key == "read")
            .and_then(crate::panels::props::checked)
    };
    assert_eq!(check(&app), Some(false));
    let _ = app.send(crate::panels::check_press("wp6prop/t.md", "read"));
    assert_eq!(
        app.active_doc().expect("tab").text(),
        "---\nread: true\n---\nx\n"
    );
    let _ = app.send(Message::Panels(crate::message::PanelsMsg::Tick));
    assert_eq!(check(&app), Some(true));
}

#[test]
fn reading_task_toggle_edits_the_buffer_and_rerenders() {
    let (_tmp, d) = fake_arc();
    d.create_note("wp2task/t.md", "- [ ] a\n- [x] b\n")
        .expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("wp2task/t.md".into())));
    let _ = app.send(Message::Reading(ReadingMsg::ToggleTask {
        path: "wp2task/t.md".into(),
        line: 0,
    }));
    let tab = app.active_doc().expect("tab");
    assert_eq!(tab.text(), "- [x] a\n- [x] b\n");
    assert!(tab.is_dirty(), "saved by the normal autosave path");
    assert_eq!(
        d.read_file("wp2task/t.md").expect("file").0,
        b"- [ ] a\n- [x] b\n"
    );
    let doc = app.reading.doc("wp2task/t.md").expect("rendered");
    assert_eq!(doc.seq, tab.edit_seq(), "re-rendered from the buffer");
    assert_eq!(
        editor_model::render::task_lines(&doc.model),
        vec![(0, 'x'), (1, 'x')]
    );
}

#[test]
fn app_open_file_adds_tab_and_status_updates() {
    let (tmp, d) = fake();
    let p = first_note(&d);
    let expected = String::from_utf8(d.read_file(&p).expect("read").0).expect("utf8");
    let state = tmp.path().join("state/crucible-vault/editor.json");
    let d: Arc<dyn DaemonClient> = Arc::new(d);
    let mut app = App::new(Some(Arc::clone(&d)), Some(state.clone()));
    assert!(app.status.connected);
    assert!(app.explorer.paths.contains(&p));

    let _ = app.send(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.open_paths().len(), 1);
    assert_eq!(app.active_doc().map(|t| t.text()), Some(expected));
    let _ = app.send(Message::Tabs(TabsMsg::Open(p.clone())));
    assert_eq!(app.open_paths().len(), 1, "re-open focuses the tab");
    let _ = app.send(Message::Tabs(TabsMsg::Open("missing.md".into())));
    assert_eq!(app.open_paths().len(), 1);
    assert_eq!(app.status.error_text(), Some("file not found"));

    let base = d.read_file(&p).expect("read").1;
    d.write_note(&p, "changed\n", &base).expect("write");
    let g = d.status().expect("status").generation;
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::IndexGeneration { n: g },
    )));
    assert_eq!(app.status.generation, g);
    assert!(app.status.line().contains(&format!("gen {g}")));
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: p.clone(),
            kind: ChangeKind::Modified,
            hash: None,
            origin: Origin::External,
        },
    )));
    assert_eq!(
        app.active_doc().map(|t| t.text()).as_deref(),
        Some("changed\n")
    );
    let _ = app.send(Message::Status(StatusMsg::Disconnected));
    assert!(app.status.line().starts_with("disconnected · gen"));

    // Tabs persist (the debounced write) and restore from editor.json.
    let _ = app.send(Message::PersistDue);
    let restored = App::new(Some(d), Some(state));
    assert_eq!(restored.open_paths().len(), 1);
    assert_eq!(restored.ui.recent.first(), Some(&p));
}
