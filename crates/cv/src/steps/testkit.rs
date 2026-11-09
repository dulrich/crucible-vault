//! Shared step-test harness: [`StepEnv`] owns a
//! [`LiveFixture`] (temp vault + writer), a temp `state.db` and the
//! config, and hands out [`StepCtx`]s; tests override single `StepCtx` fields where they differ.
//! Also hosts the plain-[`Action`] builder, the provider fakes (secrets, manager, fake completion
//! server, `providers.toml` text) and the example-config harness the materialize/capture/file/lint tests share.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::{ActionContext, ParamSpec};
use cv_providers::{ProviderManager, SecretSource};
use cv_state::{QueueConfig, StateDb};
use cv_testkit::MockHttp;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value, json};

use super::{ChildSpawner, StepCtx, StepError, StepOutcome};

/// Builds a plain test [`Action`]: id, steps and resources are explicit, every other field is the
/// default the plain literals used (`label` = the id, no group/keys/params/needs, `Auto` lane,
/// durable, `hold`/`fast` false, no starters); a test that needs another field sets it through
/// the builder method.
pub(crate) struct ActionBuilder(Action);

impl ActionBuilder {
    pub fn new(id: &str, steps: Vec<Step>, resources: ResourceSpec) -> Self {
        Self(Action {
            id: id.into(),
            label: id.into(),
            group: None,
            keys: vec![],
            params: vec![],
            needs: vec![],
            lane: LaneChoice::Auto,
            durable: true,
            hold: false,
            fast: false,
            steps,
            starters: vec![],
            resources,
        })
    }

    /// The action's prompted parameters.
    #[must_use]
    pub fn params(mut self, params: Vec<ParamSpec>) -> Self {
        self.0.params = params;
        self
    }

    pub fn build(self) -> Action {
        self.0
    }
}

/// A secret source answering `env_var` from a map and no secret files.
pub(crate) struct TestSecrets(pub HashMap<String, String>);
impl SecretSource for TestSecrets {
    fn env_var(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
    fn secret_file(&self, _name: &str) -> Option<String> {
        None
    }
}

/// A provider manager whose only secret is `CV_TEST_KEY = "test-key"`.
pub(crate) fn mgr() -> ProviderManager {
    let mut m = HashMap::new();
    m.insert("CV_TEST_KEY".to_owned(), "test-key".to_owned());
    ProviderManager::new(Arc::new(TestSecrets(m))).unwrap()
}

/// Runs before a fake provider replies (simulates an edit during the slow call).
pub(crate) type Hook = Arc<dyn Fn() + Send + Sync>;

/// An OpenAI chat-completions body with one choice.
pub(crate) fn completion(text: &str, finish: &str) -> String {
    json!({"choices": [{"message": {"content": text}, "finish_reason": finish}]}).to_string()
}

/// A fake provider answering every request with `status` and the raw `body` (after `hook`);
/// returns the base URL and the mock (its recorded requests).
pub(crate) fn fake_raw(status: u16, body: String, hook: Option<Hook>) -> (String, MockHttp) {
    let m = MockHttp::start(move |_| {
        if let Some(h) = &hook {
            h();
        }
        cv_testkit::Reply::json(status, body.clone())
    });
    (format!("{}/v1", m.url()), m)
}

/// A fake provider answering every request with a completion of `text` (finish `finish`).
pub(crate) fn fake(
    status: u16,
    text: &str,
    finish: &str,
    hook: Option<Hook>,
) -> (String, MockHttp) {
    fake_raw(status, completion(text, finish), hook)
}

/// A `providers.toml` with one `fake` openai-compatible provider (model `m1`) keyed by `key_env`.
pub(crate) fn providers(base: &str, key_env: &str) -> String {
    format!(
        r#"
[[provider]]
id = "fake"
kind = "openai-compatible"
base_url = "{base}"
api_key_env = "{key_env}"
[[provider.models]]
id = "m1"
"#
    )
}

/// Spawns nothing (tests that do not exercise child jobs): every spawn succeeds as a no-op, so
/// an action ending in `run_action` (e.g. `file-as-kind` → `clip-enrich`) runs to completion.
pub(crate) struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Ok("no-child".into())
    }

    fn spawn_child_at(&self, _: &str, _: Map<String, Value>, _: &str) -> Result<String, String> {
        Ok("no-child".into())
    }
}

