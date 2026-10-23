//! Runner ↔ daemon tests: an in-process live daemon on a synthetic temp vault (asserted under the
//! temp dir), driven over its socket. Pattern from `tests/daemon.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cv_actions::Step;
use cv_protocol::{
    ActionContext, ChangeKind, JobState, Notification, Origin, Request, RunActionParams,
    SubscribeParams, Topic, error_codes,
};
use cv_vault::{ContentHash, WatchConfig};
use serde_json::{Map, Value, json};

use super::{StepExecutor, Trigger};
use crate::steps::{StepCtx, StepError, StepOutcome};
use crate::{Client, ClientError, Config, Daemon, DaemonOptions, RunningDaemon};
use cv_testkit::{MockHttp, Reply};

struct Env {
    root: PathBuf,
    vars: HashMap<String, String>,
    _dir: tempfile::TempDir,
}

impl Env {
    fn new(actions: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let vault = root.join("vault").join("Synth");
        std::fs::create_dir_all(vault.join("inbox")).unwrap();
        cv_vault::test_support::assert_temp_root(&vault);
        std::fs::write(vault.join("a.md"), "# A\n").unwrap();
        std::fs::write(vault.join("inbox/x.md"), "# X\n").unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/actions.toml"), actions).unwrap();
        let mut vars = HashMap::new();
        for (k, v) in [
            ("HOME", root.join("home")),
            ("CV_VAULT", vault),
            ("CV_STATE_DB", root.join("state/state.db")),
            ("CV_SEARCH_DB", root.join("state/search.db")),
            ("CV_INDEX_DB", root.join("cache/index.db")),
            ("CV_CONFIG_DIR", root.join("config")),
            ("CV_SOCKET", root.join("run/d.sock")),
        ] {
            vars.insert(k.to_owned(), v.display().to_string());
        }
        Self {
            root,
            vars,
            _dir: dir,
        }
    }
    fn config(&self) -> Config {
        let vars = self.vars.clone();
        Config::resolve(move |k| vars.get(k).cloned()).unwrap()
    }
    fn vault(&self) -> PathBuf {
        PathBuf::from(&self.vars["CV_VAULT"])
    }
    fn client(&self) -> Client {
        Client::connect(
            Path::new(&self.vars["CV_SOCKET"]),
            "test",
            cv_index::SCHEMA_VERSION,
        )
        .unwrap()
    }
}

async fn start(env: &Env, exec: Arc<dyn StepExecutor>) -> RunningDaemon {
    let opts = DaemonOptions {
        watch: WatchConfig {
            debounce: Duration::from_millis(50),
            echo_ttl: Duration::from_secs(5),
        },
        config_poll: Duration::from_millis(100),
    };
    Daemon::start(env.config(), opts)
        .unwrap()
        .spawn_with(exec)
        .await
        .unwrap()
}

