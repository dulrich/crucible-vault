//! Search-step tests (plan `v0-core-daemon` WP-13; rewritten for plan semsearch-crate WP-3):
//! synthetic notes in a temp vault, a temp `state.db`, a tmpfs `semsearch` index, and a
//! hand-rolled fake embedder on 127.0.0.1 that counts every request (I1 — never a real index or
//! any real provider).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{ConfigSet, Step};
use cv_protocol::ActionContext;
use cv_providers::{ProviderManager, SecretSource};
use cv_search::SearchDb;
use cv_search::semsearch::{Query, Space};
use cv_state::{ImageDescription, QueueConfig, StateDb};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::search::run;
use super::{ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome};

/// tmpfs when available (project-setup "tmpfs test databases").
fn tmp_base() -> PathBuf {
    if Path::new("/dev/shm").is_dir() {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    }
}

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = tmp_base().join(format!(
            "cv-wp13-{}-{}",
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

#[derive(Default)]
struct FakeState {
    embed_down: bool,
    log: Vec<(String, Value)>,
}

#[derive(Clone)]
struct Fake {
    base: String,
    st: Arc<Mutex<FakeState>>,
}

impl Fake {
    fn embed_calls(&self) -> usize {
        self.st
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|(p, _)| p == "/embeddings")
            .count()
    }
}

/// Every vector the fake embedder returns (3 dims).
const VEC: [f32; 3] = [0.1, 0.2, 0.3];

fn handle(st: &mut FakeState, path: &str, body: &Value) -> (u16, Value) {
    match path {
        "/embeddings" => {
            if st.embed_down {
                return (503, json!({"error": "down"}));
            }
            let n = body["input"].as_array().map_or(0, Vec::len);
            let data: Vec<Value> = (0..n)
                .map(|i| json!({"index": i, "embedding": VEC}))
                .collect();
            (200, json!({ "data": data }))
        }
        _ => (404, json!({})),
    }
}

async fn serve(l: TcpListener, st: Arc<Mutex<FakeState>>) {
    loop {
        let Ok((mut s, _)) = l.accept().await else {
            return;
        };
        let st = st.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let head_end = loop {
                let Ok(n) = s.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let path = head.split(' ').nth(1).unwrap_or("").to_owned();
            let len: usize = head
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let Ok(n) = s.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
            let (status, out) = {
                let mut g = st.lock().unwrap();
                g.log.push((path.clone(), body.clone()));
                handle(&mut g, &path, &body)
            };
            let out = out.to_string();
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                out.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
            let _ = s.shutdown().await;
        });
    }
}

