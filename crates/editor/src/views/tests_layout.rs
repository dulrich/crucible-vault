//! View-fence decode tests: column lengths by role, and the open-only predicate and toggle (split from `tests.rs`).

use super::*;

fn dec(cols: Vec<Column>, rows: Vec<Vec<Cell>>) -> Decoded {
    Decoded {
        kind: Kind::Table,
        columns: cols,
        lines: rows
            .into_iter()
            .map(|cells| {
                Line::Row(Row {
                    path: None,
                    line: None,
                    cells,
                    key: None,
                    select: None,
                })
            })
            .collect(),
        notices: Vec::new(),
        max_height: None,
        open_lines: None,
        select_by: None,
    }
}

fn link(s: &str) -> Cell {
    Cell::Link {
        path: format!("{s}.md"),
        label: s.to_owned(),
    }
}

fn fixed(l: Length) -> f32 {
    match l {
        Length::Fixed(w) => w,
        other => panic!("expected Fixed, got {other:?}"),
    }
}

/// Plan intake-round-2 WP-4: sized, numeric, checkbox and name columns get their lengths.
#[test]
fn column_lengths_by_role() {
    let mut sized = Column::plain("note.x", "X");
    sized.width = Some(137.0);
    let d = dec(
        vec![
            Column::plain("file.name", "file name"),
            Column::plain("note.words", "Words"),
            Column::plain("note.read", "Read")
                .with_role(ColumnRole::Checkbox { key: "read".into() }),
            Column::plain("note.author", "Author"),
            sized,
        ],
        vec![vec![
            link("A long note title"),
            Cell::Number { v: 12345.0 },
            Cell::Bool { v: true },
            Cell::Text { v: "Ann".into() },
            Cell::Text { v: "x".into() },
        ]],
    );
    let ls = column_lengths(&d, 1.0);
    assert!(
        matches!(ls[0], Length::FillPortion(n) if n > 2),
        "{:?}",
        ls[0]
    );
    assert!(fixed(ls[1]) < 120.1 && fixed(ls[1]) >= 28.0);
    assert!(fixed(ls[2]) < 120.1);
    assert!(matches!(ls[3], Length::FillPortion(2)), "{:?}", ls[3]);
    assert_eq!(fixed(ls[4]), 137.0);
}

#[test]
fn column_lengths_scale_and_narrow_numbers() {
    let mut sized = Column::plain("a", "A");
    sized.width = Some(100.0);
    let d = dec(
        vec![sized, Column::plain("n", "n")],
        vec![vec![Cell::Null, Cell::Number { v: 7.0 }]],
    );
    let (one, two) = (column_lengths(&d, 1.0), column_lengths(&d, 2.0));
    assert_eq!(fixed(two[0]), 200.0);
    assert!((fixed(two[1]) - 2.0 * fixed(one[1])).abs() < 1e-3);
    assert!(fixed(one[1]) < 100.0, "a number column is narrow");
    assert_eq!(column_lengths(&dec(Vec::new(), Vec::new()), 1.0).len(), 1);
}

fn col(key: &str, role: Option<ColumnRole>) -> Column {
    let c = Column::plain(key, key);
    match role {
        Some(r) => c.with_role(r),
        None => c,
    }
}

fn open_cols() -> Vec<Column> {
    vec![
        col("name", None),
        col("read", Some(ColumnRole::Checkbox { key: "read".into() })),
        col("st", Some(ColumnRole::Status)),
    ]
}

fn open_row(cb: Option<Cell>, st: Option<Cell>) -> Row {
    let mut cells = vec![Cell::Text { v: "n".into() }];
    cells.extend(cb);
    cells.extend(st);
    Row {
        path: None,
        line: None,
        cells,
        key: None,
        select: None,
    }
}

fn txt(v: &str) -> Option<Cell> {
    Some(Cell::Text { v: v.into() })
}

