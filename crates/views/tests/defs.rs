//! Golden tests for the shipped view definitions (`crates/views/defs/*.toml`, plan `v0-editor.md`
//! WP-7) on the synthetic corpus in `tests/fixtures/vault/` (invented content only, invariant I1).
//!
//! Regenerate the goldens with `UPDATE_GOLDENS=1 cargo test -p views --test defs`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::NaiveDate;
use serde_json::Value as Json;
use views::result::Cell;
use views::{ViewHost, ViewResult};

const IDS: &[&str] = &[
    "backlinks#Here",
    "blog-story-list#Stories",
    "daily-intake#Intake",
    "daily-reading-list#Reading List",
    "hubs#Hub View A",
    "hubs#Project Alpha",
    "hubs#Hub View B",
    "hubs#Hub View C",
    "ingestion#Possible duplicates",
    "ingestion#Unprocessed clippings",
    "ingestion#Unrefined transcripts",
    "ingestion#X posts",
    "ingestion#YouTube without metadata",
    "initiatives#Active",
    "initiatives#Blocked",
    "initiatives#Paused",
    "monthly-reading#By Day",
    "monthly-reading#By Tag",
    "monthly-reading#Unread",
    "monthly-tasks#Overdue",
    "works#Active",
    "works#Complete",
    "works#Entries",
    "works#Unread",
];

const MONTH: &str = "daily/month/2026-10.md";

/// The embedding note each golden runs with.
fn host_of(id: &str) -> Option<&'static str> {
    match id.split('#').next() {
        Some("backlinks") => Some("stack/Tool One.md"),
        Some("blog-story-list") => Some("core/works/Ann Blog.md"),
        Some("daily-reading-list" | "daily-intake") => Some("daily/day/2026-10-01.md"),
        Some("hubs") => Some("core/projects/project-alpha/Hub.md"),
        Some("monthly-reading" | "monthly-tasks") => Some(MONTH),
        _ => None,
    }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn copy_tree(from: &Path, to: &Path, files: &mut Vec<PathBuf>) {
    std::fs::create_dir_all(to).unwrap();
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_tree(&p, &dest, files);
        } else {
            std::fs::copy(&p, &dest).unwrap();
            files.push(dest);
        }
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    config: PathBuf,
    base: PathBuf,
    state: PathBuf,
    _index: cv_index::Index,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    let mut files = Vec::new();
    copy_tree(
        &crate_dir().join("tests/fixtures/vault"),
        &vault,
        &mut files,
    );
    // Deterministic mtimes: one hour apart in sorted path order.
    files.sort();
    let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    for (i, f) in files.iter().enumerate() {
        let t = epoch + Duration::from_secs(3600 * u64::try_from(i).unwrap());
        std::fs::File::options()
            .write(true)
            .open(f)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
    let config = tmp.path().join("config");
    let mut defs = Vec::new();
    copy_tree(&crate_dir().join("defs"), &config.join("views"), &mut defs);
    let base = tmp.path().join("idx").join("index.db");
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    let index = cv_index::Index::create(&vault, &base).unwrap();
    let state = tmp.path().join("state.db");
    tracker_fixture(&state);
    Fx {
        _tmp: tmp,
        config,
        base,
        state,
        _index: index,
    }
}

