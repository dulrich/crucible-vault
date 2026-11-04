//! `transcript_refine` tests (plan `youtube-transcript-auto-refine` WP-1): the step against a
//! `cv_testkit::MockHttp` posing as an `openai-compatible` provider, synthetic notes in a temp
//! vault. No real provider is ever called.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, ResourceSpec};
use cv_protocol::ActionContext;
use cv_state::FailureReason;
use cv_testkit::MockHttp;
use cv_vault::test_support::LiveFixture;
use serde_json::{Value, json};

use super::testkit::{ActionBuilder, Hook, StepEnv, completion, fake, mgr, providers};
use super::transcript_refine::{Params, parse, run_note};
use super::{DeferCause, StepError, StepOutcome};

/// The user message of a recorded chat-completions request.
fn user_text(r: &cv_testkit::Request) -> String {
    let v: Value = serde_json::from_str(&r.body_str()).unwrap();
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|m| m["role"] == "user")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A fake "refiner": answers `### Part\n\n<the user text>` (retention above 1, chapter marked).
fn echo() -> (String, MockHttp) {
    let m = MockHttp::start(|r| {
        cv_testkit::Reply::json(
            200,
            completion(&format!("### Part\n\n{}", user_text(r)), "stop"),
        )
    });
    (format!("{}/v1", m.url()), m)
}

const AGENTS: &str = r#"
[transcript-refine]
name = "Transcript Refine"
prompt = "{{value}}"
input = "{{value}}"
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

const YT: &str = "https://www.youtube.com/watch?v=abcdefghijk";
const RAW: &str = "0:01 so um today we talk about the thing you know 0:09 and then another part of the talk follows with more words 0:20 until the end of it";
const EMBED: &str = "![](https://www.youtube.com/watch?v=abcdefghijk)";
const DESC: &str = "Description line OUTSIDE the transcript.";
const REFINED: &str = "### Intro\n\nToday we talk about the thing and then another part of the talk follows with more words until the end of it.";

fn note_with(tags: &str, source: &str, section: &str, tail: &str) -> String {
    format!(
        "---\ntitle: V\nsource: {source}\n{tags}---\n\n{EMBED}\n\n{DESC}\n\n## Transcript\n\n{section}\n\n{tail}"
    )
}

fn yt_note() -> String {
    note_with("tags:\n  - clippings\n", YT, RAW, "## Comments\n\nc1\n")
}

fn read(e: &StepEnv, rel: &str) -> String {
    std::fs::read_to_string(e.fx.path(rel)).unwrap()
}

fn raw_dir_absent(e: &StepEnv) -> bool {
    !e.fx.path("_raw_transcript").exists()
}

/// The tiny fixtures sit far under the real `MIN_REFINE_WORDS` floor, so it is lowered here;
/// `a_short_transcript_is_skipped_at_the_default_floor` covers the real value.
fn params() -> Params {
    let mut p = parse(&toml::Table::new()).unwrap();
    p.min_words = 0;
    p
}

/// Runs the step on a blocking thread inside the runtime hosting the fake provider.
async fn exec(e: &Arc<StepEnv>, path: &str) -> Result<StepOutcome, StepError> {
    let (e, path) = (Arc::clone(e), path.to_owned());
    tokio::task::spawn_blocking(move || {
        let context = ActionContext {
            active_path: Some(path),
            ..ActionContext::default()
        };
        let mut ctx = e.ctx();
        ctx.context = &context;
        run_note(&params(), &mut ctx, &mgr())
    })
    .await
    .unwrap()
}

fn var(o: &StepOutcome, k: &str) -> Value {
    o.vars.get(k).cloned().unwrap_or(Value::Null)
}

