use super::*;

/// plan editor-round-4 WP-3: every link cell opens its own target; a row with a path and no
/// link cell opens the row path from its first cell.
#[test]
fn row_link_cells_open_their_own_targets() {
    let open = |m: &Option<Message>| match m {
        Some(Message::Views(ViewsMsg::Open { path, line })) => Some((path.clone(), *line)),
        _ => None,
    };
    let r = Row {
        path: Some("INITIATIVE.md".into()),
        line: Some(3),
        key: None,
        select: None,
        cells: vec![
            Cell::Link {
                path: "a.md".into(),
                label: "A".into(),
            },
            Cell::Text { v: "x".into() },
            Cell::Link {
                path: "b.md".into(),
                label: "B".into(),
            },
        ],
    };
    let got: Vec<_> = row_targets(&r, &[]).iter().map(open).collect();
    assert_eq!(
        got,
        vec![
            Some(("a.md".into(), None)),
            None,
            Some(("b.md".into(), None))
        ]
    );
    let plain = Row {
        path: Some("n.md".into()),
        line: Some(7),
        key: None,
        select: None,
        cells: vec![Cell::Text { v: "t".into() }, Cell::Number { v: 1.0 }],
    };
    let got: Vec<_> = row_targets(&plain, &[]).iter().map(open).collect();
    assert_eq!(got, vec![Some(("n.md".into(), Some(7))), None]);
    let none = Row {
        path: None,
        line: None,
        key: None,
        select: None,
        cells: vec![Cell::Text { v: "t".into() }],
    };
    assert!(row_targets(&none, &[]).iter().all(Option::is_none));
}

/// Round-5 WP-11: a `daily-intake`-shaped result (keyed tracker rows, `url` + `link` titles, a
/// plain bool column).
fn intake_result() -> serde_json::Value {
    let key = views::result::TrackerKey {
        source_kind: "youtube".into(),
        source_key: "UC1".into(),
        item_id: "v1".into(),
    }
    .encode();
    json!({
        "kind": "table",
        "columns": [{"key": "formula.item", "label": "Title"}, {"key": "done", "label": "Done"}],
        "groups": [{"rows": [
            {"key": key, "cells": [
                {"t": "url", "url": "https://y.example/1", "label": "A video"},
                {"t": "bool", "v": false}
            ]},
            {"cells": [
                {"t": "link", "path": "notes/P.md", "label": "P"},
                {"t": "bool", "v": true}
            ]}
        ], "summaries": []}],
        "summaries": [],
        "notices": []
    })
}

