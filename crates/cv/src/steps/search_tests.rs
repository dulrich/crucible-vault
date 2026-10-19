//! `search` step tests (was `wp13_tests.rs`; plan `v0-core-daemon` WP-13, rewritten for plan
//! semsearch-crate WP-3): synthetic notes in a temp vault with a temp protected root (via
//! [`super::testkit`]), a temp `state.db`, a temp `semsearch` index, and a `cv_testkit::MockHttp`
//! fake embedder on 127.0.0.1 that records every request (I1 — never a real index or any real
//! provider).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use cv_actions::{ConfigSet, Step, StepKind};
use cv_protocol::ActionContext;
use cv_providers::{ProviderManager, SecretSource};
use cv_search::SearchDb;
use cv_search::semsearch::{Query, Space};
use cv_state::ImageDescription;
use cv_testkit::{MockHttp, Reply};
use cv_vault::test_support::LiveFixture;
use serde_json::{Value, json};

use super::search::run;
use super::testkit::{StepEnv, config_of};
use super::{DeferCause, StepError, StepOutcome};

#[derive(Default)]
struct FakeState {
    embed_down: bool,
}

struct Fake {
    http: MockHttp,
    st: Arc<Mutex<FakeState>>,
}

impl Fake {
    fn embed_calls(&self) -> usize {
        self.http
            .requests()
            .iter()
            .filter(|r| r.target == "/embeddings")
            .count()
    }
}

/// Every vector the fake embedder returns (3 dims).
const VEC: [f32; 3] = [0.1, 0.2, 0.3];

fn handle(st: &FakeState, path: &str, body: &Value) -> (u16, Value) {
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

fn fake() -> Fake {
    let st = Arc::new(Mutex::new(FakeState::default()));
    let st2 = Arc::clone(&st);
    let http = MockHttp::start(move |req| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let (status, out) = handle(&st2.lock().unwrap(), &req.target, &body);
        Reply::json(status, out.to_string())
    });
    Fake { http, st }
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
    env: StepEnv,
    rt: tokio::runtime::Runtime,
    fake: Fake,
    search: SearchDb,
    mgr: ProviderManager,
}

impl std::ops::Deref for World {
    type Target = StepEnv;
    fn deref(&self) -> &StepEnv {
        &self.env
    }
}

const NOTE_A: &str = "---\nauthor: Synthetic Author\n---\n# Alpha\nFirst synthetic note.\n\n![[0123456789abcdef0123456789abcdef_MD5.png]]";
const NOTE_B: &str = "# Beta\nSecond synthetic note.";

fn world() -> World {
    let fx = LiveFixture::builder()
        .file("notes/a.md", NOTE_A)
        .file("notes/b.md", NOTE_B)
        .file("att/0123456789abcdef0123456789abcdef_MD5.png", b"png")
        .build();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let fake = fake();
    let actions = "[[action]]\nid = \"s\"\n[[action.steps]]\ntype = \"search_upsert\"\n";
    let providers = format!(
        "[[provider]]\nid = \"emb\"\nkind = \"openai-compatible\"\nbase_url = \"{}\"\n",
        fake.http.url()
    );
    let config = config_of(&[("actions.toml", actions), ("providers.toml", &providers)]);
    let search = SearchDb::new(fx.root().join("state").join("search.db"));
    let action = config.action("s").unwrap().clone();
    World {
        env: StepEnv::new(fx, config, action),
        rt,
        fake,
        search,
        mgr: ProviderManager::new(Arc::new(NoSecrets)).unwrap(),
    }
}

impl World {
    fn run(&self, step: &Step, active: Option<&str>) -> Result<StepOutcome, StepError> {
        let context = ActionContext {
            active_path: active.map(str::to_owned),
            ..ActionContext::default()
        };
        let mut ctx = self.ctx();
        ctx.context = &context;
        ctx.search = Some(&self.search);
        let p = super::search::parse(step.kind, &step.params).map_err(StepError::failed)?;
        run(&p, &mut ctx, self.rt.handle(), &self.mgr)
    }

    fn upsert(&self, extra: &str) -> Step {
        let t: toml::Table = toml::from_str(extra).unwrap();
        Step {
            kind: StepKind::SearchUpsert,
            params: t,
        }
    }

    fn delete(&self) -> Step {
        Step {
            kind: StepKind::SearchDelete,
            params: toml::Table::new(),
        }
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
