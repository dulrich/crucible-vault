//! WP-7 view-fence tests against `FakeDaemon` with a scripted view registry (synthetic vault in a
//! temp dir, I1; RPC runs inline).

use std::sync::Arc;

use cv_protocol::{Notification, ViewResult};
use editor_model::render::Block;
use serde_json::json;

use super::*;
use crate::app::App;
use crate::daemon::{DaemonClient, FakeDaemon};
use crate::message::{Message, ReadingMsg, StatusMsg, TabsMsg};
use crate::reading::ViewMode;

const NOTE: &str = "Host.md";
const ID: &str = "backlinks#Here";

fn setup() -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).expect("mkdir");
    std::fs::write(
        vault.join(NOTE),
        "# Host\n\n```crucible-view\nbacklinks#Here\n```\n",
    )
    .expect("write");
    std::fs::write(vault.join("Linker.md"), "one\ntwo\nthree [[Host]]\n").expect("write");
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Tabs(TabsMsg::Open(NOTE.to_owned())));
    (tmp, d, app)
}

fn result() -> serde_json::Value {
    json!({
        "kind": "table",
        "columns": [{"key": "file.name", "label": "file name"}, {"key": "file.mtime", "label": "mtime"}],
        "groups": [{"rows": [{"path": "Linker.md", "line": 3, "cells": [
            {"t": "link", "path": "Linker.md", "label": "Linker"},
            {"t": "date", "v": "2026-10-12T08:30:00"}
        ]}], "summaries": []}],
        "summaries": [],
        "notices": []
    })
}

fn need(app: &mut App) {
    let _ = app.send(Message::Views(ViewsMsg::Need {
        id: ID.into(),
        host: Some(NOTE.into()),
    }));
}

fn state(app: &App) -> Option<&State> {
    app.views.get(ID, Some(NOTE)).map(|e| &e.state)
}

#[test]
fn fence_needs_runs_with_this_and_becomes_ready() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(result())).expect("script");
    // The note renders a view fence whose id the fence widget will ask for.
    let _ = app.send(Message::Reading(ReadingMsg::SetMode {
        path: NOTE.into(),
        mode: ViewMode::Reading,
    }));
    let blocks = &app.reading.doc(&app.docs, NOTE).expect("doc").model.blocks;
    let fence = blocks.iter().find_map(|b| match b {
        Block::ViewFence { file, name } => Some(view_id(file, name)),
        _ => None,
    });
    assert_eq!(fence.as_deref(), Some(ID));
    need(&mut app);
    assert_eq!(
        d.view_calls().expect("calls"),
        [(ID.to_owned(), Some(NOTE.to_owned()))]
    );
    let Some(State::Ready(dec)) = state(&app) else {
        panic!("not ready: {:?}", state(&app));
    };
    let labels: Vec<&str> = dec.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["file name", "mtime"]);
    assert_eq!(dec.lines.len(), 1);
    // A second sighting is served from the cache.
    need(&mut app);
    assert_eq!(d.view_calls().expect("calls").len(), 1);
}

#[test]
fn views_invalidated_re_requests_a_shown_view() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(result())).expect("script");
    need(&mut app);
    let epoch = app.views.get(ID, Some(NOTE)).expect("entry").epoch;
    // Other ids leave it alone.
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::ViewsInvalidated {
            view_ids: vec!["lists#Hub View B".into()],
        },
    )));
    need(&mut app);
    assert_eq!(d.view_calls().expect("calls").len(), 1);
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::ViewsInvalidated {
            view_ids: vec![ID.into()],
        },
    )));
    let e = app.views.get(ID, Some(NOTE)).expect("entry");
    assert!(e.stale);
    // The epoch is part of the fence sensor key, so a shown fence fires `Need` again.
    assert_eq!(e.epoch, epoch + 1);
    need(&mut app);
    assert_eq!(d.view_calls().expect("calls").len(), 2);
    // An empty list invalidates everything.
    app.views.invalidate(&[]);
    assert!(app.views.get(ID, Some(NOTE)).expect("entry").stale);
}

#[test]
fn row_click_opens_the_path_at_its_line() {
    let (_tmp, _d, mut app) = setup();
    let _ = app.send(Message::Views(ViewsMsg::Open {
        path: "Linker.md".into(),
        line: Some(3),
    }));
    let tab = app.active_tab().expect("tab");
    assert_eq!(tab.path(), Some("Linker.md"));
    // WP-R7 (Decision 4): mode-aware through `jump_to_line` — Reading tops the line's block.
    let p = "Linker.md";
    assert_eq!(app.reading.mode(&app.docs, p), ViewMode::Reading);
    let want = app
        .reading
        .doc(&app.docs, p)
        .and_then(|d| d.model.block_for_line(2));
    assert!(want.is_some());
    assert_eq!(app.reading.top_block(&app.docs, p, app.view_of(p)), want);
}

