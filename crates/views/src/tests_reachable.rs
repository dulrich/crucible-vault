//! Plan memory-release-strategy WP-2: the reachable-key walk and the projected preload.

use std::cell::RefCell;
use std::collections::BTreeSet;

use super::{A, day, fixture};
use crate::db::Db;
use crate::def::{Base, check_text};
use crate::engine::reachable_keys;
use crate::eval::{Env, RowCtx};
use crate::expr::parse;
use crate::value::Value;

fn base(toml: &str) -> Base {
    check_text("t", toml, &mut Vec::new()).unwrap()
}

fn keys_of(b: &Base, sort: Option<&str>, select: Option<&str>) -> Option<Vec<String>> {
    let s = sort.map(|p| cv_protocol::ViewSort {
        property: p.into(),
        direction: cv_protocol::ViewSortDir::Asc,
    });
    let sel = select.map(|p| cv_protocol::ViewSelect {
        property: p.into(),
        value: "x".into(),
    });
    reachable_keys(&b.formulas, &b.views[0], s.as_ref(), sel.as_ref())
        .map(|k| k.into_iter().collect())
}

/// Keys of a one-view table base whose filter is `filter`.
fn filter_keys(filter: &str) -> Option<Vec<String>> {
    let toml = format!(
        "[[views]]\ntype='table'\nname='V'\nfilters={{and=[{}]}}\norder=['file.name']\n",
        toml_str(filter)
    );
    keys_of(&base(&toml), None, None)
}

fn toml_str(s: &str) -> String {
    format!("'{s}'")
}

fn has(k: &Option<Vec<String>>, key: &str) -> bool {
    k.as_ref().is_some_and(|v| v.iter().any(|x| x == key))
}

fn expr_keys(src: &str) -> Option<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    parse(src).unwrap().prop_keys(&mut out).then_some(out)
}

