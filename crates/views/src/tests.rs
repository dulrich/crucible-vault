//! End-to-end tests on hand-written synthetic fixture notes (invariant I1: no live vault).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

use crate::db::Db;
use crate::eval::{Env, RowCtx};
use crate::expr::parse;
use crate::result::{Cell, Kind, ViewResult};
use crate::service::ViewHost;
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
        host.run_typed(g, id, this).unwrap()
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

#[test]
fn function_and_field_vectors() {
    let fx = fixture(&[]);
    let cases: &[(Option<&str>, Option<&str>, &str, &str)] = &[
        (A, None, "file.name", "a"),
        (A, None, "file.path", "daily/day/2026-10-01/a.md"),
        (A, None, "file.folder", "daily/day/2026-10-01"),
        (A, None, "file.ext", "md"),
        (A, None, "file.size > 0", "true"),
        (A, None, "file.mtime > date(\"2000-01-01\")", "true"),
        (A, None, "file.ctime > date(\"2000-01-01\")", "true"),
        (A, None, "file.tags.join(\" \")", "#blog #clippings"),
        (Some("notes/x.md"), None, "file.links.length", "2"),
        (
            Some("notes/x.md"),
            None,
            "file.links[0].path",
            "stack/tool1.md",
        ),
        (
            Some("stack/tool1.md"),
            None,
            "file.backlinks.map(value.path).join(\",\")",
            "notes/x.md,stack/tool2.md",
        ),
        (
            Some("notes/x.md"),
            None,
            "file.hasLink(\"stack/tool1.md\")",
            "true",
        ),
        (
            Some("notes/x.md"),
            Some("stack/tool1.md"),
            "file.hasLink(this.file)",
            "true",
        ),
        (
            A,
            Some("daily/day/2026-10-01.md"),
            "this.file.name",
            "2026-10-01",
        ),
        (
            A,
            Some("daily/day/2026-10-01.md"),
            "file.folder == \"daily/day/\" + this.file.name",
            "true",
        ),
        (
            A,
            Some("core/works/Ann Blog.md"),
            "this[\"author-filter\"]",
            "Ann",
        ),
        (
            A,
            Some("core/works/Ann Blog.md"),
            "this.file.properties[\"work-status\"]",
            "active",
        ),
        (A, None, "note.read", "true"),
        (A, None, "note[\"word-count\"] + 1", "401"),
        (A, None, "read", "true"),
        (A, None, "formula.minutes", "2"),
        (A, None, "formula.nope", ""),
        (A, None, "formula.loop", ""),
        (
            A,
            None,
            "file(\"_custom/Settings.md\").properties.reading_wpm",
            "200",
        ),
        (
            A,
            None,
            "file(\"_custom/Settings\").path",
            "_custom/Settings.md",
        ),
        (A, None, "if(file(\"nope.md\"), 1, 2)", "2"),
        (A, None, "if(note.read == true, \"y\", \"n\")", "y"),
        (A, None, "if(false, 1)", ""),
        (A, None, "list(note.tags).length", "2"),
        (A, None, "list(note.author).length", "1"),
        (A, None, "list(note.nothing).length", "0"),
        (A, None, "note.author.toString()", "Ann"),
        (A, None, "\"  x \".trim()", "x"),
        (A, None, "\"#a#b\".replace(\"#\", \"\")", "ab"),
        (
            A,
            None,
            "list(note.tags).map(value.toString().upper()).join(\"/\")",
            "BLOG/CLIPPINGS",
        ),
        (
            A,
            None,
            "list(note.tags).filter(value != \"clippings\")",
            "blog",
        ),
        (A, None, "list(note.tags).contains(\"blog\")", "true"),
        (A, None, "\"abc\".contains(\"bc\")", "true"),
        (A, None, "file.path.startsWith(\"daily/\")", "true"),
        (A, None, "file.name.endsWith(\"a\")", "true"),
        (A, None, "number(\"2.5\") * 2", "5"),
        (A, None, "number(note.missing)", "0"),
        (A, None, "(10 / 3).round(2)", "3.33"),
        (A, None, "(2.5).round()", "3"),
        (A, None, "(1 / 3).toFixed(1)", "0.3"),
        (A, None, "[1, 2, 3].join(\"-\")", "1-2-3"),
        (A, None, "file.inFolder(\"daily/day\")", "true"),
        (A, None, "file.inFolder(\"daily/da\")", "false"),
        (A, None, "file.hasTag(\"blog\")", "true"),
        (A, None, "file.hasTag(\"#BLOG\", \"zzz\")", "true"),
        (A, None, "file.hasTag(\"blo\")", "false"),
        (
            Some("daily/day/2026-10-03/e.md"),
            None,
            "file.hasTag(\"using\")",
            "true",
        ),
        (A, None, "link(file.path, \"L\")", "L"),
        (A, None, "link(\"x/y.md\")", "y"),
        (A, None, "link(\"x/y.md\").path", "x/y.md"),
        (A, None, "file.asLink().path", "daily/day/2026-10-01/a.md"),
        (A, None, "\"a/b/c\".split(\"/\")[1]", "b"),
        (A, None, "\"a/b/c\".split(\"/\")[-1]", "c"),
        (A, None, "\"a/b/c\".split(\"/\").length", "3"),
        (A, None, "\"ABC\".lower()", "abc"),
        (A, None, "\"abc\".length", "3"),
        (A, None, "today()", "2026-10-03"),
        (A, None, "today().format(\"YYYY-MM\")", "2026-10"),
        (A, None, "note.created", "2026-10-01"),
        (A, None, "note.created.format(\"DD/MM/YYYY\")", "01/10/2026"),
        (A, None, "note.created < today()", "true"),
        (A, None, "\"2026-10-02\" < today()", "true"),
        (A, None, "date(\"2026-10-03\") < today()", "false"),
        (A, None, "note.created == \"2026-10-01\"", "true"),
        (A, None, "today().year", "2026"),
        (A, None, "files(\"core/works\").length", "2"),
        (
            A,
            None,
            "files(\"core/works\").map(value.name).join(\",\")",
            "Ann Blog,Empty",
        ),
        (
            A,
            None,
            "files(\"core/works\").filter(value.properties[\"author-filter\"]).length",
            "1",
        ),
        (
            A,
            None,
            "files(\"core/works\").filter(list(value.properties[\"source-prefix-filter\"]).filter(note.source.startsWith(value)).length > 0).map(value.name).join(\",\")",
            "Ann Blog",
        ),
        (
            A,
            None,
            "list(\"x\").map(list(\"y\").map(outer + value).join(\"\")).join(\"\")",
            "xy",
        ),
        (
            Some("initiatives/alpha/INITIATIVE.md"),
            None,
            "file.tasks.filter(value.status == \" \" && value.heading == \"Blocked on User\").length",
            "1",
        ),
        (
            Some("initiatives/alpha/INITIATIVE.md"),
            None,
            "list(note[\"pending-plans\"]).filter(value.toString().trim() != \"\").length",
            "1",
        ),
        (A, None, "!true || 1 == 1 && 2 != 3", "true"),
        (A, None, "-2 + 5 % 3 + 1 >= 0", "true"),
        (A, None, "\"n\" + 1", "n1"),
        (A, None, "null", ""),
    ];
    for (row, this, src, want) in cases {
        assert_eq!(&t(&fx, *row, *this, src), want, "{src}");
    }
    assert!(matches!(
        ev(&fx, A, None, "\"\".isEmpty()"),
        Value::Bool(true)
    ));
    assert!(matches!(ev(&fx, A, None, "now()"), Value::Date(_, false)));
    assert!(matches!(ev(&fx, None, None, "file.name"), Value::Null));
}