#[test]
fn view_run_error_shows_an_error_entry() {
    let (_tmp, _d, mut app) = setup();
    need(&mut app);
    assert!(matches!(state(&app), Some(State::Error(e)) if e.contains(ID)));
}

#[test]
fn malformed_result_is_an_error_not_a_panic() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(json!({"kind": "table"})))
        .expect("script");
    need(&mut app);
    assert!(matches!(state(&app), Some(State::Error(_))));
    assert!(decode(&json!([1, 2])).is_err());
    assert!(decode(&json!({"kind": "graph", "columns": [], "groups": []})).is_err());
}

/// Plan round-8 WP-2: the fence's list height follows the view's `maxHeight`.
#[test]
fn fence_height_follows_max_height() {
    let with = |m: Option<serde_json::Value>| {
        let mut v = result();
        if let Some(m) = m {
            v["max_height"] = m;
        }
        list_height(&decode(&v).expect("decodes"))
    };
    assert_eq!(with(None), Some(MAX_HEIGHT));
    assert_eq!(with(Some(json!(600.0))), Some(600.0));
    assert_eq!(with(Some(json!("none"))), None);
    assert_eq!(with(Some(json!("bogus"))), Some(MAX_HEIGHT));
}

#[test]
fn decoder_round_trips_every_cell_kind() {
    let cases = [
        (json!({"t": "null"}), Cell::Null, ""),
        (json!({"t": "bool", "v": true}), Cell::Bool { v: true }, "✓"),
        (
            json!({"t": "number", "v": 13.600000000000001}),
            Cell::Number {
                v: 13.600000000000001,
            },
            "13.6",
        ),
        (
            json!({"t": "number", "v": 2.0}),
            Cell::Number { v: 2.0 },
            "2",
        ),
        (
            json!({"t": "text", "v": "·"}),
            Cell::Text { v: "·".into() },
            "·",
        ),
        (
            json!({"t": "date", "v": "2026-10-12"}),
            Cell::Date {
                v: "2026-10-12".into(),
            },
            "2026-10-12",
        ),
        (
            json!({"t": "date", "v": "2026-10-12T08:30:00"}),
            Cell::Date {
                v: "2026-10-12T08:30:00".into(),
            },
            "2026-10-12 08:30",
        ),
        (
            json!({"t": "link", "path": "a/B.md", "label": "B"}),
            Cell::Link {
                path: "a/B.md".into(),
                label: "B".into(),
            },
            "B",
        ),
        (
            json!({"t": "list", "v": [{"t": "text", "v": "x"}, {"t": "number", "v": 1}]}),
            Cell::List {
                v: vec![Cell::Text { v: "x".into() }, Cell::Number { v: 1.0 }],
            },
            "x, 1",
        ),
        (json!({"t": "hologram", "v": 1}), Cell::Unknown, ""),
        (
            json!({"t": "url", "url": "https://e.com"}),
            Cell::Url {
                url: "https://e.com".into(),
                label: String::new(),
            },
            "https://e.com",
        ),
    ];
    for (v, want, shown) in cases {
        let c: Cell = serde_json::from_value(v.clone()).expect("decodes");
        assert_eq!(c, want, "{v}");
        assert_eq!(format_cell(&c), shown, "{v}");
    }
    assert_eq!(struck("~~FEEDBACK~~"), Some("FEEDBACK"));
    assert_eq!(struck("~~~~"), None);
    assert_eq!(struck("FEEDBACK"), None);
}

#[test]
fn decode_flattens_groups_and_summaries() {
    let v = json!({
        "kind": "pivot",
        "columns": [{"key": "rows", "label": "Day"}, {"key": "Blog", "label": "Blog"}],
        "groups": [{"rows": [{"cells": [{"t": "text", "v": "2026-10-01"}, {"t": "text", "v": "400 / 2.0m"}]}], "summaries": []}],
        "summaries": [{"label": "Total", "cells": [{"t": "text", "v": "Total"}, {"t": "text", "v": "400 / 2.0m"}]}],
        "notices": ["n1"]
    });
    let d = decode(&v).expect("decode");
    assert_eq!(d.kind, Kind::Pivot);
    assert_eq!(d.notices, ["n1"]);
    assert!(matches!(
        d.lines.as_slice(),
        [Line::Row(_), Line::Summary { .. }]
    ));
    let v = json!({
        "kind": "tasks",
        "columns": [{"key": "task", "label": "Task"}, {"key": "file", "label": "File"}],
        "groups": [
            {"key": {"t": "text", "v": "General"}, "rows": [{"path": "d.md", "line": 4, "cells": []}], "summaries": []},
            {"key": {"t": "text", "v": "Work"}, "rows": [], "summaries": []}
        ],
        "summaries": [], "notices": []
    });
    let d = decode(&v).expect("decode");
    assert_eq!(d.lines.len(), 3);
    assert_eq!(
        d.lines[0],
        Line::Group(Cell::Text {
            v: "General".into()
        })
    );
}

