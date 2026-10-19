//! `agent` step tests (was `wp12_tests.rs`): the agent step against a `cv_testkit::MockHttp` posing as an
//! `openai-compatible` provider, synthetic notes in temp dirs and a temp protected root (I1).
//! No real provider is ever called.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::{ActionContext, ParamKind, ParamSpec};
use cv_providers::{ProviderManager, SecretSource};
use cv_state::FailureReason;
use cv_testkit::MockHttp;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value};

use super::agent;
use super::testkit::StepEnv;
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

/// Serves `reply` on 127.0.0.1; returns the base URL and the mock (its recorded requests).
async fn fake_provider(reply: Reply) -> (String, MockHttp) {
    let m = MockHttp::start(move |_| {
        if let Some(h) = &reply.hook {
            h();
        }
        cv_testkit::Reply::json(reply.status, reply.body.clone())
    });
    (format!("{}/v1", m.url()), m)
}

/// The JSON request bodies the fake provider received.
fn bodies(m: &MockHttp) -> Vec<Value> {
    m.requests()
        .iter()
        .filter_map(|r| serde_json::from_slice(&r.body).ok())
        .collect()
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

type Env = StepEnv;

fn env(config: ConfigSet) -> Env {
    let fx = LiveFixture::builder().build();
    std::fs::create_dir_all(fx.path("_custom/prompts")).unwrap();
    StepEnv::new(fx, config, action())
}

fn note(e: &Env, rel: &str, text: &str) {
    e.fx.write(rel, text);
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
    let mut ctx = e.ctx();
    ctx.step_index = 3;
    ctx.params = params;
    ctx.context = &context;
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
    e.fx.text(rel)
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
    let req = bodies(&seen)[0].clone();
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
    let req = bodies(&seen)[0].clone();
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
    let (base, _mock) = fake_provider(ok_reply("clean text", "stop")).await;
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
    let (base, _mock) = fake_provider(reply).await;
    let e = Arc::new(env(config_with(AGENTS, &providers_toml(&base))));
    note(&e, "c.md", "original\n");
    *slot.lock().unwrap() = Some(e.fx.path("c.md"));
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
    let (base, _mock) = fake_provider(Reply {
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
    let port = MockHttp::unused_port();
    let e = Arc::new(env(config_with(
        AGENTS,
        &providers_toml(&format!("http://127.0.0.1:{port}/v1")),
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
    let (base, _mock) = fake_provider(Reply {
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
    assert!(bodies(&seen).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncation_fails_and_writes_nothing() {
    let (base, _mock) = fake_provider(ok_reply("half", "length")).await;
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
fn template_render() {
    // The frontmatter split moved to `cv_note::span` (B10); its cases live there.
    let r = cv_text::template::render("{{a}} {{ b }} {{zz}} {{", |k| {
        Ok(match k.trim() {
            "a" => Some("1".into()),
            "b" => Some("2".into()),
            _ => None,
        })
    })
    .unwrap();
    assert_eq!(r, "1 2 {{zz}} {{");
}
