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
    app.rpc_inline = true;
    let _ = app.update(Message::Tabs(TabsMsg::Open(NOTE.to_owned())));
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
    let _ = app.update(Message::Views(ViewsMsg::Need {
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
    let _ = app.update(Message::Reading(ReadingMsg::SetMode {
        path: NOTE.into(),
        mode: ViewMode::Reading,
    }));
    let blocks = &app.reading.doc(NOTE).expect("doc").model.blocks;
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
    assert_eq!(dec.columns, ["file name", "mtime"]);
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
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::ViewsInvalidated {
            view_ids: vec!["hubs#Hub View B".into()],
        },
    )));
    need(&mut app);
    assert_eq!(d.view_calls().expect("calls").len(), 1);
    let _ = app.update(Message::Status(StatusMsg::Notification(
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
    let _ = app.update(Message::Views(ViewsMsg::Open {
        path: "Linker.md".into(),
        line: Some(3),
    }));
    let tab = app.active_tab().expect("tab");
    assert_eq!(tab.path, "Linker.md");
    assert_eq!(app.active_doc().expect("doc").cursor().position.line, 2);
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

#[test]
fn decoder_round_trips_every_cell_kind() {
    let cases = [
        (json!({"t": "null"}), Cell::Null, ""),
        (json!({"t": "bool", "v": true}), Cell::Bool(true), "✓"),
        (
            json!({"t": "number", "v": 13.600000000000001}),
            Cell::Number(13.600000000000001),
            "13.6",
        ),
        (json!({"t": "number", "v": 2.0}), Cell::Number(2.0), "2"),
        (json!({"t": "text", "v": "·"}), Cell::Text("·".into()), "·"),
        (
            json!({"t": "date", "v": "2026-10-12"}),
            Cell::Date("2026-10-12".into()),
            "2026-10-12",
        ),
        (
            json!({"t": "date", "v": "2026-10-12T08:30:00"}),
            Cell::Date("2026-10-12T08:30:00".into()),
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
            Cell::List(vec![Cell::Text("x".into()), Cell::Number(1.0)]),
            "x, 1",
        ),
        (
            json!({"t": "hologram", "v": 1}),
            Cell::Text("?".into()),
            "?",
        ),
        (json!({"v": 1}), Cell::Text("?".into()), "?"),
    ];
    for (v, want, shown) in cases {
        let c = decode_cell(&v);
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
    assert_eq!(d.lines[0], Line::Group(Cell::Text("General".into())));
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
    let _ = app.update(Message::Tabs(TabsMsg::Close(app.focus, 0)));
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
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: "Other.md".into(),
            kind: cv_protocol::ChangeKind::Renamed,
            hash: None,
            origin: cv_protocol::Origin::External,
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
    let _ = app.update(Message::Explorer(crate::message::ExplorerMsg::Done(
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
    let _ = app.update(Message::Views(ViewsMsg::Need {
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