#[test]
fn bare_name_note_member_and_literal_index() {
    assert!(has(&filter_keys("author == 1"), "author"));
    assert!(has(&filter_keys("note.read == true"), "read"));
    assert!(has(&filter_keys(r#"note["word-count"] > 1"#), "word-count"));
    // Reserved identifiers are not props; member names always count (a superset).
    let k = expr_keys("file.name + this.goal").unwrap();
    assert_eq!(k.into_iter().collect::<Vec<_>>(), vec!["goal", "name"]);
}

#[test]
fn member_on_this_and_file_values() {
    let k = expr_keys(r#"this.goal + file("x.md").target + file.links[0].peer"#).unwrap();
    for key in ["goal", "target", "peer"] {
        assert!(k.contains(key), "{key}: {k:?}");
    }
}

#[test]
fn formulas_reach_through_each_other() {
    let b = base(
        "[formulas]\na='formula.b + 1'\nb='note.deep'\n[[views]]\ntype='table'\nname='V'\norder=['formula.a']\n",
    );
    assert!(has(&keys_of(&b, None, None), "deep"));
}

#[test]
fn every_view_site_contributes_its_keys() {
    let b = base(
        r#"
[[views]]
type = "table"
name = "V"
filters = { and = ['note.k_filter == 1'] }
order = ["file.name", "note.k_order"]
sort = [{ property = "note.k_sort", direction = "ASC" }]
groupBy = { property = "note.k_group", direction = "ASC" }
summaries = { "note.k_sum" = "Sum" }
selectBy = "note.k_select_by"
[[views.summary_rows]]
label = "S"
filter = "note.k_srow == 1"
summaries = { "note.k_srow_sum" = "Sum" }
"#,
    );
    let k = keys_of(&b, Some("note.k_ui_sort"), Some("note.k_ui_select"));
    for key in [
        "k_filter",
        "k_order",
        "k_sort",
        "k_group",
        "k_sum",
        "k_select_by",
        "k_srow",
        "k_srow_sum",
        "k_ui_sort",
        "k_ui_select",
    ] {
        assert!(has(&k, key), "{key}: {k:?}");
    }
}

#[test]
fn pivot_and_task_sites_contribute_their_keys() {
    let b = base(
        r#"
[[views]]
type = "pivot"
name = "P"
rows = "note.k_rows"
value = "note.k_value"
display = "value + note.k_display"
columns = [{ label = "c", when = "note.k_when" }]
"#,
    );
    let k = keys_of(&b, None, None);
    for key in ["k_rows", "k_value", "k_display", "k_when"] {
        assert!(has(&k, key), "{key}: {k:?}");
    }
    let b = base("[[views]]\ntype='tasks'\nname='T'\ntask_filter='value.text != this.k_task'\n");
    assert!(has(&keys_of(&b, None, None), "k_task"));
}

#[test]
fn file_properties_is_all() {
    assert_eq!(filter_keys("file.properties.x == 1"), None);
    assert_eq!(filter_keys(r#"file["properties"].x == 1"#), None);
    let b = base(
        "[formulas]\nf='this.properties'\n[[views]]\ntype='table'\nname='V'\norder=['file.name']\n",
    );
    assert_eq!(
        keys_of(&b, None, None),
        None,
        "an unused formula still counts"
    );
}

#[test]
fn computed_index_is_all() {
    assert_eq!(filter_keys(r#"note["a" + "b"] == 1"#), None);
    assert_eq!(filter_keys("file[note.k] == 1"), None);
    // A literal number index is enumerable (its text is the key on a non-list).
    assert!(has(&filter_keys("note.list[0] == 1"), "list"));
}

#[test]
fn note_as_a_value_is_all() {
    assert_eq!(filter_keys("list(note).length > 0"), None);
    assert_eq!(expr_keys("note"), None);
    assert_eq!(expr_keys("note.toString()"), None);
    assert!(expr_keys("note.k.toString()").is_some());
}

#[test]
fn ui_sort_and_select_can_force_all() {
    let b = base("[[views]]\ntype='table'\nname='V'\norder=['file.name']\n");
    assert_eq!(keys_of(&b, None, None), Some(vec!["name".to_string()]));
    assert_eq!(keys_of(&b, Some("file.properties"), None), None);
    assert_eq!(keys_of(&b, None, Some("file.properties")), None);
}

const VIEW: &str = r#"
[[views]]
type = "table"
name = "V"
filters = { and = ['note.author != null || true'] }
order = ["file.name", "word-count"]
"#;

const ALL: &str = r#"
[[views]]
type = "table"
name = "V"
order = ["file.name", "file.properties"]
"#;

/// Preloads every markdown file for `toml`'s first view; returns `(cached keys of a.md,
/// whether every cached map holds only reachable keys, `src` evaluated on a.md)`.
fn preload_run(toml: &str, src: &str) -> (Vec<String>, bool, Value) {
    let fx = fixture(&[]);
    let conn = rusqlite::Connection::open_with_flags(
        cv_index::generation_path(&fx.base, fx.reader().generation()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let db = Db::new(&conn);
    let b = base(toml);
    let keys = reachable_keys(&b.formulas, &b.views[0], None, None);
    let cands = db.candidates("", &[]).unwrap();
    db.preload(&cands, keys.as_ref()).unwrap();
    let within = cands.iter().all(|f| {
        let m = db.cached_props(f.id).expect("every candidate is cached");
        keys.as_ref()
            .is_none_or(|k| m.keys().all(|x| k.contains(x)))
    });
    let a = db.file(A.unwrap()).unwrap().unwrap();
    let cached: Vec<String> = db.cached_props(a.id).unwrap().keys().cloned().collect();
    let env = Env {
        db: &db,
        formulas: &b.formulas,
        this: None,
        today: day(2026, 10, 3),
        now: day(2026, 10, 3).and_hms_opt(0, 0, 0).unwrap(),
        notices: RefCell::default(),
        serial: None,
        folders: None,
    };
    let v = RowCtx::new(&env, Some(a))
        .eval(&parse(src).unwrap())
        .unwrap();
    (cached, within, v)
}

#[test]
fn a_view_naming_k_keys_preloads_only_those() {
    let (cached, within, v) = preload_run(VIEW, "note.author");
    assert_eq!(cached, vec!["author", "word-count"]);
    assert!(within);
    assert_eq!(v.to_text(), "Ann");
}

#[test]
fn a_file_properties_view_still_sees_every_property() {
    let (cached, within, v) = preload_run(ALL, "file.properties.source");
    assert_eq!(
        cached,
        vec!["author", "created", "read", "source", "tags", "word-count"]
    );
    assert!(within);
    assert_eq!(v.to_text(), "https://blog.example/a");
}
