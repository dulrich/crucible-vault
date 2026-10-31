//! `intake_enrich` tests (plan `intake-auto-enrich` WP-3): the step against a
//! `cv_testkit::MockHttp` posing as an `openai-compatible` provider, synthetic notes in a temp
//! vault. No real provider is ever called.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec};
use cv_protocol::ActionContext;
use cv_providers::{ProviderManager, SecretSource};
use cv_state::FailureReason;
use cv_testkit::MockHttp;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value, json};

use super::intake_enrich::{
    Params, already_enriched, effective_limit, is_youtube_source, parse, pick_backfill, run_note,
};
use super::testkit::{AtRecorder, StepEnv};
use super::{DeferCause, StepError, StepOutcome};

struct TestSecrets(HashMap<String, String>);
impl SecretSource for TestSecrets {
    fn env_var(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
    fn secret_file(&self, _name: &str) -> Option<String> {
        None
    }
}

fn mgr() -> ProviderManager {
    let mut m = HashMap::new();
    m.insert("CV_TEST_KEY".to_owned(), "test-key".to_owned());
    ProviderManager::new(Arc::new(TestSecrets(m))).unwrap()
}

type Hook = Arc<dyn Fn() + Send + Sync>;

/// A fake provider answering every request with `text` (finish `finish`), counting requests.
fn fake(status: u16, text: &str, finish: &str, hook: Option<Hook>) -> (String, MockHttp) {
    let body =
        json!({"choices": [{"message": {"content": text}, "finish_reason": finish}]}).to_string();
    let m = MockHttp::start(move |_| {
        if let Some(h) = &hook {
            h();
        }
        cv_testkit::Reply::json(status, body.clone())
    });
    (format!("{}/v1", m.url()), m)
}

const AGENTS: &str = r#"
[intake-enrich]
name = "Intake Enrich"
prompt = "{{value}}"
input = "{{content}}"
model = "fake:m1"
max_tokens = 100
"#;

fn providers(base: &str, key_env: &str) -> String {
    format!(
        r#"
[[provider]]
id = "fake"
kind = "openai-compatible"
base_url = "{base}"
api_key_env = "{key_env}"
[[provider.models]]
id = "m1"
"#
    )
}

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
    Action {
        id: "intake-enrich".into(),
        label: "Intake: enrich note".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec {
            note: true,
            keys: vec!["clip::enrich".into()],
        },
    }
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

#[test]
fn params_and_pure_selection() {
    assert!(parse(&"scope = \"nope\"".parse().unwrap()).is_err());
    assert!(parse(&"limit = 0".parse().unwrap()).is_err());
    assert!(parse(&"invented = 1".parse().unwrap()).is_err());
    assert!(parse(&"scope = \"backfill\"\nlimit = 5".parse().unwrap()).is_ok());

    assert!(is_youtube_source("https://www.youtube.com/watch?v=abc"));
    assert!(is_youtube_source("https://youtu.be/abc"));
    assert!(!is_youtube_source("https://notyoutube.com/x"));
    assert!(!is_youtube_source("https://example.com/youtube.com"));

    let cands: BTreeMap<String, i64> = [("a.md", 1), ("b.md", 3), ("c.md", 2), ("d.md", 3)]
        .into_iter()
        .map(|(p, c)| (p.to_owned(), c))
        .collect();
    let done: BTreeSet<String> = ["d.md".to_owned()].into();
    assert_eq!(pick_backfill(&cands, &done, 2), ["b.md", "c.md"]);
    assert_eq!(pick_backfill(&cands, &BTreeSet::new(), 10).len(), 4);

    let job = |v: Value| Map::from_iter([("limit".to_owned(), v)]);
    assert_eq!(effective_limit(&Map::new(), 20), Ok(20));
    assert_eq!(effective_limit(&job(json!("5")), 20), Ok(5));
    assert_eq!(effective_limit(&job(json!(7)), 20), Ok(7));
    for bad in [
        json!("0"),
        json!("2.5"),
        json!("x"),
        json!(-1),
        json!(1.5),
        json!(true),
    ] {
        assert!(effective_limit(&job(bad.clone()), 20).is_err(), "{bad}");
    }
}

#[test]
fn backfill_enqueues_only_unenriched_intake_notes_newest_first() {
    let fx = LiveFixture::new([
        ("Posts/blog.md", "---\npost-id: \"1\"\n---\nbody\n"),
        ("Posts/mail.md", "---\nmessage-id: \"<m@x>\"\n---\nbody\n"),
        (
            "Clips/yt.md",
            "---\nsource: https://www.youtube.com/watch?v=abc\n---\nbody\n",
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

/// Like [`exec`] with the earlier steps' vars (what the runner hands a later step).
async fn exec_vars(
    e: &Arc<StepEnv>,
    path: &str,
    job_params: Map<String, Value>,
    vars: Map<String, Value>,
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
        ctx.vars = &vars;
        run_note(&params(false), &mut ctx, &mgr())
    })
    .await
    .unwrap()
}

/// Plan `youtube-transcript-auto-refine` WP-1 (audit E1): a refine that rewrote the note forces
/// the re-enrich even though the job's `force` param is present and `"false"` (the action's
/// default), and an unset flag changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refine_that_wrote_forces_enrich_over_a_false_force_param() {
    let (base, mock) = fake(200, &answer(), "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", long_note());
    exec(&e, "n.md", Map::new(), params(false)).await.unwrap();
    assert_eq!(mock.requests().len(), 1);
    let false_force = || Map::from_iter([("force".to_owned(), json!("false"))]);
    let flag = |v: Value| Map::from_iter([("transcript_refined".to_owned(), v)]);

    // No refine var (or false): skipped as before, whatever the param says.
    for vars in [Map::new(), flag(json!(false))] {
        let o = exec_vars(&e, "n.md", false_force(), vars).await.unwrap();
        assert_eq!(var(&o, "skipped"), json!("already enriched"));
    }
    assert_eq!(mock.requests().len(), 1);

    // Refine wrote: forced, with the param absent, `"false"` and boolean `false`.
    for job in [
        Map::new(),
        false_force(),
        Map::from_iter([("force".to_owned(), json!(false))]),
    ] {
        let before = mock.requests().len();
        let o = exec_vars(&e, "n.md", job, flag(json!(true))).await.unwrap();
        assert_eq!(var(&o, "enriched"), json!(true));
        assert_eq!(mock.requests().len(), before + 1);
    }
    let t = read(&e, "n.md");
    assert_eq!(t.matches("[!removed]-").count(), 1, "{t}");
}
