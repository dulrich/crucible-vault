//! End-to-end tests on hand-written synthetic fixture notes (invariant I1: no live vault).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::db::Db;
use crate::eval::{Env, RowCtx};
use crate::expr::parse;
use crate::result::{Cell, Kind, ViewResult};
use crate::service::{RunRequest, ViewHost};
use crate::value::Value;

const NOTES: &[(&str, &str)] = &[
    ("_custom/Settings.md", "---\nreading_wpm: 200\n---\n"),
    (
        "daily/day/2026-10-01.md",
        "- [ ] loose task\n\n## Morning\n\n- [ ] open one\n- [x] done one\n- [>] moved one\n",
    ),
    (
        "daily/day/2026-10-02.md",
        "## Work\n\n- [ ] yesterday task\n",
    ),
    ("daily/day/2026-10-03.md", "- [ ] today task\n"),
    (
        "daily/day/2026-10-01/a.md",
        "---\nword-count: 400\ntags: [blog, clippings]\nread: true\ncreated: 2026-10-01\nauthor: Ann\nsource: https://blog.example/a\n---\nbody\n",
    ),
    (
        "daily/day/2026-10-01/b.md",
        "---\nword-count: 1000\ntags: [news]\nread: false\ncreated: 2026-10-01\nauthor: Bob\nsource: https://news.example/b\n---\nbody\n",
    ),
    (
        "daily/day/2026-10-02/c.md",
        "---\nword-count: 600\ntags: [blog]\nread: false\ncreated: 2026-10-02\nauthor: \"#Ann\"\nsource: https://blog.example/c\n---\nbody\n",
    ),
    (
        "daily/day/2026-10-02/d.md",
        "---\nword-count: 200\ntags: [daily]\ncreated: 2026-10-02\n---\nbody\n",
    ),
    (
        "daily/day/2026-10-03/e.md",
        "---\nword-count: 300\ntags: [fanfic, transcript]\nread: false\ncreated: 2026-10-03\n---\nbody #using\n",
    ),
    (
        "core/works/Ann Blog.md",
        "---\nauthor-filter: Ann\nsource-prefix-filter: [\"https://blog.example/\"]\nwork-status: active\n---\n",
    ),
    ("core/works/Empty.md", "---\nwork-status: complete\n---\n"),
    (
        "initiatives/alpha/INITIATIVE.md",
        "---\ninitiative-title: Alpha\npending-plans: [\"p1\", \" \"]\n---\n# Blocked on User\n\n- [ ] ask one\n- [x] asked\n\n# Other\n\n- [ ] not counted\n",
    ),
    ("initiatives/alpha/README.md", "# readme\n"),
    (
        "initiatives/beta/INITIATIVE.md",
        "---\ninitiative-status: \" Blocked \"\n---\n",
    ),
    ("stack/tool1.md", "---\ndescription: first\n---\n#using\n"),
    (
        "stack/tool2.md",
        "---\ndescription: second\n---\nsee [[tool1]]\n",
    ),
    (
        "notes/x.md",
        "links [[tool1]] and [[daily/day/2026-10-01|day]]\n",
    ),
];

struct Fx {
    _tmp: tempfile::TempDir,
    config: PathBuf,
    base: PathBuf,
    _index: cv_index::Index,
}

fn fixture(views: &[(&str, &str)]) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    for (p, body) in NOTES {
        let f = vault.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, body).unwrap();
    }
    let config = tmp.path().join("config");
    std::fs::create_dir_all(config.join("views")).unwrap();
    for (name, body) in views {
        std::fs::write(config.join("views").join(name), body).unwrap();
    }
    let base = tmp.path().join("idx").join("index.db");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    let index = cv_index::Index::create(&vault, &base).unwrap();
    Fx {
        _tmp: tmp,
        config,
        base,
        _index: index,
    }
}

