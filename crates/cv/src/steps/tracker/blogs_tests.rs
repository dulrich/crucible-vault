//! WP-8 `track_blogs` tests: a synthetic temp vault, a temp `state.db` and a local feed server
//! (never the network, never the live vault, never env vars).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::TrackerOutcome;
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::{Value, json};

use super::super::hook;
use super::KIND;
use crate::steps::testkit::StepEnv;
use crate::steps::{StepError, StepOutcome};
use crate::steps::{execute, validate};

/// Path → (status, body); swappable between runs.
type Routes = Arc<Mutex<Vec<(&'static str, u16, String)>>>;

struct World {
    env: StepEnv,
    rt: tokio::runtime::Runtime,
    vault: PathBuf,
    base: String,
    routes: Routes,
    _http: MockHttp,
}

impl std::ops::Deref for World {
    type Target = StepEnv;
    fn deref(&self) -> &StepEnv {
        &self.env
    }
}

const CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n";
const KINDS: &str =
    "[kinds.blog]\nlabel = \"Blog\"\ntags = [\"blog\"]\nfolder = \"{period.day}\"\n";

/// One RSS item: (slug, full body?).
fn rss(items: &[(&str, bool)]) -> String {
    let body: String = items
        .iter()
        .map(|(s, full)| {
            let content = if *full {
                format!("<content:encoded><![CDATA[<p>Body of {s} here</p>]]></content:encoded>")
            } else {
                String::new()
            };
            format!(
                "<item><title>Post {s}</title><link>{}/p/{s}</link><dc:creator>Ann</dc:creator><pubDate>Tue, 14 Oct 2026 10:00:00 +0000</pubDate><description>About {s}</description>{content}</item>",
                base()
            )
        })
        .collect();
    format!("<rss><channel><title>B</title>{body}</channel></rss>")
}

fn post_url(s: &str) -> String {
    format!("{}/p/{s}", base())
}

thread_local! {
    /// This test's local server base URL: post links (and so Substack probes) hit the same server.
    static BASE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn base() -> String {
    BASE.with(|b| b.borrow().clone())
}

/// `sources` rows as (name, path, extra toml). `BASE` in `files` is the server base URL.
fn world(files: &[(&str, &str)], pipelines: &str, rows: &[(&str, &str, &str)]) -> World {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let routes: Routes = Arc::new(Mutex::new(Vec::new()));
    let table = Arc::clone(&routes);
    let http = MockHttp::start(move |req| {
        table
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _, _)| req.target == *p)
            .map_or_else(
                || Reply::new(404, Vec::new()),
                |(_, s, b)| Reply::new(*s, b.clone()),
            )
    });
    let base = http.url();
    BASE.with(|b| b.borrow_mut().clone_from(&base));
    let fx = LiveFixture::new(files.iter().map(|(p, t)| (*p, t.replace("BASE", &base))));
    let sources: String = rows
        .iter()
        .map(|(name, path, extra)| {
            let link = if path.starts_with('/') {
                format!("{base}{path}")
            } else {
                (*path).to_owned()
            };
            format!("[[blog]]\nname = \"{name}\"\nlink = \"{link}\"\n{extra}\n")
        })
        .collect();
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        ("sources.toml".to_owned(), Some(sources)),
        ("captures.toml".to_owned(), Some(CAPTURES.to_owned())),
        ("kinds.toml".to_owned(), Some(KINDS.to_owned())),
    ]);
    let mut config = ConfigSet::new();
    config.reload(&files, &crate::steps::validate);
    World {
        vault: fx.vault.clone(),
        env: StepEnv::new(fx, config, action()),
        rt,
        base,
        routes,
        _http: http,
    }
}

fn set(w: &World, path: &'static str, status: u16, body: String) {
    let mut r = w.routes.lock().unwrap();
    r.retain(|(p, _, _)| *p != path);
    r.push((path, status, body));
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"track_blogs\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "track-blogs".into(),
        label: "Track blogs".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec::default(),
    }
}