struct NoKids;
impl ChildSpawner for NoKids {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

struct NoSecrets;
impl SecretSource for NoSecrets {
    fn env_var(&self, _: &str) -> Option<String> {
        None
    }
    fn secret_file(&self, _: &str) -> Option<String> {
        None
    }
}

struct World {
    /// Held for its `Drop` (removes the temp tree).
    _tmp: Tmp,
    rt: tokio::runtime::Runtime,
    fake: Fake,
    vault: PathBuf,
    index: PathBuf,
    search: SearchDb,
    state: Arc<Mutex<StateDb>>,
    writer: cv_vault::WriterHandle,
    config: ConfigSet,
    mgr: ProviderManager,
}

const NOTE_A: &str = "---\nauthor: Synthetic Author\n---\n# Alpha\nFirst synthetic note.\n\n![[0123456789abcdef0123456789abcdef_MD5.png]]";
const NOTE_B: &str = "# Beta\nSecond synthetic note.";

fn world() -> World {
    let tmp = Tmp::new();
    let vault = tmp.0.join("vault");
    std::fs::create_dir_all(vault.join("notes")).unwrap();
    std::fs::create_dir_all(vault.join("att")).unwrap();
    std::fs::write(vault.join("notes/a.md"), NOTE_A).unwrap();
    std::fs::write(vault.join("notes/b.md"), NOTE_B).unwrap();
    std::fs::write(
        vault.join("att/0123456789abcdef0123456789abcdef_MD5.png"),
        b"png",
    )
    .unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let st = Arc::new(Mutex::new(FakeState::default()));
    let base = rt.block_on(async {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(serve(l, st.clone()));
        format!("http://{addr}")
    });
    let fake = Fake { base, st };
    let protected = tmp.0.join("protected");
    std::fs::create_dir_all(&protected).unwrap();
    let fs = cv_vault::VaultFs::live(&vault, &protected, &cv_vault::NoCutover).unwrap();
    let index = tmp.0.join("index.db");
    let w = cv_vault::VaultWriter::open(fs, &index).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = Arc::new(Mutex::new(
        StateDb::open(&tmp.0.join("state.db"), 1, QueueConfig::default()).unwrap(),
    ));
    let mut config = ConfigSet::new();
    let actions = "[[action]]\nid = \"s\"\n[[action.steps]]\ntype = \"search_upsert\"\n";
    let providers = format!(
        "[[provider]]\nid = \"emb\"\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n",
        fake.base
    );
    config.reload(
        &BTreeMap::from([
            ("actions.toml".to_owned(), Some(actions.to_owned())),
            ("providers.toml".to_owned(), Some(providers)),
        ]),
        &super::validate,
    );
    assert!(config.errors().is_empty(), "{:?}", config.errors());
    let search = SearchDb::new(tmp.0.join("state").join("search.db"));
    World {
        _tmp: tmp,
        rt,
        fake,
        vault,
        index,
        search,
        state,
        writer,
        config,
        mgr: ProviderManager::new(Arc::new(NoSecrets)).unwrap(),
    }
}

impl World {
    fn run(&self, step: &Step, active: Option<&str>) -> Result<StepOutcome, StepError> {
        let params = Map::new();
        let vars = Map::new();
        let context = ActionContext {
            active_path: active.map(str::to_owned),
            ..ActionContext::default()
        };
        let action = self.config.action("s").unwrap();
        let mut ctx = StepCtx {
            job_id: "job-1",
            action,
            step_index: 0,
            params: &params,
            context: &context,
            vars: &vars,
            config: &self.config,
            vault_root: &self.vault,
            index_db: &self.index,
            search: Some(&self.search),
            state: &self.state,
            writer: Some(self.writer.clone()),
            children: &NoKids,
            written: Vec::new(),
        };
        run(step, &mut ctx, self.rt.handle(), &self.mgr)
    }

    fn upsert(&self, extra: &str) -> Step {
        let t: toml::Table = toml::from_str(extra).unwrap();
        Step::SearchUpsert(t)
    }

    fn delete(&self) -> Step {
        Step::SearchDelete(toml::Table::new())
    }