#[test]
fn every_base_vector_evaluates_without_panic() {
    let fx = fixture(&[]);
    for v in crate::expr::tests_support::BASE_VECTORS {
        for row in [A, Some("initiatives/alpha/INITIATIVE.md"), None] {
            let _ = ev(&fx, row, Some("core/works/Ann Blog.md"), v);
        }
    }
    // The real initiatives formula on a fixture initiative.
    assert_eq!(
        t(
            &fx,
            Some("initiatives/alpha/INITIATIVE.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[16]
        ),
        "Alpha"
    );
    assert_eq!(
        t(
            &fx,
            Some("initiatives/beta/INITIATIVE.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[16]
        ),
        "beta"
    );
    assert_eq!(
        t(
            &fx,
            Some("initiatives/beta/INITIATIVE.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[20]
        ),
        "blocked"
    );
    assert_eq!(
        t(
            &fx,
            Some("initiatives/alpha/INITIATIVE.md"),
            None,
            crate::expr::tests_support::BASE_VECTORS[18]
        ),
        "README"
    );
    // The blog-story-list author/prefix filter with `this` = a work.
    let w = Some("core/works/Ann Blog.md");
    let f = crate::expr::tests_support::BASE_VECTORS[6];
    assert_eq!(t(&fx, A, w, f), "true");
    assert_eq!(t(&fx, Some("daily/day/2026-10-02/c.md"), w, f), "true");
    assert_eq!(t(&fx, Some("daily/day/2026-10-01/b.md"), w, f), "false");
    assert_eq!(t(&fx, A, Some("core/works/Empty.md"), f), "false");
}

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
    let p = h.run(&fx.reader(), "month#Month", None).unwrap();
    assert_eq!(p.0["kind"], "table");
    assert_eq!(p.0["groups"][0]["key"]["t"], "text");
    assert_eq!(p.0["groups"][0]["rows"][0]["cells"][0]["t"], "link");
    assert!(h.run(&fx.reader(), "nope#x", None).is_err());
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
    let p = h.run(&reader, "hubs#Stack", None).unwrap();
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
    assert!(h.run(&reader, "hubs#Stack", None).is_ok());

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
        let first = h.run_typed(g, id, None).unwrap();
        assert!(first.notices.is_empty(), "{:?}", first.notices);
        assert!(first.rows().count() > 0, "{id}");
        let n = 20u32;
        let start = std::time::Instant::now();
        for _ in 0..n {
            let _ = h.run_typed(g, id, None).unwrap();
        }
        let per = start.elapsed() / n;
        println!("{id}: {} rows, {per:?} per run", first.rows().count());
        assert!(per.as_secs() < 2);
    }
}

/// Plan round-5-reading-pickers-intake WP-10: a fixture `state.db` with `tracker_items` (the
/// columns of `cv-state` `migrate.rs`; this crate cannot depend on `cv-state`).
fn tracker_db(dir: &Path) -> PathBuf {
    let p = dir.join("state.db");
    let c = rusqlite::Connection::open(&p).unwrap();
    c.execute_batch(
        "CREATE TABLE tracker_items (source_kind TEXT NOT NULL, source_key TEXT NOT NULL,
            item_id TEXT NOT NULL, title TEXT NOT NULL DEFAULT '', url TEXT NOT NULL DEFAULT '',
            first_seen_ms INTEGER NOT NULL, day TEXT NOT NULL, outcome TEXT NOT NULL,
            score REAL, full_body INTEGER, note_path TEXT,
            source_name TEXT, duration_s INTEGER, word_count INTEGER, status TEXT,
            access TEXT, complete INTEGER, full_word_count INTEGER,
            PRIMARY KEY (source_kind, source_key, item_id));
         INSERT INTO tracker_items VALUES
            ('youtube', 'UC1', 'v1', 'Old video', 'https://y/1', 1, '2026-10-02', 'line', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
            ('blog', 'b', 'p1', 'Post', 'https://b/1', 3, '2026-10-03', 'note', 0.5, 1, 'notes/Post.md', 'Feed B', NULL, 900, NULL, 'paid', 0, 9000),
            ('youtube', 'UC1', 'v2', 'New video', 'https://y/2', 2, '2026-10-03', 'line', 2.0, NULL, NULL, 'Chan', 125, NULL, 'x', NULL, NULL, NULL),
            ('youtube', 'UC1', 'v3', 'Skipped', 'https://y/3', 4, '2026-10-03', 'skipped', NULL, NULL, NULL, NULL, NULL, NULL, '/', NULL, NULL, NULL);",
    )
    .unwrap();
    p
}

const INTAKE: &str = "source = 'tracker_items'\n[formulas]\nsrc = 'source_kind + \"/\" + source_key'\n[[views]]\ntype='table'\nname='Day'\nfilters = { and = ['day == this.file.name', 'outcome == \"line\" || note.outcome == \"note\"'] }\norder = ['title', 'url', 'formula.src', 'score', 'full_body', 'item_id']\n";

#[test]
fn tracker_items_view_filters_by_this_day() {
    let fx = fixture(&[("intake.toml", INTAKE)]);
    let state = tracker_db(fx._tmp.path());
    let mut host = fx.host().with_state_db(&state);
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-03.md"));
    assert_eq!(r.kind, Kind::Table);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(
        grid(&r),
        vec![
            vec!["Post", "https://b/1", "blog/b", "0.5", "true", "p1"],
            vec!["New video", "https://y/2", "youtube/UC1", "2", "", "v2"],
        ]
    );
    assert!(r.rows().all(|row| row.path.is_none()));
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-02.md"));
    assert_eq!(grid(&r).len(), 1);
    // No `this`: `day == null` matches nothing.
    assert!(grid(&fx.run(&mut host, "intake#Day", None)).is_empty());
}

#[test]
fn tracker_records_expose_done_note_path_key_and_url_cells() {
    const DONE: &str = "source = 'tracker_items'\n[formulas]\nt = 'if(outcome == \"note\" && note_path, link(note_path, title), url(url, title))'\n[[views]]\ntype='table'\nname='Day'\nfilters = { and = ['day == this.file.name'] }\norder = ['formula.t', 'done', 'note_path', 'status']\n";
    let fx = fixture(&[("intake.toml", DONE)]);
    let state = tracker_db(fx._tmp.path());
    let mut host = fx.host().with_state_db(&state);
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-03.md"));
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    let rows: Vec<_> = r.rows().collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[1].cells,
        vec![
            Cell::Link {
                path: "notes/Post.md".into(),
                label: "Post".into()
            },
            Cell::Bool { v: false },
            Cell::Text {
                v: "notes/Post.md".into()
            },
            Cell::Null,
        ]
    );
    assert_eq!(rows[2].cells[3], Cell::Text { v: "x".into() });
    assert_eq!(
        rows[2].cells[..2],
        [
            Cell::Url {
                url: "https://y/2".into(),
                label: "New video".into()
            },
            Cell::Bool { v: true },
        ]
    );
    assert_eq!(rows[2].cells[2], Cell::Null);
    let key = crate::result::TrackerKey::decode(rows[2].key.as_deref().unwrap()).unwrap();
    assert_eq!(
        (
            key.source_kind.as_str(),
            key.source_key.as_str(),
            key.item_id.as_str()
        ),
        ("youtube", "UC1", "v2")
    );
    assert_eq!(crate::result::TrackerKey::decode(&key.encode()), Some(key));
    assert_eq!(crate::result::TrackerKey::decode("a.md"), None);
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["groups"][0]["rows"][2]["cells"][0]["t"], "url");
}