type Behavior = dyn Fn(&Step, &mut StepCtx<'_>) -> Result<StepOutcome, StepError> + Send + Sync;
struct FnExec(Box<Behavior>);
impl StepExecutor for FnExec {
    fn execute(&self, step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        (self.0)(step, ctx)
    }
}

/// `set_props` stand-in: appends a line to the active note through the writer (CAS on the hash
/// it read) and asks the client to open it.
fn appending_exec(calls: Arc<AtomicUsize>) -> Arc<dyn StepExecutor> {
    Arc::new(FnExec(Box::new(move |_, ctx| {
        calls.fetch_add(1, Ordering::SeqCst);
        let path = ctx
            .context
            .active_path
            .clone()
            .ok_or_else(|| StepError::failed("no path"))?;
        let (mut bytes, hash) = ctx
            .read(&path)?
            .ok_or_else(|| StepError::failed("missing"))?;
        bytes.extend_from_slice(b"touched\n");
        ctx.write(&path, bytes, Some(hash))?;
        Ok(StepOutcome {
            open_path: Some(path.clone()),
            output_paths: vec![path],
            ..StepOutcome::default()
        })
    })))
}

fn rpc_code(r: Result<Value, ClientError>) -> i64 {
    match r {
        Err(ClientError::Rpc(e)) => e.code,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

fn poll(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("condition not reached");
}

fn run(action: &str, params: Value, ctx: ActionContext) -> Request {
    let params: Map<String, Value> = serde_json::from_value(params).unwrap();
    Request::RunAction(RunActionParams {
        action_id: action.into(),
        params,
        context: ctx,
    })
}

const ACTIONS: &str = r#"
[[action]]
id = "touch"
label = "Touch note"
group = "Notes"
keys = ["ctrl+t"]
needs = ["active_path"]
durable = false
[[action.params]]
name = "mode"
label = "Mode"
kind = "enum"
choices = ["a", "b"]
default = "a"
[[action.params]]
name = "why"
label = "Why"
kind = "text"
required = true
[[action.steps]]
type = "set_props"

[[action]]
id = "bg"
needs = ["active_path"]
[[action.steps]]
type = "set_props"

[[action]]
id = "broken"
[[action.steps]]
type = "run_action"
action = "nowhere"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_action_end_to_end_over_the_socket() {
    let env = Env::new(ACTIONS);
    let calls = Arc::new(AtomicUsize::new(0));
    let d = start(&env, appending_exec(Arc::clone(&calls))).await;
    let c = env.client();

    // actions_list: armed actions only, with their ParamSpecs.
    let list = c.request(Request::ActionsList).unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["bg", "touch"]);
    assert_eq!(list[1]["title"], "Touch note");
    assert_eq!(list[1]["durable"], false);
    assert_eq!(list[1]["params"][0]["kind"], "enum");
    assert_eq!(list[1]["params"][0]["choices"], json!(["a", "b"]));
    assert_eq!(list[1]["params"][1]["required"], true);
    let status = c.request(Request::Status).unwrap();
    assert_eq!(
        status["config_errors"].as_array().unwrap().len(),
        1,
        "broken disarmed + reported"
    );

    let a_hash = ContentHash::of(b"# A\n").as_str().to_owned();
    let ctx = |h: &str| ActionContext {
        active_path: Some("a.md".into()),
        buffer_hash: Some(h.into()),
        ..ActionContext::default()
    };
    // Unknown / disarmed / missing param / missing context.
    assert_eq!(
        rpc_code(c.request(run("nope", json!({}), ActionContext::default()))),
        error_codes::NOT_FOUND
    );
    assert_eq!(
        rpc_code(c.request(run("broken", json!({}), ActionContext::default()))),
        error_codes::INVALID_PARAMS
    );
    assert_eq!(
        rpc_code(c.request(run("touch", json!({}), ctx(&a_hash)))),
        error_codes::INVALID_PARAMS
    );
    assert_eq!(
        rpc_code(c.request(run("touch", json!({"why": "x"}), ActionContext::default()))),
        error_codes::INVALID_PARAMS
    );
    // Stale buffer_hash → CONFLICT, nothing enqueued.
    assert_eq!(
        rpc_code(c.request(run("touch", json!({"why": "x"}), ctx("deadbeef")))),
        error_codes::CONFLICT
    );
    assert!(
        d.shared()
            .state()
            .lock()
            .unwrap()
            .counts()
            .unwrap()
            .is_empty(),
        "nothing enqueued"
    );

    // Non-durable: returns after completion with open_path.
    let r = c
        .request(run("touch", json!({"why": "x"}), ctx(&a_hash)))
        .unwrap();
    assert_eq!(r["open_path"], "a.md");
    let job_id = r["job_id"].as_str().unwrap().to_owned();
    let job = d
        .shared()
        .state()
        .lock()
        .unwrap()
        .job(&job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, cv_state::JobStatus::Done);
    assert_eq!(job.lane, cv_protocol::Lane::User);
    assert_eq!(job.resource_keys, vec!["a.md".to_owned()]);
    let jp: super::JobParams = serde_json::from_value(job.params).unwrap();
    assert_eq!(jp.params["mode"], "a", "ParamSpec default applied");
    assert_eq!(jp.trigger, Trigger::Command);

    // Durable: job id at once; then completes.
    let r = c
        .request(run(
            "bg",
            json!({}),
            ActionContext {
                active_path: Some("a.md".into()),
                ..ActionContext::default()
            },
        ))
        .unwrap();
    assert!(r.get("open_path").is_none());
    let bg = r["job_id"].as_str().unwrap().to_owned();
    poll(|| {
        d.shared()
            .state()
            .lock()
            .unwrap()
            .job(&bg)
            .unwrap()
            .unwrap()
            .status
            == cv_state::JobStatus::Done
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Both jobs' writes landed in the (temp) vault.
    assert_eq!(
        std::fs::read_to_string(env.vault().join("a.md")).unwrap(),
        "# A\ntouched\ntouched\n"
    );

    // Hot reload: a broken actions.toml keeps the old actions and reports the error.
    c.request(Request::Subscribe(SubscribeParams {
        topics: vec![Topic::ConfigReloaded],
    }))
    .unwrap();
    std::fs::write(env.root.join("config/actions.toml"), "[[action]\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let errors = loop {
        assert!(Instant::now() < deadline, "no config_reloaded");
        if let Some(Notification::ConfigReloaded { errors }) =
            c.next_notification(Duration::from_millis(200)).unwrap()
        {
            break errors;
        }
    };
    assert!(
        errors.iter().any(|e| e.starts_with("actions.toml:")),
        "{errors:?}"
    );
    assert_eq!(
        c.request(Request::ActionsList)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2,
        "last valid version kept"
    );
    drop(c);
    d.shutdown().await.unwrap();
}

const EVENTS: &str = r#"
[[action]]
id = "inbox"
starters = [{ type = "event", kinds = ["created", "modified"], origin = "any", selector = { folder = "inbox" } }]
[[action.steps]]
type = "set_props"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn event_starter_fires_on_live_edits_only_and_cycle_guard_holds() {
    let env = Env::new(EVENTS);
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    let calls = Arc::new(AtomicUsize::new(0));
    let inner = appending_exec(Arc::clone(&calls));
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(move |step, ctx| {
        s2.lock()
            .unwrap()
            .push(ctx.context.active_path.clone().unwrap_or_default());
        inner.execute(step, ctx)
    })));
    let d = start(&env, exec).await;
    // I4: the startup scan of inbox/x.md fires nothing.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no startup replay");

    // An external edit matching the selector starts a background run…
    std::fs::write(env.vault().join("inbox/x.md"), "# X\nedited\n").unwrap();
    poll(|| calls.load(Ordering::SeqCst) == 1);
    // …an external edit outside the selector does not.
    std::fs::write(env.vault().join("a.md"), "# A\nedited\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(*seen.lock().unwrap(), vec!["inbox/x.md".to_owned()]);
    let jobs = d
        .shared()
        .state()
        .lock()
        .unwrap()
        .list(&cv_state::ListQuery::default())
        .unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].lane, cv_protocol::Lane::Background);
    poll(|| {
        d.shared()
            .state()
            .lock()
            .unwrap()
            .job(&jobs[0].id)
            .unwrap()
            .unwrap()
            .status
            == cv_state::JobStatus::Done
    });

    // The action wrote its own trigger note; that write's daemon-origin echo must not re-start it
    // (the echo is injected as the watcher would send it).
    let mut rx = d.subscribe();
    d.shared().publish(Notification::FileChanged {
        path: "inbox/x.md".into(),
        kind: ChangeKind::Modified,
        hash: None,
        origin: Origin::Daemon,
        from: None,
        replaced_by: None,
    });
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cycle guard: no self-feed");
    while let Ok(n) = rx.try_recv() {
        assert!(
            !matches!(n, Notification::JobUpdated(ref s) if s.status == JobState::Pending),
            "no new job"
        );
    }
    d.shutdown().await.unwrap();
}

/// Round 11 WP-1 (Decision 1): a writer delete recorded as absorbed (auto-file's `trash`) reaches
/// clients as `file_changed { deleted, replaced_by }` through the watcher's echo path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replaced_delete_reports_the_absorbing_note() {
    let env = Env::new("");
    let d = start(&env, appending_exec(Arc::new(AtomicUsize::new(0)))).await;
    let mut rx = d.subscribe();
    let w = d.shared().writer().expect("writer");
    w.call(|vw| {
        let h = vw.fs().hash("inbox/x.md")?.expect("present");
        vw.delete_replaced("inbox/x.md", h, "a.md")
    })
    .unwrap()
    .unwrap();
    drop(w); // a live handle would keep the writer thread (and shutdown) waiting
    assert!(!env.vault().join("inbox/x.md").exists());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got = None;
    while got.is_none() && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Notification::FileChanged {
                path,
                kind: ChangeKind::Deleted,
                replaced_by,
                ..
            }) if path == "inbox/x.md" => got = Some(replaced_by),
            Ok(_) => {}
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    assert_eq!(got, Some(Some("a.md".to_owned())));
    d.shutdown().await.unwrap();
}

