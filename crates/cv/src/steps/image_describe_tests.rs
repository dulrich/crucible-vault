//! WP-6 `image_describe` tests: synthetic temp vault + temp `state.db`, and a `cv_testkit::MockHttp`
//! responder posing as an `openai-compatible` provider. No real provider is ever called.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_providers::{ProviderManager, SecretSource};
use cv_state::{FailureReason, ImageDescription};
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::{Value, json};

use super::{
    MAX_LONG_EDGE, describe_wire, extract_svg_text, fit_long_edge, fnv1a32_hex, is_repetition_loop,
    parse, prepare_image, run,
};
use crate::steps::testkit::StepEnv;
use crate::steps::{DeferCause, StepError, StepOutcome};

const MD5_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MD5_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct TestSecrets;
impl SecretSource for TestSecrets {
    fn env_var(&self, name: &str) -> Option<String> {
        (name == "CV_TEST_KEY").then(|| "test-key".to_owned())
    }
    fn secret_file(&self, _name: &str) -> Option<String> {
        None
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageLuma8(image::GrayImage::new(w, h));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Serves `replies[i]` to the i-th request (the last repeats); records requests.
async fn fake_provider(replies: Vec<(u16, String)>) -> (String, Seen) {
    let m = MockHttp::script(
        replies
            .into_iter()
            .map(|(status, body)| Reply::json(status, body))
            .collect(),
    );
    (format!("{}/v1", m.url()), Seen(m))
}

/// The JSON request bodies the fake provider received.
struct Seen(MockHttp);
impl Seen {
    fn bodies(&self) -> Vec<Value> {
        self.0
            .requests()
            .iter()
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect()
    }
}

fn ok(text: &str) -> (u16, String) {
    (
        200,
        json!({"choices": [{"message": {"content": text}, "finish_reason": "stop"}]}).to_string(),
    )
}

fn config(base: &str, enabled: bool, provider: &str) -> ConfigSet {
    config_with(base, enabled, provider, "", "")
}

/// `config` plus raw extra lines for the `fake` provider table and the `[image_describe]` table.
fn config_with(
    base: &str,
    enabled: bool,
    provider: &str,
    provider_extra: &str,
    describe_extra: &str,
) -> ConfigSet {
    let providers = format!(
        r#"
[[provider]]
id = "fake"
kind = "openai-compatible"
base_url = "{base}"
api_key_env = "CV_TEST_KEY"
{provider_extra}
[[provider.models]]
id = "vl"

[[provider]]
id = "nokey"
kind = "anthropic"
base_url = "{base}"
api_key_env = "CV_TEST_MISSING"
[[provider.models]]
id = "vl"
"#
    );
    let pipelines = format!(
        "[image_describe]\nenabled = {enabled}\nprovider = \"{provider}\"\nmodel = \"vl\"\n{describe_extra}\n"
    );
    let mut c = ConfigSet::new();
    let files = BTreeMap::from([
        ("providers.toml".to_owned(), Some(providers)),
        ("pipelines.toml".to_owned(), Some(pipelines)),
    ]);
    c.reload(&files, &crate::steps::validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

type Env = StepEnv;

fn env(files: &[(&str, Vec<u8>)]) -> Env {
    let action = Action {
        id: "image-describe-note".into(),
        label: "Describe".into(),
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
            keys: vec![],
        },
    };
    let mut env = StepEnv::new(
        LiveFixture::new(files.iter().map(|(r, b)| (*r, b))),
        ConfigSet::new(),
        action,
    );
    env.context.active_path = Some("Clip.md".into());
    env
}

/// A note embedding one png (`MD5_A`) — plus whatever `extra` adds.
fn clip_env(img: Vec<u8>, extra: &[(&str, Vec<u8>)]) -> Env {
    let mut files = vec![
        (
            "Clip.md",
            format!("# Clip\n![[{MD5_A}_MD5.png]]\n").into_bytes(),
        ),
        ("att/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa_MD5.png", img),
    ];
    files.extend(extra.iter().cloned());
    env(&files)
}

fn exec(e: &Env, cfg: &ConfigSet, src: &str) -> Result<StepOutcome, StepError> {
    let mut ctx = e.ctx();
    ctx.config = cfg;
    let t: toml::Table = format!("type = \"image_describe\"\n{src}").parse().unwrap();
    let mgr = ProviderManager::new(Arc::new(TestSecrets)).unwrap();
    run(
        &parse(&Step::from_table(t).unwrap().params).unwrap(),
        &mut ctx,
        &mgr,
    )
}

async fn exec_async(
    e: Arc<Env>,
    cfg: ConfigSet,
    src: &'static str,
) -> Result<StepOutcome, StepError> {
    tokio::task::spawn_blocking(move || exec(&e, &cfg, src))
        .await
        .unwrap()
}

fn rec(e: &Env, md5: &str) -> Option<ImageDescription> {
    e.state.lock().unwrap().image_description(md5).unwrap()
}

fn var(o: &StepOutcome, k: &str) -> u64 {
    o.vars.get(k).and_then(Value::as_u64).unwrap()
}

// ---- pure ----

#[test]
fn fnv1a_matches_crucible() {
    // FNV-1a 32 reference vectors (Crucible's `fnv1a` is the standard algorithm over UTF-16).
    assert_eq!(fnv1a32_hex(""), "811c9dc5");
    assert_eq!(fnv1a32_hex("a"), "e40c292c");
    assert_eq!(fnv1a32_hex("foobar"), "bf9cf968");
}

#[test]
fn svg_text_extraction() {
    let svg = r#"<svg><title>Q3  revenue</title><g><text x="1">Up <tspan>12&amp;%</tspan></text></g><rect/></svg>"#;
    assert_eq!(extract_svg_text(svg), "Q3 revenue\nUp 12&%");
}

#[test]
fn downscale_bound_and_passthrough() {
    assert_eq!(fit_long_edge(4000, 1000), (MAX_LONG_EDGE, 392));
    assert_eq!(fit_long_edge(1000, 4000), (392, MAX_LONG_EDGE));
    assert_eq!(fit_long_edge(1568, 10), (1568, 10));
    // A real decode + resize, kept thin.
    let (mime, out) = prepare_image(&png(4000, 4)).unwrap();
    assert_eq!(mime, "image/png");
    let img = image::load_from_memory(&out).unwrap();
    assert_eq!((img.width(), img.height()), (MAX_LONG_EDGE, 2));
    let small = png(40, 30);
    assert_eq!(prepare_image(&small).unwrap(), ("image/png".into(), small));
    assert!(prepare_image(b"not an image").is_err());
}

// ---- step ----

#[test]
fn disabled_is_a_noop() {
    let e = clip_env(png(8, 8), &[]);
    let cfg = config("http://127.0.0.1:1/v1", false, "fake");
    let out = exec(&e, &cfg, "").unwrap();
    assert_eq!(out.vars.get("skipped"), Some(&json!("disabled")));
    assert!(rec(&e, MD5_A).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn describes_then_skips_recorded() {
    let (base, seen) = fake_provider(vec![ok("A bar chart."), ok("Title: Q3")]).await;
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!(var(&out, "described"), 1);
    let r = rec(&e, MD5_A).unwrap();
    assert_eq!(
        (r.kind.as_str(), r.narrative.as_str()),
        ("vision", "A bar chart.")
    );
    assert_eq!(r.description_hash, fnv1a32_hex("A bar chart.\nTitle: Q3"));
    let raw: Value = serde_json::from_str(&r.raw).unwrap();
    assert_eq!(raw["providerId"], "fake");
    assert_eq!(raw["schemaVersion"], 1);
    assert!(!r.raw.contains("base64"));
    {
        let bodies = seen.bodies();
        assert_eq!(bodies.len(), 2);
        let msgs = bodies[0]["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "no system message");
        // Loopback provider = local: Crucible's per-pass caps plus the measured sampler.
        assert_eq!(bodies[0]["max_tokens"], 512);
        assert_eq!(bodies[1]["max_tokens"], 2048);
        for b in &bodies {
            assert_eq!(b["reasoning_effort"], "none");
            assert_eq!(b["temperature"], 1.0);
            assert_eq!(b["top_p"], 0.95);
            assert_eq!(b["top_k"], 64);
            assert_eq!(b["repeat_penalty"], 1.15);
            assert_eq!(b["repeat_last_n"], 256);
        }
        let url = msgs[0]["content"][1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }
    // Second run: already described → skipped, no request.
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!((var(&out, "described"), var(&out, "skipped")), (0, 1));
    assert_eq!(seen.bodies().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_record_is_poison_skipped() {
    let (base, seen) = fake_provider(vec![ok("x")]).await;
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let m = super::Model {
        provider_id: "fake",
        model_id: "m",
    };
    let failed = super::record(MD5_A, "failed", "", "", Some(&m), Some("boom"));
    e.state
        .lock()
        .unwrap()
        .put_image_description(&failed)
        .unwrap();
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!(var(&out, "skipped"), 1);
    assert!(seen.bodies().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_failures_write_no_record() {
    for status in [500u16, 503, 429, 408] {
        let (base, _mock) = fake_provider(vec![(status, "{}".into())]).await;
        let e = Arc::new(clip_env(png(8, 8), &[]));
        let r = exec_async(Arc::clone(&e), config(&base, true, "fake"), "").await;
        assert!(
            matches!(
                r,
                Err(StepError::Deferred {
                    cause: DeferCause::Transient(_),
                    ..
                })
            ),
            "{status}: {r:?}"
        );
        assert!(rec(&e, MD5_A).is_none(), "{status}");
    }
    // Connection refused (transport) → deferred, no record.
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let r = exec_async(
        Arc::clone(&e),
        config("http://127.0.0.1:1/v1", true, "fake"),
        "",
    )
    .await;
    assert!(matches!(r, Err(StepError::Deferred { .. })), "{r:?}");
    assert!(rec(&e, MD5_A).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_key_fails_without_record() {
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let r = exec_async(
        Arc::clone(&e),
        config("http://127.0.0.1:1", true, "nokey"),
        "",
    )
    .await;
    assert!(
        matches!(
            r,
            Err(StepError::Failed {
                reason: Some(FailureReason::NoApiKey),
                ..
            })
        ),
        "{r:?}"
    );
    assert!(rec(&e, MD5_A).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_specific_failures_record_permanent() {
    // Undecodable bytes: failed, no model call.
    let (base, seen) = fake_provider(vec![ok("x")]).await;
    let e = Arc::new(clip_env(b"garbage".to_vec(), &[]));
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!(var(&out, "failed"), 1);
    let r = rec(&e, MD5_A).unwrap();
    let raw: Value = serde_json::from_str(&r.raw).unwrap();
    assert_eq!(
        (r.kind.as_str(), raw["failureClass"].as_str()),
        ("failed", Some("permanent"))
    );
    assert!(seen.bodies().is_empty());

    // Empty answer and a 400 → failed.
    for reply in [ok("  "), (400, "{\"error\":\"bad image\"}".to_owned())] {
        let (base, _mock) = fake_provider(vec![reply]).await;
        let e = Arc::new(clip_env(png(8, 8), &[]));
        let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
            .await
            .unwrap();
        assert_eq!(var(&out, "failed"), 1);
        assert_eq!(rec(&e, MD5_A).unwrap().kind, "failed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn svg_and_vault_scope_and_limit() {
    let (base, seen) = fake_provider(vec![ok("n"), ok("x")]).await;
    let svg = b"<svg><text>Hello</text></svg>".to_vec();
    let e = Arc::new(clip_env(
        png(8, 8),
        &[
            ("att/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb_MD5.svg", svg),
            ("att/plain.png", png(4, 4)),
        ],
    ));
    // Vault scope, limit 1: the png (a…) first, the svg waits.
    let out = exec_async(
        Arc::clone(&e),
        config(&base, true, "fake"),
        "scope = \"vault\"\nlimit = 1",
    )
    .await
    .unwrap();
    assert_eq!(
        (
            var(&out, "described"),
            var(&out, "remaining"),
            var(&out, "skipped")
        ),
        (1, 1, 1)
    );
    let out = exec_async(
        Arc::clone(&e),
        config(&base, true, "fake"),
        "scope = \"vault\"",
    )
    .await
    .unwrap();
    assert_eq!(var(&out, "svg"), 1);
    let r = rec(&e, MD5_B).unwrap();
    assert_eq!(
        (r.kind.as_str(), r.extraction.as_str()),
        ("svg-text", "Hello")
    );
    assert_eq!(seen.bodies().len(), 2, "svg made no model call");
}

#[test]
fn prune_transient_and_degenerate_only() {
    let e = clip_env(png(8, 8), &[]);
    let mut st = e.state.lock().unwrap();
    let mk = |md5: &str, kind: &str, extraction: &str, raw: Value| ImageDescription {
        md5: md5.into(),
        kind: kind.into(),
        narrative: String::new(),
        extraction: extraction.into(),
        description_hash: "0".into(),
        described_at: "t".into(),
        raw: raw.to_string(),
    };
    let big = "x".repeat(20_001);
    for d in [
        mk("t1", "failed", "", json!({"failureClass": "transient"})),
        mk(
            "t2",
            "failed",
            "",
            json!({"failure": "request timed out after 120000ms"}),
        ),
        mk("p1", "failed", "", json!({"failureClass": "permanent"})),
        mk("d1", "vision", &big, json!({})),
        mk("s1", "svg-text", &big, json!({})),
        mk("v1", "vision", "ok", json!({})),
    ] {
        st.put_image_description(&d).unwrap();
    }
    assert_eq!(st.prune_transient_failed_image_descriptions().unwrap(), 2);
    assert_eq!(st.prune_degenerate_image_descriptions(20_000).unwrap(), 1);
    let left: Vec<String> = ["t1", "t2", "p1", "d1", "s1", "v1"]
        .iter()
        .filter(|m| st.image_description(m).unwrap().is_some())
        .map(|m| (*m).to_owned())
        .collect();
    assert_eq!(left, ["p1", "s1", "v1"]);
}

// ---- round 16 WP-2: wire, `length`, retry ----

fn length(text: &str) -> (u16, String) {
    (
        200,
        json!({"choices": [{"message": {"content": text}, "finish_reason": "length"}]}).to_string(),
    )
}

fn provider_of(base: &str, extra: &str) -> cv_providers::config::ProviderConfig {
    cv_providers::config::ProvidersConfig::from_toml(&format!(
        "[[provider]]\nid = \"p\"\nkind = \"openai-compatible\"\nbase_url = \"{base}\"\n{extra}"
    ))
    .unwrap()
    .providers
    .remove(0)
}

#[test]
fn wire_is_local_gated_and_override_caps_both_passes() {
    let local = describe_wire(&provider_of("http://127.0.0.1:1/v1", ""), None);
    assert_eq!((local.narrative_cap, local.extraction_cap), (512, 2048));
    assert_eq!(local.extras.reasoning_effort.as_deref(), Some("none"));
    assert_eq!(local.extras.top_k, Some(64));
    let over = describe_wire(&provider_of("http://127.0.0.1:1/v1", ""), Some(800));
    assert_eq!((over.narrative_cap, over.extraction_cap), (800, 800));
    // Remote: nothing extra, the single 4096 cap as before; explicit `local = true` flips it.
    let remote = describe_wire(&provider_of("https://api.example.com/v1", ""), None);
    assert_eq!(remote.extras, cv_providers::manager::WireExtras::default());
    assert_eq!((remote.narrative_cap, remote.extraction_cap), (4096, 4096));
    let forced = describe_wire(
        &provider_of("https://api.example.com/v1", "local = true\n"),
        None,
    );
    assert_eq!(forced.extras.reasoning_effort.as_deref(), Some("none"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_body_carries_no_new_fields_and_old_cap() {
    let (base, seen) = fake_provider(vec![ok("A."), ok("T.")]).await;
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let cfg = config_with(&base, true, "fake", "local = false", "");
    exec_async(Arc::clone(&e), cfg, "").await.unwrap();
    for b in seen.bodies() {
        let mut keys: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["max_tokens", "messages", "model"]);
        assert_eq!(b["max_tokens"], 4096);
    }
    // The override applies to a local provider's both passes.
    let (base, seen) = fake_provider(vec![ok("A."), ok("T.")]).await;
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let cfg = config_with(&base, true, "fake", "", "max_tokens = 800");
    exec_async(Arc::clone(&e), cfg, "").await.unwrap();
    assert!(seen.bodies().iter().all(|b| b["max_tokens"] == 800));
}

#[test]
fn repetition_loop_detector() {
    // Multi-line loop: 3 unique lines out of 39.
    let looped = "Model A: 1\nModel B: 2\nModel C: 3\n".repeat(13);
    assert!(is_repetition_loop(&looped));
    // Genuinely long, all-unique lines.
    let long: String = (0..60)
        .map(|i| format!("row {i}: value {}\n", i * 7))
        .collect();
    assert!(!is_repetition_loop(&long));
    // One-paragraph loop (no newlines).
    assert!(is_repetition_loop(&"the quick brown fox jumps ".repeat(30)));
    let prose: String = (0..80).map(|i| format!("w{i} ")).collect();
    assert!(!is_repetition_loop(&prose));
    // Short bodies are never judged.
    assert!(!is_repetition_loop("a\na\na"));
    assert!(!is_repetition_loop(""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_degenerate_length_is_stored_truncated() {
    let long: String = (0..60).map(|i| format!("row {i}: value {i}\n")).collect();
    let (base, _mock) = fake_provider(vec![ok("A chart."), length(&long)]).await;
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!((var(&out, "described"), var(&out, "failed")), (1, 0));
    let r = rec(&e, MD5_A).unwrap();
    assert_eq!(r.kind, "vision");
    assert_eq!(r.extraction, long.trim());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn length_loop_or_empty_is_a_recorded_failure() {
    let looped = "Model A: 1\nModel B: 2\n".repeat(30);
    for reply in [length(&looped), length("")] {
        let (base, _mock) = fake_provider(vec![ok("A chart."), reply]).await;
        let e = Arc::new(clip_env(png(8, 8), &[]));
        let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
            .await
            .unwrap();
        assert_eq!((var(&out, "described"), var(&out, "failed")), (0, 1));
        let r = rec(&e, MD5_A).unwrap();
        assert_eq!(r.kind, "failed");
        assert!(!r.raw.contains(super::LEGACY_LENGTH_FAILURE), "{}", r.raw);
        // Re-run: poison-skipped, no new request.
        let before = rec(&e, MD5_A);
        let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
            .await
            .unwrap();
        assert_eq!(var(&out, "skipped"), 1);
        assert_eq!(rec(&e, MD5_A), before);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_length_failure_is_retried_and_other_failures_are_not() {
    let (base, seen) = fake_provider(vec![ok("A chart."), ok("Title: Q3")]).await;
    let e = Arc::new(clip_env(
        png(8, 8),
        &[("att/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb_MD5.png", png(9, 9))],
    ));
    {
        let mut st = e.state.lock().unwrap();
        st.put_image_description(&super::record(
            MD5_A,
            "failed",
            "",
            "",
            None,
            Some("extraction: finish reason \"length\"; output discarded"),
        ))
        .unwrap();
        // B carries a different permanent failure and stays poison-skipped.
        let m = super::Model {
            provider_id: "fake",
            model_id: "m",
        };
        st.put_image_description(&super::record(
            MD5_B,
            "failed",
            "",
            "",
            Some(&m),
            Some("boom"),
        ))
        .unwrap();
    }
    let out = exec_async(
        Arc::clone(&e),
        config(&base, true, "fake"),
        "scope = \"vault\"",
    )
    .await
    .unwrap();
    assert_eq!(var(&out, "described"), 1);
    assert_eq!(seen.bodies().len(), 2, "only the length-failed image");
    assert_eq!(rec(&e, MD5_A).unwrap().kind, "vision");
    assert_eq!(rec(&e, MD5_B).unwrap().kind, "failed");
}

/// Plan remote-media-sprint WP-4: a `failed` record without `providerId` (a Crucible-era failure)
/// is described again once; the re-failure carries provenance and is then poison-skipped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unattributed_failure_is_retried_once() {
    let (base, seen) = fake_provider(vec![(400, "bad image".into()), ok("never used")]).await;
    let e = Arc::new(clip_env(png(8, 8), &[]));
    e.state
        .lock()
        .unwrap()
        .put_image_description(&super::record(
            MD5_A,
            "failed",
            "",
            "",
            None,
            Some("API returned 500"),
        ))
        .unwrap();
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!((var(&out, "failed"), var(&out, "skipped")), (1, 0));
    let r = rec(&e, MD5_A).unwrap();
    assert_eq!(r.kind, "failed");
    assert!(r.raw.contains("\"providerId\":\"fake\""), "{}", r.raw);
    let calls = seen.bodies().len();
    assert!(calls >= 1);
    // The second pass skips the now-attributed failure.
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!((var(&out, "failed"), var(&out, "skipped")), (0, 1));
    assert_eq!(seen.bodies().len(), calls);
}