/// Records every `spawn_child_at` as `action@path` (every other spawn is refused).
#[derive(Default)]
pub(crate) struct AtRecorder(pub Mutex<Vec<String>>);
impl AtRecorder {
    /// The recorded `action@path` calls.
    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}
impl ChildSpawner for AtRecorder {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("unexpected spawn_child".into())
    }

    fn spawn_child_at(
        &self,
        id: &str,
        _: Map<String, Value>,
        path: &str,
    ) -> Result<String, String> {
        self.0.lock().unwrap().push(format!("{id}@{path}"));
        Ok("job".into())
    }
}

/// A fixture vault + writer + state db + config, and the defaults every [`StepCtx`] starts from.
pub(crate) struct StepEnv {
    pub fx: LiveFixture,
    pub state: Arc<Mutex<StateDb>>,
    pub config: ConfigSet,
    pub action: Action,
    pub params: Map<String, Value>,
    pub context: ActionContext,
    pub vars: Map<String, Value>,
}

impl StepEnv {
    /// Opens `<fixture>/state.db` beside the vault; `action` is the running action.
    pub fn new(fx: LiveFixture, config: ConfigSet, action: Action) -> Self {
        let state = StateDb::open(&fx.root().join("state.db"), 1, QueueConfig::default())
            .expect("state db");
        Self {
            fx,
            state: Arc::new(Mutex::new(state)),
            config,
            action,
            params: Map::new(),
            context: ActionContext::default(),
            vars: Map::new(),
        }
    }

    /// The vault root.
    pub fn vault(&self) -> &Path {
        &self.fx.vault
    }

    /// A step context over this env's defaults (step 0, no search, no children).
    pub fn ctx(&self) -> StepCtx<'_> {
        StepCtx {
            action: &self.action,
            step_index: 0,
            params: &self.params,
            context: &self.context,
            vars: &self.vars,
            config: &self.config,
            vault_root: &self.fx.vault,
            index_db: &self.fx.index_db,
            search: None,
            system_trash: None,
            state: &self.state,
            writer: Some(self.fx.writer.clone()),
            children: &NoChildren,
            written: Vec::new(),
            guard: None,
        }
    }

    /// Runs `step` with explicit params/context/vars.
    pub fn exec_with(
        &self,
        step: &Step,
        params: &Map<String, Value>,
        context: &ActionContext,
        vars: &Map<String, Value>,
    ) -> Result<StepOutcome, StepError> {
        let mut ctx = self.ctx();
        ctx.params = params;
        ctx.context = context;
        ctx.vars = vars;
        super::execute(step, &mut ctx)
    }

    /// The note as the writer sees it.
    pub fn read(&self, path: &str) -> Option<String> {
        self.fx
            .writer
            .read(path)
            .unwrap()
            .map(|b| String::from_utf8(b).unwrap())
    }
}

