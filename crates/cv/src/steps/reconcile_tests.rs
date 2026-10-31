//! `intake_reconcile` tests (plan auto-intake-sprint WP-6): synthetic temp vaults, a temp
//! `state.db` and a local fixture server for the Substack probe (never the network, never the
//! live vault).

use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_ingest::access::Access;
use cv_protocol::TrackerOutcome;
use cv_providers::FailureKind;
use cv_providers::web::FetchError;
use cv_state::TrackerItem;
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::{Value, json};

use super::reconcile::{ProbeOutcome, body_verdict, classify_probe};
use crate::steps::testkit::StepEnv;
use crate::steps::{StepError, StepOutcome, execute, validate};

type Routes = Arc<Mutex<Vec<(String, u16, String)>>>;

struct World {
    env: StepEnv,
    rt: tokio::runtime::Runtime,
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

fn action() -> Action {
    Action {
        id: "intake-reconcile".into(),
        label: "Reconcile".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        hold: false,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec::default(),
    }
}

/// `files` use `BASE` for the fixture server's base URL.
fn world(files: &[(&str, String)]) -> World {
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
    let fx = LiveFixture::new(files.iter().map(|(p, t)| (*p, t.replace("BASE", &base))));
    World {
        env: StepEnv::new(fx, ConfigSet::new(), action()),
        rt,
        base,
        routes,
        _http: http,
    }
}

fn probe_route(w: &World, slug: &str, status: u16, body: String) {
    w.routes
        .lock()
        .unwrap()
        .push((format!("/api/v1/posts/{slug}"), status, body));
}

fn probe_json(audience: &str, wordcount: u32) -> String {
    format!(r#"{{"id":7,"audience":"{audience}","wordcount":{wordcount}}}"#)
}

fn words(n: usize) -> String {
    (0..n)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn note(source: &str, body: &str) -> String {
    format!("---\ntitle: T\nsource: {source}\n---\n\n{body}\n")
}

fn add_row(w: &World, slug: &str, path: Option<&str>) {
    let url = format!("{}/p/{slug}", w.base);
    w.state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: "feed".into(),
            item_id: slug.into(),
            title: "T".into(),
            url,
            first_seen_ms: 1,
            day: "2026-10-16".into(),
            outcome: TrackerOutcome::Note,
            score: None,
            full_body: Some(true),
            note_path: path.map(str::to_owned),
            source_name: None,
            duration_s: None,
            word_count: None,
            access: None,
            complete: None,
            full_word_count: None,
        })
        .unwrap();
}

fn row(w: &World, slug: &str) -> (TrackerItem, Option<char>) {
    let db = w.state.lock().unwrap();
    let r = db.tracker_item("blog", "feed", slug).unwrap().unwrap();
    let s = db
        .tracker_item_status("blog", "feed", slug)
        .unwrap()
        .unwrap();
    (r, s)
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"intake_reconcile\"\n{src}")
        .parse()
        .unwrap();
    Step::from_table(t).unwrap()
}

fn run(w: &World, src: &str) -> Result<StepOutcome, StepError> {
    let _rt = w.rt.enter();
    execute(&step(src), &mut w.ctx())
}

fn n(o: &StepOutcome, k: &str) -> Value {
    o.vars[k].clone()
}

#[test]
fn params_validate() {
    let c = ConfigSet::new();
    assert!(validate(&step(""), &c).is_ok());
    assert!(validate(&step("dry_run = true"), &c).is_ok());
    assert!(validate(&step("dry_run = \"yes\""), &c).is_err());
    assert!(validate(&step("nope = 1"), &c).is_err());
}