#[test]
fn view_id_from_fence_body() {
    assert_eq!(view_id("backlinks", "Here"), "backlinks#Here");
    assert_eq!(
        view_id("_custom/crucible-vault/views/monthly-tasks.toml", "Overdue"),
        "monthly-tasks#Overdue"
    );
    assert_eq!(view_id("works.base", "Unread"), "works#Unread");
}

#[test]
fn closing_the_host_tab_drops_its_results() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(result())).expect("script");
    need(&mut app);
    assert!(app.views.get(ID, Some(NOTE)).is_some());
    let _ = app.send(Message::Tabs(TabsMsg::Close(app.focus, 0)));
    assert!(app.views.get(ID, Some(NOTE)).is_none());
}

/// editor-usability-round WP-5 (view live-update invariant): refreshing an entry that already
/// has rows keeps them on screen (never `Loading…`/empty) and keeps its scroll window until the
/// new result lands.
#[test]
fn refresh_of_a_ready_entry_keeps_rows_and_window() {
    let mut store = ViewStore::default();
    let key = (ID.to_owned(), Some(NOTE.to_owned()));
    let need = || ViewsMsg::Need {
        id: ID.into(),
        host: Some(NOTE.into()),
    };
    assert!(matches!(update(&mut store, need()), Effect::Run { .. }));
    let _ = update(
        &mut store,
        ViewsMsg::Loaded {
            id: ID.into(),
            host: Some(NOTE.into()),
            result: Ok(result()),
        },
    );
    let window = crate::widget::virtual_list::ListWindow {
        offset: 120.0,
        height: 300.0,
    };
    let _ = update(
        &mut store,
        ViewsMsg::Scrolled {
            id: ID.into(),
            host: Some(NOTE.into()),
            window,
        },
    );
    store.invalidate(&[ID.into()]);
    assert!(matches!(update(&mut store, need()), Effect::Run { .. }));
    let e = store.entries.get(&key).expect("entry");
    assert!(
        matches!(&e.state, State::Ready(d) if d.lines.len() == 1),
        "rows stay while the refresh is in flight: {:?}",
        e.state
    );
    assert_eq!(e.window, window);
    let mut next = result();
    let row = next["groups"][0]["rows"][0].clone();
    next["groups"][0]["rows"]
        .as_array_mut()
        .expect("rows")
        .push(row);
    let _ = update(
        &mut store,
        ViewsMsg::Loaded {
            id: ID.into(),
            host: Some(NOTE.into()),
            result: Ok(next),
        },
    );
    let e = store.entries.get(&key).expect("entry");
    assert!(matches!(&e.state, State::Ready(d) if d.lines.len() == 2));
    assert_eq!(e.window, window, "scroll window kept across the refresh");
}

// ---- editor-feedback-round-2 WP-8: views follow a rename without a flash ----

fn ready(app: &App, host: &str) -> bool {
    matches!(
        app.views.get(ID, Some(host)).map(|e| &e.state),
        Some(State::Ready(_))
    )
}

#[test]
fn file_changed_renamed_marks_every_view_stale() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(result())).expect("script");
    need(&mut app);
    let epoch = app.views.get(ID, Some(NOTE)).expect("entry").epoch;
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            id: None,
            path: "Other.md".into(),
            kind: cv_protocol::ChangeKind::Renamed,
            hash: None,
            origin: cv_protocol::Origin::External,
            from: None,
            replaced_by: None,
            seq: None,
        },
    )));
    let e = app.views.get(ID, Some(NOTE)).expect("entry");
    assert!(e.stale && e.epoch == epoch + 1);
    assert!(ready(&app, NOTE), "old result stays shown");
    need(&mut app);
    assert_eq!(d.view_calls().expect("calls").len(), 2);
}