fn intake_rows(app: &App) -> Vec<(Row, Vec<Column>)> {
    let Some(State::Ready(dec)) = state(app) else {
        panic!("not ready: {:?}", state(app));
    };
    dec.lines
        .iter()
        .filter_map(|l| match l {
            Line::Row(r) => Some((r.clone(), dec.columns.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn url_cells_open_in_the_browser_and_link_cells_open_the_note() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(intake_result()))
        .expect("script");
    need(&mut app);
    let rows = intake_rows(&app);
    let (keyed, keys) = &rows[0];
    let t = row_targets(keyed, keys);
    assert!(matches!(&t[0], Some(Message::OpenUrl(u)) if u == "https://y.example/1"));
    // No key (an index row): the bool is plain, the link opens the note.
    let (plain, keys) = &rows[1];
    let t = row_targets(plain, keys);
    assert!(
        matches!(&t[0], Some(Message::Views(ViewsMsg::Open { path, .. })) if path == "notes/P.md")
    );
    assert!(t[1].is_none());
}

/// Plan round-6 WP-5: a right-click on a view link cell yields the vault link menu (its path
/// resolves to itself: Copy path copies it); a URL cell yields the external menu; text cells none.
#[test]
fn view_cell_right_click_yields_the_link_menu() {
    use crate::context_menu::{Item, MenuMsg, Target, items};
    let link = cell_menu_target(&Cell::Link {
        path: "Linker.md".into(),
        label: "Linker".into(),
    })
    .expect("link target");
    assert!(
        matches!(&link, Target::Link { target, external: false, missing: false, .. } if target == "Linker.md")
    );
    let names: Vec<String> = items(&link).into_iter().map(|(_, l)| l).collect();
    assert_eq!(names, ["Open", "Open in new pane", "Copy path"]);
    let url = cell_menu_target(&Cell::Url {
        url: "https://e.com".into(),
        label: "e".into(),
    })
    .expect("url target");
    let names: Vec<String> = items(&url).into_iter().map(|(_, l)| l).collect();
    assert_eq!(names, ["Open", "Copy URL"]);
    assert!(cell_menu_target(&Cell::Text { v: "x".into() }).is_none());

    let (_tmp, _d, mut app) = setup();
    let _ = app.send(Message::Menu(MenuMsg::Open(link, iced::Point::ORIGIN)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyPath)));
    assert_eq!(app.copied(), ["Linker.md"]);
}

/// Plan round-6-feedback-todos WP-6: a `status` cell of a keyed row — left click toggles `x` ↔
/// open through `tracker_item_set_status`; right-click opens the status menu, whose pick sets that
/// status (`[ ]` = open).
#[test]
fn status_cell_click_toggles_and_right_click_menu_sets_the_status() {
    use crate::context_menu::{Item, MenuMsg, Target, items};
    let key = |id: &str| {
        views::result::TrackerKey {
            source_kind: "youtube".into(),
            source_key: "UC1".into(),
            item_id: id.into(),
        }
        .encode()
    };
    let (_tmp, d, mut app) = setup();
    d.set_view_result(
        ID,
        ViewResult(json!({
            "kind": "table",
            "columns": [{"key": "title", "label": "Title"}, {"key": "status", "label": "Done", "role": {"kind": "status"}}],
            "groups": [{"rows": [
                {"key": key("v1"), "cells": [{"t": "text", "v": "A"}, {"t": "text", "v": "x"}]},
                {"key": key("v2"), "cells": [{"t": "text", "v": "B"}, {"t": "null"}]}
            ], "summaries": []}],
            "summaries": [],
            "notices": []
        })),
    )
    .expect("script");
    need(&mut app);
    let rows = intake_rows(&app);
    let params = |id: &str, status: Option<char>| cv_protocol::TrackerItemSetStatusParams {
        source_kind: "youtube".into(),
        source_key: "UC1".into(),
        item_id: id.into(),
        status,
    };
    let (done, keys) = &rows[0];
    assert_eq!(status_cell(done, keys, 1), Some(Some('x')));
    assert_eq!(status_cell(done, keys, 0), None);
    for (r, keys) in &rows {
        let Some(click) = row_targets(r, keys).remove(1) else {
            panic!("no status target");
        };
        let _ = app.send(click);
    }
    assert_eq!(
        d.status_calls().expect("calls"),
        [params("v1", None), params("v2", Some('x'))]
    );
    let target = status_menu_target(&rows[1].0).expect("menu target");
    assert!(matches!(&target, Target::TrackerStatus { .. }));
    assert_eq!(
        items(&target).len(),
        crate::task_status::TASK_STATUSES.len()
    );
    let _ = app.send(Message::Menu(MenuMsg::Open(
        target.clone(),
        iced::Point::ORIGIN,
    )));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::TaskStatus('/'))));
    let _ = app.send(Message::Menu(MenuMsg::Open(target, iced::Point::ORIGIN)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::TaskStatus(' '))));
    let calls = d.status_calls().expect("calls");
    assert_eq!(calls[2..], [params("v2", Some('/')), params("v2", None)]);
    assert!(app.status.error_text().is_none(), "success is silent");
}

/// The Tasks view's columns as the engine produces them.
fn tasks_columns() -> Vec<Column> {
    vec![
        Column::plain("task", "Task").with_role(ColumnRole::TaskText),
        Column::plain("file", "File").with_role(ColumnRole::TaskFile),
        Column::plain("status", "Status").with_role(ColumnRole::Status),
    ]
}

/// Plan round-6-feedback-todos WP-6: a `Tasks` row carries its real status char (cell 2) and,
/// with path + line, the Reading note-edit targets: the click toggles the task in the open
/// buffer, the menu target is `Target::Task` at the 0-based line.
#[test]
fn tasks_row_shows_its_status_and_toggles_through_the_note_edit_path() {
    let (_tmp, d, mut app) = setup();
    d.create_note("wp6t/t.md", "- [ ] a\n- [/] b\n")
        .expect("create");
    let r = Row {
        path: Some("wp6t/t.md".into()),
        line: Some(2),
        key: None,
        select: None,
        cells: vec![
            Cell::Text { v: "b".into() },
            Cell::Link {
                path: "wp6t/t.md".into(),
                label: "t".into(),
            },
            Cell::Text { v: "/".into() },
        ],
    };
    let (c, targets) = task_row_status(&r, &tasks_columns());
    assert_eq!(c, '/');
    let Some((click, menu)) = targets else {
        panic!("no targets");
    };
    assert_eq!(
        menu,
        crate::context_menu::Target::Task {
            path: "wp6t/t.md".into(),
            line: 1,
        }
    );
    // Not open: a notice, not a silent no-op.
    let _ = app.send(click.clone());
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("wp6t/t.md"))
    );
    let _ = app.send(Message::Tabs(TabsMsg::Open("wp6t/t.md".into())));
    let _ = app.send(click);
    assert_eq!(app.active_doc().expect("doc").text(), "- [ ] a\n- [x] b\n");
    // No path / line: display-only, open status for a missing cell.
    let bare = Row {
        path: None,
        line: None,
        key: None,
        select: None,
        cells: vec![Cell::Text { v: "t".into() }],
    };
    assert!(matches!(
        task_row_status(&bare, &tasks_columns()),
        (' ', None)
    ));
}