fn tags(text: &str) -> Vec<String> {
    cv_index::note_tags(text)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_chunk_refine_replaces_only_the_section_and_keeps_a_raw_copy() {
    let (base, mock) = fake(200, REFINED, "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("Clips/n.md", yt_note());
    let o = exec(&e, "Clips/n.md").await.unwrap();
    let t = read(&e, "Clips/n.md");
    // Only the section text changed; heading line, embed, description and tail are untouched.
    assert!(
        t.ends_with(&format!(
            "{EMBED}\n\n{DESC}\n\n## Transcript\n\n{REFINED}\n\n## Comments\n\nc1\n"
        )),
        "{t}"
    );
    assert!(
        t.contains("\ntitle: V\nsource: https://www.youtube.com/watch?v=abcdefghijk\n"),
        "{t}"
    );
    assert!(tags(&t).iter().any(|x| x == "refined"), "{t}");
    assert!(tags(&t).iter().any(|x| x == "clippings"), "{t}");
    assert!(t.contains("\nrefine-model: fake:m1\n"), "{t}");
    assert!(t.contains("[[_raw_transcript/n]]"), "{t}");
    // The raw copy is the original note, byte for byte.
    assert_eq!(read(&e, "_raw_transcript/n.md"), yt_note());
    // The model got only the chunk text.
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(user_text(&reqs[0]), RAW);
    assert!(!reqs[0].body_str().contains(DESC));
    assert_eq!(var(&o, "chunks"), json!(1));
    assert_eq!(var(&o, "headings"), json!(1));
    assert_eq!(
        var(&o, "words_in"),
        json!(cv_ingest::transcript::words(RAW))
    );
    assert!(o.output_paths.contains(&"Clips/n.md".to_owned()));
    assert!(o.output_paths.contains(&"_raw_transcript/n.md".to_owned()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skips_make_no_model_call_and_no_write() {
    let (base, mock) = fake(200, REFINED, "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    let cases = [
        (
            "a.md",
            note_with("", "https://example.com/post", RAW, ""),
            "not youtube",
        ),
        (
            "b.md",
            note_with("tags:\n  - refined\n", YT, RAW, ""),
            "already refined",
        ),
        (
            "c.md",
            format!("---\nsource: {YT}\n---\n\nno section here\n"),
            "no transcript",
        ),
        (
            "d.md",
            format!("---\nsource: {YT}\n---\n\n## Transcript\n\n## Next\n\nx\n"),
            "no transcript",
        ),
        (
            "e.md",
            format!(
                "---\nsource: {YT}\n---\n\n## Transcript\n\n%% cv-enrich %%\n> [!removed]-\n> x\n\n%% /cv-enrich %%\n"
            ),
            "no transcript",
        ),
        ("f.md", "no frontmatter\n".to_owned(), "not youtube"),
    ];
    for (name, text, why) in cases {
        e.fx.write(name, text.clone());
        let o = exec(&e, name).await.unwrap();
        assert_eq!(var(&o, "skipped"), json!(why), "{name}");
        assert_eq!(read(&e, name), text, "{name}");
    }
    assert!(mock.requests().is_empty());
    assert!(raw_dir_absent(&e));
}

/// A transcript under `MIN_REFINE_WORDS` is skipped before any model call (live: 173/196-word
/// transcripts failed retention on every backfill pass); one at the floor is refined.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_transcript_is_skipped_at_the_default_floor() {
    let (base, mock) = echo();
    let e = env(&base, "CV_TEST_KEY");
    let floor = cv_ingest::transcript::MIN_REFINE_WORDS;
    assert_eq!(parse(&toml::Table::new()).unwrap().min_words, floor);
    let run = |name: &'static str, words: usize| {
        let e = Arc::clone(&e);
        async move {
            let text = note_with("", YT, &"word ".repeat(words), "");
            e.fx.write(name, text.clone());
            let (e2, n) = (Arc::clone(&e), name.to_owned());
            let o = tokio::task::spawn_blocking(move || {
                let context = ActionContext {
                    active_path: Some(n),
                    ..ActionContext::default()
                };
                let mut ctx = e2.ctx();
                ctx.context = &context;
                run_note(&parse(&toml::Table::new()).unwrap(), &mut ctx, &mgr())
            })
            .await
            .unwrap()
            .unwrap();
            (o, text, read(&e, name))
        }
    };
    let (o, before, after) = run("short.md", floor - 1).await;
    assert_eq!(var(&o, "skipped"), json!("short transcript"));
    assert_eq!(after, before);
    assert!(mock.requests().is_empty());
    let (o, _, _) = run("long.md", floor).await;
    assert_eq!(var(&o, "chunks"), json!(1));
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_run_is_skipped_as_already_refined() {
    let (base, mock) = fake(200, REFINED, "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", yt_note());
    exec(&e, "n.md").await.unwrap();
    let after = read(&e, "n.md");
    let o = exec(&e, "n.md").await.unwrap();
    assert_eq!(var(&o, "skipped"), json!("already refined"));
    assert_eq!(read(&e, "n.md"), after);
    assert_eq!(mock.requests().len(), 1);
}

/// Plan `tn-review-2026-11-04-remediation` Decision 5: the refine write clears the `enriched`
/// stamp (and only it), so the following `intake_enrich` re-enriches the refined text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refine_write_clears_the_enriched_stamp() {
    let (base, _mock) = fake(200, REFINED, "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    let before = note_with("enriched: 2026-10-01\nenrich-model: fake:m1\n", YT, RAW, "");
    e.fx.write("n.md", before);
    exec(&e, "n.md").await.unwrap();
    let t = read(&e, "n.md");
    assert!(!t.contains("enriched:"), "{t}");
    assert!(t.contains("\nenrich-model: fake:m1\n"), "{t}");
    assert!(!super::intake_enrich::already_enriched(&t), "{t}");
    assert!(t.contains(REFINED), "{t}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_section_is_chunked_and_concatenated_in_order() {
    let (base, mock) = echo();
    let e = env(&base, "CV_TEST_KEY");
    // 130 paragraphs x 100 words = 13,000 words -> two chunks of 6,500.
    let para = |i: usize| format!("p{i:03} {}", "word ".repeat(99).trim());
    let section = (0..130).map(para).collect::<Vec<_>>().join("\n\n");
    e.fx.write("n.md", note_with("", YT, &section, ""));
    let o = exec(&e, "n.md").await.unwrap();
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 2);
    let (c1, c2) = (user_text(&reqs[0]), user_text(&reqs[1]));
    assert!(
        c1.starts_with("p000") && c2.contains("p129"),
        "{}",
        &c2[..40]
    );
    assert_eq!(format!("{c1}\n\n{c2}"), section);
    let t = read(&e, "n.md");
    assert!(
        t.contains(&format!("### Part\n\n{c1}\n\n### Part\n\n{c2}")),
        "len {}",
        t.len()
    );
    assert_eq!(var(&o, "chunks"), json!(2));
    assert_eq!(var(&o, "headings"), json!(2));
    assert_eq!(var(&o, "words_in"), json!(13_000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_low_retention_answer_fails_and_writes_nothing() {
    let (base, mock) = fake(200, "### Summary\n\nshort", "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", yt_note());
    let err = exec(&e, "n.md").await.unwrap_err();
    assert!(
        matches!(&err, StepError::Failed { reason: None, .. }),
        "{err:?}"
    );
    assert_eq!(read(&e, "n.md"), yt_note());
    assert!(raw_dir_absent(&e));
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_second_chunk_writes_nothing_even_after_a_good_first() {
    let calls = Arc::new(Mutex::new(0_usize));
    let c2 = Arc::clone(&calls);
    let m = MockHttp::start(move |r| {
        let n = {
            let mut g = c2.lock().unwrap();
            *g += 1;
            *g
        };
        if n == 1 {
            cv_testkit::Reply::json(200, completion(&user_text(r), "stop"))
        } else {
            cv_testkit::Reply::json(200, completion("tiny", "stop"))
        }
    });
    let e = env(&format!("{}/v1", m.url()), "CV_TEST_KEY");
    let para = |i: usize| format!("p{i:03} {}", "word ".repeat(99).trim());
    let section = (0..130).map(para).collect::<Vec<_>>().join("\n\n");
    let original = note_with("", YT, &section, "");
    e.fx.write("n.md", original.clone());
    exec(&e, "n.md").await.unwrap_err();
    assert_eq!(read(&e, "n.md"), original);
    assert!(raw_dir_absent(&e));
    assert_eq!(*calls.lock().unwrap(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncation_empty_output_and_outages_fail_or_defer_without_writing() {
    for (text, finish) in [(REFINED, "length"), ("", "stop"), ("   \n", "stop")] {
        let (base, _mock) = fake(200, text, finish, None);
        let e = env(&base, "CV_TEST_KEY");
        e.fx.write("n.md", yt_note());
        let err = exec(&e, "n.md").await.unwrap_err();
        assert!(
            matches!(&err, StepError::Failed { reason: None, .. }),
            "{finish} {err:?}"
        );
        assert_eq!(read(&e, "n.md"), yt_note());
        assert!(raw_dir_absent(&e));
    }
    let (base, _mock) = fake(503, "{}", "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", yt_note());
    let err = exec(&e, "n.md").await.unwrap_err();
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
    assert_eq!(read(&e, "n.md"), yt_note());
    assert!(raw_dir_absent(&e));

    let (base, mock) = fake(200, REFINED, "stop", None);
    let e = env(&base, "CV_TEST_MISSING");
    e.fx.write("n.md", yt_note());
    let err = exec(&e, "n.md").await.unwrap_err();
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_enrich_blocks_survive_byte_identical() {
    let top = "%% cv-enrich %%\n> [!abstract]-\n> TOPSUMMARY\n\n%% /cv-enrich %%\n";
    let fold = "%% cv-enrich %%\n> [!removed]-\n> Subscribe FOLDED line\n\n%% /cv-enrich %%\n";
    // The transcript is the last section, so the trailing fold sits inside its range.
    let original = format!(
        "---\ntitle: V\nsource: {YT}\n---\n\n{top}\n{EMBED}\n\n## Transcript\n\n{RAW}\n\n{fold}"
    );
    let (base, mock) = fake(200, REFINED, "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", original.clone());
    exec(&e, "n.md").await.unwrap();
    let body = user_text(&mock.requests()[0]);
    assert_eq!(body, RAW);
    assert!(!body.contains("FOLDED") && !body.contains("TOPSUMMARY"));
    let t = read(&e, "n.md");
    assert!(t.contains(top), "{t}");
    assert!(
        t.ends_with(&format!("## Transcript\n\n{REFINED}\n\n{fold}")),
        "{t}"
    );
    assert_eq!(t.matches("cv-enrich %%").count(), 4, "two open/close pairs");
    assert_eq!(read(&e, "_raw_transcript/n.md"), original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_during_the_model_call_conflicts_and_changes_nothing() {
    let slot: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let s2 = Arc::clone(&slot);
    let hook: Hook = Arc::new(move || {
        if let Some(p) = s2.lock().unwrap().as_ref() {
            std::fs::write(p, "user edit\n").unwrap();
        }
    });
    let (base, _mock) = fake(200, REFINED, "stop", Some(hook));
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("c.md", yt_note());
    *slot.lock().unwrap() = Some(e.fx.path("c.md"));
    let err = exec(&e, "c.md").await.unwrap_err();
    assert_eq!(
        err,
        StepError::Deferred {
            retry_after_ms: super::CONFLICT_RETRY_MS,
            service: None,
            cause: DeferCause::Wait,
        }
    );
    assert_eq!(read(&e, "c.md"), "user edit\n");
    assert!(raw_dir_absent(&e));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_file_at_the_raw_path_is_never_overwritten() {
    let (base, _mock) = fake(200, REFINED, "stop", None);
    let e = env(&base, "CV_TEST_KEY");
    e.fx.write("n.md", yt_note());
    e.fx.write("_raw_transcript/n.md", "an older raw copy\n");
    exec(&e, "n.md").await.unwrap();
    assert_eq!(read(&e, "_raw_transcript/n.md"), "an older raw copy\n");
    assert_eq!(read(&e, "_raw_transcript/n-2.md"), yt_note());
    assert!(read(&e, "n.md").contains("[[_raw_transcript/n-2]]"));
}

#[test]
fn params_parse() {
    assert!(parse(&toml::Table::new()).is_ok());
    assert!(parse(&"agent = \"x\"".parse().unwrap()).is_ok());
    assert!(parse(&"agent = \" \"".parse().unwrap()).is_err());
    assert!(parse(&"agent = 3".parse().unwrap()).is_err());
    assert!(parse(&"invented = 1".parse().unwrap()).is_err());
}

/// Every shipped agent whose `prompt_file` is a shipped example prompt must place its input in
/// that prompt: the agent step sends only the rendered prompt as the user message, so a prompt
/// without `{{value}}`/`{{input}}` (or `{{content}}`/`{{selection}}`) drops the input entirely.
/// Live 2026-10-30: the first `Refine Transcript Chapters.md` had none, and every refine came back
/// as a 7-word answer.
#[test]
fn shipped_prompts_carry_their_input() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/examples");
    let agents: toml::Table = std::fs::read_to_string(root.join("agents.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let mut checked = 0;
    for (id, def) in &agents {
        let Some(pf) = def.get("prompt_file").and_then(toml::Value::as_str) else {
            continue;
        };
        let name = pf.rsplit('/').next().unwrap();
        let Ok(text) = std::fs::read_to_string(root.join("prompts").join(name)) else {
            continue; // a live-only prompt (not shipped in the repo)
        };
        let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            ["{{value}}", "{{input}}", "{{content}}", "{{selection}}"]
                .iter()
                .any(|p| squeezed.contains(p)),
            "agent {id}: {name} has no input placeholder"
        );
        checked += 1;
    }
    assert!(checked >= 2, "expected the shipped refine + enrich prompts");
}