impl Fx {
    fn host(&self) -> ViewHost {
        ViewHost::new(&self.config, &self.base).with_today(day(2026, 10, 3))
    }
    fn reader(&self) -> cv_index::Reader {
        cv_index::Reader::open_current(&self.base).unwrap()
    }
    fn run(&self, host: &mut ViewHost, id: &str, this: Option<&str>) -> ViewResult {
        let g = self.reader().generation();
        host.run(
            g,
            &RunRequest {
                this_path: this,
                ..RunRequest::view(id)
            },
        )
        .unwrap()
    }
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn text(c: &Cell) -> String {
    match c {
        Cell::Null | Cell::Unknown => String::new(),
        Cell::Bool { v } => v.to_string(),
        Cell::Number { v } => crate::value::fmt_num(*v),
        Cell::Text { v } | Cell::Date { v } => v.clone(),
        Cell::Link { label, .. } | Cell::Url { label, .. } => label.clone(),
        Cell::List { v } => v.iter().map(text).collect::<Vec<_>>().join("|"),
    }
}

fn grid(r: &ViewResult) -> Vec<Vec<String>> {
    r.rows()
        .map(|row| row.cells.iter().map(text).collect())
        .collect()
}

/// Evaluates `src` with `file` = `row` and `this` = `this`.
fn ev(fx: &Fx, row: Option<&str>, this: Option<&str>, src: &str) -> Value {
    let conn = rusqlite::Connection::open_with_flags(
        cv_index::generation_path(&fx.base, fx.reader().generation()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let db = Db::new(&conn);
    let mut formulas = BTreeMap::new();
    formulas.insert(
        "minutes".to_string(),
        parse(r#"(number(note["word-count"]) / number(file("_custom/Settings.md").properties.reading_wpm)).round(1)"#)
            .map_err(|e| e.to_string()),
    );
    formulas.insert(
        "loop".to_string(),
        parse("formula.loop").map_err(|e| e.to_string()),
    );
    let env = Env {
        db: &db,
        formulas: &formulas,
        this: this.and_then(|p| db.file(p).unwrap()),
        today: day(2026, 10, 3),
        now: day(2026, 10, 3).and_hms_opt(12, 0, 0).unwrap(),
        notices: RefCell::default(),
        serial: None,
    };
    let file = row.and_then(|p| db.file(p).unwrap());
    let ctx = RowCtx::new(&env, file);
    let e = parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    let v = ctx.eval(&e).unwrap();
    // Unknown/cyclic formulas surface as notices, not errors.
    if src.contains("formula.loop") || src.contains("formula.nope") {
        assert!(!env.notices.borrow().is_empty());
    }
    v
}

fn t(fx: &Fx, row: Option<&str>, this: Option<&str>, src: &str) -> String {
    ev(fx, row, this, src).to_text()
}

const A: Option<&str> = Some("daily/day/2026-10-01/a.md");

const READING: &str = r##"
[filters]
and = ['file.folder == "daily/day/" + this.file.name']

[formulas]
tags = 'list(note.tags).map(value.toString().replace("#", "")).filter(value != "clippings" && value != "refined" && value != "using").join(", ")'
minutes = '(number(note["word-count"]) / number(file("_custom/Settings.md").properties.reading_wpm)).round(1)'
unread_words = 'if(note.read == true, 0, number(note["word-count"]))'
read = 'if(note.read == true, "✅", "❌")'

[properties."note.word-count"]
displayName = "Words"

[[views]]
type = "table"
name = "Reading List"
order = ["file.name", "formula.tags", "word-count", "formula.minutes", "formula.read", "formula.unread_words"]
summaries = { "word-count" = "Sum", "formula.minutes" = "Sum", "formula.unread_words" = "Sum", "formula.read" = "Count" }
columnSize = { "formula.read" = 137 }
"##;

const MONTH: &str = r##"
[formulas]
category = 'if(list(note.tags).contains("blog"), "blog", "other")'

[[views]]
type = "table"
name = "Month"
filters = { and = [{ or = ["note.read == false", "file.hasTag(\"news\")"] }, { not = ['file.hasTag("daily")'] }] }
order = ["file.name", "note.word-count"]
sort = [{ property = "note.word-count", direction = "DESC" }]
groupBy = { property = "formula.category", direction = "ASC" }
summaries = { "note.word-count" = "Sum" }
limit = 10
"##;

/// Plan round-8 WP-2: `maxHeight` travels from the def to the `ViewResult`.
#[test]
fn max_height_reaches_the_result() {
    let def = "[[views]]\ntype = \"table\"\nname = \"Capped\"\nmaxHeight = 600\norder = [\"file.name\"]\n\n[[views]]\ntype = \"table\"\nname = \"Open\"\nmaxHeight = \"none\"\norder = [\"file.name\"]\n\n[[views]]\ntype = \"table\"\nname = \"Plain\"\norder = [\"file.name\"]\n";
    let fx = fixture(&[("mh.toml", def)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let mut mh = |v: &str| fx.run(&mut h, &format!("mh#{v}"), None).max_height;
    assert_eq!(mh("Capped"), Some(crate::result::MaxHeight::Px(600.0)));
    assert_eq!(mh("Open"), Some(crate::result::MaxHeight::Uncapped));
    assert_eq!(mh("Plain"), None);
}

#[test]
fn table_view_end_to_end() {
    let fx = fixture(&[("daily-reading-list.toml", READING), ("month.toml", MONTH)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let r = fx.run(
        &mut h,
        "daily-reading-list#Reading List",
        Some("daily/day/2026-10-01.md"),
    );
    assert_eq!(r.kind, Kind::Table);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(r.columns[2].label, "Words");
    assert_eq!(r.columns[0].key, "file.name");
    assert_eq!(
        grid(&r),
        vec![
            vec!["a", "blog", "400", "2", "✅", "0"],
            vec!["b", "news", "1000", "5", "❌", "1000"],
        ]
    );
    let row0 = r.rows().next().unwrap();
    assert_eq!(row0.path.as_deref(), Some("daily/day/2026-10-01/a.md"));
    assert!(
        matches!(&row0.cells[0], Cell::Link { path, .. } if path == "daily/day/2026-10-01/a.md")
    );
    assert_eq!(r.summaries.len(), 2);
    assert_eq!(r.summaries[0].label, "Sum");
    assert_eq!(
        r.summaries[0].cells.iter().map(text).collect::<Vec<_>>(),
        vec!["", "", "1400", "7", "", "1000"]
    );
    assert_eq!(r.summaries[1].label, "Count");
    assert_eq!(text(&r.summaries[1].cells[4]), "2");

    // Different `this` → different rows; none → no rows.
    let r2 = fx.run(
        &mut h,
        "daily-reading-list#Reading List",
        Some("daily/day/2026-10-02.md"),
    );
    assert_eq!(grid(&r2).len(), 2);
    let r3 = fx.run(&mut h, "daily-reading-list#Reading List", None);
    assert_eq!(grid(&r3).len(), 0);

    // and/or/not, sort DESC, groupBy, per-group Sum.
    let m = fx.run(&mut h, "month#Month", None);
    let keys: Vec<String> = m
        .groups
        .iter()
        .map(|g| text(g.key.as_ref().unwrap()))
        .collect();
    assert_eq!(keys, vec!["blog", "other"]);
    let g0: Vec<Vec<String>> = m.groups[0]
        .rows
        .iter()
        .map(|r| r.cells.iter().map(text).collect())
        .collect();
    assert_eq!(g0, vec![vec!["c", "600"]]);
    let g1: Vec<String> = m.groups[1].rows.iter().map(|r| text(&r.cells[0])).collect();
    assert_eq!(g1, vec!["b", "e"]);
    assert_eq!(text(&m.groups[1].summaries[0].cells[1]), "1300");
    assert_eq!(text(&m.summaries[0].cells[1]), "1900");

    // The protocol shape.
    let p = h
        .run(fx.reader().generation(), &RunRequest::view("month#Month"))
        .unwrap()
        .into_protocol()
        .unwrap();
    assert_eq!(p.0["kind"], "table");
    assert_eq!(p.0["groups"][0]["key"]["t"], "text");
    assert_eq!(p.0["groups"][0]["rows"][0]["cells"][0]["t"], "link");
    assert!(
        h.run(fx.reader().generation(), &RunRequest::view("nope#x"))
            .is_err()
    );
}

const PIVOT: &str = r##"
[filters]
and = ['file.inFolder("daily/day")', "note.created"]

[[views]]
type = "pivot"
name = "By Day"
filters = { and = ['!list(note.tags).contains("daily")'] }
rows = 'note.created.format("YYYY-MM-DD")'
rows_label = "Day"
columns = [
  { label = "blog", when = 'list(note.tags).contains("blog")' },
  { label = "news", when = 'list(note.tags).contains("news")' },
  { label = "transcript", when = 'list(note.tags).contains("transcript")' },
  { label = "fanfic", when = 'list(note.tags).contains("fanfic")' },
  { label = "other" },
]
value = 'number(note["word-count"])'
display = 'value.round(0).toString() + " / " + (value / 200).round(1)'
total_column = "Total"
total_row = "Total"
average_row = "Average/day"
"##;

#[test]
fn pivot_first_match_total_and_average() {
    let fx = fixture(&[("monthly-reading.toml", PIVOT)]);
    let mut h = fx.host();
    let r = fx.run(&mut h, "monthly-reading#By Day", None);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(r.kind, Kind::Pivot);
    let labels: Vec<&str> = r.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Day",
            "blog",
            "news",
            "transcript",
            "fanfic",
            "other",
            "Total"
        ]
    );
    assert_eq!(
        grid(&r),
        vec![
            vec!["2026-10-01", "400 / 2", "1000 / 5", "", "", "", "1400 / 7"],
            vec!["2026-10-02", "600 / 3", "", "", "", "", "600 / 3"],
            // fanfic+transcript → first match is transcript; the `daily` note is excluded.
            vec!["2026-10-03", "", "", "300 / 1.5", "", "", "300 / 1.5"],
        ]
    );
    assert_eq!(r.summaries.len(), 2);
    let tot: Vec<String> = r.summaries[0].cells.iter().map(text).collect();
    assert_eq!(
        tot,
        vec![
            "Total",
            "1000 / 5",
            "1000 / 5",
            "300 / 1.5",
            "0 / 0",
            "0 / 0",
            "2300 / 11.5"
        ]
    );
    // Average/day = total ÷ 3 days with entries.
    let avg: Vec<String> = r.summaries[1].cells.iter().map(text).collect();
    assert_eq!(avg[0], "Average/day");
    assert_eq!(avg[1], "333 / 1.7");
    assert_eq!(avg[6], "767 / 3.8");
}

const TASKS: &str = r##"
[[views]]
type = "tasks"
name = "Overdue"
filters = { and = ['file.inFolder("daily/day")', "file.name < today()"] }
status = [" "]

[[views]]
type = "tasks"
name = "Blocked"
filters = 'file.name == "INITIATIVE"'
status = [" "]
task_filter = '["Blocked on User", "Blockers"].contains(value.heading)'
task_group = "file"
"##;

#[test]
fn tasks_overdue_grouped_by_heading() {
    let fx = fixture(&[("monthly-tasks.toml", TASKS)]);
    let mut h = fx.host();
    let r = fx.run(&mut h, "monthly-tasks#Overdue", None);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    // The Tasks columns carry their roles.
    let roles: Vec<_> = r.columns.iter().map(|c| c.role.clone()).collect();
    assert_eq!(
        roles,
        [
            Some(crate::result::ColumnRole::TaskText),
            Some(crate::result::ColumnRole::TaskFile),
            Some(crate::result::ColumnRole::Status),
        ]
    );
    let groups: Vec<(String, Vec<String>)> = r
        .groups
        .iter()
        .map(|g| {
            (
                text(g.key.as_ref().unwrap()),
                g.rows.iter().map(|r| text(&r.cells[0])).collect(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        vec![
            ("General".to_string(), vec!["loose task".to_string()]),
            ("Morning".to_string(), vec!["open one".to_string()]),
            ("Work".to_string(), vec!["yesterday task".to_string()]),
        ]
    );
    let first = r.rows().next().unwrap();
    assert_eq!(first.path.as_deref(), Some("daily/day/2026-10-01.md"));
    assert_eq!(first.line, Some(1));

    let b = fx.run(&mut h, "monthly-tasks#Blocked", None);
    assert_eq!(grid(&b), vec![vec!["ask one", "INITIATIVE", " "]]);
    assert!(
        matches!(&b.groups[0].key, Some(Cell::Link { path, .. }) if path == "initiatives/alpha/INITIATIVE.md")
    );
}

#[test]
fn bad_files_and_unknown_functions_are_notices() {
    let good = r##"
[formulas]
x = 'bogus(1)'

[[views]]
type = "table"
name = "Formula"
filters = 'file.inFolder("stack")'
order = ["file.name", "formula.x", "formula.missing"]

[[views]]
type = "table"
name = "Filter"
filters = 'nope()'
order = ["file.name"]

[[views]]
type = "pivot"
name = "Half"
"##;
    let fx = fixture(&[
        ("a-bad.toml", "[[views]\ntype = "),
        ("b-good.toml", good),
        (
            "c-unknown-key.toml",
            "[[views]]\ntype='table'\nname='x'\nwat=1\n",
        ),
    ]);
    let mut h = fx.host();
    assert_eq!(h.load_notices().len(), 2, "{:?}", h.load_notices());
    assert!(h.load_notices()[0].starts_with("a-bad.toml"));
    assert_eq!(
        h.view_ids(),
        vec!["b-good#Filter", "b-good#Formula", "b-good#Half"]
    );
    let r = fx.run(&mut h, "b-good#Formula", None);
    assert_eq!(grid(&r), vec![vec!["tool1", "", ""], vec!["tool2", "", ""]]);
    assert!(
        r.notices
            .iter()
            .any(|n| n.contains("unknown function `bogus`"))
    );
    assert!(
        r.notices
            .iter()
            .any(|n| n.contains("unknown formula `missing`"))
    );
    assert!(r.notices.iter().any(|n| n.starts_with("load: a-bad.toml")));
    let f = fx.run(&mut h, "b-good#Filter", None);
    assert_eq!(f.rows().count(), 0);
    assert!(
        f.notices
            .iter()
            .any(|n| n.contains("unknown function `nope`"))
    );
    let p = fx.run(&mut h, "b-good#Half", None);
    assert!(p.notices.iter().any(|n| n.contains("pivot needs")));
}

#[test]
fn hosting_ids_invalidation_and_reload() {
    let fx = fixture(&[(
        "hubs.toml",
        "[[views]]\ntype='table'\nname='Stack'\nfilters='file.folder == \"stack\"'\norder=['file.name','note.description']\n",
    )]);
    let mut h = fx.host();
    assert_eq!(h.view_ids(), vec!["hubs#Stack"]);
    let reader = fx.reader();
    let p = h
        .run(reader.generation(), &RunRequest::view("hubs#Stack"))
        .unwrap()
        .into_protocol()
        .unwrap();
    assert_eq!(p.0["kind"], "table");
    assert_eq!(p.0["columns"][1]["label"], "description");
    assert_eq!(
        p.0["groups"][0]["rows"][0]["cells"][1],
        serde_json::json!({"t":"text","v":"first"})
    );
    assert_eq!(
        h.on_index_generation(reader.generation() + 1),
        vec!["hubs#Stack"]
    );
    // Still serves the old generation through a fresh connection.
    assert!(
        h.run(reader.generation(), &RunRequest::view("hubs#Stack"))
            .is_ok()
    );

    std::fs::write(
        fx.config.join("views").join("more.toml"),
        "[[views]]\ntype='tasks'\nname='All'\n",
    )
    .unwrap();
    std::fs::write(fx.config.join("views").join("hubs.toml"), "not toml [").unwrap();
    let ids = h.on_config_changed(&fx.config);
    assert_eq!(ids, vec!["hubs#Stack", "more#All"]);
    assert_eq!(h.view_ids(), vec!["more#All"]);
    assert_eq!(h.load_notices().len(), 1);
    // A missing views dir is simply no views.
    let empty = ViewHost::new(Path::new("/nonexistent-cv-views"), &fx.base);
    assert!(empty.view_ids().is_empty() && empty.load_notices().is_empty());
}

#[test]
fn sql_narrowing_compiles_simple_conjuncts() {
    let fx = fixture(&[]);
    let conn = rusqlite::Connection::open_with_flags(
        cv_index::generation_path(&fx.base, fx.reader().generation()),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let db = Db::new(&conn);
    let formulas = BTreeMap::new();
    let env = Env {
        db: &db,
        formulas: &formulas,
        this: db.file("daily/day/2026-10-01.md").unwrap(),
        today: day(2026, 10, 3),
        now: day(2026, 10, 3).and_hms_opt(0, 0, 0).unwrap(),
        notices: RefCell::default(),
        serial: None,
    };
    let ctx = RowCtx::new(&env, None);
    let f = |s: &str| crate::def::Filter::Expr(parse(s).unwrap());
    let n = crate::engine::narrow(
        &[
            f(r#"file.folder == "daily/day/" + this.file.name && file.hasTag("blog")"#),
            crate::def::Filter::And(vec![
                f(r#"file.inFolder("daily")"#),
                f(r#"note["read"] == true"#),
                f(r#"file.path.startsWith("daily/")"#),
                f(r#"note.x == note.y"#),
            ]),
            crate::def::Filter::Or(vec![f(r#"file.name == "zz""#)]),
        ],
        &ctx,
    )
    .unwrap();
    assert_eq!(n.args.len(), 8, "{}", n.sql);
    let c = db.candidates(&n.sql, &n.args).unwrap();
    let paths: Vec<&str> = c.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["daily/day/2026-10-01/a.md"]);
}

#[test]
fn timing_2000_note_synthetic_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    let spec = cv_testkit::VaultSpec {
        notes: 2000,
        daily_notes: 60,
        ..cv_testkit::VaultSpec::default()
    };
    cv_testkit::SyntheticVault::build(&vault, &spec).unwrap();
    let config = tmp.path().join("config");
    std::fs::create_dir_all(config.join("views")).unwrap();
    std::fs::write(
        config.join("views").join("t.toml"),
        r##"
[formulas]
label = 'note.kind.toString().upper() + " " + file.name'

[[views]]
type = "table"
name = "Simple"
filters = { and = ['file.folder == "daily/2026/02"'] }
order = ["file.name", "note.date"]

[[views]]
type = "table"
name = "Narrowed"
filters = { and = ['file.inFolder("notes")', 'note.kind == "idea"', "note.rating >= 3"] }
order = ["file.name", "note.rating", "formula.label"]
sort = [{ property = "note.rating", direction = "DESC" }]
summaries = { "note.rating" = "Sum" }

[[views]]
type = "table"
name = "Scan"
filters = 'list(note.tags).contains("topic/a") || file.hasTag("kind/ref")'
order = ["file.name", "formula.label"]
"##,
    )
    .unwrap();
    let base = tmp.path().join("idx").join("index.db");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    let _index = cv_index::Index::create(&vault, &base).unwrap();
    let g = cv_index::Reader::open_current(&base).unwrap().generation();
    let mut h = ViewHost::new(&config, &base);
    for id in ["t#Simple", "t#Narrowed", "t#Scan"] {
        let first = h.run(g, &RunRequest::view(id)).unwrap();
        assert!(first.notices.is_empty(), "{:?}", first.notices);
        assert!(first.rows().count() > 0, "{id}");
        let n = 20u32;
        let start = std::time::Instant::now();
        for _ in 0..n {
            let _ = h.run(g, &RunRequest::view(id)).unwrap();
        }
        let per = start.elapsed() / n;
        println!("{id}: {} rows, {per:?} per run", first.rows().count());
        assert!(per.as_secs() < 2);
    }
}

#[path = "tests_trash.rs"]
mod trash;

#[path = "tests_ui_sort.rs"]
mod ui_sort;

#[path = "tests_cells.rs"]
mod cells;

#[path = "tests_picker_props.rs"]
mod picker_props;

#[path = "tests_tracker.rs"]
mod tracker;

#[path = "tests_vectors.rs"]
mod vectors;
