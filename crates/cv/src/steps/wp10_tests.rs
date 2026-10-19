//! WP-10 step tests: synthetic notes in temp dirs, a temp `CV_PROTECTED_VAULT` (I1), the example
//! config from `config/examples/`, steps driven through `steps::validate`/`steps::execute`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cv_actions::{ConfigSet, Step};
use cv_protocol::ActionContext;
use cv_state::{QueueConfig, StateDb};
use cv_vault::{NoCutover, VaultFs, VaultWriter, WriterHandle};
use serde_json::{Map, Value, json};

use super::{ChildSpawner, StepCtx, StepError, StepOutcome};

static N: AtomicUsize = AtomicUsize::new(0);

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children in tests".into())
    }
}

const DAY_TEMPLATE: &str = "# {{date}}\n\n# Todos\n\n# Daylogs\n";
const WEEK_TEMPLATE: &str = "# Week {{title}}\n";
const MONTH_TEMPLATE: &str = "# Month {{title}}\n\n# Observations\n";

fn example_config() -> ConfigSet {
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

struct Env {
    root: PathBuf,
    vault: PathBuf,
    index: PathBuf,
    writer: WriterHandle,
    state: Arc<Mutex<StateDb>>,
    config: ConfigSet,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn mk_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "cv-wp10-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("vault")).unwrap();
    std::fs::create_dir_all(root.join("prot")).unwrap();
    std::fs::create_dir_all(root.join("cache")).unwrap();
    root
}

fn seed(vault: &Path, files: &[(&str, &str)]) {
    let base: [(&str, &str); 3] = [
        ("_custom/day-template.md", DAY_TEMPLATE),
        ("_custom/week-template.md", WEEK_TEMPLATE),
        ("_custom/month-template.md", MONTH_TEMPLATE),
    ];
    for (p, c) in base.iter().chain(files) {
        let full = vault.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, c).unwrap();
    }
}

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = mk_root();
        let vault = root.join("vault");
        seed(&vault, files);
        let fs = VaultFs::live(&vault, &root.join("prot"), &NoCutover).unwrap();
        Self::finish(root, vault, fs)
    }

    fn finish(root: PathBuf, vault: PathBuf, fs: VaultFs) -> Self {
        let index = root.join("cache/index.db");
        let w = VaultWriter::open(fs, &index).unwrap();
        let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
        let state = StateDb::open(&root.join("state.db"), 1, QueueConfig::default()).unwrap();
        let config = example_config();
        assert!(config.errors().is_empty(), "{:?}", config.errors());
        Self {
            root,
            vault,
            index,
            writer,
            state: Arc::new(Mutex::new(state)),
            config,
        }
    }

    /// The note as the writer sees it.
    fn read(&self, path: &str) -> Option<String> {
        self.writer
            .read(path)
            .unwrap()
            .map(|b| String::from_utf8(b).unwrap())
    }

    fn exec(
        &self,
        step: &Step,
        params: &Value,
        context: &ActionContext,
        vars: &Map<String, Value>,
    ) -> Result<StepOutcome, StepError> {
        super::validate(step, &self.config).expect("step validates");
        let params = params.as_object().cloned().unwrap_or_default();
        let action = self.config.action("lint-note").unwrap();
        let mut ctx = StepCtx {
            job_id: "job-wp10",
            action,
            step_index: 0,
            params: &params,
            context,
            vars,
            config: &self.config,
            vault_root: &self.vault,
            index_db: &self.index,
            search: None,
            state: &self.state,
            writer: Some(self.writer.clone()),
            children: &NoChildren,
            written: Vec::new(),
        };
        super::execute(step, &mut ctx)
    }

    /// Runs one step given as TOML.
    fn run(
        &self,
        toml_src: &str,
        params: Value,
        context: ActionContext,
    ) -> Result<StepOutcome, StepError> {
        self.run_vars(toml_src, params, context, &Map::new())
    }

    fn run_vars(
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
    fn run_action(
        &self,
        id: &str,
        params: Value,
        context: ActionContext,
    ) -> Result<(Option<String>, Map<String, Value>), StepError> {
        let action = self.config.armed(id).expect("armed").clone();
        self.run_steps(&action.steps, params, context)
    }

    /// Runs `steps` like the runner does (vars merged, stop honoured, active path follows a move).
    fn run_steps(
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

fn active(p: &str) -> ActionContext {
    ActionContext {
        active_path: Some(p.to_owned()),
        ..ActionContext::default()
    }
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn tomorrow() -> String {
    (chrono::Local::now().date_naive() + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string()
}

fn fails(r: Result<StepOutcome, StepError>) -> String {
    match r {
        Err(StepError::Failed { message, .. }) => message,
        other => panic!("expected failure, got {other:?}"),
    }
}

// ---- config ----------------------------------------------------------------------------------

#[test]
fn example_config_arms_every_action() {
    let c = example_config();
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    assert_eq!(c.armed_actions().count(), 31);
    for id in [
        "period.open",
        "note.create",
        "period.fill-created",
        "capture.daylog",
        "capture.forward-task",
        "capture.replace-note",
        "capture.monthly-observation",
        "file-as-kind",
        "refine-transcript",
        "search-refresh",
    ] {
        assert!(c.armed(id).is_some(), "{id} armed");
    }
    let kinds = c.table("kinds.toml")["kinds"].as_table().unwrap();
    assert_eq!(kinds.len(), 10);
    assert_eq!(
        c.table("captures.toml")["capture"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn validation_rejects_bad_params() {
    let c = example_config();
    let v = |src: &str| {
        let step = Step::from_table(src.parse::<toml::Table>().unwrap()).unwrap();
        super::validate(&step, &c)
    };
    assert!(v("type = \"materialize\"\nperiod = \"year\"").is_err());
    assert!(v("type = \"materialize\"\nperiod = \"day\"").is_ok());
    let no_periods = ConfigSet::new();
    let step =
        Step::from_table("type = \"materialize\"\nperiod = \"day\"".parse().unwrap()).unwrap();
    assert!(
        super::validate(&step, &no_periods).is_err(),
        "period without settings"
    );
    assert!(v("type = \"capture\"\ntarget = \"nowhere\"").is_err());
    assert!(v("type = \"capture\"\ncapture = \"nope\"").is_err());
    assert!(v("type = \"capture\"\nmode = \"section\"").is_err());
    assert!(v("type = \"upsert_tags\"").is_err());
    assert!(v("type = \"move\"\nkind = \"nope\"").is_err());
    assert!(v("type = \"guard\"\nbogus = 1").is_err());
    assert!(v("type = \"lint\"\nsteps = { nope = true }").is_err());
    // Bare steps the runner tests use stay valid.
    assert!(v("type = \"guard\"").is_ok());
    assert!(v("type = \"set_props\"").is_ok());
}

// ---- materialize -----------------------------------------------------------------------------

const DAY_STEP: &str = "type = \"materialize\"\nperiod = \"day\"\ndate = \"2026-10-13\"";

#[test]
fn materialize_creates_from_template_and_rerun_converges() {
    let env = Env::new(&[]);
    let o = env
        .run(DAY_STEP, json!({}), ActionContext::default())
        .unwrap();
    let p = "daily/day/2026-10-13.md";
    assert_eq!(o.open_path.as_deref(), Some(p));
    assert_eq!(o.output_paths, vec![p.to_owned()]);
    assert_eq!(o.vars["asset_folder"], json!("daily/day/2026-10-13"));
    assert_eq!(
        env.read(p).unwrap(),
        "# 2026-10-13\n\n# Todos\n\n# Daylogs\n"
    );
    // Re-run (E2): untouched, still opens it.
    let o2 = env
        .run(DAY_STEP, json!({}), ActionContext::default())
        .unwrap();
    assert_eq!(o2.open_path.as_deref(), Some(p));
    assert!(o2.output_paths.is_empty());
    assert_eq!(
        env.read(p).unwrap(),
        "# 2026-10-13\n\n# Todos\n\n# Daylogs\n"
    );
}

#[test]
fn materialize_fills_empty_and_leaves_non_empty() {
    let env = Env::new(&[
        ("daily/day/2026-10-13.md", ""),
        ("daily/day/2026-10-14.md", "mine\n"),
    ]);
    let o = env
        .run(DAY_STEP, json!({}), ActionContext::default())
        .unwrap();
    assert_eq!(o.output_paths.len(), 1);
    assert!(
        env.read("daily/day/2026-10-13.md")
            .unwrap()
            .starts_with("# 2026-10-13")
    );
    let o = env
        .run(
            "type = \"materialize\"\nperiod = \"day\"\ndate = \"2026-10-14\"",
            json!({}),
            ActionContext::default(),
        )
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(o.open_path.as_deref(), Some("daily/day/2026-10-14.md"));
    assert_eq!(env.read("daily/day/2026-10-14.md").unwrap(), "mine\n");
}

#[test]
fn materialize_only_if_empty_guard() {
    let env = Env::new(&[
        ("daily/day/2026-10-13.md", "written elsewhere\n"),
        ("daily/day/2026-10-15.md", ""),
        ("daily/day/2026-10-15/clip.md", ""),
    ]);
    let step = "type = \"materialize\"\ntarget = \"active\"\nonly_if_empty = true";
    let o = env
        .run(step, json!({}), active("daily/day/2026-10-13.md"))
        .unwrap();
    assert!(o.stop, "non-empty note stops the action");
    assert_eq!(
        env.read("daily/day/2026-10-13.md").unwrap(),
        "written elsewhere\n"
    );
    let o = env
        .run(step, json!({}), active("daily/day/2026-10-15.md"))
        .unwrap();
    assert!(!o.stop);
    assert_eq!(
        env.read("daily/day/2026-10-15.md").unwrap(),
        "# 2026-10-15\n\n# Todos\n\n# Daylogs\n"
    );
    // Re-run on its own output: stops, unchanged.
    assert!(
        env.run(step, json!({}), active("daily/day/2026-10-15.md"))
            .unwrap()
            .stop
    );
    // A note in an asset subfolder is not a period note.
    let o = env
        .run(step, json!({}), active("daily/day/2026-10-15/clip.md"))
        .unwrap();
    assert!(o.stop);
    assert_eq!(env.read("daily/day/2026-10-15/clip.md").unwrap(), "");
}

#[test]
fn period_open_end_to_end_returns_open_path() {
    let env = Env::new(&[]);
    let (open, _) = env
        .run_action(
            "period.open",
            json!({"period": "week", "date": "2026-10-13"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("daily/week/2026-W42.md"));
    assert_eq!(
        env.read("daily/week/2026-W42.md").unwrap(),
        "# Week 2026-W42\n"
    );
    // Default date = today.
    let (open, _) = env
        .run_action(
            "period.open",
            json!({"period": "day", "date": ""}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open, Some(format!("daily/day/{}.md", today())));
    let (open, _) = env
        .run_action(
            "period.open",
            json!({"period": "month", "date": "2026-10-13"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("daily/month/2026-10.md"));
    // Resource keys name the period note, not the active path.
    let a = env.config.armed("period.open").unwrap();
    let keys = a
        .resources
        .derive(None, json!({"period": "day"}).as_object().unwrap())
        .unwrap();
    assert_eq!(keys, vec!["note::period::day".to_owned()]);
}

#[test]
fn note_create_with_and_without_template() {
    let env = Env::new(&[("_custom/plain.md", "title: {{title}}\n")]);
    let (open, _) = env
        .run_action(
            "note.create",
            json!({"path": "notes/fresh"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("notes/fresh.md"));
    assert_eq!(env.read("notes/fresh.md").unwrap(), "");
    let (open, _) = env
        .run_action(
            "note.create",
            json!({"path": "notes/tpl.md", "template": "_custom/plain.md"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("notes/tpl.md"));
    assert_eq!(env.read("notes/tpl.md").unwrap(), "title: tpl\n");
}

// ---- capture ---------------------------------------------------------------------------------

#[test]
fn capture_daylog_materializes_daily_and_rerun_does_not_duplicate() {
    let env = Env::new(&[]);
    let daily = format!("daily/day/{}.md", today());
    let step = "type = \"capture\"\ncapture = \"daylog\"\nformat = \"- {{value}}\"";
    let o = env
        .run(step, json!({"value": "hello"}), ActionContext::default())
        .unwrap();
    assert_eq!(o.output_paths, vec![daily.clone()]);
    let want = format!("# {}\n\n# Todos\n\n# Daylogs\n- hello\n", today());
    assert_eq!(env.read(&daily).unwrap(), want);
    let o = env
        .run(step, json!({"value": "hello"}), ActionContext::default())
        .unwrap();
    assert!(o.output_paths.is_empty(), "E2 re-run converges");
    assert_eq!(env.read(&daily).unwrap(), want);
    env.run(step, json!({"value": "second"}), ActionContext::default())
        .unwrap();
    assert!(env.read(&daily).unwrap().ends_with("- hello\n- second\n"));
    // The preset's own `{{time}}` format renders too.
    env.run(
        "type = \"capture\"\ncapture = \"daylog\"",
        json!({"value": "t"}),
        ActionContext::default(),
    )
    .unwrap();
    let text = env.read(&daily).unwrap();
    let last = text.lines().last().unwrap();
    assert!(last.starts_with("- [") && last.ends_with("] t"), "{last}");
}

#[test]
fn capture_whole_note_modes_and_targets() {
    let env = Env::new(&[
        ("a.md", "---\ntitle: a\n---\n\nbody\n"),
        ("b.md", "first\n"),
    ]);
    // prepend into the active note keeps frontmatter on top.
    let pre = "type = \"capture\"\ntarget = \"active\"\nmode = \"prepend\"";
    env.run(pre, json!({"value": "top"}), active("a.md"))
        .unwrap();
    assert_eq!(
        env.read("a.md").unwrap(),
        "---\ntitle: a\n---\n\ntop\n\nbody\n"
    );
    let o = env
        .run(pre, json!({"value": "top"}), active("a.md"))
        .unwrap();
    assert!(o.output_paths.is_empty(), "prepend re-run converges");
    // append into a path target (value from the context).
    let app = "type = \"capture\"\ntarget = \"path\"\npath = \"{param.p}\"\nmode = \"append\"";
    let ctx = ActionContext {
        value: Some("tail".into()),
        ..ActionContext::default()
    };
    env.run(app, json!({"p": "b.md"}), ctx.clone()).unwrap();
    assert_eq!(env.read("b.md").unwrap(), "first\n\ntail");
    assert!(
        env.run(app, json!({"p": "b.md"}), ctx)
            .unwrap()
            .output_paths
            .is_empty()
    );
    // section mode into the active note.
    let sec = "type = \"capture\"\ntarget = \"active\"\nmode = \"section\"\nsection = \"## Log\"\nformat = \"- {{value}}\"";
    env.run(sec, json!({"value": "x"}), active("b.md")).unwrap();
    assert_eq!(env.read("b.md").unwrap(), "first\n\ntail\n\n## Log\n\n- x");
    env.run(sec, json!({"value": "y"}), active("b.md")).unwrap();
    assert_eq!(
        env.read("b.md").unwrap(),
        "first\n\ntail\n\n## Log\n\n- x\n- y"
    );
    assert!(
        env.run(sec, json!({"value": "y"}), active("b.md"))
            .unwrap()
            .output_paths
            .is_empty()
    );
    // A missing active/path target fails closed.
    assert!(
        fails(env.run(app, json!({"p": "nope.md", "value": "v"}), active("b.md")))
            .contains("not found")
    );
}

#[test]
fn capture_monthly_observation_uses_selection_and_source_link() {
    let env = Env::new(&[("src/Idea.md", "text\n")]);
    let ctx = ActionContext {
        active_path: Some("src/Idea.md".into()),
        selection: Some("a  multi\nline".into()),
        ..ActionContext::default()
    };
    let (_, vars) = env
        .run_action("capture.monthly-observation", json!({}), ctx)
        .unwrap();
    let month = chrono::Local::now().format("%Y-%m").to_string();
    let path = format!("daily/month/{month}.md");
    assert_eq!(vars["captured_path"], json!(path));
    assert_eq!(
        env.read(&path).unwrap(),
        format!("# Month {month}\n\n# Observations\n- [[Idea]]\n\t- a multi line\n")
    );
}

#[test]
fn capture_interpolates_vars_for_agent_output() {
    let env = Env::new(&[("_raw_transcript/t.md", "raw\n")]);
    let mut vars = Map::new();
    vars.insert("agent_output".into(), json!("refined text"));
    vars.insert("copied_path".into(), json!("_raw_transcript/t.md"));
    let step = "type = \"capture\"\ntarget = \"path\"\npath = \"{vars.copied_path}\"\nmode = \"append\"\nformat = \"{{vars.agent_output}}\"";
    let o = env
        .run_vars(step, json!({}), ActionContext::default(), &vars)
        .unwrap();
    assert_eq!(o.output_paths, vec!["_raw_transcript/t.md".to_owned()]);
    assert_eq!(
        env.read("_raw_transcript/t.md").unwrap(),
        "raw\n\nrefined text"
    );
    // Non-string vars render as JSON text.
    vars.insert("agent_output".into(), json!({"k": 1}));
    env.run_vars(step, json!({}), ActionContext::default(), &vars)
        .unwrap();
    assert!(
        env.read("_raw_transcript/t.md")
            .unwrap()
            .ends_with("{\"k\":1}")
    );
}

// Vectors from `obsidian-crucible/tests/captureEmptyBodyGuard.test.mjs`.
#[test]
fn capture_empty_body_guard_vectors() {
    let existing = "---\ntitle: note\n---\n\nOriginal body, must survive untouched.";
    let env = Env::new(&[("note.md", existing)]);
    let replace = "type = \"capture\"\ntarget = \"active\"\nmode = \"replace\"";
    // 1. YAML-only value + replace: success, no write, byte-identical.
    let o = env
        .run(
            replace,
            json!({"value": "---\ncaptured: true\n---\n"}),
            active("note.md"),
        )
        .unwrap();
    assert!(o.output_paths.is_empty() && !o.stop);
    assert_eq!(env.read("note.md").unwrap(), existing);
    // 2. A normal replace still writes, keeping the frontmatter.
    let o = env
        .run(
            replace,
            json!({"value": "Fresh replacement body."}),
            active("note.md"),
        )
        .unwrap();
    assert_eq!(o.output_paths.len(), 1);
    assert_eq!(
        env.read("note.md").unwrap(),
        "---\ntitle: note\n---\n\nFresh replacement body."
    );
    // 3. YAML-only + append is not gated by the replace guard: it succeeds (and, a deviation,
    //    writes nothing because the stripped body is empty).
    let append = "type = \"capture\"\ntarget = \"active\"\nmode = \"append\"";
    let o = env
        .run(
            append,
            json!({"value": "---\ncaptured: true\n---\n"}),
            active("note.md"),
        )
        .unwrap();
    assert!(!o.stop);
    // An empty value is a no-op success (Crucible's first guard).
    let o = env
        .run(append, json!({"value": "  "}), active("note.md"))
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(
        env.read("note.md").unwrap(),
        "---\ntitle: note\n---\n\nFresh replacement body."
    );
}

// ---- forward task / mark_forwarded -----------------------------------------------------------

#[test]
fn forward_task_captures_into_next_daily_and_marks_forwarded() {
    let src = "# Project\n## Doing\n- [ ] carry me\nplain\n";
    let env = Env::new(&[("proj.md", src)]);
    let ctx = ActionContext {
        active_path: Some("proj.md".into()),
        line: Some(2),
        ..ActionContext::default()
    };
    env.run_action("capture.forward-task", json!({}), ctx.clone())
        .unwrap();
    let next = format!("daily/day/{}.md", tomorrow());
    // `## Doing` is not in the target, so the fixed `# Todos` section is used.
    assert_eq!(
        env.read(&next).unwrap(),
        format!("# {}\n\n# Todos\n- [ ] carry me\n\n# Daylogs\n", tomorrow())
    );
    assert_eq!(
        env.read("proj.md").unwrap(),
        "# Project\n## Doing\n- [>] carry me\nplain\n"
    );
    // E2: both steps re-run on their own output converge.
    let cap = Step::from_table(
        "type = \"capture\"\ncapture = \"forward-task\""
            .parse()
            .unwrap(),
    )
    .unwrap();
    let o = env.exec(&cap, &json!({}), &ctx, &Map::new()).unwrap();
    assert!(o.output_paths.is_empty());
    let o = env
        .run("type = \"mark_forwarded\"", json!({}), ctx.clone())
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(
        env.read("proj.md").unwrap(),
        "# Project\n## Doing\n- [>] carry me\nplain\n"
    );
    assert_eq!(env.read(&next).unwrap().matches("carry me").count(), 1);
    // A non-task line fails closed in both steps.
    let bad = ActionContext {
        line: Some(3),
        ..ctx
    };
    assert!(
        fails(env.run("type = \"mark_forwarded\"", json!({}), bad.clone()))
            .contains("not an open task")
    );
    assert!(fails(env.exec(&cap, &json!({}), &bad, &Map::new())).contains("not a task"));
}

#[test]
fn mark_forwarded_open_task_variants() {
    let env = Env::new(&[("t.md", "- [>] a\r\n  * [ ] b\r\n")]);
    let ctx = |l| ActionContext {
        active_path: Some("t.md".into()),
        line: Some(l),
        ..ActionContext::default()
    };
    env.run("type = \"mark_forwarded\"", json!({}), ctx(1))
        .unwrap();
    assert_eq!(env.read("t.md").unwrap(), "- [>] a\r\n  * [>] b\r\n");
    assert!(
        env.run("type = \"mark_forwarded\"", json!({}), ctx(0))
            .unwrap()
            .output_paths
            .is_empty()
    );
    assert!(
        fails(env.run("type = \"mark_forwarded\"", json!({}), ctx(9))).contains("out of range")
    );
}

// ---- file steps ------------------------------------------------------------------------------

#[test]
fn guard_stops_without_error() {
    let env = Env::new(&[
        ("done.md", "---\ntags: [refined]\n---\nx\n"),
        ("todo.md", "---\ntags: transcript\n---\nx\n"),
    ]);
    let g = "type = \"guard\"\nnot_tag = \"refined\"";
    assert!(env.run(g, json!({}), active("done.md")).unwrap().stop);
    assert!(!env.run(g, json!({}), active("todo.md")).unwrap().stop);
    assert!(env.run(g, json!({}), active("missing.md")).unwrap().stop);
    assert!(
        !env.run(
            "type = \"guard\"\ntag = \"#transcript\"\nfolder = \"\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    assert!(
        env.run(
            "type = \"guard\"\nfolder = \"inbox\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    assert!(
        env.run(
            "type = \"guard\"\nprop = \"source\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    assert!(
        !env.run(
            "type = \"guard\"\nnot_prop = \"source\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    // A refine run on an already-refined note stops at the guard and writes nothing.
    let before = env.read("done.md");
    let (_, vars) = env
        .run_action(
            "refine-transcript",
            json!({"provider": "local"}),
            active("done.md"),
        )
        .unwrap();
    assert!(vars.is_empty());
    assert_eq!(env.read("done.md"), before);
    assert!(env.read("_raw_transcript/done.md").is_none());
}

#[test]
fn move_rewrites_links_and_rerun_converges() {
    let env = Env::new(&[
        ("inbox/x.md", "# X\n"),
        ("ref.md", "see [[inbox/x|the x]]\n"),
    ]);
    let step = "type = \"move\"\nto = \"archive\"";
    let o = env.run(step, json!({}), active("inbox/x.md")).unwrap();
    assert_eq!(o.vars["moved_path"], json!("archive/x.md"));
    assert_eq!(o.open_path.as_deref(), Some("archive/x.md"));
    assert!(o.output_paths.contains(&"ref.md".to_owned()));
    assert_eq!(env.read("ref.md").unwrap(), "see [[archive/x|the x]]\n");
    assert!(env.read("inbox/x.md").is_none());
    // Re-run: source gone, destination present → success.
    let o = env.run(step, json!({}), active("inbox/x.md")).unwrap();
    assert_eq!(o.vars["moved_path"], json!("archive/x.md"));
    assert!(o.output_paths.is_empty());
}

#[test]
fn file_as_kind_tags_moves_and_lints() {
    let env = Env::new(&[
        ("inbox/clip.md", "---\ntags: [clip]\n---\nbody\n"),
        ("ref.md", "[[inbox/clip]]\n"),
    ]);
    let (open, vars) = env
        .run_action(
            "file-as-kind",
            json!({"kind": "repo"}),
            active("inbox/clip.md"),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("stack/clip.md"));
    assert_eq!(vars["moved_path"], json!("stack/clip.md"));
    let text = env.read("stack/clip.md").unwrap();
    assert!(text.contains("- repo"), "{text}");
    assert!(text.contains(&format!("created: {}", today())), "{text}");
    assert_eq!(env.read("ref.md").unwrap(), "[[stack/clip]]\n");
    // A daily-folder kind lands in today's daily asset folder.
    let env = Env::new(&[("inbox/n.md", "news\n")]);
    let (open, _) = env
        .run_action(
            "file-as-kind",
            json!({"kind": "news"}),
            active("inbox/n.md"),
        )
        .unwrap();
    assert_eq!(open, Some(format!("daily/day/{}/n.md", today())));
}

/// plan round-5 WP-3: the example `file-as-kind` moves first, so a destination collision fails
/// with "already exists" and leaves the note (and its tags) untouched.
#[test]
fn file_as_kind_collision_fails_before_tagging() {
    let src = "---\ntags: [clip]\n---\nbody\n";
    let env = Env::new(&[("inbox/clip.md", src), ("stack/clip.md", "other\n")]);
    let err = env
        .run_action(
            "file-as-kind",
            json!({"kind": "repo"}),
            active("inbox/clip.md"),
        )
        .expect_err("collision fails");
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(env.read("inbox/clip.md").as_deref(), Some(src));
    assert_eq!(env.read("stack/clip.md").as_deref(), Some("other\n"));
}

/// WP-8 (editor-feedback-round-2): the live `file-as-kind` orders `move` first; the following
/// `upsert_tags` (no `path`) must target the moved note, not the dead pre-move path.
#[test]
fn file_as_kind_move_first_tags_the_moved_note() {
    let env = Env::new(&[("inbox/clip.md", "---\ntags: [clip]\n---\nbody\n")]);
    let steps: Vec<Step> = [
        "type = \"move\"\nkind = \"{param.kind}\"",
        "type = \"upsert_tags\"\nkind = \"{param.kind}\"",
        "type = \"lint\"\npath = \"{vars.moved_path}\"\nkind = \"{param.kind}\"",
    ]
    .iter()
    .map(|s| Step::from_table(s.parse::<toml::Table>().unwrap()).unwrap())
    .collect();
    let (open, vars) = env
        .run_steps(&steps, json!({"kind": "repo"}), active("inbox/clip.md"))
        .unwrap();
    assert_eq!(open.as_deref(), Some("stack/clip.md"));
    assert_eq!(vars["moved_path"], json!("stack/clip.md"));
    assert!(env.read("inbox/clip.md").is_none());
    let text = env.read("stack/clip.md").unwrap();
    assert!(text.contains("- repo"), "{text}");
    assert!(
        text.contains(&format!("created: {}", today())),
        "lint ran: {text}"
    );
}

#[test]
fn follow_move_only_follows_the_active_note() {
    let mut ctx = active("a.md");
    let mut o = StepOutcome::default();
    o.vars.insert("moved_path".into(), json!("b/a.md"));
    o.vars.insert(super::MOVED_FROM.into(), json!("other.md"));
    super::follow_move(&mut ctx, &o);
    assert_eq!(ctx.active_path.as_deref(), Some("a.md"));
    o.vars.insert(super::MOVED_FROM.into(), json!("a.md"));
    super::follow_move(&mut ctx, &o);
    assert_eq!(ctx.active_path.as_deref(), Some("b/a.md"));
}

#[test]
fn copy_set_props_upsert_tags_are_idempotent() {
    let env = Env::new(&[("t.md", "---\ntags:\n  - transcript\n---\nraw words\n")]);
    let copy = "type = \"copy\"\nto = \"_raw_transcript\"";
    let o = env.run(copy, json!({}), active("t.md")).unwrap();
    assert_eq!(o.vars["copied_path"], json!("_raw_transcript/t.md"));
    assert_eq!(o.vars["copied_link"], json!("[[_raw_transcript/t]]"));
    assert_eq!(env.read("_raw_transcript/t.md"), env.read("t.md"));
    assert!(
        env.run(copy, json!({}), active("t.md")).is_ok(),
        "identical copy converges"
    );

    let mut vars = Map::new();
    vars.insert("copied_link".into(), json!("[[_raw_transcript/t]]"));
    let sp = "type = \"set_props\"\nprops = { raw-transcript = \"{vars.copied_link}\", n = 3 }";
    let o = env.run_vars(sp, json!({}), active("t.md"), &vars).unwrap();
    assert_eq!(o.output_paths.len(), 1);
    let after = env.read("t.md").unwrap();
    assert!(
        after.contains("raw-transcript: \"[[_raw_transcript/t]]\""),
        "{after}"
    );
    let o = env.run_vars(sp, json!({}), active("t.md"), &vars).unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(env.read("t.md").unwrap(), after);

    let up = "type = \"upsert_tags\"\ntags = [\"#refined\", \"{param.extra}\"]";
    env.run(up, json!({"extra": "transcript"}), active("t.md"))
        .unwrap();
    let tagged = env.read("t.md").unwrap();
    assert!(tagged.contains("  - transcript\n  - refined\n"), "{tagged}");
    assert!(
        env.run(up, json!({"extra": "transcript"}), active("t.md"))
            .unwrap()
            .output_paths
            .is_empty()
    );

    let rp = "type = \"remove_props\"\nprops = [\"n\"]";
    assert_eq!(
        env.run(rp, json!({}), active("t.md"))
            .unwrap()
            .output_paths
            .len(),
        1
    );
    assert!(
        env.run(rp, json!({}), active("t.md"))
            .unwrap()
            .output_paths
            .is_empty()
    );
}

#[test]
fn lint_writes_only_on_change() {
    let env = Env::new(&[("n.md", "---\ntitle: n\n---\nsome words here\n")]);
    let o = env
        .run("type = \"lint\"", json!({}), active("n.md"))
        .unwrap();
    assert_eq!(o.output_paths, vec!["n.md".to_owned()]);
    let once = env.read("n.md").unwrap();
    assert!(once.contains(&format!("updated: {}", today())), "{once}");
    let o = env
        .run("type = \"lint\"", json!({}), active("n.md"))
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(env.read("n.md").unwrap(), once);
}
