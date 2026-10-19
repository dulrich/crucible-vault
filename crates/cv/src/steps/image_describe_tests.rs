//! WP-6 `image_describe` tests: synthetic temp vault + temp `state.db`, and a hand-rolled local
//! HTTP responder posing as an `openai-compatible` provider. No real provider is ever called.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_providers::{ProviderManager, SecretSource};
use cv_state::{FailureReason, ImageDescription, QueueConfig, StateDb};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{
    MAX_LONG_EDGE, extract_svg_text, fit_long_edge, fnv1a32_hex, parse, prepare_image, run,
};
use crate::steps::{ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome};

const MD5_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MD5_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

struct TestSecrets;
impl SecretSource for TestSecrets {
    fn env_var(&self, name: &str) -> Option<String> {
        (name == "CV_TEST_KEY").then(|| "test-key".to_owned())
    }
    fn secret_file(&self, _name: &str) -> Option<String> {
        None
    }
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cv-wp6-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageLuma8(image::GrayImage::new(w, h));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Serves `replies[i]` to the i-th request (the last repeats); captures request bodies.
async fn fake_provider(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    let n = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let i = n.fetch_add(1, Ordering::SeqCst);
            let (status, body) = replies[i.min(replies.len() - 1)].clone();
            let seen = Arc::clone(&seen2);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 65536];
                let (head_end, len) = loop {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (i + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                if let Ok(v) = serde_json::from_slice::<Value>(&buf[head_end..]) {
                    seen.lock().unwrap().push(v);
                }
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/v1"), seen)
}

fn ok(text: &str) -> (u16, String) {
    (
        200,
        json!({"choices": [{"message": {"content": text}, "finish_reason": "stop"}]}).to_string(),
    )
}

fn config(base: &str, enabled: bool, provider: &str) -> ConfigSet {
    let providers = format!(
        r#"
[[provider]]
id = "fake"
kind = "openai-compatible"
base_url = "{base}"
api_key_env = "CV_TEST_KEY"
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
        "[image_describe]\nenabled = {enabled}\nprovider = \"{provider}\"\nmodel = \"vl\"\n"
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

struct Env {
    _tmp: Tmp,
    vault: PathBuf,
    index: PathBuf,
    writer: cv_vault::WriterHandle,
    state: Arc<Mutex<StateDb>>,
}

fn env(files: &[(&str, Vec<u8>)]) -> Env {
    let tmp = Tmp::new();
    let vault = tmp.0.join("vault");
    let protected = tmp.0.join("protected");
    std::fs::create_dir_all(&protected).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    for (rel, bytes) in files {
        let p = vault.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    let fs = cv_vault::VaultFs::live(&vault, &protected, &cv_vault::NoCutover).unwrap();
    let index = tmp.0.join("index.db");
    let w = cv_vault::VaultWriter::open(fs, &index).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = StateDb::open(&tmp.0.join("state.db"), 1, QueueConfig::default()).unwrap();
    Env {
        vault: std::fs::canonicalize(&vault).unwrap(),
        index,
        _tmp: tmp,
        writer,
        state: Arc::new(Mutex::new(state)),
    }
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
    let context = ActionContext {
        active_path: Some("Clip.md".into()),
        ..ActionContext::default()
    };
    let (params, vars) = (Map::new(), Map::new());
    let mut ctx = StepCtx {
        job_id: "job-1",
        action: &action,
        step_index: 0,
        params: &params,
        context: &context,
        vars: &vars,
        config: cfg,
        vault_root: &e.vault,
        index_db: &e.index,
        search: None,
        state: &e.state,
        writer: Some(e.writer.clone()),
        children: &NoChildren,
        written: Vec::new(),
    };
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
    // A real decode + resize, kept thin: the shadow parity test reads this binary's peak RSS.
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
        let bodies = seen.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        let msgs = bodies[0]["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "no system message");
        assert_eq!(bodies[0]["max_tokens"], 4096);
        let url = msgs[0]["content"][1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }
    // Second run: already described → skipped, no request.
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!((var(&out, "described"), var(&out, "skipped")), (0, 1));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_record_is_poison_skipped() {
    let (base, seen) = fake_provider(vec![ok("x")]).await;
    let e = Arc::new(clip_env(png(8, 8), &[]));
    let failed = super::record(MD5_A, "failed", "", "", None, Some("boom"));
    e.state
        .lock()
        .unwrap()
        .put_image_description(&failed)
        .unwrap();
    let out = exec_async(Arc::clone(&e), config(&base, true, "fake"), "")
        .await
        .unwrap();
    assert_eq!(var(&out, "skipped"), 1);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_failures_write_no_record() {
    for status in [500u16, 503, 429, 408] {
        let (base, _) = fake_provider(vec![(status, "{}".into())]).await;
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
    assert!(seen.lock().unwrap().is_empty());

    // Empty answer and a 400 → failed.
    for reply in [ok("  "), (400, "{\"error\":\"bad image\"}".to_owned())] {
        let (base, _) = fake_provider(vec![reply]).await;
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
    assert_eq!(seen.lock().unwrap().len(), 2, "svg made no model call");
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