#[test]
fn retarget_rehosts_view_entries_under_the_new_path() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(result())).expect("script");
    need(&mut app);
    d.rename(NOTE, "Moved.md").expect("rename");
    let _ = app.send(Message::Explorer(crate::message::ExplorerMsg::Done(
        crate::explorer::FileOpDone {
            op: crate::explorer::FileOp::Rename {
                from: NOTE.into(),
                to: "Moved.md".into(),
            },
            result: Ok("Moved.md".into()),
        },
    )));
    assert!(app.views.get(ID, Some(NOTE)).is_none());
    let e = app.views.get(ID, Some("Moved.md")).expect("rehosted");
    assert!(e.stale && !e.in_flight);
    assert!(ready(&app, "Moved.md"), "no flash: the old rows stay up");
    let _ = app.send(Message::Views(ViewsMsg::Need {
        id: ID.into(),
        host: Some("Moved.md".into()),
    }));
    assert_eq!(
        d.view_calls().expect("calls").last(),
        Some(&(ID.to_owned(), Some("Moved.md".to_owned())))
    );
}

#[test]
fn invalidation_during_a_run_re_requests_after_it_lands() {
    let mut store = ViewStore::default();
    let need = || ViewsMsg::Need {
        id: ID.into(),
        host: Some(NOTE.into()),
    };
    assert!(matches!(update(&mut store, need()), Effect::Run { .. }));
    store.invalidate(&[]);
    let epoch = store.get(ID, Some(NOTE)).expect("entry").epoch;
    // The fence's re-sighting while in flight is refused …
    assert_eq!(update(&mut store, need()), Effect::None);
    let _ = update(
        &mut store,
        ViewsMsg::Loaded {
            id: ID.into(),
            host: Some(NOTE.into()),
            result: Ok(result()),
        },
    );
    // … so landing bumps the epoch (new sensor key) and the next sighting re-runs.
    assert_eq!(store.get(ID, Some(NOTE)).expect("entry").epoch, epoch + 1);
    assert!(matches!(update(&mut store, need()), Effect::Run { .. }));
}

/// Plan round-8-intake-views-anchor WP-3: a `checkbox` column of `note.read`.
fn checkbox_result() -> serde_json::Value {
    json!({
        "kind": "table",
        "columns": [
            {"key": "file.name", "label": "name"},
            {"key": "read", "label": "read", "role": {"kind": "checkbox", "key": "read"}}
        ],
        "groups": [{"rows": [
            {"path": NOTE, "cells": [{"t": "text", "v": "Host"}, {"t": "null"}]},
            {"path": "Linker.md", "cells": [{"t": "text", "v": "Linker"}, {"t": "bool", "v": true}]},
            {"cells": [{"t": "text", "v": "pathless"}, {"t": "bool", "v": false}]}
        ], "summaries": []}],
        "summaries": [],
        "notices": []
    })
}

#[path = "tests_layout.rs"]
mod layout;

fn sort_by(app: &mut App, column: &str) {
    let _ = app.send(Message::Views(ViewsMsg::SortBy {
        id: ID.into(),
        host: Some(NOTE.into()),
        column: column.into(),
    }));
}

fn active(app: &App) -> Option<(String, bool)> {
    app.views
        .sort_of(ID, Some(NOTE))
        .map(|s| (s.column.clone(), s.desc))
}

/// Plan round-11 WP-5: a header click cycles ASC → DESC → cleared and re-runs with the sort;
/// another header replaces the active one at ASC.
#[test]
fn header_click_cycles_and_switching_columns_restarts_at_asc() {
    use cv_protocol::{
        ViewSort,
        ViewSortDir::{Asc, Desc},
    };
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(result())).expect("script");
    need(&mut app);
    let sort = |p: &str, dir| {
        Some(ViewSort {
            property: p.into(),
            direction: dir,
        })
    };
    sort_by(&mut app, "file.name");
    assert_eq!(active(&app), Some(("file.name".into(), false)));
    sort_by(&mut app, "file.name");
    assert_eq!(active(&app), Some(("file.name".into(), true)));
    sort_by(&mut app, "file.mtime");
    assert_eq!(active(&app), Some(("file.mtime".into(), false)));
    sort_by(&mut app, "file.mtime");
    sort_by(&mut app, "file.mtime");
    assert_eq!(active(&app), None);
    // The fake saw: the initial unsorted run, then each click's sort.
    assert_eq!(
        d.view_sorts().expect("sorts"),
        [
            None,
            sort("file.name", Asc),
            sort("file.name", Desc),
            sort("file.mtime", Asc),
            sort("file.mtime", Desc),
            None,
        ]
    );
}

const ACTIVE: &str = "works#Active";
const ENTRIES: &str = "works#Entries";

