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

use super::intake_enrich::{Params, already_enriched, parse, pick_capped, reattach, run_note};
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
    assert!(!t.contains("[!abstract]"), "{t}");
    assert!(t.contains("> [!removed]-"), "{t}");
    // Nothing deleted: the CTA text lives on inside the fold.
    assert_eq!(t.matches(CTA).count(), 1, "{t}");
    assert!(t.contains(&format!("> {CTA}")), "{t}");
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(var(&o, "enriched"), json!(true));
    assert_eq!(var(&o, "anchors_resolved"), json!(1));
    assert_eq!(var(&o, "anchors_dropped"), json!(0));
    assert_eq!(var(&o, "lines_folded"), json!(1));
    assert_eq!(var(&o, "top_block"), Value::Null);
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
    // One fold, never a second copy, and no top block.
    let t = read(&e, "n.md");
    assert_eq!(t.matches("[!removed]-").count(), 1, "{t}");
    assert_eq!(t.matches("[!abstract]-").count(), 0, "{t}");
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
async fn short_body_is_folded_and_stamped() {
    let (base, _mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("s.md", short_note());
    let o = exec(&e, "s.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "s.md");
    assert!(t.contains("> [!removed]-"), "{t}");
    assert!(!t.contains("[!abstract]"), "{t}");
    assert!(t.contains("\nenriched: 20"), "{t}");
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
        chapters: vec![cv_ingest::enrich::Chapter {
            title: "Part".into(),
            starts_at: "More text".into(),
        }],
        ..EnrichOutput::default()
    };
    let opts = EnrichOpts { headings: true };
    let n = format!("{fm}Some body text.\n\nMore text.\n");
    let run = |text: &str| {
        let doc = FrontmatterDoc::new(text);
        let body = doc.body();
        let e = enrich(body, &out, &[], opts);
        reattach(text, body, &e.text).into_text()
    };
    let once = run(&n);
    assert!(once.starts_with(fm), "{once}");
    assert!(once.contains("## Part"), "{once}");
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
    assert_eq!(
        pick_capped(&cands, &done, &none, &BTreeMap::new(), 2).0,
        ["b.md", "c.md"]
    );
    assert_eq!(
        pick_capped(&cands, &none, &none, &BTreeMap::new(), 10)
            .0
            .len(),
        4
    );
    // An enriched path is picked only when refine-pending; no duplicates, same order and limit.
    let pending: BTreeSet<String> = ["d.md".to_owned(), "a.md".to_owned()].into();
    assert_eq!(
        pick_capped(&cands, &done, &pending, &BTreeMap::new(), 10).0,
        ["b.md", "d.md", "c.md", "a.md"]
    );
    assert_eq!(
        pick_capped(&cands, &done, &pending, &BTreeMap::new(), 2).0,
        ["b.md", "d.md"]
    );

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
    json!({"chapters": [], "spans": [], "content_ends_after": null}).to_string()
}

/// An answer whose every anchor fails to resolve (span, `content_ends_after`, chapter).
fn unresolved_answer() -> String {
    json!({
        "chapters": [{"title": "Ghost", "starts_at": "no such line anywhere"}],
        "spans": [{"kind": "cta", "first_line": "not in the note", "last_line": "nor this"}],
        "content_ends_after": "missing ending"
    })
    .to_string()
}

/// A legacy `[!abstract]` top block as the pre-WP-2 pass wrote it (ToC linking `#Transcript`).
const LEGACY_TOP: &str = "%% cv-enrich %%\n> [!abstract]-\n> A summary.\n>\n> Contents:\n> - [[#Transcript]]\n\n%% /cv-enrich %%\n\n";

/// Plan abstract-removal-confidence-deletion WP-2 (b): a long note whose answer's anchors all fail
/// is a miss (no stamp, body unchanged, counted); a second miss makes 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_note_miss_counts_without_stamping_and_accumulates() {
    let (base, _mock) = fake(200, &unresolved_answer(), "stop", None);
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

/// WP-2 (a): the miss write removes a legacy top block, and so does a stamping write; nothing
/// else in the body changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_enrich_write_removes_a_legacy_top_block() {
    let orig = long_note();
    let legacy = orig.replacen("---\n\n", &format!("---\n{LEGACY_TOP}"), 1);
    assert_ne!(legacy, orig);
    let (base, _mock) = fake(200, &unresolved_answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", legacy.clone());
    let o = exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    assert_eq!(var(&o, "miss"), json!(true));
    let t = read(&e, "n.md");
    assert_eq!(
        t,
        orig.replacen("---\n\n", "enrich-miss: 1\n---\n", 1),
        "{t}"
    );
    let (base, _mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", legacy);
    exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    let t = read(&e, "n.md");
    assert!(
        t.contains("\nenriched: 20") && !t.contains("[!abstract]"),
        "{t}"
    );
    assert!(t.contains("> [!removed]-"), "{t}");
}

/// WP-2 (b): a long note whose answer requested no anchors is stamped, not a miss, and a reply
/// still carrying the retired `summary` / `entities` / `key_claims` keys is accepted with them
/// ignored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_no_anchor_answer_stamps_and_retired_keys_are_ignored() {
    let retired = json!({
        "summary": "A test post.", "entities": [{"name": "Acme"}],
        "key_claims": [{"claim": "c", "quote": "q"}], "chapters": [], "spans": []
    })
    .to_string();
    for reply in [empty_answer(), retired] {
        let (base, _mock) = fake(200, &reply, "stop", None);
        let e = env(&base, "CV_TEST_KEY");
        e.fx.write("n.md", long_note());
        let o = exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
        let t = read(&e, "n.md");
        assert!(
            t.contains("\nenriched: 20") && !t.contains("enrich-miss"),
            "{t}"
        );
        assert!(
            !t.contains("[!abstract]") && !t.contains("A test post."),
            "{t}"
        );
        assert_eq!(var(&o, "enriched"), json!(true));
        assert_eq!(var(&o, "miss"), Value::Null);
    }
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
    let (base, _mock) = fake(200, &unresolved_answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write(
        "m.md",
        long_note().replacen("title: T\n", "title: T\nenrich-miss: lots\n", 1),
    );
    exec(&e, "m.md", Map::new(), params(false)).await.unwrap();
    assert!(read(&e, "m.md").contains("\nenrich-miss: 1\n"));
}

/// WP-7 / WP-2 (b): a short note is never a miss, even when every anchor failed; it is stamped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_note_with_unresolved_anchors_is_stamped() {
    let (base, _mock) = fake(200, &unresolved_answer(), "stop", None);
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

fn strip_step() -> cv_actions::Step {
    let mut s: toml::Table = "scope = \"strip-top\"".parse().unwrap();
    s.insert("type".into(), "intake_enrich".into());
    cv_actions::Step::from_table(s).unwrap()
}

/// The retired-recovery refusal text (`what` = step key, job param or scope).
fn recover_refusal(what: &str) -> String {
    format!(
        "intake_enrich: {what} `recover` is no longer accepted; recovery is retired (enrich writes \
         no top block, so a stamped note without one is not a recovery target); use \
         `scope = \"strip-top\"` to remove legacy top blocks"
    )
}

/// WP-2 (e): the `recover` step key and `scope = "recover"` are refused, never mapped; an unknown
/// scope names the accepted values.
#[test]
fn recover_key_and_scope_are_refused() {
    let err = parse(&"scope = \"backfill\"\nrecover = true".parse().unwrap()).unwrap_err();
    assert_eq!(err, recover_refusal("step key"));
    let err = parse(&"scope = \"recover\"".parse().unwrap()).unwrap_err();
    assert_eq!(err, recover_refusal("scope"));
    let err = parse(&"scope = \"nope\"".parse().unwrap()).unwrap_err();
    assert!(err.contains("note|backfill|strip-top"), "{err}");
    assert!(parse(&"scope = \"strip-top\"".parse().unwrap()).is_ok());
}

#[test]
fn a_recover_job_param_is_refused() {
    let fx = LiveFixture::new([("Posts/n.md", "---\npost-id: \"1\"\n---\nbody\n")]);
    let e = StepEnv::new(fx, config("http://127.0.0.1:1/v1", "CV_TEST_KEY"), action());
    let rec = AtRecorder::default();
    let job = Map::from_iter([("recover".to_owned(), json!(true))]);
    let mut ctx = e.ctx();
    ctx.children = &rec;
    ctx.params = &job;
    let err = super::execute(&backfill_step(""), &mut ctx).unwrap_err();
    assert_eq!(err, StepError::failed(recover_refusal("job param")));
    assert!(rec.calls().is_empty());
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

/// WP-2 (d): `strip-top` removes only managed legacy blocks — the stamp, inserted headings and the
/// fold (with its metadata) stay byte-identical; a fenced literal marker, a user-authored
/// `[!abstract]` callout and a note without a block are never rewritten; a second run writes
/// nothing; no model call, nothing enqueued.
#[test]
fn strip_top_removes_only_legacy_blocks_and_is_idempotent() {
    let stamp = "---\npost-id: \"1\"\nenriched: 2026-10-01\nenrich-model: fake:m1\n---\n";
    let rest = "%% cv-enrich heading %%\n## Intro\n\nBody text.\n\n%% cv-enrich %%\n%% cv-fold n=1 kind=cta %%\n> [!removed]-\n> Buy now.\n\n%% /cv-enrich %%\n";
    let legacy = format!("{stamp}{LEGACY_TOP}{rest}");
    let fenced = format!("---\ntitle: F\n---\nIntro.\n\n```\n{LEGACY_TOP}```\n");
    let user = "---\ntitle: U\n---\n> [!abstract]- Mine\n> my summary\n\nText.\n";
    let plain = format!("{stamp}{rest}");
    let fx = LiveFixture::new([
        ("Posts/legacy.md", legacy.as_str()),
        ("Notes/fenced.md", fenced.as_str()),
        ("Notes/user.md", user),
        ("Posts/plain.md", plain.as_str()),
    ]);
    let e = StepEnv::new(fx, config("http://127.0.0.1:1/v1", "CV_TEST_KEY"), action());
    let rec = AtRecorder::default();
    let run = || {
        let mut ctx = e.ctx();
        ctx.children = &rec;
        super::execute(&strip_step(), &mut ctx).unwrap()
    };
    let o = run();
    assert_eq!(var(&o, "stripped"), json!(1));
    assert_eq!(var(&o, "strip_conflict"), json!(0));
    assert_eq!(var(&o, "heading_restored"), json!(0));
    assert_eq!(var(&o, "candidates"), json!(4));
    assert_eq!(read(&e, "Posts/legacy.md"), plain);
    assert_eq!(read(&e, "Notes/fenced.md"), fenced);
    assert_eq!(read(&e, "Notes/user.md"), user);
    assert_eq!(read(&e, "Posts/plain.md"), plain);
    assert!(rec.calls().is_empty());
    let o = run();
    assert_eq!(var(&o, "stripped"), json!(0));
    assert_eq!(read(&e, "Posts/legacy.md"), plain);
}

/// WP-2 (c)+(d): on a YouTube clip, strip-top then the lint cleanup then refine eligibility keep
/// the `## Transcript` section — with the heading present beforehand, and with it missing
/// beforehand (restored from the legacy ToC link before the block goes).
#[test]
fn strip_top_then_lint_then_refine_keeps_a_clip_transcript() {
    use cv_ingest::transcript::{Eligibility, eligibility};
    let fm =
        "---\nsource: https://www.youtube.com/watch?v=abcdefghijk\nenriched: 2026-10-01\n---\n";
    let long = "word ".repeat(cv_ingest::transcript::MIN_REFINE_WORDS);
    let present =
        format!("{fm}{LEGACY_TOP}## Transcript\n\n**0:01** {long}\n\n**0:05** b\n\n**0:09** c\n");
    let missing = format!("{fm}{LEGACY_TOP}{}\n", long.trim_end());
    let fx = LiveFixture::new([
        ("Clips/present.md", present.as_str()),
        ("Clips/missing.md", missing.as_str()),
    ]);
    let e = StepEnv::new(fx, config("http://127.0.0.1:1/v1", "CV_TEST_KEY"), action());
    let mut ctx = e.ctx();
    let rec = AtRecorder::default();
    ctx.children = &rec;
    let o = super::execute(&strip_step(), &mut ctx).unwrap();
    assert_eq!(var(&o, "stripped"), json!(2));
    assert_eq!(var(&o, "heading_restored"), json!(1));
    for p in ["Clips/present.md", "Clips/missing.md"] {
        let stripped = read(&e, p);
        assert!(!stripped.contains("[!abstract]"), "{stripped}");
        assert!(stripped.starts_with(fm), "{stripped}");
        let linted = cv_note::cleanup_youtube_transcript(&stripped);
        assert!(linted.contains("## Transcript\n\n"), "{p}: {linted}");
        assert!(
            matches!(
                eligibility(cv_note::FrontmatterDoc::new(linted.as_str()).body()),
                Eligibility::Ready { .. }
            ),
            "{p}: {linted}"
        );
    }
}

/// A legacy YouTube clip whose `## Transcript` heading a pre-WP-2 cleanup dropped (the block's ToC
/// still links it).
fn legacy_clip_without_heading() -> String {
    let para = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu ";
    format!(
        "---\ntitle: V\nsource: https://www.youtube.com/watch?v=abcdefghijk\n---\n{LEGACY_TOP}{}\n\n{CTA}\n",
        para.repeat(15).trim()
    )
}

/// WP-2 (c): the success write of a legacy clip restores the heading before dropping the block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn success_write_restores_a_legacy_clip_heading() {
    let (base, _mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("v.md", legacy_clip_without_heading());
    let o = exec(&e, "v.md", Map::new(), params(false)).await.unwrap();
    assert_eq!(var(&o, "enriched"), json!(true));
    let t = read(&e, "v.md");
    assert!(
        t.contains("## Transcript\n") && !t.contains("[!abstract]"),
        "{t}"
    );
}

/// WP-2 (c): the miss write of a legacy clip restores the heading before dropping the block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn miss_write_restores_a_legacy_clip_heading() {
    let (base, _mock) = fake(200, &unresolved_answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("v.md", legacy_clip_without_heading());
    let o = exec(&e, "v.md", Map::new(), params(false)).await.unwrap();
    assert_eq!(var(&o, "miss"), json!(true));
    let t = read(&e, "v.md");
    assert!(
        t.contains("## Transcript\n") && !t.contains("[!abstract]"),
        "{t}"
    );
    assert!(t.contains("\nenrich-miss: 1\n"), "{t}");
}