fn run(w: &World) -> Result<StepOutcome, StepError> {
    let _rt = w.rt.enter();
    execute(&step(""), &mut w.ctx())
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn daily(w: &World) -> String {
    std::fs::read_to_string(w.vault.join(format!("daily/{}.md", today()))).unwrap_or_default()
}

fn note(w: &World, slug: &str) -> Option<String> {
    std::fs::read_to_string(w.vault.join(format!("daily/{}/Post {slug}.md", today()))).ok()
}

fn outcome(w: &World, key: &str, s: &str) -> Option<TrackerOutcome> {
    w.state
        .lock()
        .unwrap()
        .tracker_item(KIND, key, &post_url(s))
        .unwrap()
        .map(|r| r.outcome)
}

fn n(o: &StepOutcome, k: &str) -> Value {
    o.vars[k].clone()
}

const ON: &str = "[blogs]\nenabled = true\nautomatable_window = 2\n";

#[test]
fn params_validate() {
    let c = ConfigSet::new();
    assert!(validate(&step(""), &c).is_ok());
    assert!(validate(&step("nope = 1"), &c).is_err());
}

#[test]
fn snippet_posts_become_lines_and_rerun_is_a_noop() {
    let w = world(&[], ON, &[("Snip", "/a", "body = \"snippet\"")]);
    set(&w, "/a", 200, rss(&[("b", true), ("a", true)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "sources"), json!(1));
    assert_eq!(n(&o, "new"), json!(2));
    assert_eq!(n(&o, "lines"), json!(2));
    assert_eq!(n(&o, "notes"), json!(0));
    let d = daily(&w);
    assert!(d.contains(&format!("- [ ] [Post a]({}) — Snip", post_url("a"))));
    assert!(d.find(&post_url("a")).unwrap() < d.find(&post_url("b")).unwrap());
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "a"), Some(TrackerOutcome::Line));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(daily(&w), d);
}

#[test]
fn rows_record_body_word_count_and_source_name() {
    let w = world(&[], ON, &[("Mix", "/a", "")]);
    set(&w, "/a", 200, rss(&[("b", false), ("a", true)]));
    run(&w).unwrap();
    let key = format!("{}/a", w.base);
    let row = |s: &str| {
        w.state
            .lock()
            .unwrap()
            .tracker_item(KIND, &key, &post_url(s))
            .unwrap()
            .unwrap()
    };
    let (a, b) = (row("a"), row("b"));
    assert_eq!(a.word_count, Some(4), "\"Body of a here\"");
    assert_eq!(b.word_count, None, "no body → no word count");
    assert_eq!(a.source_name.as_deref(), Some("Mix"));
    assert_eq!(b.source_name.as_deref(), Some("Mix"));
    assert_eq!((a.duration_s, b.duration_s), (None, None));
}

#[test]
fn recorded_posts_backfill_null_word_count_of_line_and_note_rows_only() {
    let w = world(&[], ON, &[("Mix", "/a", "")]);
    set(&w, "/a", 200, rss(&[("d", true), ("c", true), ("a", true)]));
    let key = format!("{}/a", w.base);
    let mk = |s: &str, outcome: TrackerOutcome, wc: Option<i64>| cv_state::TrackerItem {
        source_kind: KIND.into(),
        source_key: key.clone(),
        item_id: post_url(s),
        title: String::new(),
        url: post_url(s),
        first_seen_ms: 0,
        day: "2026-10-16".into(),
        outcome,
        score: None,
        full_body: Some(true),
        note_path: None,
        source_name: None,
        duration_s: None,
        word_count: wc,
        access: None,
        complete: None,
        full_word_count: None,
    };
    {
        let mut s = w.state.lock().unwrap();
        for it in [
            mk("a", TrackerOutcome::Line, None),
            mk("c", TrackerOutcome::Note, Some(99)),
            mk("d", TrackerOutcome::Skipped, None),
        ] {
            assert!(s.insert_tracker_item(&it).unwrap());
        }
    }
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0), "backfill never counts as an item");
    assert_eq!(n(&o, "lines"), json!(0));
    assert_eq!(n(&o, "notes"), json!(0));
    let row = |s: &str| {
        w.state
            .lock()
            .unwrap()
            .tracker_item(KIND, &key, &post_url(s))
            .unwrap()
            .unwrap()
    };
    assert_eq!(row("a").word_count, Some(4), "null → filled");
    assert_eq!(row("c").word_count, Some(99), "non-null untouched");
    assert_eq!(row("d").word_count, None, "skipped rows untouched");
    assert_eq!(row("a").duration_s, None);
}

