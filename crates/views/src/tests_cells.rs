use super::*;

fn row_of(cells: Vec<Cell>) -> crate::result::Row {
    crate::result::Row {
        path: None,
        line: None,
        cells,
        key: None,
        select: None,
    }
}

fn text_cell(v: &str) -> Cell {
    Cell::Text { v: v.into() }
}

#[test]
fn cell_checked_is_only_true_bool() {
    use crate::result::cell_checked;
    assert!(cell_checked(Some(&Cell::Bool { v: true })));
    assert!(!cell_checked(Some(&Cell::Bool { v: false })), "false");
    assert!(!cell_checked(Some(&Cell::Null)), "null");
    assert!(!cell_checked(None), "missing");
    assert!(!cell_checked(Some(&text_cell("x"))), "text");
}

#[test]
fn cell_status_reads_first_char_and_null_is_open() {
    use crate::result::cell_status;
    assert_eq!(cell_status(None), Some(None), "missing is open");
    assert_eq!(cell_status(Some(&Cell::Null)), Some(None), "null is open");
    assert_eq!(
        cell_status(Some(&text_cell(" "))),
        Some(None),
        "space is open"
    );
    assert_eq!(
        cell_status(Some(&text_cell(""))),
        Some(None),
        "empty is open"
    );
    assert_eq!(cell_status(Some(&text_cell("x"))), Some(Some('x')));
    assert_eq!(cell_status(Some(&text_cell("-done"))), Some(Some('-')));
    assert_eq!(
        cell_status(Some(&Cell::Bool { v: true })),
        None,
        "other kind"
    );
}

#[test]
fn row_is_open_follows_role_columns() {
    use crate::result::{Column, ColumnRole, row_is_open};
    let cb = [Column::plain("d", "D").with_role(ColumnRole::Checkbox { key: "d".into() })];
    assert!(row_is_open(&row_of(vec![Cell::Bool { v: false }]), &cb));
    assert!(!row_is_open(&row_of(vec![Cell::Bool { v: true }]), &cb));
    assert!(row_is_open(&row_of(vec![Cell::Null]), &cb), "null");
    assert!(row_is_open(&row_of(vec![]), &cb), "missing");
    let st = [Column::plain("s", "S").with_role(ColumnRole::Status)];
    assert!(row_is_open(&row_of(vec![text_cell(" ")]), &st), "space");
    assert!(!row_is_open(&row_of(vec![text_cell("x")]), &st), "x done");
    assert!(
        !row_is_open(&row_of(vec![text_cell("-")]), &st),
        "cancelled"
    );
    assert!(row_is_open(&row_of(vec![Cell::Null]), &st), "null");
    assert!(row_is_open(&row_of(vec![]), &st), "missing");
    assert!(!row_is_open(
        &row_of(vec![Cell::Bool { v: false }]),
        &[Column::plain("n", "N")]
    ));
}

#[test]
fn has_open_columns_needs_a_checkbox_or_status_role() {
    use crate::result::{Column, ColumnRole, has_open_columns};
    assert!(!has_open_columns(&[]));
    assert!(!has_open_columns(&[Column::plain("n", "N")]));
    assert!(!has_open_columns(&[
        Column::plain("t", "T").with_role(ColumnRole::TaskText)
    ]));
    assert!(has_open_columns(&[
        Column::plain("s", "S").with_role(ColumnRole::Status)
    ]));
    assert!(has_open_columns(&[
        Column::plain("c", "C").with_role(ColumnRole::Checkbox { key: "c".into() })
    ]));
}