/// Plan round-8-intake-views-anchor WP-1: a keyed row whose status cell is `-` is cancelled (its
/// text cells draw struck + faint); other statuses, unkeyed rows and non-status columns are not.
#[test]
fn cancelled_status_row_draws_struck() {
    let key = views::result::TrackerKey {
        source_kind: "youtube".into(),
        source_key: "UC1".into(),
        item_id: "v1".into(),
    }
    .encode();
    let (_tmp, d, mut app) = setup();
    d.set_view_result(
        ID,
        ViewResult(json!({
            "kind": "table",
            "columns": [{"key": "title", "label": "Title"}, {"key": "status", "label": "Done", "role": {"kind": "status"}}],
            "groups": [{"rows": [
                {"key": key, "cells": [{"t": "text", "v": "A"}, {"t": "text", "v": "-"}]},
                {"key": key, "cells": [{"t": "text", "v": "B"}, {"t": "text", "v": "x"}]},
                {"key": key, "cells": [{"t": "text", "v": "C"}, {"t": "null"}]},
                {"cells": [{"t": "text", "v": "D"}, {"t": "text", "v": "-"}]}
            ], "summaries": []}],
            "summaries": [],
            "notices": []
        })),
    )
    .expect("script");
    need(&mut app);
    let rows = intake_rows(&app);
    let cancelled: Vec<bool> = rows.iter().map(|(r, k)| row_cancelled(r, k)).collect();
    assert_eq!(cancelled, [true, false, false, false]);
    let (r, k) = &rows[0];
    let titles_only = vec![Column::plain("title", ""), Column::plain("other", "")];
    assert!(!row_cancelled(r, &titles_only));
    assert_eq!(status_cell(r, k, 1), Some(Some('-')));
}

#[test]
fn checkbox_cells_toggle_the_buffer_or_call_note_set_bool() {
    let (_tmp, d, mut app) = setup();
    d.set_view_result(ID, ViewResult(checkbox_result()))
        .expect("script");
    need(&mut app);
    let Some(State::Ready(dec)) = state(&app).cloned() else {
        panic!("not ready");
    };
    let flags: Vec<bool> = dec
        .columns
        .iter()
        .map(|c| matches!(c.role, Some(ColumnRole::Checkbox { .. })))
        .collect();
    assert_eq!(flags, [false, true]);
    let rows: Vec<Row> = dec
        .lines
        .iter()
        .filter_map(|l| match l {
            Line::Row(r) => Some(r.clone()),
            _ => None,
        })
        .collect();
    let cb = |r: &Row| checkbox_cell(r, &dec.columns, 1);
    assert!(
        cb(&rows[0]).is_some_and(|(c, _)| !c),
        "null draws unchecked"
    );
    assert!(cb(&rows[2]).is_none(), "no path, no checkbox");
    assert!(checkbox_cell(&rows[0], &dec.columns, 0).is_none());
    // Open note: the buffer gains `read: true`; no daemon call.
    let (_, click) = cb(&rows[0]).expect("checkbox");
    let _ = app.send(click);
    assert!(d.bool_calls().expect("calls").is_empty());
    let text = app.doc(NOTE).expect("doc").text();
    assert!(text.starts_with("---\nread: true\n---\n# Host"), "{text}");
    // Closed note, checked: exactly one `note_set_bool` to false.
    let (checked, click) = cb(&rows[1]).expect("checkbox");
    assert!(checked);
    let _ = app.send(click);
    assert_eq!(
        d.bool_calls().expect("calls"),
        [cv_protocol::NoteSetBoolParams {
            id: None,
            path: "Linker.md".into(),
            key: "read".into(),
            value: false,
        }]
    );
    assert!(app.status.error_text().is_none());
}