/// A `ConfigSet` loaded from `(file name, source)` pairs; asserts it loads cleanly.
pub(crate) fn config_of(files: &[(&str, &str)]) -> ConfigSet {
    let files: BTreeMap<String, Option<String>> = files
        .iter()
        .map(|(n, s)| ((*n).to_owned(), Some((*s).to_owned())))
        .collect();
    let mut c = ConfigSet::new();
    c.reload(&files, &super::validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

// ---- example-config harness (materialize/capture/file/lint tests) ----------------------------

const DAY_TEMPLATE: &str = "# {{date}}\n\n# Todos\n\n# Daylogs\n";
const WEEK_TEMPLATE: &str = "# Week {{title}}\n";
const MONTH_TEMPLATE: &str = "# Month {{title}}\n\n# Observations\n";

/// The shipped `config/examples/` set (unasserted: tests check `errors()` themselves).
pub(crate) fn example_config() -> ConfigSet {
    let files = BTreeMap::from([
        (
            "actions.toml".to_owned(),
            Some(include_str!("../../../../config/examples/actions.toml").to_owned()),
        ),
        (
            "captures.toml".to_owned(),
            Some(include_str!("../../../../config/examples/captures.toml").to_owned()),
        ),
        (
            "kinds.toml".to_owned(),
            Some(include_str!("../../../../config/examples/kinds.toml").to_owned()),
        ),
        // WP-12's files: the refine-transcript action's agent step validates against them.
        (
            "agents.toml".to_owned(),
            Some(include_str!("../../../../config/examples/agents.toml").to_owned()),
        ),
        (
            "providers.toml".to_owned(),
            Some(include_str!("../../../../config/examples/providers.toml").to_owned()),
        ),
    ]);
    let mut c = ConfigSet::new();
    c.reload(&files, &super::validate);
    c
}

/// A [`StepEnv`] over the example config with the period templates seeded plus `files`.
pub(crate) fn example_env(files: &[(&str, &str)]) -> StepEnv {
    let base: [(&str, &str); 3] = [
        ("_custom/day-template.md", DAY_TEMPLATE),
        ("_custom/week-template.md", WEEK_TEMPLATE),
        ("_custom/month-template.md", MONTH_TEMPLATE),
    ];
    let fx = LiveFixture::new(base.iter().chain(files).copied());
    let config = example_config();
    assert!(config.errors().is_empty(), "{:?}", config.errors());
    let action = config.action("lint-note").unwrap().clone();
    StepEnv::new(fx, config, action)
}

impl StepEnv {
    /// Validates then runs `step` (the example-config harness).
    pub fn exec(
        &self,
        step: &Step,
        params: &Value,
        context: &ActionContext,
        vars: &Map<String, Value>,
    ) -> Result<StepOutcome, StepError> {
        super::validate(step, &self.config).expect("step validates");
        let params = params.as_object().cloned().unwrap_or_default();
        self.exec_with(step, &params, context, vars)
    }

    /// Runs one step given as TOML.
    pub fn run(
        &self,
        toml_src: &str,
        params: Value,
        context: ActionContext,
    ) -> Result<StepOutcome, StepError> {
        self.run_vars(toml_src, params, context, &Map::new())
    }

    pub fn run_vars(
        &self,
        toml_src: &str,
        params: Value,
        context: ActionContext,
        vars: &Map<String, Value>,
    ) -> Result<StepOutcome, StepError> {
        let step = Step::from_table(toml_src.parse::<toml::Table>().unwrap()).unwrap();
        self.exec(&step, &params, &context, vars)
    }

    /// Runs every step of an example action like the runner does (vars merged, stop honoured).
    pub fn run_action(
        &self,
        id: &str,
        params: Value,
        context: ActionContext,
    ) -> Result<(Option<String>, Map<String, Value>), StepError> {
        let action = self.config.armed(id).expect("armed").clone();
        self.run_steps(&action.steps, params, context)
    }

    /// Runs `steps` like the runner does (vars merged, stop honoured, active path follows a move).
    pub fn run_steps(
        &self,
        steps: &[Step],
        params: Value,
        mut context: ActionContext,
    ) -> Result<(Option<String>, Map<String, Value>), StepError> {
        let mut vars = Map::new();
        let mut open = None;
        for step in steps {
            let o = self.exec(step, &params, &context, &vars)?;
            if o.open_path.is_some() {
                open = o.open_path.clone();
            }
            super::follow_move(&mut context, &o);
            vars.extend(o.vars);
            if o.stop {
                break;
            }
        }
        Ok((open, vars))
    }
}

pub(crate) fn active(p: &str) -> ActionContext {
    ActionContext {
        active_path: Some(p.to_owned()),
        ..ActionContext::default()
    }
}

pub(crate) fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub(crate) fn tomorrow() -> String {
    (chrono::Local::now().date_naive() + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

/// The path of a [`StepError::NoteGone`] (D11: a missing single target note settles quietly).
pub(crate) fn note_gone(r: Result<StepOutcome, StepError>) -> String {
    match r {
        Err(StepError::NoteGone { path }) => path,
        other => panic!("expected NoteGone, got {other:?}"),
    }
}

pub(crate) fn fails(r: Result<StepOutcome, StepError>) -> String {
    match r {
        Err(StepError::Failed { message, .. }) => message,
        other => panic!("expected failure, got {other:?}"),
    }
}