/// A synthetic `state.db` `tracker_items` (the columns of `cv-state` `migrate.rs` v5 + v6 + v7 + v8 + v12)
/// for `daily-intake`: on 2026-10-01 a named YouTube line with an hour-plus duration (done), an
/// unnamed blog note with a word count (kind-label fallback, reading minutes), a YouTube line
/// under an hour, a named blog line without a body (blank length) that is paid (Access 🔒); a skipped row, another day.
fn tracker_fixture(p: &Path) {
    let c = rusqlite::Connection::open(p).unwrap();
    c.execute_batch(
        "CREATE TABLE tracker_items (source_kind TEXT NOT NULL, source_key TEXT NOT NULL,
            item_id TEXT NOT NULL, title TEXT NOT NULL DEFAULT '', url TEXT NOT NULL DEFAULT '',
            first_seen_ms INTEGER NOT NULL, day TEXT NOT NULL, outcome TEXT NOT NULL,
            score REAL, full_body INTEGER, note_path TEXT,
            source_name TEXT, duration_s INTEGER, word_count INTEGER, status TEXT,
            access TEXT, complete INTEGER, full_word_count INTEGER,
            PRIMARY KEY (source_kind, source_key, item_id));
         INSERT INTO tracker_items VALUES
            ('youtube', 'UC1', 'v1', 'A video', 'https://y.example/1', 2, '2026-10-01', 'line', NULL, NULL, NULL, 'Chan A', 3725, NULL, 'x', NULL, NULL, NULL),
            ('blog', 'https://b.example/feed', 'p1', 'A post', 'https://b.example/1', 1, '2026-10-01', 'note', NULL, 1, 'core/works/A post.md', NULL, NULL, 1000, '/', 'free', 1, 1000),
            ('youtube', 'UC1', 'v2', 'Skipped', 'https://y.example/2', 3, '2026-10-01', 'skipped', NULL, NULL, NULL, 'Chan A', 30, NULL, NULL, NULL, NULL, NULL),
            ('youtube', 'UC1', 'v3', 'Other day', 'https://y.example/3', 4, '2026-10-02', 'line', NULL, NULL, NULL, 'Chan A', 60, NULL, NULL, NULL, NULL, NULL),
            ('youtube', 'UC1', 'v4', 'Short-ish', 'https://y.example/4', 5, '2026-10-01', 'line', NULL, NULL, NULL, 'Chan A', 65, NULL, NULL, NULL, NULL, NULL),
            ('blog', 'https://c.example/feed', 'p2', 'Teaser', 'https://c.example/2', 6, '2026-10-01', 'line', NULL, 0, NULL, 'Feed C', NULL, NULL, NULL, 'paid', 0, NULL);",
    )
    .unwrap();
}

impl Fx {
    fn host(&self) -> ViewHost {
        ViewHost::new(&self.config, &self.base)
            .with_state_db(&self.state)
            .with_today(NaiveDate::from_ymd_opt(2026, 10, 3).unwrap())
    }
    fn run(&self, host: &mut ViewHost, id: &str, this: Option<&str>) -> ViewResult {
        let g = cv_index::Reader::open_current(&self.base)
            .unwrap()
            .generation();
        host.run_typed(g, id, this).unwrap()
    }
}

/// Date-times (file mtimes) render in the local zone; mask them so goldens are zone-independent.
/// Their order is still asserted by the row order of the mtime-sorted views.
fn mask(v: &mut Json) {
    match v {
        Json::Object(m) => {
            let is_dt = m.get("t").and_then(Json::as_str) == Some("date")
                && m.get("v")
                    .and_then(Json::as_str)
                    .is_some_and(|s| s.contains('T'));
            if is_dt {
                m.insert("v".into(), Json::String("<datetime>".into()));
            }
            m.values_mut().for_each(mask);
        }
        Json::Array(a) => a.iter_mut().for_each(mask),
        _ => {}
    }
}

#[test]
fn defs_load_without_notices_and_yield_the_24_ids() {
    let fx = fixture();
    let host = fx.host();
    assert!(host.load_notices().is_empty(), "{:?}", host.load_notices());
    let mut ids = host.view_ids();
    ids.sort();
    assert_eq!(ids, IDS);
}

