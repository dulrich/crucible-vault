//! WP-13 step tests: synthetic notes in a temp vault, a temp `state.db`, and a hand-rolled fake
//! companion + embedder on 127.0.0.1 that records every request (I1 — never the real companion
//! at :4801 or any real provider).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{ConfigSet, Step};
use cv_protocol::ActionContext;
use cv_providers::{ProviderManager, SecretSource};
use cv_state::{ImageDescription, QueueConfig, StateDb};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::search::run;
use super::{ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome};

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
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

/// contentHash, chunk ids, embedding space per chunk.
type Stored = (String, Vec<String>, Vec<Option<String>>);

#[derive(Default)]
struct FakeState {
    /// path → (contentHash, chunk ids, embedding space per chunk)
    store: BTreeMap<String, Stored>,
    health: Value,
    embed_down: bool,
    log: Vec<(String, Value)>,
}

#[derive(Clone)]
struct Fake {
    base: String,
    st: Arc<Mutex<FakeState>>,
}

impl Fake {
    fn paths(&self) -> Vec<String> {
        self.st
            .lock()
            .unwrap()
            .log
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
    fn mutations(&self) -> usize {
        self.paths()
            .iter()
            .filter(|p| p.starts_with("/v1/chunks/") || p.as_str() == "/embeddings")
            .count()
    }
    fn clear_log(&self) {
        self.st.lock().unwrap().log.clear();
    }
    fn set_health(&self, h: Value) {
        self.st.lock().unwrap().health = h;
    }
}

fn health(model: Option<&str>, dim: Option<u64>) -> Value {
    json!({"ok": true, "schemaVersion": 7, "embeddingModel": model, "embeddingDim": dim})
}

fn handle(st: &mut FakeState, path: &str, body: &Value) -> (u16, Value) {
    let vault = body["vaultId"].as_str().unwrap_or("");
    match path {
        "/health" => (200, st.health.clone()),
        "/embeddings" => {
            if st.embed_down {
                return (503, json!({"error": "down"}));
            }
            let n = body["input"].as_array().map_or(0, Vec::len);
            let data: Vec<Value> = (0..n)
                .map(|i| json!({"index": i, "embedding": [0.1, 0.2, 0.3]}))
                .collect();
            (200, json!({ "data": data }))
        }
        _ if vault != "vault-x" => (400, json!({"error": "wrong vault"})),
        "/v1/paths" => {
            let paths: Vec<Value> = st
                .store
                .iter()
                .map(|(p, (h, ids, sp))| {
                    json!({"path": p, "contentHash": h, "chunkCount": ids.len(),
                        "embeddedCount": sp.iter().filter(|s| s.is_some()).count(), "mtime": 0})
                })
                .collect();
            (200, json!({"ok": true, "paths": paths}))
        }
        "/v1/files/state" => {
            let files: Vec<Value> = body["paths"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|p| {
                    let p = p.as_str().unwrap();
                    let (h, ids, sp) = st.store.get(p)?;
                    let all = !ids.is_empty() && sp.iter().all(Option::is_some);
                    let space = sp.first().cloned().flatten();
                    Some(json!({"path": p, "contentHash": h, "chunkCount": ids.len(),
                        "hasEmbeddings": all, "embeddingSpace": if all { space } else { None }}))
                })
                .collect();
            (200, json!({"ok": true, "files": files}))
        }
        "/v1/chunks/upsert" => {
            let mut seen: Vec<String> = Vec::new();
            for c in body["chunks"].as_array().unwrap() {
                let p = c["path"].as_str().unwrap().to_owned();
                if !seen.contains(&p) {
                    st.store.insert(p.clone(), (String::new(), vec![], vec![]));
                    seen.push(p.clone());
                }
                let e = st.store.get_mut(&p).unwrap();
                e.0 = c["contentHash"].as_str().unwrap().to_owned();
                e.1.push(c["id"].as_str().unwrap().to_owned());
                e.2.push(c["embeddingSpace"].as_str().map(str::to_owned));
            }
            (200, json!({"ok": true}))
        }
        "/v1/chunks/delete" => {
            for p in body["paths"].as_array().unwrap() {
                st.store.remove(p.as_str().unwrap());
            }
            (200, json!({"ok": true}))
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
    _tmp: Tmp,
    rt: tokio::runtime::Runtime,
    fake: Fake,
    vault: PathBuf,
    index: PathBuf,
    state: Arc<Mutex<StateDb>>,
    writer: cv_vault::WriterHandle,
    config: ConfigSet,
    mgr: ProviderManager,
}

const NOTE_A: &str = "---\nauthor: Synthetic Author\n---\n# Alpha\nFirst synthetic note.\n\n![[0123456789abcdef0123456789abcdef_MD5.png]]";
const NOTE_B: &str = "# Beta\nSecond synthetic note.";

fn world(shadow: bool, companion_up: bool) -> World {
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
    let st = Arc::new(Mutex::new(FakeState {
        health: health(None, None),
        ..FakeState::default()
    }));
    let base = rt.block_on(async {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        if companion_up {
            tokio::spawn(serve(l, st.clone()));
        }
        format!("http://{addr}")
    });
    let fake = Fake { base, st };
    let protected = tmp.0.join("protected");
    std::fs::create_dir_all(&protected).unwrap();
    let fs = if shadow {
        cv_vault::VaultFs::shadow(
            &vault,
            &tmp.0.join("scratch"),
            &[],
            Box::new(cv_vault::MemoryShadowLog::default()),
        )
        .unwrap()
    } else {
        cv_vault::VaultFs::live(&vault, &protected, &cv_vault::NoCutover).unwrap()
    };
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
    World {
        _tmp: tmp,
        rt,
        fake,
        vault,
        index,
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
            state: &self.state,
            writer: Some(self.writer.clone()),
            children: &NoKids,
            written: Vec::new(),
        };
        run(step, &mut ctx, self.rt.handle(), &self.mgr)
    }

    fn upsert(&self, extra: &str) -> Step {
        let t: toml::Table = toml::from_str(&format!(
            "companion_url = \"{}\"\nvault_id = \"vault-x\"\n{extra}",
            self.fake.base
        ))
        .unwrap();
        Step::SearchUpsert(t)
    }

    fn delete(&self) -> Step {
        let t: toml::Table = toml::from_str(&format!(
            "companion_url = \"{}\"\nvault_id = \"vault-x\"",
            self.fake.base
        ))
        .unwrap();
        Step::SearchDelete(t)
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
fn schema_other_than_7_fails_and_outages_defer() {
    let w = world(false, true);
    w.fake.set_health(json!({"ok": true, "schemaVersion": 6}));
    let r = w.run(&w.upsert(""), Some("notes/b.md"));
    assert!(matches!(r, Err(StepError::Failed { .. })), "{r:?}");
    w.fake.set_health(json!({"ok": true}));
    assert!(matches!(
        w.run(&w.upsert(""), Some("notes/b.md")),
        Err(StepError::Failed { .. })
    ));
    w.fake.set_health(json!({"ok": false, "schemaVersion": 7}));
    assert_deferred(w.run(&w.upsert(""), Some("notes/b.md")), "search-companion");
    // Embedder down → deferred on the embedder's provider id, nothing upserted.
    w.fake.set_health(health(Some("m1"), Some(3)));
    w.fake.st.lock().unwrap().embed_down = true;
    assert_deferred(
        w.run(&w.upsert("embed = \"emb:m1\""), Some("notes/b.md")),
        "emb",
    );
    assert!(!w.fake.paths().iter().any(|p| p.starts_with("/v1/chunks/")));

    let down = world(false, false);
    assert_deferred(
        down.run(&down.upsert(""), Some("notes/b.md")),
        "search-companion",
    );
}

#[test]
fn shadow_mode_never_mutates_or_embeds() {
    let w = world(true, true);
    {
        let mut st = w.fake.st.lock().unwrap();
        st.store.insert(
            "notes/b.md".into(),
            ("stale".into(), vec!["x".into()], vec![None]),
        );
        st.store
            .insert("gone.md".into(), ("h".into(), vec!["y".into()], vec![None]));
        st.health = health(Some("m1"), Some(3));
    }
    let embed = "embed = \"emb:m1\"";
    let up = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&up, "missing_remote"), 1);
    let del = w.run(&w.delete(), Some("gone.md")).unwrap();
    assert_eq!(var(&del, "missing_local"), 1);
    let rec = w.run(&w.upsert(embed), None).unwrap();
    assert_eq!(var(&rec, "missing_remote"), 1);
    assert_eq!(var(&rec, "mismatched"), 1);
    assert_eq!(var(&rec, "missing_local"), 1);
    assert_eq!(var(&rec, "matched"), 0);
    // Only aggregate counts — no paths, no content, no live-only keys.
    let mut keys: Vec<&str> = rec.vars.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "embedding_mismatch",
            "matched",
            "mismatched",
            "missing_local",
            "missing_remote"
        ]
    );
    assert_eq!(w.fake.mutations(), 0, "{:?}", w.fake.paths());
    assert!(
        w.fake
            .paths()
            .iter()
            .all(|p| { ["/health", "/v1/paths", "/v1/files/state"].contains(&p.as_str()) })
    );
    assert!(w.fake.st.lock().unwrap().store.contains_key("gone.md"));
}