/// A record source's `status` column carries
/// `ColumnRole::Status` (nothing else does); it round-trips the wire and an index view's columns
/// carry no role.
#[test]
fn record_status_column_carries_the_status_role() {
    use crate::result::{ColumnRole, ViewResult};
    const DONE: &str = "source = 'tracker_items'\n[[views]]\ntype='table'\nname='Day'\nfilters = { and = ['day == this.file.name'] }\norder = ['title', 'done', 'status']\n";
    let fx = fixture(&[("intake.toml", DONE)]);
    let state = tracker_db(fx._tmp.path());
    let mut host = fx.host().with_state_db(&state);
    let r = fx.run(&mut host, "intake#Day", Some("daily/day/2026-10-03.md"));
    let roles: Vec<_> = r.columns.iter().map(|c| c.role.clone()).collect();
    assert_eq!(roles, [None, None, Some(ColumnRole::Status)]);
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(
        json["columns"][2]["role"],
        serde_json::json!({"kind": "status"})
    );
    assert!(json["columns"][0].get("role").is_none(), "no role = absent");
    let back: ViewResult = serde_json::from_value(json).unwrap();
    assert_eq!(back.columns, r.columns);
}

#[test]
fn tracker_records_expose_source_name_duration_and_word_count() {
    use crate::value::Value;
    let dir = tempfile::tempdir().unwrap();
    let c = rusqlite::Connection::open(tracker_db(dir.path())).unwrap();
    let recs = crate::service::tracker_records(&c).unwrap();
    let show = |v: &Value| match v {
        Value::Null => "null".to_owned(),
        Value::Str(s) => format!("s:{s}"),
        Value::Num(n) => format!("n:{n}"),
        _ => "other".to_owned(),
    };
    let get = |id: &str| {
        let r = recs
            .iter()
            .find(|r| matches!(&r["item_id"], Value::Str(s) if s == id))
            .unwrap();
        [
            show(&r["source_name"]),
            show(&r["duration"]),
            show(&r["word_count"]),
        ]
    };
    assert_eq!(get("v2"), ["s:Chan", "n:125", "null"]);
    assert_eq!(get("p1"), ["s:Feed B", "null", "n:900"]);
    assert_eq!(get("v1"), ["null", "null", "null"]);
    let p1 = recs
        .iter()
        .find(|r| matches!(&r["item_id"], Value::Str(s) if s == "p1"))
        .unwrap();
    assert_eq!(show(&p1["access"]), "s:paid");
    assert!(matches!(p1["complete"], Value::Bool(false)));
    assert_eq!(show(&p1["full_word_count"]), "n:9000");
    let v1 = recs
        .iter()
        .find(|r| matches!(&r["item_id"], Value::Str(s) if s == "v1"))
        .unwrap();
    assert_eq!(
        [
            show(&v1["access"]),
            show(&v1["complete"]),
            show(&v1["full_word_count"])
        ],
        ["null", "null", "null"]
    );
}

