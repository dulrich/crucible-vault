//! `intake_enrich` tests (plan `intake-auto-enrich` WP-3): the step against a
//! `cv_testkit::MockHttp` posing as an `openai-compatible` provider, synthetic notes in a temp
//! vault. No real provider is ever called.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, ResourceSpec};
use cv_protocol::ActionContext;
use cv_state::FailureReason;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value, json};

use super::intake_enrich::{
    Params, already_enriched, parse, pick_backfill, pick_capped, reattach, run_note,
};
use super::params::job_pos_int;
use super::testkit::{ActionBuilder, AtRecorder, Hook, StepEnv, fake, mgr, providers};
use super::{DeferCause, StepError, StepOutcome};

const AGENTS: &str = r#"
[intake-enrich]
name = "Intake Enrich"
prompt = "{{value}}"
input = "{{content}}"
model = "fake:m1"
max_tokens = 100
"#;

fn config(base: &str, key_env: &str) -> ConfigSet {
    let mut c = ConfigSet::new();
    let files = BTreeMap::from([
        ("agents.toml".to_owned(), Some(AGENTS.to_owned())),
        ("providers.toml".to_owned(), Some(providers(base, key_env))),
    ]);
    c.reload(&files, &super::validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

fn action() -> Action {
    ActionBuilder::new(
        "intake-enrich",
        vec![],
        ResourceSpec {
            note: true,
            keys: vec!["clip::enrich".into()],
        },
    )
    .build()
}

fn env(base: &str, key_env: &str) -> Arc<StepEnv> {
    let fx = LiveFixture::builder().build();
    Arc::new(StepEnv::new(fx, config(base, key_env), action()))
}

const CTA: &str = "Subscribe to our newsletter for more.";

/// A long-enough body (>= 150 words) with one CTA line.
fn long_note() -> String {
    let para = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu ";
    format!(
        "---\ntitle: T\nsource: https://posts.example/a\n---\n\n{}\n\n{CTA}\n\n{}\n",
        para.repeat(15).trim(),
        para.repeat(5).trim()
    )
}

fn short_note() -> String {
    format!("---\ntitle: S\n---\n\nshort body of a few words\n\n{CTA}\n")
}

fn answer() -> String {
    json!({
        "summary": "A test post.",
        "entities": [],
        "key_claims": [],
        "chapters": [],
        "spans": [{"kind": "cta", "first_line": CTA, "last_line": CTA, "confidence": 0.9}],
        "content_ends_after": null
    })
    .to_string()
}

fn read(e: &StepEnv, rel: &str) -> String {
    std::fs::read_to_string(e.fx.path(rel)).unwrap()
}

fn params(force: bool) -> Params {
    let t: toml::Table = if force { "force = true" } else { "" }.parse().unwrap();
    parse(&t).unwrap()
}

/// Runs the step on a blocking thread inside the runtime hosting the fake provider.
async fn exec(
    e: &Arc<StepEnv>,
    path: &str,
    job_params: Map<String, Value>,
    p: Params,
) -> Result<StepOutcome, StepError> {
    let (e, path) = (Arc::clone(e), path.to_owned());
    tokio::task::spawn_blocking(move || {
        let context = ActionContext {
            active_path: Some(path),
            ..ActionContext::default()
        };
        let mut ctx = e.ctx();
        ctx.params = &job_params;
        ctx.context = &context;
        run_note(&p, &mut ctx, &mgr())
    })
    .await
    .unwrap()
}

fn var(o: &StepOutcome, k: &str) -> Value {
    o.vars.get(k).cloned().unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn success_writes_once_with_enriched_and_a_folded_cta() {
    let (base, mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", long_note());
    let o = exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "n.md");
    assert!(t.contains("\nenriched: 20"), "{t}");
    assert!(t.contains("\nenrich-model: fake:m1\n"), "{t}");
    assert!(t.contains("> [!abstract]-"), "{t}");
    assert!(t.contains("> [!removed]-"), "{t}");
    // Nothing deleted: the CTA text lives on inside the fold.
    assert_eq!(t.matches(CTA).count(), 1, "{t}");
    assert!(t.contains(&format!("> {CTA}")), "{t}");
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(var(&o, "enriched"), json!(true));
    assert_eq!(var(&o, "anchors_resolved"), json!(1));
    assert_eq!(var(&o, "anchors_dropped"), json!(0));
    assert_eq!(var(&o, "lines_folded"), json!(1));
    assert_eq!(var(&o, "top_block"), json!(true));
    assert_eq!(o.output_paths, vec!["n.md".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_run_skips_without_a_model_call_and_force_reruns() {
    let (base, mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", long_note());
    exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    assert!(already_enriched(&read(&e, "n.md")));
    let o = exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    assert_eq!(var(&o, "skipped"), json!("already enriched"));
    assert_eq!(var(&o, "enriched"), json!(false));
    assert_eq!(mock.requests().len(), 1, "a skip never calls the model");

    // Forced by the step key, and by a job param (which wins over the step key).
    let o = exec(&e, "n.md", Map::new(), params(true)).await.unwrap();
    assert_eq!(var(&o, "enriched"), json!(true));
    assert_eq!(mock.requests().len(), 2);
    let job = Map::from_iter([("force".to_owned(), json!(true))]);
    exec(&e, "n.md", job, params(false)).await.unwrap();
    assert_eq!(mock.requests().len(), 3);
    // One fold and one top block, never a second copy.
    let t = read(&e, "n.md");
    assert_eq!(t.matches("[!removed]-").count(), 1, "{t}");
    assert_eq!(t.matches("[!abstract]-").count(), 1, "{t}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_during_the_model_call_conflicts_and_is_never_clobbered() {
    let slot: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let s2 = Arc::clone(&slot);
    let hook: Hook = Arc::new(move || {
        if let Some(p) = s2.lock().unwrap().as_ref() {
            std::fs::write(p, "user edit\n").unwrap();
        }
    });
    let (base, _mock) = fake(200, &answer(), "stop", Some(hook));
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("c.md", long_note());
    *slot.lock().unwrap() = Some(e.fx.path("c.md"));
    let err = exec(&e, "c.md", Map::new(), params(false))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        StepError::Deferred {
            retry_after_ms: super::CONFLICT_RETRY_MS,
            service: None,
            cause: DeferCause::Wait,
        }
    );
    assert_eq!(read(&e, "c.md"), "user edit\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unparseable_or_truncated_answers_write_nothing() {
    for (text, finish) in [("I cannot do that.", "stop"), (answer().as_str(), "length")] {
        let (base, mock) = fake(200, text, finish, None);
        let e = env(&base, "CV_TEST_KEY");
        e.fx.write("n.md", long_note());
        let err = exec(&e, "n.md", Map::new(), params(false))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, StepError::Failed { reason: None, .. }),
            "{err:?}"
        );
        assert_eq!(read(&e, "n.md"), long_note());
        assert_eq!(mock.requests().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_body_is_folded_but_gets_no_top_block() {
    let (base, _mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("s.md", short_note());
    let o = exec(&e, "s.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "s.md");
    assert!(t.contains("> [!removed]-"), "{t}");
    assert!(!t.contains("[!abstract]"), "{t}");
    assert!(t.contains("\nenriched: 20"), "{t}");
    assert_eq!(var(&o, "top_block"), json!(false));
    assert_eq!(var(&o, "lines_folded"), json!(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_key_fails_no_api_key_and_an_outage_defers() {
    let (base, mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_MISSING");
    e.fx.write("n.md", long_note());
    let err = exec(&e, "n.md", Map::new(), params(false))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            StepError::Failed {
                reason: Some(FailureReason::NoApiKey),
                ..
            }
        ),
        "{err:?}"
    );
    assert!(mock.requests().is_empty());

    let (base, _mock) = fake(503, "{}", "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", long_note());
    let err = exec(&e, "n.md", Map::new(), params(false))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            StepError::Deferred {
                retry_after_ms: 60_000,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(read(&e, "n.md"), long_note());
}

/// Moved from `cv_ingest::enrich_tests` (`enrich` now takes the body; plan tn-review-2026-11-04-remediation WP-R2): the frontmatter survives enrich + reattach byte for byte, on a first run and a re-run.
#[test]
fn frontmatter_untouched_byte_for_byte() {
    use cv_ingest::enrich::{EnrichOpts, EnrichOutput, enrich};
    use cv_note::FrontmatterDoc;
    let fm = "---\r\nweird:   spacing  \r\nlist:\n  - a\n---\n";
    let out = EnrichOutput {
        summary: Some("A summary.".into()),
        ..EnrichOutput::default()
    };
    let opts = EnrichOpts { top_block: true };
    let n = format!("{fm}Some body text.\n\nMore text.\n");
    let run = |text: &str| {
        let doc = FrontmatterDoc::new(text);
        let body = doc.body();
        let e = enrich(body, &out, &[], opts);
        reattach(text, body, &e.text).into_text()
    };
    let once = run(&n);
    assert!(once.starts_with(fm), "{once}");
    assert!(once.contains("[!abstract]"), "{once}");
    let again = run(&once);
    assert!(again.starts_with(fm), "{again}");
}

#[test]
fn params_and_pure_selection() {
    assert!(parse(&"scope = \"nope\"".parse().unwrap()).is_err());
    assert!(parse(&"limit = 0".parse().unwrap()).is_err());
    assert!(parse(&"invented = 1".parse().unwrap()).is_err());
    assert!(parse(&"scope = \"backfill\"\nlimit = 5".parse().unwrap()).is_ok());

    let cands: BTreeMap<String, i64> = [("a.md", 1), ("b.md", 3), ("c.md", 2), ("d.md", 3)]
        .into_iter()
        .map(|(p, c)| (p.to_owned(), c))
        .collect();
    let done: BTreeSet<String> = ["d.md".to_owned()].into();
    let none = BTreeSet::new();
    assert_eq!(pick_backfill(&cands, &done, &none, 2), ["b.md", "c.md"]);
    assert_eq!(pick_backfill(&cands, &none, &none, 10).len(), 4);
    // An enriched path is picked only when refine-pending; no duplicates, same order and limit.
    let pending: BTreeSet<String> = ["d.md".to_owned(), "a.md".to_owned()].into();
    assert_eq!(
        pick_backfill(&cands, &done, &pending, 10),
        ["b.md", "d.md", "c.md", "a.md"]
    );
    assert_eq!(pick_backfill(&cands, &done, &pending, 2), ["b.md", "d.md"]);

    let job = |v: Value| Map::from_iter([("limit".to_owned(), v)]);
    assert_eq!(job_pos_int(&Map::new(), "limit"), Ok(None));
    assert_eq!(job_pos_int(&job(json!("5")), "limit"), Ok(Some(5)));
    assert_eq!(job_pos_int(&job(json!(7)), "limit"), Ok(Some(7)));
    for bad in [
        json!("0"),
        json!("2.5"),
        json!("x"),
        json!(-1),
        json!(1.5),
        json!(true),
    ] {
        assert!(job_pos_int(&job(bad.clone()), "limit").is_err(), "{bad}");
    }
}

/// Plan `youtube-transcript-auto-refine` WP-2: an enriched-but-unrefined YouTube clip with a
/// transcript is picked; a refined one, and an enriched one without a transcript, are not.
#[test]
fn backfill_picks_enriched_unrefined_youtube_clips() {
    let yt = "source: https://www.youtube.com/watch?v=abcdefghijk\n";
    let long = "word ".repeat(cv_ingest::transcript::MIN_REFINE_WORDS);
    let fx = LiveFixture::new([
        (
            "Clips/pending.md",
            format!("---\n{yt}enriched: 2026-10-01\n---\n## Transcript\n\n{long}\n"),
        ),
        (
            "Clips/short.md",
            format!("---\n{yt}enriched: 2026-10-01\n---\n## Transcript\n\nhello there\n"),
        ),
        (
            "Clips/refined.md",
            format!(
                "---\n{yt}enriched: 2026-10-01\ntags: [refined]\n---\n## Transcript\n\n{long}\n"
            ),
        ),
        (
            "Clips/nots.md",
            format!("---\n{yt}enriched: 2026-10-01\n---\n## Notes\n\nx\n"),
        ),
        (
            "Clips/fresh.md",
            format!("---\n{yt}---\n## Transcript\n\nhello\n"),
        ),
    ]);
    let e = StepEnv::new(fx, config("http://127.0.0.1:1/v1", "CV_TEST_KEY"), action());
    let rec = AtRecorder::default();
    let mut ctx = e.ctx();
    ctx.children = &rec;
    let t: toml::Table = "scope = \"backfill\"\nlimit = 10".parse().unwrap();
    let o = super::execute(
        &cv_actions::Step::from_table({
            let mut s = toml::Table::new();
            s.insert("type".into(), "intake_enrich".into());
            s.extend(t);
            s
        })
        .unwrap(),
        &mut ctx,
    )
    .unwrap();
    let mut got = rec.calls();
    got.sort();
    assert_eq!(
        got,
        [
            "intake-enrich@Clips/fresh.md",
            "intake-enrich@Clips/pending.md"
        ]
    );
    assert_eq!(var(&o, "enqueued"), json!(2));
}

#[test]
fn backfill_enqueues_only_unenriched_intake_notes_newest_first() {
    let fx = LiveFixture::new([
        ("Posts/blog.md", "---\npost-id: \"1\"\n---\nbody\n"),
        ("Posts/mail.md", "---\nmessage-id: \"<m@x>\"\n---\nbody\n"),
        (
            "Clips/yt.md",
            "---\nsource: https://www.youtube.com/watch?v=abcdefghijk\n---\nbody\n",
        ),
        (
            "Posts/done.md",
            "---\npost-id: \"2\"\nenriched: 2026-10-01\n---\nbody\n",
        ),
        ("Posts/plain.md", "---\ntitle: x\n---\nbody\n"),
        ("_yt_metadata/m.md", "---\npost-id: \"3\"\n---\nbody\n"),
    ]);
    let e = StepEnv::new(fx, config("http://127.0.0.1:1/v1", "CV_TEST_KEY"), action());
    let rec = AtRecorder::default();
    let mut ctx = e.ctx();
    ctx.children = &rec;
    let t: toml::Table = "scope = \"backfill\"\nlimit = 10".parse().unwrap();
    let o = super::execute(
        &cv_actions::Step::from_table({
            let mut s = toml::Table::new();
            s.insert("type".into(), "intake_enrich".into());
            s.extend(t);
            s
        })
        .unwrap(),
        &mut ctx,
    )
    .unwrap();
    let mut got = rec.calls();
    got.sort();
    assert_eq!(
        got,
        [
            "intake-enrich@Clips/yt.md",
            "intake-enrich@Posts/blog.md",
            "intake-enrich@Posts/mail.md"
        ]
    );
    assert_eq!(var(&o, "enqueued"), json!(3));
}

/// A recurring banner (>= 3 notes of one publication, top of body) folds into `[!removed]-`
/// even when the model names no span (plan `intake-auto-enrich` WP-5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recurring_banner_folds_without_any_model_span() {
    let none = json!({
        "summary": "A test post.", "entities": [], "key_claims": [], "chapters": [],
        "spans": [], "content_ends_after": null
    })
    .to_string();
    let (base, _mock) = fake(200, &none, "stop", None);
    let banner = "![](https://cdn.example/banner.png?v=2)";
    let note = |i: usize| {
        format!(
            "---\ntitle: T{i}\nsource: https://posts.example/{i}\npublication: Pub\n---\n\n{banner}\n\nreal body line one\n\nreal body line two\n\nmore\n\nmore\n\nmore\n\nmore\n\nmore\n\nmore\n\nmore\n\nend\n"
        )
    };
    let mut b = LiveFixture::builder();
    for i in 0..3 {
        b = b.file(&format!("n{i}.md"), note(i));
    }
    let e = Arc::new(StepEnv::new(
        b.build(),
        config(&base, "CV_TEST_KEY"),
        action(),
    ));
    let o = exec(&e, "n1.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "n1.md");
    assert!(t.contains("> [!removed]-"), "{t}");
    assert!(t.contains(&format!("> {banner}")), "{t}");
    assert_eq!(t.matches(banner).count(), 1, "{t}");
    assert_eq!(var(&o, "banners_folded"), json!(1));
}

fn empty_answer() -> String {
    json!({
        "summary": null, "entities": [], "key_claims": [], "chapters": [],
        "spans": [], "content_ends_after": null
    })
    .to_string()
}

/// WP-7: a long note with an empty answer is a miss (no stamp, body unchanged, counted); a second
/// miss makes 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_note_miss_counts_without_stamping_and_accumulates() {
    let (base, _mock) = fake(200, &empty_answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    let orig = long_note();
    e.fx.write("n.md", orig.clone());
    let o = exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "n.md");
    assert!(t.contains("\nenrich-miss: 1\n"), "{t}");
    assert!(
        !t.contains("enriched:") && !t.contains("enrich-model"),
        "{t}"
    );
    assert_eq!(t.replace("enrich-miss: 1\n", ""), orig);
    assert_eq!(var(&o, "enriched"), json!(false));
    assert_eq!(var(&o, "miss"), json!(true));
    assert_eq!(var(&o, "enrich_miss"), json!(1));
    let o = exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    assert!(read(&e, "n.md").contains("\nenrich-miss: 2\n"));
    assert_eq!(var(&o, "enrich_miss"), json!(2));
}

/// WP-7: a later successful enrich stamps and removes `enrich-miss`; a malformed value reads as 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_enrich_removes_enrich_miss() {
    let (base, _mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write(
        "n.md",
        long_note().replacen("title: T\n", "title: T\nenrich-miss: 2\n", 1),
    );
    let o = exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "n.md");
    assert!(
        t.contains("\nenriched: 20") && !t.contains("enrich-miss"),
        "{t}"
    );
    assert_eq!(var(&o, "enriched"), json!(true));
    assert_eq!(var(&o, "miss"), Value::Null);
    // Malformed count: reads as 0, overwritten by the next miss.
    let (base, _mock) = fake(200, &empty_answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write(
        "m.md",
        long_note().replacen("title: T\n", "title: T\nenrich-miss: lots\n", 1),
    );
    exec(&e, "m.md", Map::new(), params(false)).await.unwrap();
    assert!(read(&e, "m.md").contains("\nenrich-miss: 1\n"));
}

/// WP-7: a short note with an empty answer is never a miss; it is stamped as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_note_with_an_empty_answer_is_stamped() {
    let (base, _mock) = fake(200, &empty_answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("s.md", short_note());
    let o = exec(&e, "s.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "s.md");
    assert!(
        t.contains("\nenriched: 20") && !t.contains("enrich-miss"),
        "{t}"
    );
    assert_eq!(var(&o, "enriched"), json!(true));
    assert_eq!(var(&o, "miss"), Value::Null);
}

#[test]
fn pick_capped_holds_back_notes_at_the_cap_and_counts_them() {
    let cands: BTreeMap<String, i64> = [("a.md", 4), ("b.md", 3), ("c.md", 2), ("d.md", 1)]
        .into_iter()
        .map(|(p, c)| (p.to_owned(), c))
        .collect();
    let none = BTreeSet::new();
    let misses: BTreeMap<String, u32> = [("a.md", 3), ("b.md", 2), ("c.md", 9)]
        .into_iter()
        .map(|(p, c)| (p.to_owned(), c))
        .collect();
    let (picks, capped) = pick_capped(&cands, &none, &none, &misses, 10);
    assert_eq!(
        (picks, capped),
        (vec!["b.md".to_owned(), "d.md".to_owned()], 2)
    );
    // A refine-pending pick is left alone by the cap.
    let pending: BTreeSet<String> = ["a.md".to_owned()].into();
    let (picks, capped) = pick_capped(&cands, &none, &pending, &misses, 10);
    assert_eq!((picks.len(), capped), (3, 1));
}

fn backfill_step(extra: &str) -> cv_actions::Step {
    let mut s: toml::Table = format!("scope = \"backfill\"\n{extra}").parse().unwrap();
    s.insert("type".into(), "intake_enrich".into());
    cv_actions::Step::from_table(s).unwrap()
}

#[test]
fn backfill_skips_a_note_at_the_cap_and_reports_it() {
    let fx = LiveFixture::new([
        (
            "Posts/ok.md",
            "---\npost-id: \"1\"\nenrich-miss: 2\n---\nbody\n",
        ),
        (
            "Posts/capped.md",
            "---\npost-id: \"2\"\nenrich-miss: 3\n---\nbody\n",
        ),
    ]);
    let e = StepEnv::new(fx, config("http://127.0.0.1:1/v1", "CV_TEST_KEY"), action());
    let rec = AtRecorder::default();
    let mut ctx = e.ctx();
    ctx.children = &rec;
    let o = super::execute(&backfill_step(""), &mut ctx).unwrap();
    assert_eq!(rec.calls(), ["intake-enrich@Posts/ok.md"]);
    assert_eq!(var(&o, "miss_capped"), json!(1));
}

#[test]
fn recover_unstamps_only_long_notes_with_neither_block_and_is_idempotent() {
    use cv_ingest::enrich::{EnrichOpts, enrich, parse as parse_answer};
    let para = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu ";
    let body = format!(
        "\n{}\n\n{CTA}\n\n{}\n",
        para.repeat(15).trim(),
        para.repeat(5).trim()
    );
    let parsed = parse_answer(&answer()).unwrap();
    let with = |top| enrich(&body, &parsed, &[], EnrichOpts { top_block: top }).text;
    let stamp = "enriched: 2026-10-01\nenrich-model: fake:m1\nenrich-miss: 1\n";
    let note = |id: u32, b: &str| format!("---\npost-id: \"{id}\"\nmine: x\n{stamp}---\n{b}");
    let (long_none, long_top, long_fold) =
        (note(1, &body), note(2, &with(true)), note(3, &with(false)));
    assert!(long_top.contains("[!abstract]-") && long_fold.contains("[!removed]-"));
    let short = note(4, "short body\n");
    let fx = LiveFixture::new([
        ("Posts/none.md", long_none.as_str()),
        ("Posts/top.md", long_top.as_str()),
        ("Posts/fold.md", long_fold.as_str()),
        ("Posts/short.md", short.as_str()),
    ]);
    let e = StepEnv::new(fx, config("http://127.0.0.1:1/v1", "CV_TEST_KEY"), action());
    let rec = AtRecorder::default();
    let run = || {
        let mut ctx = e.ctx();
        ctx.children = &rec;
        super::execute(&backfill_step("recover = true"), &mut ctx).unwrap()
    };
    let o = run();
    assert_eq!(var(&o, "recovered"), json!(1));
    assert_eq!(var(&o, "recover_conflict"), json!(0));
    assert_eq!(var(&o, "candidates"), json!(4));
    let t = read(&e, "Posts/none.md");
    assert!(
        !t.contains("enriched:") && !t.contains("enrich-model"),
        "{t}"
    );
    assert!(
        t.contains("mine: x\n") && t.contains("enrich-miss: 1\n"),
        "{t}"
    );
    assert_eq!(read(&e, "Posts/top.md"), long_top);
    assert_eq!(read(&e, "Posts/fold.md"), long_fold);
    assert_eq!(read(&e, "Posts/short.md"), short);
    assert!(rec.calls().is_empty());
    // The index catches up asynchronously in the fixture; the second run reads files, and the
    // un-stamped note is no longer stamped on disk, so nothing else changes.
    let o = run();
    assert_eq!(var(&o, "recovered"), json!(0));
}
