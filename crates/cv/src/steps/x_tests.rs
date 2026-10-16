//! WP-5 `x_metadata` / `x_discover` / `x_backfill` tests: a synthetic temp vault, a temp
//! `state.db` and a local fake oEmbed endpoint (never the network, never the live vault, never env vars).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_state::{LinkRecordInput, QueueConfig, StateDb};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{execute, validate};
use crate::steps::web::{Overrides, create_or_found, test_overrides};
use crate::steps::{ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome};

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Route substring (of path + query) → (status, body); counts every request.
async fn serve(routes: Vec<(&'static str, u16, String)>, hits: Arc<AtomicUsize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let (routes, hits) = (routes.clone(), hits.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let head = String::from_utf8_lossy(&buf).into_owned();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| path.contains(p))
                    .map_or((500, String::new()), |(_, s, b)| (*s, b.clone()));
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

struct World {
    _tmp: Tmp,
    rt: tokio::runtime::Runtime,
    vault: std::path::PathBuf,
    index: std::path::PathBuf,
    writer: cv_vault::WriterHandle,
    state: Arc<Mutex<StateDb>>,
    config: ConfigSet,
    base: String,
    hits: Arc<AtomicUsize>,
}

fn world(
    files: &[(&str, &str)],
    routes: Vec<(&'static str, u16, String)>,
    pipelines: &str,
) -> World {
    use std::sync::atomic::AtomicUsize as N;
    static SEQ: N = N::new(0);
    let root = std::env::temp_dir().join(format!(
        "cv-wp5-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let tmp = Tmp(root.clone());
    let vault = root.join("vault");
    let protected = root.join("protected");
    std::fs::create_dir_all(&protected).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    for (rel, text) in files {
        let p = vault.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let fs = cv_vault::VaultFs::live(&vault, &protected, &cv_vault::NoCutover).unwrap();
    let index = root.join("index.db");
    let w = cv_vault::VaultWriter::open(fs, &index).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = StateDb::open(&root.join("state.db"), 1, QueueConfig::default()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let base = format!("{}/oembed", rt.block_on(serve(routes, hits.clone())));
    let files = BTreeMap::from([
        ("pipelines.toml".to_owned(), Some(pipelines.to_owned())),
        ("sources.toml".to_owned(), Some(String::new())),
    ]);
    let mut config = ConfigSet::new();
    config.reload(&files, &crate::steps::validate);
    World {
        vault: std::fs::canonicalize(&vault).unwrap(),
        _tmp: tmp,
        rt,
        index,
        writer,
        state: Arc::new(Mutex::new(state)),
        config,
        base,
        hits,
    }
}

fn step(kind: &str, src: &str) -> Step {
    let t: toml::Table = format!("type = \"{kind}\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "x-metadata".into(),
        label: "X".into(),
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
    }
}

fn run(
    w: &World,
    s: &Step,
    active: Option<&str>,
    vars: &Map<String, Value>,
) -> Result<StepOutcome, StepError> {
    let _rt = w.rt.enter();
    let _o = test_overrides(Overrides {
        bases: HashMap::from([("x-oembed".to_owned(), w.base.clone())]),
        secrets: None,
    });
    let action = action();
    let context = ActionContext {
        active_path: active.map(str::to_owned),
        ..ActionContext::default()
    };
    let params = Map::new();
    let mut ctx = StepCtx {
        job_id: "job-1",
        action: &action,
        step_index: 0,
        params: &params,
        context: &context,
        vars,
        config: &w.config,
        vault_root: &w.vault,
        index_db: &w.index,
        state: &w.state,
        writer: Some(w.writer.clone()),
        children: &NoChildren,
        written: Vec::new(),
    };
    execute(s, &mut ctx)
}

fn read(w: &World, rel: &str) -> String {
    std::fs::read_to_string(w.vault.join(rel)).unwrap()
}

fn oembed(handle: &str) -> String {
    format!(
        r#"{{"url":"https://x.com/{handle}/status/1","author_name":"Synth","author_url":"https://x.com/{handle}","html":"<blockquote><p>synthetic post</p></blockquote><script src=\"w.js\"></script>","type":"rich","version":"1.0"}}"#
    )
}

const PIPES: &str = "[x]\nenabled = true\ninbox = \"clips\"\n";
const NOTE: &str = "---\ntitle: n\nsource: https://x.com/synth/status/111\n---\nBody\n";

fn meta(w: &World, note: &str) -> Result<StepOutcome, StepError> {
    run(w, &step("x_metadata", ""), Some(note), &Map::new())
}

#[test]
fn params_validate() {
    let c = ConfigSet::new();
    assert!(validate(&step("x_metadata", "status_id = \"{vars.id}\""), &c).is_ok());
    assert!(validate(&step("x_metadata", "status_id = \"{bogus}\""), &c).is_err());
    assert!(validate(&step("x_metadata", "limit = 1"), &c).is_err());
    assert!(validate(&step("x_discover", "limit = 5"), &c).is_ok());
    assert!(validate(&step("x_backfill", "limit = 0"), &c).is_err());
}

#[test]
fn creates_links_counts_usage_then_rerun_is_a_noop() {
    let w = world(
        &[("n/a.md", NOTE)],
        vec![("status%2F111", 200, oembed("Synth"))],
        PIPES,
    );
    let o = meta(&w, "n/a.md").unwrap();
    let path = "_x_metadata/synth/111.md";
    assert_eq!(o.vars["status_id"], json!("111"));
    assert_eq!(o.vars["metadata_path"], json!(path));
    assert_eq!(o.vars["created"], json!(true));
    assert_eq!(o.vars["linked"], json!(true));
    let m = read(&w, path);
    assert!(
        m.starts_with(
            "---\nstatus-id: \"111\"\nurl: https://x.com/synth/status/111\nauthor: \"Synth\"\n"
        ),
        "{m}"
    );
    assert!(m.contains("synthetic post") && !m.contains("script"));
    let n = read(&w, "n/a.md");
    assert!(n.contains("[[_x_metadata/synth/111]]"), "{n}");
    let day = crate::steps::web::local_day();
    assert_eq!(
        w.state.lock().unwrap().api_usage("x-oembed", &day).unwrap(),
        1
    );
    // Already linked + exists → no fetch, no write.
    let o = meta(&w, "n/a.md").unwrap();
    assert_eq!(o.vars["linked"], json!(false));
    assert!(o.output_paths.is_empty());
    assert_eq!(read(&w, "n/a.md"), n);
    assert_eq!(w.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn not_found_and_forbidden_tombstone_without_the_breaker() {
    for st in [404u16, 403] {
        let w = world(
            &[("n/a.md", NOTE), ("n/b.md", NOTE)],
            vec![("status%2F111", st, String::new())],
            PIPES,
        );
        let o = meta(&w, "n/a.md").unwrap();
        let path = "_x_metadata/_unavailable/111.md";
        assert_eq!(o.vars["tombstone"], json!(true), "{st}");
        assert_eq!(o.vars["metadata_path"], json!(path));
        assert!(
            read(&w, path)
                .contains("state: unavailable\nunavailable-reason: \"deleted-or-private\"")
        );
        // Another note citing the same status finds the tombstone: no refetch.
        let o = meta(&w, "n/b.md").unwrap();
        assert_eq!(o.vars["tombstone"], json!(true));
        assert_eq!(o.vars["linked"], json!(true));
        assert_eq!(w.hits.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn rate_limit_and_5xx_defer_transient_on_x_oembed() {
    for st in [429u16, 503] {
        let w = world(
            &[("n/a.md", NOTE)],
            vec![("status%2F111", st, String::new())],
            PIPES,
        );
        match meta(&w, "n/a.md") {
            Err(StepError::Deferred {
                service,
                cause: DeferCause::Transient(_),
                ..
            }) => assert_eq!(service.as_deref(), Some("x-oembed")),
            other => panic!("{st}: {other:?}"),
        }
        assert!(!w.vault.join("_x_metadata").exists());
    }
}

#[test]
fn find_or_create_twice_yields_one_note() {
    let w = world(&[], vec![], PIPES);
    let _rt = w.rt.enter();
    let action = action();
    let context = ActionContext::default();
    let params = Map::new();
    let vars = Map::new();
    let mut ctx = StepCtx {
        job_id: "job-1",
        action: &action,
        step_index: 0,
        params: &params,
        context: &context,
        vars: &vars,
        config: &w.config,
        vault_root: &w.vault,
        index_db: &w.index,
        state: &w.state,
        writer: Some(w.writer.clone()),
        children: &NoChildren,
        written: Vec::new(),
    };
    let p = "_x_metadata/a/1.md";
    assert!(create_or_found(&mut ctx, p, b"one".to_vec()).unwrap());
    assert!(!create_or_found(&mut ctx, p, b"two".to_vec()).unwrap());
    assert_eq!(read(&w, p), "one");
}

#[test]
fn discover_ignores_outside_inbox_and_dedupes() {
    let body = "---\nsource: https://x.com/synth/status/111\n---\nsee https://twitter.com/synth/status/111?s=20 and [x](https://x.com/other/status/222) and https://x.com/synth/status/111\n";
    let w = world(
        &[("clips/c.md", body), ("elsewhere/c.md", body)],
        vec![
            ("status%2F111", 200, oembed("synth")),
            ("status%2F222", 404, String::new()),
        ],
        PIPES,
    );
    let d = step("x_discover", "");
    let o = run(&w, &d, Some("elsewhere/c.md"), &Map::new()).unwrap();
    assert_eq!(o.vars["found"], json!(0));
    assert_eq!(w.hits.load(Ordering::SeqCst), 0);
    let o = run(&w, &d, Some("clips/c.md"), &Map::new()).unwrap();
    assert_eq!(o.vars["found"], json!(2));
    assert_eq!(o.vars["ensured"], json!(2));
    assert_eq!(w.hits.load(Ordering::SeqCst), 2);
    let note = read(&w, "clips/c.md");
    assert!(
        note.contains("[[_x_metadata/synth/111]]")
            && note.contains("[[_x_metadata/_unavailable/222]]"),
        "{note}"
    );
    // Unset inbox → no-op.
    let w2 = world(&[("clips/c.md", body)], vec![], "");
    let o = run(&w2, &d, Some("clips/c.md"), &Map::new()).unwrap();
    assert_eq!(o.vars["found"], json!(0));
}

#[test]
fn backfill_skips_statuses_with_a_note_and_bounds_by_limit() {
    let w = world(
        &[
            ("src/a.md", "---\ntitle: a\n---\n"),
            ("_x_metadata/old/100.md", "---\nstatus-id: \"100\"\n---\n"),
        ],
        vec![
            ("status%2F2", 200, oembed("synth")),
            ("status%2F3", 200, oembed("synth")),
        ],
        PIPES,
    );
    {
        let mut s = w.state.lock().unwrap();
        for (url, id) in [
            ("https://x.com/old/status/100", "100"),
            ("https://x.com/synth/status/2", "2"),
            ("https://x.com/synth/status/3", "3"),
        ] {
            s.upsert_link_record(&LinkRecordInput {
                canonical_url: url.into(),
                url: url.into(),
                source_notes: vec!["[[src/a]]".into()],
                x_status_id: Some(id.into()),
                ..LinkRecordInput::default()
            })
            .unwrap();
        }
    }
    let b = step("x_backfill", "limit = 1");
    let o = run(&w, &b, None, &Map::new()).unwrap();
    assert_eq!(o.vars["pending"], json!(2));
    assert_eq!(o.vars["ensured"], json!(1));
    assert_eq!(o.vars["remaining"], json!(1));
    assert!(read(&w, "src/a.md").contains("[[_x_metadata/synth/2]]"));
    let o = run(&w, &b, None, &Map::new()).unwrap();
    assert_eq!(o.vars["pending"], json!(1));
    assert_eq!(o.vars["remaining"], json!(0));
    assert_eq!(w.hits.load(Ordering::SeqCst), 2);
}