#[test]
fn tracker_items_view_needs_a_state_db() {
    let fx = fixture(&[("intake.toml", INTAKE)]);
    let mut host = fx.host();
    let g = fx.reader().generation();
    let e = host.run_typed(g, "intake#Day", None).unwrap_err();
    assert!(e.to_string().contains("no state.db"), "{e}");
}

#[test]
fn state_change_invalidates_only_tracker_views() {
    let fx = fixture(&[
        ("intake.toml", INTAKE),
        (
            "notes.toml",
            "[[views]]\ntype='table'\nname='All'\norder=['file.name']\n",
        ),
    ]);
    let mut host = fx.host();
    assert_eq!(host.on_state_changed("tracker_items"), vec!["intake#Day"]);
    assert!(host.on_state_changed("link_records").is_empty());
    assert_eq!(host.on_index_generation(99), vec!["notes#All"]);
}

/// Plan round-8 WP-3: `checkbox = true` reaches the result `Column` on `note.*` ids only;
/// the default is false.
#[test]
fn checkbox_property_reaches_the_column() {
    let def = "[properties.read]\ncheckbox = true\n\n[properties.\"file.name\"]\ncheckbox = true\n\n[[views]]\ntype = \"table\"\nname = \"T\"\norder = [\"file.name\", \"note.read\", \"word-count\"]\n";
    let fx = fixture(&[("cb.toml", def)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let cols = fx.run(&mut h, "cb#T", None).columns;
    let flags: Vec<bool> = cols
        .iter()
        .map(|c| matches!(c.role, Some(crate::result::ColumnRole::Checkbox { .. })))
        .collect();
    assert_eq!(flags, [false, true, false]);
    assert_eq!(
        cols[1].role,
        Some(crate::result::ColumnRole::Checkbox { key: "read".into() })
    );
}

/// Plan intake-round-2 WP-4: `columnSize` travels def → compiled view → result JSON → decode;
/// non-positive sizes are ignored and an old payload without `width` still decodes.
#[test]
fn column_size_reaches_the_result() {
    let def = "[[views]]\ntype = \"table\"\nname = \"Sized\"\norder = [\"file.name\", \"word-count\", \"note.author\"]\ncolumnSize = { \"note.word-count\" = 90, \"author\" = 0 }\n";
    let fx = fixture(&[("cs.toml", def)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let r = fx.run(&mut h, "cs#Sized", None);
    let w: Vec<Option<f32>> = r.columns.iter().map(|c| c.width).collect();
    assert_eq!(w, vec![None, Some(90.0), None]);
    let j = serde_json::to_value(&r).unwrap();
    assert!(
        j["columns"][0].get("width").is_none(),
        "unsized omits width"
    );
    let back: ViewResult = serde_json::from_value(j).unwrap();
    assert_eq!(back.columns[1].width, Some(90.0));
    let old =
        serde_json::json!({"kind": "table", "columns": [{"key": "a", "label": "A"}], "groups": []});
    let old: ViewResult = serde_json::from_value(old).unwrap();
    assert_eq!(old.columns[0].width, None);
}

/// Plan round-10 WP-4: filtered summary rows (totals, empty match → nulls, per group), and the
/// bracket id form keying a display name.
#[test]
fn filtered_summary_rows_and_bracket_ids() {
    let def = r#"
[properties.'note["word-count"]']
displayName = "Words"

[[views]]
type = "table"
name = "T"
filters = 'file.inFolder("daily/day/2026-10-01") || file.inFolder("daily/day/2026-10-02")'
order = ["file.name", "word-count", "note.read"]
sort = [{ property = "file.name", direction = "ASC" }]
summaries = { "word-count" = "Sum" }
columnSize = { 'note["word-count"]' = 90 }

[[views.summary_rows]]
label = "Unread"
filter = 'note.read == false'
summaries = { "word-count" = "Sum", 'note["read"]' = "Count" }

[[views.summary_rows]]
label = "None"
filter = 'note.read == "never"'
summaries = { "word-count" = "Sum" }
"#;
    let fx = fixture(&[("sr.toml", def)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let r = fx.run(&mut h, "sr#T", None);
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(r.columns[1].label, "Words");
    assert_eq!(r.columns[1].width, Some(90.0));
    let labels: Vec<&str> = r.summaries.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["Sum", "Unread", "None"]);
    let cells = |i: usize| r.summaries[i].cells.iter().map(text).collect::<Vec<_>>();
    assert_eq!(cells(0), ["", "2200", ""]);
    // b (1000) and c (600) are `read: false`; a is true and d has no `read`.
    assert_eq!(cells(1), ["", "1600", "2"]);
    // No match: null cells, not the plain footer's zero.
    assert_eq!(cells(2), ["", "", ""]);
    assert!(matches!(r.summaries[2].cells[1], Cell::Null));
}

/// Plan round-10 WP-4: the migration-0006 output parses and its `Unread` row totals the
/// not-read rows' words and minutes.
#[test]
fn migrated_reading_list_has_an_unread_row() {
    let migrated = include_str!(
        "../../../deploy/migrations/fixtures/0006-expected/views/daily-reading-list.toml"
    );
    let fx = fixture(&[("daily-reading-list.toml", migrated)]);
    let mut h = fx.host();
    assert!(h.load_notices().is_empty(), "{:?}", h.load_notices());
    let r = fx.run(
        &mut h,
        "daily-reading-list#Reading List",
        Some("daily/day/2026-10-01.md"),
    );
    assert!(r.notices.is_empty(), "{:?}", r.notices);
    assert_eq!(
        r.columns
            .iter()
            .find(|c| c.key == "word-count")
            .unwrap()
            .label,
        "Words"
    );
    let unread = r
        .summaries
        .iter()
        .find(|s| s.label == "Unread")
        .expect("Unread row");
    let cells: Vec<String> = unread.cells.iter().map(text).collect();
    assert_eq!(cells, ["", "", "", "1000", "5", ""]);
}

/// WP-R6: the vocabulary the editor's view pickers list — `file.*` ids (each resolves to a
/// non-null value on a real file), the aggregates in display order, the canonical id rule.
#[test]
fn exported_picker_vocabulary() {
    let fx = fixture(&[]);
    for id in crate::FILE_PROPERTY_IDS {
        assert!(!matches!(ev(&fx, A, None, id), Value::Null), "{id}");
    }
    let labels: Vec<&str> = crate::SumFn::ALL.iter().map(|f| f.label()).collect();
    assert_eq!(labels, ["Sum", "Average", "Count", "Min", "Max"]);
    let c = crate::canonical_property_id;
    assert_eq!(c("word-count"), "note.word-count");
    assert_eq!(c("note[\"rating\"]"), "note.rating");
    assert_eq!(c("formula['age']"), "formula.age");
    assert_eq!(c("file.name"), "file.name");
}

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
    let mut run =
        |id: &str, s: Option<cv_protocol::ViewSort>| h.run_sorted(g, id, None, s.as_ref()).unwrap();
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
    let all = h.run_sorted(g, "us#Sel", None, None).unwrap();
    assert_eq!(all.select_by.as_deref(), Some("file.name"));
    let first = names(&all)[0].clone();
    assert!(names(&all).len() > 1);
    let count = |r: &ViewResult| text(&r.summaries[0].cells[0]);
    assert_eq!(count(&all), names(&all).len().to_string());
    let sel = cv_protocol::ViewSelect {
        property: "file.name".into(),
        value: first.clone(),
    };
    let one = h.run_selected(g, "us#Sel", None, None, Some(&sel)).unwrap();
    assert_eq!(names(&one), [first]);
    assert_eq!(count(&one), "1");
    let none = cv_protocol::ViewSelect {
        property: "file.name".into(),
        value: "zzz-none".into(),
    };
    assert!(
        names(
            &h.run_selected(g, "us#Sel", None, None, Some(&none))
                .unwrap()
        )
        .is_empty()
    );
}