#[test]
fn open_predicate_treats_null_and_missing_as_open() {
    let cb_only = vec![col(
        "read",
        Some(ColumnRole::Checkbox { key: "read".into() }),
    )];
    let with = |cells: Vec<Cell>| Row {
        cells,
        ..open_row(None, None)
    };
    assert!(row_is_open(&with(vec![Cell::Bool { v: false }]), &cb_only));
    assert!(!row_is_open(&with(vec![Cell::Bool { v: true }]), &cb_only));
    assert!(row_is_open(&with(vec![Cell::Null]), &cb_only), "null");
    assert!(row_is_open(&with(vec![]), &cb_only), "missing");

    let st_only = vec![col("st", Some(ColumnRole::Status))];
    let s = |c: Option<Cell>| with(c.into_iter().collect());
    assert!(row_is_open(&s(txt(" ")), &st_only), "' ' is open");
    assert!(!row_is_open(&s(txt("x")), &st_only), "x is done");
    assert!(!row_is_open(&s(txt("-")), &st_only), "- is cancelled");
    assert!(row_is_open(&s(Some(Cell::Null)), &st_only), "null is open");
    assert!(row_is_open(&s(None), &st_only), "missing is open");

    let cols = open_cols();
    let done = Some(Cell::Bool { v: true });
    assert!(row_is_open(&open_row(done.clone(), txt(" ")), &cols));
    assert!(!row_is_open(&open_row(done, txt("x")), &cols));
    assert!(has_open_columns(&cols));
    assert!(!has_open_columns(&[col("name", None)]));
}

#[test]
fn open_only_drops_empty_groups_and_keeps_summaries() {
    let cols = open_cols();
    let done = || Line::Row(open_row(Some(Cell::Bool { v: true }), txt("x")));
    let open = || Line::Row(open_row(Some(Cell::Bool { v: false }), txt("x")));
    let g = |k: &str| Line::Group(Cell::Text { v: k.into() });
    let sum = || Line::Summary {
        label: "n".into(),
        cells: vec![],
    };
    let lines = vec![
        g("a"),
        done(),
        done(),
        sum(),
        g("b"),
        done(),
        open(),
        sum(),
        sum(),
    ];
    let visible: Vec<Line> = open_only_lines(&lines, &cols)
        .into_iter()
        .map(|i| lines[i].clone())
        .collect();
    assert_eq!(
        visible,
        vec![sum(), g("b"), open(), sum(), sum()],
        "group a dropped, group b kept, every summary kept"
    );
}

#[test]
fn decode_precomputes_open_lines_only_with_a_role_column() {
    let d = decode(&checkbox_result()).expect("decode");
    let open = d.open_lines.expect("checkbox column");
    assert_eq!(open.len(), 2, "the checked row is gone");
    let plain = decode(&result()).expect("decode");
    assert!(plain.open_lines.is_none());
}

#[test]
fn open_only_toggle_flips_and_persists() {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(&vault).expect("mkdir");
    std::fs::write(vault.join(NOTE), "# Host\n").expect("write");
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let state = tmp.path().join("editor.json");
    let mut app = App::new(Some(Arc::clone(&dc)), Some(state.clone()));
    app.queue_rpcs();
    let toggle = || Message::Views(ViewsMsg::ToggleOpenOnly { id: ID.into() });
    let _ = app.send(toggle());
    assert!(app.views.filters.contains(ID));
    let _ = app.send(Message::PersistDue);
    assert!(crate::ui_state::load(&state).view_filters.contains(ID));
    let mut app2 = App::new(Some(Arc::clone(&dc)), Some(state.clone()));
    app2.queue_rpcs();
    assert!(app2.views.filters.contains(ID), "restored on boot");
    let _ = app2.send(toggle());
    assert!(
        !app2.views.filters.contains(ID),
        "second press turns it off"
    );
    let _ = app2.send(Message::PersistDue);
    assert!(crate::ui_state::load(&state).view_filters.is_empty());
}