#[test]
fn example_config_files_load_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(
        dir.join("actions.toml"),
        include_str!("../../../../config/examples/actions.toml"),
    )
    .unwrap();
    std::fs::write(
        dir.join("schedules.toml"),
        include_str!("../../../../config/examples/schedules.toml"),
    )
    .unwrap();
    // WP-10's example files (period settings, capture presets, kinds).
    std::fs::write(
        dir.join("captures.toml"),
        include_str!("../../../../config/examples/captures.toml"),
    )
    .unwrap();
    std::fs::write(
        dir.join("kinds.toml"),
        include_str!("../../../../config/examples/kinds.toml"),
    )
    .unwrap();
    // WP-12's files: refine-transcript's agent step validates against them (WP-10 x WP-12 contract).
    std::fs::write(
        dir.join("agents.toml"),
        include_str!("../../../../config/examples/agents.toml"),
    )
    .unwrap();
    std::fs::write(
        dir.join("providers.toml"),
        include_str!("../../../../config/examples/providers.toml"),
    )
    .unwrap();
    // Plan v0-automation-pipelines WP-12: the pipeline config and the source registry.
    std::fs::write(
        dir.join("pipelines.toml"),
        include_str!("../../../../config/examples/pipelines.toml"),
    )
    .unwrap();
    std::fs::write(
        dir.join("sources.toml"),
        include_str!("../../../../config/examples/sources.toml"),
    )
    .unwrap();
    let mut set = cv_actions::ConfigSet::new();
    super::load_config_dir(&dir, &mut set);
    drop(tmp);
    assert!(set.errors().is_empty(), "{:?}", set.errors());
    assert_eq!(set.armed_actions().count(), 34);
    assert_eq!(set.pipelines().x.inbox.as_deref(), Some("Clippings"));
    assert_eq!(set.sources().blog.len(), 1);
    assert!(
        set.armed("search-refresh")
            .unwrap()
            .starters
            .iter()
            .any(|s| matches!(s, cv_actions::Starter::Schedule(_)))
    );
}