#[test]
fn full_source_post_becomes_a_linted_filed_blog_note() {
    let w = world(
        &[],
        ON,
        &[("Full", "/a", "body = \"full\"\ntags = [\"research\"]")],
    );
    set(&w, "/a", 200, rss(&[("x", true)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    assert_eq!(n(&o, "lines"), json!(0));
    assert!(daily(&w).is_empty());
    let t = note(&w, "x").expect("note");
    let keys: Vec<&str> = t
        .lines()
        .skip(1)
        .take_while(|l| *l != "---")
        .filter(|l| !l.starts_with(' '))
        .map(|l| l.split(':').next().unwrap())
        .collect();
    // Lint appends its modified stamp (as on a filed blog note); the clipper keys keep order.
    assert_eq!(keys.last(), Some(&"updated"));
    assert_eq!(
        keys[..keys.len() - 1],
        [
            "title",
            "source",
            "post-id",
            "author",
            "published",
            "created",
            "description",
            "word-count",
            "access",
            "tags",
            "read"
        ]
    );
    assert!(t.contains("tags:\n  - blog\n  - research\n"), "{t}");
    assert!(t.contains("read: false"));
    assert!(t.contains("author:\n  - \"[[Ann]]\""));
    // Lint recomputed the word count from the Markdown body ("Body of x here").
    assert!(t.contains("word-count: 4"), "{t}");
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "x"), Some(TrackerOutcome::Note));
    // Round-5 WP-11: the row carries the captured note's vault-relative path.
    let row = w
        .state
        .lock()
        .unwrap()
        .tracker_item(KIND, &key, &post_url("x"))
        .unwrap()
        .unwrap();
    assert_eq!(
        row.note_path.as_deref(),
        Some(format!("daily/{}/Post x.md", today()).as_str())
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
}

#[test]
fn auto_source_becomes_automatable_after_window_full_posts_across_restarts() {
    let w = world(&[], ON, &[("Auto", "/a", "")]);
    set(&w, "/a", 200, rss(&[("p2", true), ("p1", true)]));
    // Window 2: p1 and p2 have no prior evidence window → lines.
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(2));
    // A fresh StepCtx (new run) sees the persisted evidence: p3 becomes a note.
    set(
        &w,
        "/a",
        200,
        rss(&[("p3", true), ("p2", true), ("p1", true)]),
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    assert!(note(&w, "p3").is_some());
    // A snippet-only post breaks the window: line, and the next full post is a line too.
    set(
        &w,
        "/a",
        200,
        rss(&[("p5", true), ("p4", false), ("p3", true)]),
    );
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(2));
    assert_eq!(n(&o, "notes"), json!(0));
}

#[test]
fn interrupted_note_write_replays_as_record_only() {
    let w = world(&[], ON, &[("Full", "/a", "body = \"full\"")]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    hook::fail_after(Some(1));
    assert!(run(&w).is_err());
    hook::fail_after(None);
    let before = note(&w, "x").expect("note written before the crash");
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "x"), None);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(0));
    assert_eq!(outcome(&w, &key, "x"), Some(TrackerOutcome::Note));
    assert_eq!(note(&w, "x").unwrap(), before);
    let day = w.vault.join(format!("daily/{}", today()));
    assert_eq!(std::fs::read_dir(day).unwrap().count(), 1);
}

#[test]
fn seeding_from_existing_blog_notes_produces_nothing() {
    let files = [
        (
            "_blog_metadata/b/post-a.md",
            "---\nsource: BASE/p/a\npost-id: \"BASE/p/a\"\n---\n",
        ),
        (
            "_crucible/orchestration/blogs/new-posts/2026-10-01.md",
            "- [ ] [B](BASE/p/b?utm_source=rss)\n",
        ),
    ];
    let w = world(&files, ON, &[("Full", "/a", "body = \"full\"")]);
    set(&w, "/a", 200, rss(&[("b", true), ("a", true)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "new"), json!(0));
    assert_eq!(n(&o, "notes"), json!(0));
    assert!(daily(&w).is_empty());
    assert!(note(&w, "a").is_none() && note(&w, "b").is_none());
}

#[test]
fn one_feed_failing_counts_all_failing_defers() {
    let rows = [("A", "/a", ""), ("B", "/b", "")];
    let w = world(&[], ON, &rows);
    set(&w, "/a", 200, rss(&[("a", false)]));
    set(&w, "/b", 503, String::new());
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "sources"), json!(2));
    assert_eq!(n(&o, "failed"), json!(1));
    assert_eq!(n(&o, "lines"), json!(1));

    let w = world(&[], ON, &rows);
    set(&w, "/a", 503, String::new());
    set(&w, "/b", 502, String::new());
    match run(&w) {
        Err(StepError::Deferred { service, .. }) => {
            assert_eq!(service.as_deref(), Some(super::SERVICE));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn invalid_and_excluded_rows_do_not_fail_the_run() {
    let rows = [
        ("A", "/a", ""),
        ("Bad", "ftp://nope", ""),
        ("Scrape", "/s", "method = \"scrape\""),
        ("Skip", "/k", "priority = \"skip\""),
    ];
    let w = world(&[], ON, &rows);
    set(&w, "/a", 200, rss(&[("a", false)]));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "invalid"), json!(2));
    assert_eq!(n(&o, "sources"), json!(1));
    assert_eq!(n(&o, "lines"), json!(1));
}