#[test]
fn live_upserts_changed_skips_unchanged_and_reembeds_on_model_drift() {
    let w = world(false, true);
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
    w.fake.st.lock().unwrap().store.insert(
        "notes/a.md".into(),
        ("old".into(), vec!["notes/a.md#9:stale".into()], vec![None]),
    );
    w.fake.set_health(health(Some("m1"), Some(3)));
    let embed = "embed = \"emb:m1\"";

    // Changed note → upsert of exactly its chunks; the stale id is gone (full replace).
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "mismatched"), 1);
    assert_eq!(var(&o, "upserted"), 1);
    let ups: Vec<Value> = w
        .fake
        .st
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|(p, _)| p == "/v1/chunks/upsert")
        .map(|(_, b)| b.clone())
        .collect();
    assert_eq!(ups.len(), 1);
    let chunks = ups[0]["chunks"].as_array().unwrap();
    assert_eq!(ups[0]["vaultId"], "vault-x");
    assert!(chunks.iter().all(|c| c["path"] == "notes/a.md"));
    let headings: Vec<&str> = chunks
        .iter()
        .map(|c| c["heading"].as_str().unwrap())
        .collect();
    assert_eq!(
        headings,
        ["Alpha", "Image: 0123456789abcdef0123456789abcdef_MD5.png"]
    );
    assert_eq!(chunks[0]["entities"][0]["text"], "Synthetic Author");
    assert_eq!(chunks[0]["embeddingModel"], "m1");
    let stored = w.fake.st.lock().unwrap().store["notes/a.md"].1.clone();
    assert!(!stored.contains(&"notes/a.md#9:stale".to_owned()));
    assert_eq!(stored.len(), 2);

    // Unchanged and fully embedded → no mutation (only health, the dim probe, files/state).
    w.fake.clear_log();
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "matched"), 1);
    assert!(!w.fake.paths().iter().any(|p| p.starts_with("/v1/chunks/")));

    // Lexical-only and unchanged → no request beyond the read half.
    w.fake.clear_log();
    let o = w.run(&w.upsert(""), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "matched"), 1);
    assert_eq!(w.fake.mutations(), 0);

    // E3: hashes match but /health reports another model → re-upsert.
    w.fake.set_health(health(Some("other-model"), Some(3)));
    w.fake.clear_log();
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "embedding_mismatch"), 1);
    assert_eq!(var(&o, "upserted"), 1);
    // E3: same model, other dimension → re-upsert too.
    w.fake.set_health(health(Some("m1"), Some(1024)));
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "embedding_mismatch"), 1);

    // A description change moves the hash → the note is mismatched again.
    w.fake.set_health(health(Some("m1"), Some(3)));
    let o = w.run(&w.upsert(embed), Some("notes/a.md")).unwrap();
    assert_eq!(var(&o, "matched"), 1);
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

    // Reconcile: b.md missing remotely gets upserted; an orphan is deleted.
    w.fake.st.lock().unwrap().store.insert(
        "orphan.md".into(),
        ("h".into(), vec!["o".into()], vec![None]),
    );
    let o = w.run(&w.upsert(embed), None).unwrap();
    assert_eq!(var(&o, "missing_remote"), 1);
    assert_eq!(var(&o, "missing_local"), 1);
    assert_eq!(var(&o, "matched"), 1);
    let store: Vec<String> = w.fake.st.lock().unwrap().store.keys().cloned().collect();
    assert_eq!(store, ["notes/a.md", "notes/b.md"]);

    // search_delete (live) drops the path.
    let o = w.run(&w.delete(), Some("notes/b.md")).unwrap();
    assert_eq!(var(&o, "deleted"), 1);
    assert!(!w.fake.st.lock().unwrap().store.contains_key("notes/b.md"));
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