const MOVE_ACTIONS: &str = r#"
[[action]]
id = "file-away"
needs = ["active_path"]
[[action.steps]]
type = "move"
to = "archive"
"#;

/// Plan editor-usability-round WP-5 (audit B1): a durable action whose `move` step relocates the
/// note publishes the destination as `open_path` on its terminal `JobUpdated`, so the editor can
/// follow the note instead of orphaning its tab.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_move_publishes_open_path_on_terminal_status() {
    let env = Env::new(MOVE_ACTIONS);
    let d = Daemon::start(
        env.config(),
        DaemonOptions {
            watch: WatchConfig {
                debounce: Duration::from_millis(50),
                echo_ttl: Duration::from_secs(5),
            },
            config_poll: Duration::from_millis(100),
        },
    )
    .unwrap()
    .spawn()
    .await
    .unwrap();
    let mut rx = d.subscribe();
    let c = env.client();
    let r = c
        .request(run(
            "file-away",
            json!({}),
            ActionContext {
                active_path: Some("inbox/x.md".into()),
                ..ActionContext::default()
            },
        ))
        .unwrap();
    assert!(r.get("open_path").is_none(), "durable: job id only");
    let job_id = r["job_id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(10);
    let terminal = loop {
        assert!(Instant::now() < deadline, "no terminal status");
        match rx.try_recv() {
            Ok(Notification::JobUpdated(s)) if s.job_id == job_id => {
                if matches!(s.status, JobState::Pending | JobState::Running) {
                    assert_eq!(s.open_path, None, "non-terminal carries no open_path");
                } else {
                    break s;
                }
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert_eq!(terminal.status, JobState::Done, "{:?}", terminal.message);
    assert_eq!(terminal.open_path.as_deref(), Some("archive/x.md"));
    d.shutdown().await.unwrap();
}

// --- `search` request (plan semsearch-crate WP-4) ------------------------------------------

/// A fake OpenAI-compatible embedder: every `/embeddings` input gets [`SEARCH_VEC`].
fn fake_embedder() -> MockHttp {
    MockHttp::start(|req| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
        let n = body["input"].as_array().map_or(1, Vec::len);
        let data: Vec<Value> = (0..n)
            .map(|i| json!({"index": i, "embedding": SEARCH_VEC}))
            .collect();
        Reply::json(200, json!({ "data": data }).to_string())
    })
}

const SEARCH_VEC: [f32; 3] = [0.1, 0.2, 0.3];

/// A feed stand-in: writes one embedded chunk for `notes/zebra.md` through the runner's shared
/// handle (`StepCtx::search`), in the space the `embed = "emb:m1"` target maps to.
fn feeding_exec() -> Arc<dyn StepExecutor> {
    Arc::new(FnExec(Box::new(|_, ctx| {
        let db = ctx
            .search
            .ok_or_else(|| StepError::failed("no search db"))?;
        let shared = db.shared().map_err(|e| StepError::failed(e.to_string()))?;
        let chunk = cv_search::semsearch::Chunk::new(
            "c1",
            "Zebra",
            "Stripes",
            "synthetic zebra stripes text",
        )
        .with_embedding(SEARCH_VEC.to_vec());
        let space = cv_search::semsearch::Space::new("m1", 3);
        cv_search::index::lock(&shared)
            .upsert_source_in(&space, "notes/zebra.md", "h1", &[chunk])
            .map_err(|e| StepError::failed(e.to_string()))?;
        Ok(StepOutcome::default())
    })))
}

fn search_env(embed_base: &str) -> Env {
    let env = Env::new(
        "[[action]]\nid = \"feed\"\ndurable = false\n[[action.steps]]\ntype = \"search_upsert\"\nembed = \"emb:m1\"\n",
    );
    std::fs::write(
        env.root.join("config/providers.toml"),
        format!(
            "[[provider]]\nid = \"emb\"\nkind = \"openai-compatible\"\nbase_url = \"{embed_base}\"\nlocal = true\n"
        ),
    )
    .unwrap();
    env
}

fn search(c: &mut Client, q: &str) -> cv_protocol::SearchResult {
    let v = c
        .request(Request::Search(cv_protocol::SearchParams {
            query: q.into(),
            limit: 10,
        }))
        .unwrap();
    serde_json::from_value(v).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_sees_a_feed_write_in_the_same_daemon_and_is_semantic_with_the_embedder_up() {
    let embedder = fake_embedder();
    let env = search_env(&embedder.url());
    let d = start(&env, feeding_exec()).await;
    let mut c = env.client();
    assert!(
        c.request(Request::Status).unwrap()["config_errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let r = search(&mut c, "zebra");
    assert!(r.hits.is_empty(), "nothing fed yet");
    assert!(!r.semantic, "empty index: keyword-only");

    c.request(run("feed", json!({}), ActionContext::default()))
        .unwrap();
    let r = search(&mut c, "zebra");
    assert!(r.semantic, "embedder up, matching space");
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].path, "notes/zebra.md");
    assert_eq!(r.hits[0].heading, "Stripes");

    let r = search(&mut c, "   ");
    assert!(r.hits.is_empty() && !r.semantic);
    d.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_falls_back_to_keyword_only_when_the_embedder_is_down() {
    // Port 9 (discard) on loopback: connection refused → a transport error.
    let env = search_env("http://127.0.0.1:9");
    let d = start(&env, feeding_exec()).await;
    let mut c = env.client();
    c.request(run("feed", json!({}), ActionContext::default()))
        .unwrap();
    let r = search(&mut c, "zebra");
    assert!(!r.semantic, "embedder down: keyword-only");
    assert_eq!(r.hits.len(), 1, "hits still returned");
    assert_eq!(r.hits[0].path, "notes/zebra.md");
    d.shutdown().await.unwrap();
}

/// A slow call runs off the connection's read loop, so a `status` sent after it on
/// the same connection answers first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_call_does_not_block_its_connection() {
    use cv_protocol::client::RpcClient;
    use cv_protocol::methods;
    const SLOW: &str = "[[action]]\nid = \"slow\"\nneeds = [\"active_path\"]\ndurable = false\n[[action.steps]]\ntype = \"set_props\"\n";
    let env = Env::new(SLOW);
    let exec: Arc<dyn StepExecutor> = Arc::new(FnExec(Box::new(|_, _| {
        std::thread::sleep(Duration::from_millis(1500));
        Ok(StepOutcome::default())
    })));
    let d = start(&env, exec).await;
    let c = Arc::new(
        RpcClient::connect(
            Path::new(&env.vars["CV_SOCKET"]),
            "test",
            cv_index::SCHEMA_VERSION,
        )
        .unwrap(),
    );
    let slow = {
        let c = Arc::clone(&c);
        std::thread::spawn(move || {
            let t = Instant::now();
            c.call::<methods::RunAction>(RunActionParams {
                action_id: "slow".into(),
                params: Map::new(),
                context: ActionContext {
                    active_path: Some("a.md".into()),
                    ..ActionContext::default()
                },
            })
            .unwrap();
            t.elapsed()
        })
    };
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    c.call::<methods::Status>(()).unwrap();
    let status_took = t.elapsed();
    let slow_took = slow.join().unwrap();
    assert!(slow_took >= Duration::from_millis(1200), "{slow_took:?}");
    assert!(status_took < Duration::from_millis(800), "{status_took:?}");
    d.shutdown().await.unwrap();
}