#[test]
fn disabled_pipeline_does_nothing() {
    let w = world(&[], "", &[("A", "/a", "")]);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "disabled"), json!(true));
    assert!(daily(&w).is_empty());
}

/// One full-body RSS item titled `title` whose body embeds the image at `img`.
fn rss_with_image(title: &str, img: &str) -> String {
    format!(
        "<rss><channel><title>B</title><item><title>{title}</title><link>{}/p/i</link><dc:creator>Ann</dc:creator><pubDate>Tue, 14 Oct 2026 10:00:00 +0000</pubDate><description>About</description><content:encoded><![CDATA[<p>Look</p><p><img src=\"{img}\" alt=\"pic\"></p>]]></content:encoded></item></channel></rss>",
        base()
    )
}

const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"></svg>";

#[test]
fn full_body_capture_is_title_named_tag_normalized_and_localized_inline() {
    let w = world(
        &[],
        ON,
        &[(
            "Full",
            "/a",
            "body = \"full\"\ntags = [\"#a #b\", \"a, c\"]",
        )],
    );
    let img = format!("{}/img.svg", w.base);
    set(&w, "/a", 200, rss_with_image("Why: Rust? #1", &img));
    set(&w, "/img.svg", 200, SVG.to_owned());
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    assert_eq!(n(&o, "localize_failed"), json!(0));
    let path = w.vault.join(format!("daily/{}/Why Rust 1.md", today()));
    let t = std::fs::read_to_string(&path).expect("title-named note");
    assert!(t.contains("tags:\n  - blog\n  - a\n  - b\n  - c\n"), "{t}");
    assert!(!t.contains(&img), "{t}");
    assert!(t.contains("_MD5.svg"), "{t}");
}

#[test]
fn image_404_keeps_the_remote_ref_and_still_captures() {
    let w = world(&[], ON, &[("Full", "/a", "body = \"full\"")]);
    let img = format!("{}/gone.png", w.base);
    set(&w, "/a", 200, rss_with_image("Gone", &img));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    let t =
        std::fs::read_to_string(w.vault.join(format!("daily/{}/Gone.md", today()))).expect("note");
    assert!(t.contains(&img), "{t}");
    let key = format!("{}/a", w.base);
    assert_eq!(outcome(&w, &key, "i"), Some(TrackerOutcome::Note));
}

// ---- access / completeness (plan auto-intake-sprint WP-2) ------------------------------------

fn item(w: &World, s: &str) -> cv_state::TrackerItem {
    let key = format!("{}/a", w.base);
    w.state
        .lock()
        .unwrap()
        .tracker_item(KIND, &key, &post_url(s))
        .unwrap()
        .expect("row")
}

fn status(w: &World, s: &str) -> Option<char> {
    let key = format!("{}/a", w.base);
    w.state
        .lock()
        .unwrap()
        .tracker_item_status(KIND, &key, &post_url(s))
        .unwrap()
        .expect("row")
}