#[test]
fn goldens() {
    let fx = fixture();
    let mut host = fx.host();
    inject_sources(&mut host);
    let update = std::env::var_os("UPDATE_GOLDENS").is_some();
    let dir = crate_dir().join("tests/fixtures/golden");
    let mut failures = Vec::new();
    for id in IDS {
        let r = fx.run(&mut host, id, host_of(id));
        assert!(r.notices.is_empty(), "{id}: {:?}", r.notices);
        let mut got: Json = serde_json::to_value(&r).unwrap();
        mask(&mut got);
        // Round-trip through text so `got` sees the same (lossy) float parse as the golden file.
        let got: Json = serde_json::from_str(&serde_json::to_string(&got).unwrap()).unwrap();
        let path = dir.join(format!("{id}.json"));
        if update {
            let mut s = serde_json::to_string_pretty(&got).unwrap();
            s.push('\n');
            std::fs::write(&path, s).unwrap();
            continue;
        }
        let want: Json = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if got != want {
            failures.push(format!(
                "{id}:\n{}",
                serde_json::to_string_pretty(&got).unwrap()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatch:\n{}",
        failures.join("\n")
    );
}

fn text(c: &Cell) -> String {
    match c {
        Cell::Null | Cell::Unknown => String::new(),
        Cell::Bool { v } => v.to_string(),
        Cell::Number { v } => v.to_string(),
        Cell::Text { v } | Cell::Date { v } => v.clone(),
        Cell::Link { label, .. } | Cell::Url { label, .. } => label.clone(),
        Cell::List { v } => v.iter().map(text).collect::<Vec<_>>().join(","),
    }
}

fn col(r: &ViewResult, label: &str) -> usize {
    r.columns
        .iter()
        .position(|c| c.label == label)
        .unwrap_or_else(|| panic!("no column {label}: {:?}", r.columns))
}

/// `(first cell, cell under `label`)` per row.
fn column(r: &ViewResult, label: &str) -> Vec<(String, String)> {
    let i = col(r, label);
    r.rows()
        .map(|row| (text(&row.cells[0]), text(&row.cells[i])))
        .collect()
}

#[test]
fn by_day_pivot_total_and_average() {
    let fx = fixture();
    let mut host = fx.host();
    let r = fx.run(&mut host, "monthly-reading#By Day", Some(MONTH));
    let days: Vec<String> = r.rows().map(|row| text(&row.cells[0])).collect();
    // `Day Rollup` (tagged daily) contributes nothing; September is out of the month.
    assert_eq!(days, ["2026-10-01", "2026-10-02", "2026-10-03"]);
    let total = r.summaries.iter().find(|s| s.label == "Total").unwrap();
    let avg = r
        .summaries
        .iter()
        .find(|s| s.label == "Average/day")
        .unwrap();
    let t = col(&r, "Total");
    // 400 + 1000 + 600 + 300 + 150 + 250 = 2700 words at 200 wpm; 3 days with entries.
    assert_eq!(text(&total.cells[t]), "2700 / 13.5m");
    assert_eq!(text(&avg.cells[t]), "900 / 4.5m");
    // Blog: 400 + 600 + 250 over the 3 days with entries (not over the days with blog entries).
    let b = col(&r, "Blog");
    // 6.25 → "6.2": Rust's `toFixed` rounds half-to-even where JS printed "6.3".
    assert_eq!(text(&total.cells[b]), "1250 / 6.2m");
    assert_eq!(text(&avg.cells[b]), "417 / 2.1m");
    // First match wins: fanfic+transcript counts as Transcript.
    let tr = col(&r, "Transcript");
    let f = col(&r, "Fanfic");
    assert_eq!(text(&total.cells[tr]), "300 / 1.5m");
    assert!(matches!(total.cells[f], Cell::Null) || text(&total.cells[f]) == "—");
}

#[test]
fn overdue_excludes_today_future_blank_and_other_months() {
    let fx = fixture();
    let mut host = fx.host();
    let r = fx.run(&mut host, "monthly-tasks#Overdue", Some(MONTH));
    let tasks: Vec<String> = r.rows().map(|row| text(&row.cells[0])).collect();
    assert_eq!(tasks, ["loose task", "open one", "yesterday task"]);
    let groups: Vec<String> = r
        .groups
        .iter()
        .map(|g| g.key.as_ref().map(text).unwrap_or_default())
        .collect();
    assert_eq!(groups, ["General", "Morning", "Work"]);
    let first = r.rows().next().unwrap();
    assert_eq!(first.path.as_deref(), Some("daily/day/2026-10-01.md"));
    assert_eq!(first.line, Some(4));
}

/// Synthetic `source = "sources"` rows and `file.serial` set (plan round-11-feedback-todos
/// Decision 5): in the daemon these come from `sources.toml` + the shared serial matcher
/// (`crates/cv/src/views.rs`); here they are injected as given.
fn inject_sources(host: &mut ViewHost) {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use views::value::Value;
    let s = |v: &str| Value::Str(v.into());
    let row = |row: &str,
               name: &str,
               author: Option<&str>,
               status: &str,
               entries: f64,
               unread: f64,
               last: Option<NaiveDate>| {
        let mut m = BTreeMap::new();
        m.insert("row".to_owned(), s(row));
        m.insert("name".to_owned(), s(name));
        m.insert("publication".to_owned(), s(name));
        m.insert("author".to_owned(), author.map_or(Value::Null, s));
        m.insert(
            "prefix".to_owned(),
            Value::list(vec![s("https://blog.example/")]),
        );
        m.insert("kind".to_owned(), s("fanfic"));
        m.insert("status".to_owned(), s(status));
        m.insert("entries".to_owned(), Value::Num(entries));
        m.insert("unread".to_owned(), Value::Num(unread));
        m.insert(
            "last_update".to_owned(),
            last.map_or(Value::Null, Value::date),
        );
        Arc::new(m)
    };
    let d = |m, d| NaiveDate::from_ymd_opt(2026, m, d);
    let records = vec![
        row(
            "serial",
            "Ann Blog",
            Some("Ann"),
            "active",
            2.0,
            1.0,
            d(10, 2),
        ),
        row("serial", "Cy Story", Some("Cy"), "active", 0.0, 0.0, None),
        row("serial", "Bob News", None, "complete", 2.0, 2.0, d(10, 1)),
        row(
            "serial",
            "Gone Work",
            Some("Gus"),
            "ignored",
            1.0,
            1.0,
            d(9, 30),
        ),
        row("blog", "Feed Row", None, "active", 1.0, 0.0, d(10, 3)),
    ];
    let serial: BTreeMap<String, String> = [
        ("daily/day/2026-10-02/Beta Post.md", "Ann Blog"),
        ("daily/day/2026-10-01/Alpha Post.md", "Ann Blog"),
        ("daily/day/2026-09-30/Old News.md", "Bob News"),
        ("core/works/Ann Blog.md", "Ann Blog"),
    ]
    .into_iter()
    .map(|(p, n)| (p.to_owned(), n.to_owned()))
    .collect();
    host.set_sources(records, serial);
}

#[test]
fn works_views_read_injected_sources_rows() {
    let fx = fixture();
    let mut host = fx.host();
    inject_sources(&mut host);
    let r = fx.run(&mut host, "works#Active", None);
    assert_eq!(
        column(&r, "Entries"),
        [
            ("Ann Blog".into(), "2".into()),
            ("Cy Story".into(), "·".into()),
            ("Feed Row".into(), "1".into()),
        ]
    );
    assert_eq!(
        column(&r, "Unread"),
        [
            ("Ann Blog".into(), "1".into()),
            ("Cy Story".into(), "·".into()),
            ("Feed Row".into(), "·".into()),
        ]
    );
    assert_eq!(
        column(&r, "Last Update"),
        [
            ("Ann Blog".into(), "2026-10-02".into()),
            ("Cy Story".into(), String::new()),
            ("Feed Row".into(), "2026-10-03".into()),
        ]
    );
    // The Author column is the bare name (FEEDBACK:13); `ignored` rows are in neither view.
    assert_eq!(column(&r, "Author")[0], ("Ann Blog".into(), "Ann".into()));
    let r = fx.run(&mut host, "works#Complete", None);
    assert_eq!(column(&r, "Entries"), [("Bob News".into(), "2".into())]);
    // A record `status` column bound as `note.status` is plain text, not a task-status cell.
    assert!(
        r.columns.iter().all(|c| c.role.is_none()),
        "{:?}",
        r.columns
    );
}

#[test]
fn works_unread_uses_the_injected_serial_match_set() {
    let fx = fixture();
    let mut host = fx.host();
    // Without match data nothing matches.
    assert_eq!(fx.run(&mut host, "works#Unread", None).rows().count(), 0);
    inject_sources(&mut host);
    let r = fx.run(&mut host, "works#Unread", None);
    let names: Vec<String> = r.rows().map(|row| text(&row.cells[0])).collect();
    // Alpha Post is read; the Work note itself is outside daily/day.
    assert_eq!(names, ["Old News", "Beta Post"]);
}

#[test]
fn initiative_task_counts_and_missing_files() {
    let fx = fixture();
    let mut host = fx.host();
    let r = fx.run(&mut host, "initiatives#Active", None);
    assert_eq!(
        column(&r, "Blocked on User"),
        [("Alpha".into(), "2".into()), ("delta".into(), "·".into())]
    );
    assert_eq!(
        column(&r, "Tasks"),
        [("Alpha".into(), "2".into()), ("delta".into(), "·".into())]
    );
    assert_eq!(
        column(&r, "feedback"),
        [
            ("Alpha".into(), "FEEDBACK".into()),
            ("delta".into(), "~~FEEDBACK~~".into())
        ]
    );
    let r = fx.run(&mut host, "initiatives#Blocked", None);
    assert_eq!(column(&r, "Blocked on User"), [("beta".into(), "1".into())]);
    assert_eq!(column(&r, "readme"), [("beta".into(), "~~README~~".into())]);
    let r = fx.run(&mut host, "initiatives#Paused", None);
    assert_eq!(column(&r, "Tasks"), [("Gamma".into(), "·".into())]);
}

#[test]
fn backlinks_here() {
    let fx = fixture();
    let mut host = fx.host();
    let r = fx.run(&mut host, "backlinks#Here", Some("stack/Tool One.md"));
    let names: Vec<String> = r.rows().map(|row| text(&row.cells[0])).collect();
    // mtime DESC: stack/ sorts after notes/ so gets the later mtime.
    assert_eq!(names, ["Tool Two", "Linker"]);
}
