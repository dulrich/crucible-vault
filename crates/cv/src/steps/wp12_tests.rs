//! WP-12 tests: the agent step against a hand-rolled local HTTP responder posing as an
//! `openai-compatible` provider, synthetic notes in temp dirs and a temp protected root (I1).
//! No real provider is ever called.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::{ActionContext, ParamKind, ParamSpec};
use cv_providers::{ProviderManager, SecretSource};
use cv_state::{FailureReason, QueueConfig, StateDb};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::agent;
use super::{ChildSpawner, DeferCause, StepCtx, StepError, StepOutcome};

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cv-wp12-{}-{}",
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

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

/// One canned reply; `hook` runs before replying (simulates an edit during the slow call).
#[derive(Clone)]
struct Reply {
    status: u16,
    body: String,
    hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn ok_reply(text: &str, finish: &str) -> Reply {
    Reply {
        status: 200,
        body: serde_json::json!({
            "choices": [{"message": {"content": text}, "finish_reason": finish}]
        })
        .to_string(),
        hook: None,
    }
}

/// Serves `reply` on 127.0.0.1; returns the base URL and the captured request bodies.
async fn fake_provider(reply: Reply) -> (String, Arc<Mutex<Vec<Value>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let reply = reply.clone();
            let seen = Arc::clone(&seen2);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
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
                if let Some(h) = &reply.hook {
                    h();
                }
                let resp = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/v1"), seen)
}

fn providers_toml(base: &str) -> String {
    format!(
        r#"
[[provider]]
id = "fake"
kind = "openai-compatible"
base_url = "{base}"
api_key_env = "CV_TEST_KEY"
[[provider.models]]
id = "m1"
[[provider.models]]
id = "emb"
capabilities = ["embed"]

[[provider]]
id = "nokey"
kind = "openai-compatible"
base_url = "{base}"
api_key_env = "CV_TEST_MISSING"
[[provider.models]]
id = "m1"
"#
    )
}

const AGENTS: &str = r#"
[refine]
prompt = "Refine {{title}}:\n{{value}}"
input = "{{content}}"
model = "fake:m1"
max_tokens = 100

[refine-file]
system = "sys"
prompt_file = "_custom/prompts/p.md"
model = "fake:m1"
max_tokens = 100
"#;

fn config_with(agents: &str, providers: &str) -> ConfigSet {
    let mut c = ConfigSet::new();
    let files = BTreeMap::from([
        ("agents.toml".to_owned(), Some(agents.to_owned())),
        ("providers.toml".to_owned(), Some(providers.to_owned())),
    ]);
    c.reload(&files, &super::validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"agent\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn action() -> Action {
    Action {
        id: "refine".into(),
        label: "Refine".into(),
        group: None,
        keys: vec![],
        params: vec![ParamSpec {
            name: "provider".into(),
            label: "Provider".into(),
            kind: ParamKind::Enum,
            required: true,
            choices: vec![],
            default: None,
        }],
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

struct Env {
    _tmp: Tmp,
    vault: PathBuf,
    writer: cv_vault::WriterHandle,
    state: Arc<Mutex<StateDb>>,
    config: ConfigSet,
    action: Action,
}

fn env(config: ConfigSet) -> Env {
    let tmp = Tmp::new();
    let vault = tmp.0.join("vault");
    let protected = tmp.0.join("protected");
    std::fs::create_dir_all(vault.join("_custom/prompts")).unwrap();
    std::fs::create_dir_all(&protected).unwrap();
    let fs = cv_vault::VaultFs::live(&vault, &protected, &cv_vault::NoCutover).unwrap();
    let w = cv_vault::VaultWriter::open(fs, &tmp.0.join("index.db")).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = StateDb::open(&tmp.0.join("state.db"), 1, QueueConfig::default()).unwrap();
    Env {
        vault: std::fs::canonicalize(&vault).unwrap(),
        _tmp: tmp,
        writer,
        state: Arc::new(Mutex::new(state)),
        config,
        action: action(),
    }
}

fn note(e: &Env, rel: &str, text: &str) {
    let p = e.vault.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn run_with(
    e: &Env,
    st: &Step,
    params: &Map<String, Value>,
    path: Option<&str>,
    m: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    let context = ActionContext {
        active_path: path.map(str::to_owned),
        ..ActionContext::default()
    };
    let vars = Map::new();
    let idx = e.vault.join("idx");
    let mut ctx = StepCtx {
        job_id: "job-1",
        action: &e.action,
        step_index: 3,
        params,
        context: &context,
        vars: &vars,
        config: &e.config,
        vault_root: &e.vault,
        index_db: &idx,
        search: None,
        state: &e.state,
        writer: Some(e.writer.clone()),
        children: &NoChildren,
        written: Vec::new(),
    };
    let p = agent::parse(&st.params, &e.config).map_err(StepError::failed)?;
    agent::run(&p, &mut ctx, m)
}

/// Runs the step on a blocking thread inside the runtime hosting the fake provider.
async fn exec(
    e: Arc<Env>,
    src: &str,
    params: Map<String, Value>,
    path: Option<&str>,
) -> Result<StepOutcome, StepError> {
    let st = step(src);
    let path = path.map(str::to_owned);
    tokio::task::spawn_blocking(move || run_with(&e, &st, &params, path.as_deref(), &mgr()))
        .await
        .unwrap()
}

fn read(e: &Env, rel: &str) -> String {
    std::fs::read_to_string(e.vault.join(rel)).unwrap()
}

// ---- validate ----

fn validate(src: &str, agents: &str) -> Result<(), String> {
    let mut c = ConfigSet::new();
    let files = BTreeMap::from([
        ("agents.toml".to_owned(), Some(agents.to_owned())),
        (
            "providers.toml".to_owned(),
            Some(providers_toml("http://127.0.0.1:1/v1")),
        ),
    ]);
    c.reload(&files, &super::validate);
    super::validate(&step(src), &c)
}

#[test]
fn validate_accepts_good_step() {
    assert_eq!(validate("agent = \"refine\"", AGENTS), Ok(()));
    assert_eq!(
        validate(
            "agent = \"refine\"\nmodel = \"{param.provider}\"\noutput = \"replace\"\nmax_tokens = 5\noutput_var = \"x\"",
            AGENTS
        ),
        Ok(())
    );
}

#[test]
fn validate_rejects_bad_steps() {
    assert!(validate("agent = \"nope\"", AGENTS).is_err());
    assert!(validate("model = \"fake:m1\"", AGENTS).is_err());
    assert!(validate("agent = \"refine\"\nbogus = 1", AGENTS).is_err());
    assert!(validate("agent = \"refine\"\nmodel = \"other:m1\"", AGENTS).is_err());
    assert!(validate("agent = \"refine\"\nmodel = \"fake:zz\"", AGENTS).is_err());
    assert!(validate("agent = \"refine\"\nmodel = \"fake:emb\"", AGENTS).is_err());
    assert!(validate("agent = \"refine\"\nmodel = \"{param.}\"", AGENTS).is_err());
    assert!(validate("agent = \"refine\"\nmax_tokens = 0", AGENTS).is_err());
    assert!(validate("agent = \"refine\"\noutput = \"append\"", AGENTS).is_err());
    let no_max = "[a]\nprompt = \"x\"\nmodel = \"fake:m1\"\n";
    assert!(validate("agent = \"a\"", no_max).is_err());
    let zero_max = "[a]\nprompt = \"x\"\nmodel = \"fake:m1\"\nmax_tokens = 0\n";
    assert!(validate("agent = \"a\"", zero_max).is_err());
    let unknown = "[a]\nprompt = \"x\"\nmodel = \"fake:m1\"\nmax_tokens = 5\ncolour = 1\n";
    assert!(validate("agent = \"a\"", unknown).is_err());
}

#[test]
fn shipped_examples_validate_a_refine_step() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/examples");
    let agents = std::fs::read_to_string(root.join("agents.toml")).unwrap();
    let providers = std::fs::read_to_string(root.join("providers.toml")).unwrap();
    let c = config_with(&agents, &providers);
    for id in [
        "refine-transcript",
        "refine-transcript-openrouter",
        "refine-transcript-openrouter-gemma",
        "refine-transcript-cli",
    ] {
        let s = step(&format!("agent = \"{id}\""));
        assert_eq!(super::validate(&s, &c), Ok(()), "{id}");
    }
    let s = step(
        "agent = \"refine-transcript\"\nmodel = \"{param.provider}\"\noutput_var = \"agent_output\"",
    );
    assert_eq!(super::validate(&s, &c), Ok(()));
    for m in [
        "local:local-chat-model",
        "openrouter:@preset/example-model",
        "openrouter:@preset/example-model",
        "codex:gpt-5.5",
    ] {
        let s = step(&format!("agent = \"refine-transcript\"\nmodel = \"{m}\""));
        assert_eq!(super::validate(&s, &c), Ok(()), "{m}");
    }
}

// ---- execute ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renders_content_into_var() {
    let (base, seen) = fake_provider(ok_reply("REFINED", "stop")).await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    note(&e, "notes/talk.md", "---\ntags: [x]\n---\nraw words\n");
    let out = exec(
        Arc::clone(&e),
        "agent = \"refine\"",
        Map::new(),
        Some("notes/talk.md"),
    )
    .await
    .unwrap();
    assert_eq!(out.vars["agent_output"], "REFINED");
    assert_eq!(out.vars["agent_model"], "m1");
    assert_eq!(out.vars["agent_provider"], "fake");
    assert!(out.output_paths.is_empty());
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req["model"], "m1");
    assert_eq!(req["max_tokens"], 100);
    assert_eq!(req["messages"][1]["content"], "Refine talk:\nraw words\n");
    // var mode never writes
    assert_eq!(
        read(&e, "notes/talk.md"),
        "---\ntags: [x]\n---\nraw words\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_file_and_param_model() {
    let (base, seen) = fake_provider(ok_reply("OUT", "stop")).await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    note(&e, "_custom/prompts/p.md", "Body: {{content}}");
    note(&e, "n.md", "hello");
    let mut params = Map::new();
    params.insert("provider".into(), Value::String("fake:m1".into()));
    let out = exec(
        Arc::clone(&e),
        "agent = \"refine-file\"\nmodel = \"{param.provider}\"\noutput_var = \"r\"\nmax_tokens = 7",
        params,
        Some("n.md"),
    )
    .await
    .unwrap();
    assert_eq!(out.vars["r"], "OUT");
    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(req["messages"][0]["content"], "sys");
    assert_eq!(req["messages"][1]["content"], "Body: hello");
    assert_eq!(req["max_tokens"], 7);

    // a bare provider id picks the agent's default model on it
    let mut params = Map::new();
    params.insert("provider".into(), Value::String("fake".into()));
    let out = exec(
        Arc::clone(&e),
        "agent = \"refine-file\"\nmodel = \"{param.provider}\"",
        params,
        Some("n.md"),
    )
    .await
    .unwrap();
    assert_eq!(out.vars["agent_model"], "m1");

    // undeclared / unknown provider param fails
    let mut params = Map::new();
    params.insert("provider".into(), Value::String("ghost:m1".into()));
    let err = exec(
        Arc::clone(&e),
        "agent = \"refine-file\"\nmodel = \"{param.provider}\"",
        params,
        Some("n.md"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StepError::Failed { .. }));
    let err = exec(
        Arc::clone(&e),
        "agent = \"refine-file\"\nmodel = \"{param.other}\"",
        Map::new(),
        Some("n.md"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StepError::Failed { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_mode_cas_write() {
    let (base, _) = fake_provider(ok_reply("clean text", "stop")).await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    note(&e, "t.md", "---\na: 1\n---\nmessy\n");
    let out = exec(
        Arc::clone(&e),
        "agent = \"refine\"\noutput = \"replace\"",
        Map::new(),
        Some("t.md"),
    )
    .await
    .unwrap();
    assert_eq!(out.output_paths, vec!["t.md".to_owned()]);
    assert_eq!(read(&e, "t.md"), "---\na: 1\n---\nclean text\n");
    // convergent re-run: same completion → identical bytes
    exec(
        Arc::clone(&e),
        "agent = \"refine\"\noutput = \"replace\"",
        Map::new(),
        Some("t.md"),
    )
    .await
    .unwrap();
    assert_eq!(read(&e, "t.md"), "---\na: 1\n---\nclean text\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_mode_conflict_defers_and_never_clobbers() {
    let slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let s2 = Arc::clone(&slot);
    let mut reply = ok_reply("clobber", "stop");
    reply.hook = Some(Arc::new(move || {
        if let Some(p) = s2.lock().unwrap().as_ref() {
            std::fs::write(p, "user edit\n").unwrap();
        }
    }));
    let (base, _) = fake_provider(reply).await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    note(&e, "c.md", "original\n");
    *slot.lock().unwrap() = Some(e.vault.join("c.md"));
    let err = exec(
        Arc::clone(&e),
        "agent = \"refine\"\noutput = \"replace\"",
        Map::new(),
        Some("c.md"),
    )
    .await
    .unwrap_err();
    // SR7: Replace keeps strict CAS (its output is not re-appliable) but a lost race defers.
    assert_eq!(
        err,
        StepError::Deferred {
            retry_after_ms: super::CONFLICT_RETRY_MS,
            service: None,
            cause: super::DeferCause::Wait,
        }
    );
    assert_eq!(read(&e, "c.md"), "user edit\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outage_is_deferred() {
    let (base, _) = fake_provider(Reply {
        status: 503,
        body: "{}".into(),
        hook: None,
    })
    .await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    note(&e, "n.md", "x");
    let err = exec(
        Arc::clone(&e),
        "agent = \"refine\"",
        Map::new(),
        Some("n.md"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        StepError::Deferred {
            retry_after_ms: 60_000,
            service: Some("fake".into()),
            cause: DeferCause::Transient(cv_state::ServiceFailureKind::ServerError)
        }
    );

    // server down: bind then drop to get a port nobody listens on
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let e = Arc::new(env(config_with(
        AGENTS,
        &providers_toml(&format!("http://{addr}/v1")),
    )));
    note(&e, "n.md", "x");
    let err = exec(
        Arc::clone(&e),
        "agent = \"refine\"",
        Map::new(),
        Some("n.md"),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err,
        StepError::Deferred {
            retry_after_ms: 60_000,
            service: Some("fake".into()),
            cause: DeferCause::Transient(cv_state::ServiceFailureKind::Refused)
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_4xx_fails() {
    let (base, _) = fake_provider(Reply {
        status: 400,
        body: "{}".into(),
        hook: None,
    })
    .await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    let err = exec(Arc::clone(&e), "agent = \"refine\"", Map::new(), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StepError::Failed { reason: None, .. }),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_key_is_no_api_key() {
    let (base, seen) = fake_provider(ok_reply("x", "stop")).await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    let err = exec(
        Arc::clone(&e),
        "agent = \"refine\"\nmodel = \"nokey:m1\"",
        Map::new(),
        None,
    )
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
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncation_fails_and_writes_nothing() {
    let (base, _) = fake_provider(ok_reply("half", "length")).await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    note(&e, "n.md", "orig\n");
    let err = exec(
        Arc::clone(&e),
        "agent = \"refine\"\noutput = \"replace\"",
        Map::new(),
        Some("n.md"),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, StepError::Failed { message, .. } if message.contains("truncated")),
        "{err:?}"
    );
    assert_eq!(read(&e, "n.md"), "orig\n");
}

#[test]
fn no_runtime_fails() {
    let e = env(config_with(
        AGENTS,
        &providers_toml("http://127.0.0.1:1/v1"),
    ));
    let err = run_with(&e, &step("agent = \"refine\""), &Map::new(), None, &mgr()).unwrap_err();
    assert_eq!(err, StepError::failed("no async runtime"));
}

#[test]
fn shared_manager_is_one_instance() {
    let a = agent::shared_manager_ptr().unwrap();
    let b = agent::shared_manager_ptr().unwrap();
    assert!(std::ptr::eq(a, b));
}

#[test]
fn frontmatter_split_and_render() {
    assert_eq!(
        agent::split_frontmatter("---\na: 1\n---\nbody\n"),
        ("---\na: 1\n---\n", "body\n")
    );
    assert_eq!(agent::split_frontmatter("no fm\n"), ("", "no fm\n"));
    assert_eq!(
        agent::split_frontmatter("---\nunclosed\n"),
        ("", "---\nunclosed\n")
    );
    let r = agent::render("{{a}} {{ b }} {{zz}} {{", &|k| match k {
        "a" => Some("1".into()),
        "b" => Some("2".into()),
        _ => None,
    });
    assert_eq!(r, "1 2 {{zz}} {{");
}