fn sources_result() -> serde_json::Value {
    json!({
        "kind": "table",
        "columns": [{"key": "note.name", "label": "Name"}, {"key": "note.kind", "label": "Kind"}],
        "groups": [{"rows": [
            {"cells": [{"t": "text", "v": "Feed"}, {"t": "text", "v": "blog"}]},
            {"cells": [{"t": "text", "v": "Saga"}, {"t": "text", "v": "fanfic"}], "select": "Saga"}
        ], "summaries": []}],
        "summaries": [],
        "notices": []
    })
}

fn entries_result() -> serde_json::Value {
    let mut v = result();
    v["select_by"] = json!("file.serial");
    v
}

fn load(app: &mut App, id: &str) {
    let _ = app.send(Message::Views(ViewsMsg::Need {
        id: id.into(),
        host: Some(NOTE.into()),
    }));
}

fn source_rows(app: &App) -> Vec<Row> {
    let Some(State::Ready(d)) = app.views.get(ACTIVE, Some(NOTE)).map(|e| &e.state) else {
        panic!("active not loaded");
    };
    d.lines
        .iter()
        .filter_map(|l| match l {
            Line::Row(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

/// WP-R3 (review E12b): the select cell finds the Name column by its role, wherever it sits
/// (not by key), and falls back to the first column when no column carries the role.
#[test]
fn select_cell_finds_name_by_role() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ACTIVE, ViewResult(sources_result()))
        .expect("script");
    d.set_view_result(ENTRIES, ViewResult(entries_result()))
        .expect("script");
    load(&mut app, ACTIVE);
    load(&mut app, ENTRIES);
    let rows = source_rows(&app);
    let cols = [
        Column::plain("note.kind", "Kind"),
        Column::plain("renamed", "Title").with_role(ColumnRole::Name),
    ];
    let (i, _) = select_cell(&rows[1], &cols, NOTE).expect("serial row selects");
    assert_eq!(i, 1, "found by role, not first and not by key");
    let plain = [Column::plain("a", "A"), Column::plain("note.name", "Name")];
    let (i, _) = select_cell(&rows[1], &plain, NOTE).expect("serial row selects");
    assert_eq!(i, 0, "no role: the first column");
}

/// Plan round-13 WP-4: a serial row's Name click selects (the `select_by` section re-runs with
/// `select`, its header shows the name); clicking it again clears; a non-serial row is inert.
#[test]
fn serial_row_click_selects_and_reclick_clears() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ACTIVE, ViewResult(sources_result()))
        .expect("script");
    d.set_view_result(ENTRIES, ViewResult(entries_result()))
        .expect("script");
    load(&mut app, ACTIVE);
    load(&mut app, ENTRIES);
    assert!(app.views.host_selects(NOTE));
    let rows = source_rows(&app);
    let cols = [
        Column::plain("note.name", "Name").with_role(ColumnRole::Name),
        Column::plain("note.kind", "Kind"),
    ];
    assert!(
        select_cell(&rows[0], &cols, NOTE).is_none(),
        "non-serial row inert"
    );
    let (i, click) = select_cell(&rows[1], &cols, NOTE).expect("serial row selects");
    assert_eq!(i, 0);
    assert_eq!(selection_chip(&app.views, ENTRIES, NOTE), None);
    let _ = app.send(click.clone());
    let saga = Some(cv_protocol::ViewSelect {
        property: "file.serial".into(),
        value: "Saga".into(),
    });
    assert_eq!(d.view_selects().expect("selects").last(), Some(&saga));
    assert_eq!(
        d.view_calls().expect("calls").last().map(|c| c.0.as_str()),
        Some(ENTRIES)
    );
    assert_eq!(
        selection_chip(&app.views, ENTRIES, NOTE).as_deref(),
        Some("Saga")
    );
    // The sources section never carries the selection.
    assert_eq!(selection_chip(&app.views, ACTIVE, NOTE), None);
    let _ = app.send(click);
    assert_eq!(d.view_selects().expect("selects").last(), Some(&None));
    assert_eq!(selection_chip(&app.views, ENTRIES, NOTE), None);
    assert!(app.views.selections.is_empty());
}

/// Plan round-13 WP-4: without a `select_by` section on the host, serial rows are not clickable;
/// the selection is dropped with the host's tab.
#[test]
fn selection_needs_a_select_by_host_and_drops_with_the_tab() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ACTIVE, ViewResult(sources_result()))
        .expect("script");
    load(&mut app, ACTIVE);
    assert!(!app.views.host_selects(NOTE));
    app.views.selections.insert(NOTE.into(), "Saga".into());
    app.views.on_tab_closed(NOTE);
    assert!(app.views.selections.is_empty());
}

#[path = "tests_rows.rs"]
mod rows;
