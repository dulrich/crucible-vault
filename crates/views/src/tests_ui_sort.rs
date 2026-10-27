use super::*;

const UI_SORT: &str = r##"
[formulas]
k = '1'

[[views]]
type = "table"
name = "All"
order = ["file.name", "formula.k", "note.word-count"]
sort = [{ property = "note.word-count", direction = "DESC" }]

[[views]]
type = "table"
name = "Top2"
order = ["file.name", "formula.k"]
limit = 2

[[views]]
type = "table"
name = "Sel"
order = ["file.name", "note.word-count"]
summaries = { "file.name" = "Count" }
select_by = "file.name"

[[views]]
type = "table"
name = "Empty"
filters = 'file.name == "zzz-none"'
order = ["file.name", "note.word-count"]
summaries = { "note.word-count" = "Sum" }
"##;

fn names(r: &ViewResult) -> Vec<String> {
    r.rows().map(|row| text(&row.cells[0])).collect()
}

fn ui(property: &str, direction: cv_protocol::ViewSortDir) -> cv_protocol::ViewSort {
    cv_protocol::ViewSort {
        property: property.into(),
        direction,
    }
}

/// Plan round-11 WP-5: the UI sort is the primary key; the definition sort breaks its ties; it
/// runs before `limit`; an unknown column is ignored.
#[test]
fn ui_sort_is_primary_then_definition_keys_and_precedes_limit() {
    use cv_protocol::ViewSortDir::{Asc, Desc};
    let fx = fixture(&[("us.toml", UI_SORT)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let g = fx.reader().generation();
    let mut run = |id: &str, s: Option<cv_protocol::ViewSort>| {
        h.run(
            g,
            &RunRequest {
                sort: s.as_ref(),
                ..RunRequest::view(id)
            },
        )
        .unwrap()
    };
    let base = run("us#All", None);
    let asc = names(&run("us#All", Some(ui("file.name", Asc))));
    let mut desc = names(&run("us#All", Some(ui("file.name", Desc))));
    desc.reverse();
    assert_eq!(asc, desc, "DESC is the reverse of ASC");
    assert!(asc.len() > 3);
    assert_ne!(
        asc,
        names(&base),
        "the UI sort overrides the definition sort"
    );
    // Every row ties on the UI column, so the definition sort (word-count DESC) decides.
    assert_eq!(
        names(&run("us#All", Some(ui("formula.k", Asc)))),
        names(&base)
    );
    assert_eq!(
        names(&run("us#All", Some(ui("nope", Asc)))),
        names(&base),
        "unknown property ignored"
    );
    // Before `limit`: the two largest names, not the first two rows sorted.
    let top = names(&run("us#Top2", Some(ui("file.name", Desc))));
    let mut all_desc = asc.clone();
    all_desc.reverse();
    assert_eq!(top, all_desc[..2]);
}

/// Plan round-11 WP-5: an empty Sum is `0`, never `-0`.
#[test]
fn empty_sum_renders_zero() {
    let fx = fixture(&[("us.toml", UI_SORT)]);
    let mut h = fx.host();
    let r = fx.run(&mut h, "us#Empty", None);
    assert_eq!(text(&r.summaries[0].cells[1]), "0");
}

/// Plan round-13 WP-4: the selection reduces rows and summaries; absent = unchanged; the
/// result carries the definition's `select_by`.
#[test]
fn ui_select_filters_rows_and_summaries() {
    let fx = fixture(&[("us.toml", UI_SORT)]);
    let mut h = fx.host();
    let g = fx.reader().generation();
    let all = h.run(g, &RunRequest::view("us#Sel")).unwrap();
    assert_eq!(all.select_by.as_deref(), Some("file.name"));
    let first = names(&all)[0].clone();
    assert!(names(&all).len() > 1);
    let count = |r: &ViewResult| text(&r.summaries[0].cells[0]);
    assert_eq!(count(&all), names(&all).len().to_string());
    let sel = cv_protocol::ViewSelect {
        property: "file.name".into(),
        value: first.clone(),
    };
    let one = h
        .run(
            g,
            &RunRequest {
                select: Some(&sel),
                ..RunRequest::view("us#Sel")
            },
        )
        .unwrap();
    assert_eq!(names(&one), [first]);
    assert_eq!(count(&one), "1");
    let none = cv_protocol::ViewSelect {
        property: "file.name".into(),
        value: "zzz-none".into(),
    };
    assert!(
        names(
            &h.run(
                g,
                &RunRequest {
                    select: Some(&none),
                    ..RunRequest::view("us#Sel")
                },
            )
            .unwrap()
        )
        .is_empty()
    );
}
