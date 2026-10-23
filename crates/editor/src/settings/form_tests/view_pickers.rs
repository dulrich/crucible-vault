//! Round-10 WP-6: the Views page's value-set typeaheads (property ids, aggregates), by schema
//! reference hint (WP-R6), on view files only; free text still commits.

use super::*;
use crate::settings::pickers::{self, Picker};
use crate::widget::typeahead::Key;

const VIEW: &str = "_custom/crucible-vault/views/books.toml";

const BASE: &str = r#"[formulas]
age = "now() - file.ctime"

[properties]
"note.status" = { displayName = "Status" }

[[views]]
type = "table"
name = "T"
order = ["file.name", "word-count", "note[\"rating\"]"]
summaries = { "note.pages" = "Sum" }

[[views.sort]]
property = "file.mtime"

[views.groupBy]
property = "formula.age"

[views.columnSize]
"file.name" = 120
"#;

fn view_settings(text: &str) -> Settings {
    let mut st = Settings::default();
    let mut f = ConfigFile::new(VIEW, Some(cv_schema::view_schema()));
    f.apply_read(Ok((text.as_bytes().to_vec(), ContentHash::from_hex("00"))));
    st.files.insert(VIEW.to_owned(), f);
    st.form.toggle(VIEW, &p("views[0]"));
    st.form.toggle(VIEW, &p("views[0].sort[0]"));
    st
}

fn picker_at(rows: &[Row], path: &str) -> Option<Option<Picker>> {
    let at = p(path);
    rows.iter().find_map(|r| match &r.kind {
        Kind::Text { picker, .. } | Kind::Chips { picker, .. } if r.path == at => Some(*picker),
        _ => None,
    })
}

fn values(c: &[crate::widget::typeahead::Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.value.as_str()).collect()
}

#[test]
fn sort_property_has_the_property_picker_only_in_view_files() {
    let st = view_settings(BASE);
    let rows = rows_of(&st, VIEW);
    assert_eq!(
        picker_at(&rows, "views[0].sort[0].property"),
        Some(Some(Picker::PropertyId))
    );
    assert_eq!(
        picker_at(&rows, "views[0].groupBy.property"),
        Some(Some(Picker::PropertyId))
    );
    assert_eq!(
        picker_at(&rows, "views[0].order"),
        Some(Some(Picker::PropertyId))
    );
    // A non-view file's Str row carries no such picker (its schema has no such hint).
    let st = loaded(KINDS, "[kinds.book]\nfolder = \"b\"\n");
    let rows = rows_of(&st, KINDS);
    assert!(rows.iter().all(|r| !matches!(
        &r.kind,
        Kind::Text {
            picker: Some(Picker::PropertyId | Picker::Aggregate),
            ..
        }
    )));
}

#[test]
fn candidates_are_canonical_dotted_ids() {
    let st = view_settings(BASE);
    let ids = pickers::property_ids(&st, VIEW);
    let v = values(&ids);
    assert!(
        v.contains(&"file.name") && v.contains(&"file.links"),
        "{v:?}"
    );
    assert!(v.contains(&"formula.age"), "{v:?}");
    assert!(v.contains(&"note.word-count"), "bare order entry: {v:?}");
    assert!(v.contains(&"note.rating"), "bracket order entry: {v:?}");
    assert!(v.contains(&"note.status"), "properties key: {v:?}");
    assert!(!v.contains(&"word-count") && !v.iter().any(|i| i.contains('[')));
    let mut sorted = v.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), v.len(), "deduplicated");
}

#[test]
fn summary_values_offer_the_five_aggregates_and_keys_offer_property_ids() {
    let st = view_settings(BASE);
    let rows = rows_of(&st, VIEW);
    assert_eq!(
        picker_at(&rows, "views[0].summaries.\"note.pages\""),
        Some(Some(Picker::Aggregate))
    );
    let cands = pickers::visible(
        &st,
        Picker::Aggregate,
        VIEW,
        &p("views[0].summaries.\"note.pages\""),
        &[],
        "",
    );
    let mut got = values(&cands);
    got.sort_unstable();
    assert_eq!(got, ["Average", "Count", "Max", "Min", "Sum"]);
    let keyed = |path: &str| {
        rows.iter().any(|r| {
            matches!(&r.kind, Kind::MapKey { picker: Some(Picker::PropertyId), map, .. }
                if *map == p(path))
        })
    };
    assert!(keyed("views[0].summaries") && keyed("views[0].columnSize") && keyed("properties"));
}

#[test]
fn add_key_under_column_size_offers_property_ids_and_a_pick_adds_it() {
    let mut st = view_settings(BASE);
    let rows = rows_of(&st, VIEW);
    let node = rows
        .iter()
        .find_map(|r| match &r.kind {
            Kind::AddKey {
                map,
                picker: Some(Picker::PropertyId),
                value,
                ..
            } if *map == p("views[0].columnSize") => Some(*value),
            _ => None,
        })
        .expect("columnSize AddKey carries the picker");
    st.form_msg(
        VIEW,
        FormMsg::NewKeyTa {
            map: p("views[0].columnSize"),
            text: "formula.a".into(),
            picker: Picker::PropertyId,
            value: node,
        },
    );
    let vis = st.ta_visible(&[]);
    assert_eq!(vis.first().map(|c| c.value.as_str()), Some("formula.age"));
    st.typeahead_key(Key::Enter, &[]);
    let d = st.file(VIEW).and_then(|f| f.doc()).expect("doc");
    assert!(
        d.keys(&p("views[0].columnSize"))
            .contains(&"formula.age".to_owned())
    );
}

#[test]
fn free_text_commits_unchanged() {
    let mut st = view_settings(BASE);
    // A value row: a non-candidate property commits as typed.
    st.form_msg(
        VIEW,
        FormMsg::Text {
            path: p("views[0].sort[0].property"),
            text: "my own prop".into(),
            required: true,
            picker: Some(Picker::PropertyId),
        },
    );
    st.typeahead_key(Key::Escape, &[]);
    assert!(doc_text(&st, VIEW).contains("property = \"my own prop\""));
    assert!(pickers::is_valid(
        &st,
        Picker::PropertyId,
        VIEW,
        &p("views[0].order"),
        &[],
        "zzz"
    ));
    // A rename draft keeps the typed key through the Rename commit.
    st.form_msg(
        VIEW,
        FormMsg::RenameTa {
            at: p("views[0].columnSize.\"file.name\""),
            text: "custom-col".into(),
            picker: Picker::PropertyId,
        },
    );
    st.form_msg(
        VIEW,
        FormMsg::RenameCommit {
            map: p("views[0].columnSize"),
            from: "file.name".into(),
        },
    );
    let d = st.file(VIEW).and_then(|f| f.doc()).expect("doc");
    assert_eq!(d.keys(&p("views[0].columnSize")), ["custom-col"]);
}