#[test]
fn null_path_rows_link_by_canonical_url_and_ambiguous_stays_null() {
    let w = world(&[
        // The note's `source` is the clean URL; the row URL carries a tracking param.
        ("notes/a.md", note("BASE/p/one", &words(20))),
        ("notes/b1.md", note("BASE/p/two", &words(20))),
        ("notes/b2.md", note("BASE/p/two", &words(21))),
    ]);
    add_row(&w, "one", None);
    add_row(&w, "two", None);
    add_row(&w, "three", None);
    // Rewrite row `one`'s URL with tracking params.
    {
        let mut db = w.state.lock().unwrap();
        let mut it = db.tracker_item("blog", "feed", "one").unwrap().unwrap();
        it.item_id = "one-x".into();
        it.url = format!("{}/p/one/?utm_source=rss&utm_medium=feed", w.base);
        db.insert_tracker_item(&it).unwrap();
    }
    let o = run(&w, "dry_run = false").unwrap();
    assert_eq!(n(&o, "null_paths"), json!(4));
    // `one` and `one-x` link to the one note; `two` matches two notes; `three` matches none.
    assert_eq!(n(&o, "linked"), json!(2));
    assert_eq!(n(&o, "ambiguous"), json!(1));
    assert_eq!(n(&o, "unresolved"), json!(1));
    let linked = |id: &str| {
        w.state
            .lock()
            .unwrap()
            .tracker_item("blog", "feed", id)
            .unwrap()
            .unwrap()
            .note_path
    };
    assert_eq!(linked("one-x").as_deref(), Some("notes/a.md"));
    assert_eq!(linked("two"), None);
    assert_eq!(linked("three"), None);
    // Aggregates only.
    let text = serde_json::to_string(&o.vars).unwrap();
    assert!(!text.contains("notes/"));
}

fn complete_and_paid() -> World {
    let w = world(&[
        ("notes/free.md", note("BASE/p/free", &words(20))),
        ("notes/paid.md", note("BASE/p/paid", &words(20))),
    ]);
    add_row(&w, "free", Some("notes/free.md"));
    add_row(&w, "paid", Some("notes/paid.md"));
    probe_route(&w, "free", 200, probe_json("everyone", 20));
    probe_route(&w, "paid", 200, probe_json("only_paid", 1000));
    w
}

#[test]
fn complete_body_ticks_and_paid_short_body_stays_open() {
    let w = complete_and_paid();
    let o = run(&w, "").unwrap();
    assert_eq!(n(&o, "probed"), json!(2));
    assert_eq!(n(&o, "ticked"), json!(1));
    assert_eq!(n(&o, "left_open"), json!(1));
    let (free, fs) = row(&w, "free");
    assert_eq!(fs, Some('x'));
    // Evidence is written with the tick.
    assert_eq!(
        (free.access.as_deref(), free.complete),
        (Some("free"), Some(true))
    );
    let (paid, ps) = row(&w, "paid");
    assert_eq!(ps, None);
    assert_eq!(
        (paid.access.as_deref(), paid.complete, paid.full_word_count),
        (Some("paid"), Some(false), Some(1000))
    );
    // A second run has nothing more to tick and does not re-tick.
    let o2 = run(&w, "").unwrap();
    assert_eq!(
        (n(&o2, "ticked"), n(&o2, "left_open")),
        (json!(0), json!(1))
    );
}

#[test]
fn dry_run_reports_the_same_counts_and_writes_nothing() {
    let w = complete_and_paid();
    add_row(&w, "lost", None);
    let before = (row(&w, "free"), row(&w, "paid"), row(&w, "lost"));
    let dry = run(&w, "dry_run = true").unwrap();
    assert_eq!(n(&dry, "dry_run"), json!(true));
    assert_eq!(
        (row(&w, "free"), row(&w, "paid"), row(&w, "lost")),
        before,
        "a dry run changes no tracker row"
    );
    let real = run(&w, "").unwrap();
    for k in [
        "null_paths",
        "linked",
        "unresolved",
        "ambiguous",
        "probed",
        "probe_failed",
        "probe_skipped",
        "ticked",
        "left_open",
    ] {
        assert_eq!(n(&dry, k), n(&real, k), "{k}");
    }
    assert_eq!(n(&real, "dry_run"), json!(false));
}

#[test]
fn job_param_turns_the_dry_run_on() {
    let mut w = complete_and_paid();
    w.env.params.insert("dry_run".into(), json!(true));
    let o = run(&w, "").unwrap();
    assert_eq!((n(&o, "dry_run"), n(&o, "ticked")), (json!(true), json!(1)));
    assert_eq!(row(&w, "free").1, None);
}