fn probe(audience: &str, wordcount: u32) -> String {
    format!(r#"{{"id":7,"audience":"{audience}","wordcount":{wordcount},"title":"t"}}"#)
}

const FULL: &str = "body = \"full\"";

#[test]
fn free_probed_post_is_complete_and_auto_done() {
    let w = world(&[], ON, &[("Full", "/a", FULL)]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    set(&w, "/api/v1/posts/x", 200, probe("everyone", 4));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(1));
    assert_eq!(n(&o, "probe_failed"), json!(0));
    let t = note(&w, "x").expect("note");
    assert!(t.contains("access: free\ncomplete: true\n"), "{t}");
    let r = item(&w, "x");
    assert_eq!(
        (r.access.as_deref(), r.complete, r.full_word_count),
        (Some("free"), Some(true), Some(4))
    );
    assert_eq!(status(&w, "x"), Some('x'));
}

#[test]
fn paid_probed_post_is_incomplete_and_stays_open() {
    let w = world(&[], ON, &[("Full", "/a", FULL)]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    set(&w, "/api/v1/posts/x", 200, probe("only_paid", 40));
    run(&w).unwrap();
    let t = note(&w, "x").expect("note");
    assert!(t.contains("access: paid\ncomplete: false\n"), "{t}");
    let r = item(&w, "x");
    assert_eq!(
        (r.access.as_deref(), r.complete),
        (Some("paid"), Some(false))
    );
    assert_eq!(status(&w, "x"), None);
}

#[test]
fn failed_probe_records_unknown_and_still_writes_the_note() {
    let w = world(&[], ON, &[("Full", "/a", FULL)]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    set(&w, "/api/v1/posts/x", 500, String::new());
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "probe_failed"), json!(1));
    assert_eq!(n(&o, "notes"), json!(1));
    let t = note(&w, "x").expect("note");
    assert!(
        t.contains("access: unknown\n") && !t.contains("complete:"),
        "{t}"
    );
    let r = item(&w, "x");
    assert_eq!((r.access.as_deref(), r.complete), (Some("unknown"), None));
    assert_eq!(status(&w, "x"), None);
}

#[test]
fn non_p_post_url_is_not_probed_and_complete_with_a_body() {
    let w = world(&[], ON, &[("Full", "/a", FULL)]);
    // A probe would 404 (no route) and count as failed; none must be made.
    let feed = rss(&[("x", true)]).replace("/p/x<", "/2026/x<");
    set(&w, "/a", 200, feed);
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "probe_failed"), json!(0));
    assert_eq!(n(&o, "notes"), json!(1));
    let key = format!("{}/a", w.base);
    let rows = w
        .state
        .lock()
        .unwrap()
        .tracker_items_for_source(KIND, &key, 10)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].access.as_deref(), rows[0].complete),
        (None, Some(true))
    );
}

#[test]
fn paywall_marker_marks_a_free_post_incomplete() {
    let extra = "body = \"full\"\npaywall_markers = [\"BODY OF X\"]";
    let w = world(&[], ON, &[("Full", "/a", extra)]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    set(&w, "/api/v1/posts/x", 200, probe("everyone", 4));
    run(&w).unwrap();
    let r = item(&w, "x");
    assert_eq!(
        (r.access.as_deref(), r.complete),
        (Some("free"), Some(false))
    );
    assert_eq!(status(&w, "x"), None);
}

#[test]
fn paid_line_outcome_carries_access_and_is_never_auto_done() {
    let w = world(&[], ON, &[("Auto", "/a", "")]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    set(&w, "/api/v1/posts/x", 200, probe("only_paid", 4));
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "lines"), json!(1));
    let r = item(&w, "x");
    assert_eq!(r.outcome, TrackerOutcome::Line);
    assert_eq!(r.access.as_deref(), Some("paid"));
    assert_eq!(status(&w, "x"), None);
}

// ---- supersede engine (plan auto-intake-sprint WP-3) ----------------------------------------

#[test]
fn canonical_url_match_with_a_different_raw_source_is_not_captured_again() {
    let files = [(
        "notes/old.md",
        "---\nsource: BASE/p/x/?utm_source=rss\naccess: free\ncomplete: true\n---\n\nBody of x here\n",
    )];
    let w = world(&files, ON, &[("Full", "/a", FULL)]);
    set(&w, "/a", 200, rss(&[("x", true)]));
    let o = run(&w).unwrap();
    assert_eq!((n(&o, "new"), n(&o, "notes")), (json!(0), json!(0)));
    assert!(note(&w, "x").is_none());
    let r = item(&w, "x");
    assert_eq!(r.outcome, TrackerOutcome::Skipped);
    assert_eq!(
        (r.access.as_deref(), r.complete),
        (Some("free"), Some(true))
    );
    assert!(
        w.read("notes/old.md")
            .unwrap()
            .ends_with("\nBody of x here\n")
    );
}

#[test]
fn clipped_note_without_complete_is_kept_against_a_complete_feed_body() {
    let files = [(
        "notes/clip.md",
        "---\nsource: BASE/two\n---\n\nMy own highlights and annotations of this article\n",
    )];
    let w = world(&files, ON, &[("Full", "/a", FULL)]);
    let feed = format!(
        "<rss><channel><title>B</title><item><title>Two</title><link>{}/two</link><description>d</description><content:encoded><![CDATA[<p>The feed delivered a completely different full body text</p>]]></content:encoded></item></channel></rss>",
        w.base
    );
    set(&w, "/a", 200, feed);
    let before = w.read("notes/clip.md").unwrap();
    let o = run(&w).unwrap();
    assert_eq!(n(&o, "notes"), json!(0));
    assert_eq!(w.read("notes/clip.md").unwrap(), before);
    assert!(!w.fx.root().join("superseded").exists());
}