    /// `(source, hash, chunks, embedded)` through the shared handle.
    fn states(&self) -> Vec<(String, String, u64, u64)> {
        let idx = self.search.shared().unwrap();
        let g = idx.lock().unwrap();
        g.source_states()
            .unwrap()
            .into_iter()
            .map(|s| (s.source, s.content_hash, s.chunks, s.embedded_chunks))
            .collect()
    }
}

fn var(o: &StepOutcome, k: &str) -> u64 {
    o.vars.get(k).and_then(Value::as_u64).unwrap()
}

fn assert_deferred(r: Result<StepOutcome, StepError>, service: &str) {
    match r {
        Err(StepError::Deferred {
            retry_after_ms,
            service: Some(s),
            cause: DeferCause::Transient(_),
        }) => {
            assert_eq!(retry_after_ms, 60_000);
            assert_eq!(s, service);
        }
        other => panic!("expected deferral on {service}, got {other:?}"),
    }
}

#[test]
fn embedder_outage_defers_and_writes_nothing() {
    let w = world();
    w.fake.st.lock().unwrap().embed_down = true;
    assert_deferred(
        w.run(&w.upsert("embed = \"emb:m1\""), Some("notes/b.md")),
        "emb",
    );
    assert!(w.states().is_empty());
}

#[test]
fn live_round_trip_skips_unchanged_and_fails_on_space_drift() {
    let w = world();
    w.state
        .lock()
        .unwrap()
        .put_image_description(&ImageDescription {
            md5: "0123456789abcdef0123456789abcdef".into(),
            kind: "vision".into(),
            narrative: "A synthetic chart.".into(),
            extraction: String::new(),
            description_hash: "00c0ffee".into(),
            described_at: "t".into(),
            raw: "{}".into(),
        })
        .unwrap();
    let embed = "embed = \"emb:m1\"";

    // Upsert → the source holds exactly its chunks, all embedded in Space { m1, 3 }.
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "missing_index"), 1);
    assert_eq!(var(&o, "upserted"), 1);
    assert_eq!(var(&o, "chunks"), 2);
    let st = w.states();
    assert_eq!(st.len(), 1);
    assert_eq!((st[0].0.as_str(), st[0].2, st[0].3), ("notes/a.md", 2, 2));
    {
        let idx = w.search.shared().unwrap();
        let g = idx.lock().unwrap();
        assert_eq!(g.space(), Some(&Space::new("m1", 3)));
        let hits = g.search(&Query::new("synthetic author")).unwrap();
        assert_eq!(hits[0].source, "notes/a.md");
    }

    // Unchanged and fully embedded → no embed call, no write.
    let calls = w.fake.embed_calls();
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "matched"), 1);
    assert_eq!(var(&o, "upserted"), 0);
    assert_eq!(w.fake.embed_calls(), calls);
    // Lexical-only and unchanged → matched too.
    let o = w.run(&w.upsert(""), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "matched"), 1);

    // Another configured model: a Failed naming both spaces, before any embed call.
    let r = w.run(&w.upsert("embed = \"emb:m2\""), Some("notes/a.md"));
    match r {
        Err(StepError::Failed { message, .. }) => {
            assert!(
                message.contains("m1/3d") && message.contains("m2"),
                "{message}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(w.fake.embed_calls(), calls);

    // A description change moves the hash → mismatched.
    {
        let mut db = w.state.lock().unwrap();
        let mut d = db
            .image_description("0123456789abcdef0123456789abcdef")
            .unwrap()
            .unwrap();
        d.description_hash = "0badf00d".into();
        db.put_image_description(&d).unwrap();
    }
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "mismatched"), 1);

    // Reconcile: b.md missing from the index gets upserted; an orphan is deleted.
    {
        let idx = w.search.shared().unwrap();
        let ch = cv_search::semsearch::Chunk::new("o", "t", "", "x");
        idx.lock()
            .unwrap()
            .upsert_source("orphan.md", "h", &[ch])
            .unwrap();
    }
    let o = w.run(&w.upsert(embed), None).unwrap();
    assert_eq!(var(&o, "missing_index"), 1);
    assert_eq!(var(&o, "missing_local"), 1);
    assert_eq!(var(&o, "matched"), 1);
    let held: Vec<String> = w.states().into_iter().map(|s| s.0).collect();
    assert_eq!(held, ["notes/a.md", "notes/b.md"]);

    // search_delete (live) drops the path.
    let o = w.run(&w.delete(), Some("notes/b.md")).unwrap();
    assert_eq!(var(&o, "deleted"), 1);
    let held: Vec<String> = w.states().into_iter().map(|s| s.0).collect();
    assert_eq!(held, ["notes/a.md"]);
}

#[test]
fn steps_share_one_index_handle() {
    let w = world();
    let a = w.search.shared().unwrap();
    // A write by one step is visible to a search through the same handle, without reopening.
    w.run(&w.upsert(""), Some("notes/b.md")).unwrap();
    assert!(Arc::ptr_eq(&a, &w.search.shared().unwrap()));
    let hits = a.lock().unwrap().search(&Query::new("beta")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source, "notes/b.md");
    w.run(&w.delete(), Some("notes/b.md")).unwrap();
    assert!(
        a.lock()
            .unwrap()
            .search(&Query::new("beta"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn search_refresh_example_step_arms() {
    // The shipped `search-refresh` action: a param-less `search_upsert`.
    let mut c = ConfigSet::new();
    let src = "[[action]]\nid = \"search-refresh\"\nstarters = []\nresources = { note = false, keys = [\"search::feed\"] }\n[[action.steps]]\ntype = \"search_upsert\"\n";
    c.reload(
        &BTreeMap::from([("actions.toml".to_owned(), Some(src.to_owned()))]),
        &super::validate,
    );
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    assert!(c.armed("search-refresh").is_some());
}