#[test]
fn job_false_overrides_a_step_dry_run_true() {
    let mut w = complete_and_paid();
    w.env.params.insert("dry_run".into(), json!("false"));
    let o = run(&w, "dry_run = true").unwrap();
    assert_eq!(n(&o, "dry_run"), json!(false));
    assert!(row(&w, "free").1.is_some());
}

#[test]
fn probe_failure_leaves_the_row_open_and_is_counted() {
    let w = world(&[("notes/x.md", note("BASE/p/x", &words(20)))]);
    add_row(&w, "x", Some("notes/x.md"));
    // No route: the probe 404s.
    let o = run(&w, "").unwrap();
    assert_eq!(n(&o, "probe_failed"), json!(1));
    assert_eq!((n(&o, "ticked"), n(&o, "left_open")), (json!(0), json!(1)));
    let (r, s) = row(&w, "x");
    assert_eq!((s, r.access, r.complete), (None, None, None));
}

#[test]
fn classify_probe_and_verdict_are_pure() {
    let ok = Ok(r#"{"audience":"only_paid","wordcount":50}"#.to_owned());
    assert_eq!(
        classify_probe(&ok),
        ProbeOutcome::Probed(Access::Paid, Some(50))
    );
    assert_eq!(classify_probe(&Ok("<html>".into())), ProbeOutcome::Failed);
    let quota = Err(FetchError::Quota {
        service: "substack-api".into(),
        retry_after: None,
    });
    assert_eq!(classify_probe(&quota), ProbeOutcome::Skipped);
    let limited = Err(FetchError::Transient {
        service: "substack-api".into(),
        retry_after: None,
        kind: FailureKind::RateLimited,
        detail: "429".into(),
    });
    assert_eq!(classify_probe(&limited), ProbeOutcome::Skipped);
    let missing = Err(FetchError::NotFound {
        service: "substack-api".into(),
        status: 404,
    });
    assert_eq!(classify_probe(&missing), ProbeOutcome::Failed);

    let body = words(20);
    assert_eq!(body_verdict(None, &body, None, &[], 80), Some(true));
    assert_eq!(
        body_verdict(Some(Access::Paid), &body, None, &[], 80),
        Some(false)
    );
    assert_eq!(body_verdict(None, "", None, &[], 80), Some(false));
    assert_eq!(
        body_verdict(None, &body, None, &["w5 w6".to_owned()], 80),
        Some(false)
    );
}

#[test]
fn non_blog_open_rows_are_never_ticked() {
    let w = world(&[
        ("notes/m.md", note("BASE/p/m", &words(20))),
        ("notes/y.md", note("BASE/p/y", &words(20))),
    ]);
    for (kind, key, id, path) in [
        ("mail", "unknown-sender", "m1", "notes/m.md"),
        ("youtube", "UC1", "y1", "notes/y.md"),
    ] {
        w.state
            .lock()
            .unwrap()
            .insert_tracker_item(&TrackerItem {
                source_kind: kind.into(),
                source_key: key.into(),
                item_id: id.into(),
                title: "T".into(),
                url: format!("{}/p/{id}", w.base),
                first_seen_ms: 1,
                day: "2026-10-16".into(),
                outcome: TrackerOutcome::Note,
                score: None,
                full_body: Some(true),
                note_path: Some(path.into()),
                source_name: None,
                duration_s: None,
                word_count: None,
                access: None,
                complete: None,
                full_word_count: None,
            })
            .unwrap();
    }
    let o = run(&w, "").unwrap();
    assert_eq!(
        (n(&o, "ticked"), n(&o, "skipped_kind")),
        (json!(0), json!(1))
    );
    let db = w.state.lock().unwrap();
    for (k, key, id) in [("mail", "unknown-sender", "m1"), ("youtube", "UC1", "y1")] {
        assert_eq!(db.tracker_item_status(k, key, id).unwrap().unwrap(), None);
        assert_eq!(db.tracker_item(k, key, id).unwrap().unwrap().complete, None);
    }
}
