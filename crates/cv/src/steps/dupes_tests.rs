//! `dupes_scan` tests (plan `round-12-feedback-todos` WP-6): synthetic temp vaults through the
//! real writer thread, plus one `#[ignore]` scratch-copy driver for the resource measurement.

use cv_actions::Step;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value};

use super::StepOutcome;
use super::dupes::{POSSIBLE_KEY, post_id_options, score};
use super::testkit::{StepEnv, config_of};

const ACTIONS: &str = "[[action]]\nid = \"dupes-scan\"\n[[action.steps]]\ntype = \"dupes_scan\"\n";

struct Env(StepEnv);

impl Env {
    fn new(files: &[(&str, String)]) -> Self {
        let config = config_of(&[("actions.toml", ACTIONS)]);
        let action = config.armed("dupes-scan").unwrap().clone();
        Self(StepEnv::new(
            LiveFixture::new(files.iter().map(|(p, t)| (*p, t.as_str()))),
            config,
            action,
        ))
    }

    fn run_with(&self, dry: bool) -> StepOutcome {
        self.run_job(dry, None)
    }

    fn run_job(&self, dry: bool, job: Option<Value>) -> StepOutcome {
        let mut ctx = self.0.ctx();
        let mut params = ctx.params.clone();
        if let Some(j) = job {
            params.insert("dry_run".into(), j);
        }
        ctx.params = &params;
        let mut t = toml::Table::new();
        t.insert("type".into(), "dupes_scan".into());
        t.insert("dry_run".into(), dry.into());
        let step = Step::from_table(t).unwrap();
        crate::steps::validate(&step, &self.0.config).unwrap();
        crate::steps::execute(&step, &mut ctx).unwrap()
    }

    fn run(&self) -> StepOutcome {
        self.run_with(false)
    }

    fn labels(&self, rel: &str) -> Vec<String> {
        cv_note::FrontmatterDoc::new(self.0.read(rel).unwrap()).get_strings(POSSIBLE_KEY)
    }
}

fn var(o: &StepOutcome, k: &str) -> u64 {
    o.vars
        .get(k)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{k}"))
}

/// `n` distinct words keyed by `seed` (no shingle overlap across seeds).
fn words(seed: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("{seed}w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn note(fm: &str, body: &str) -> String {
    if fm.is_empty() {
        format!("{body}\n")
    } else {
        format!("---\n{fm}---\n{body}\n")
    }
}

#[test]
fn exclusions_drop_daily_pairs_excluded_folders_and_transcript_pairing() {
    let body = words("x", 60);
    let src = "source: \"https://blog.example/p/one\"\n";
    let env = Env::new(&[
        ("daily/day/a.md", note("", &body)),
        ("daily/day/b.md", note("", &body)),
        ("initiatives/c.md", note("", &body)),
        ("_yt_metadata/d.md", note("", &body)),
        (".trash/e.md", note("", &body)),
        ("_raw_transcript/t.md", note(src, &words("t", 60))),
        ("daily/day/t.md", note(src, &words("u", 60))),
    ]);
    let o = env.run();
    assert_eq!(var(&o, "pairs"), 0, "{:?}", o.vars);
    assert_eq!(var(&o, "written"), 0);
    assert_eq!(var(&o, "notes"), 4, "excluded folders are not scanned");
}

#[test]
fn identity_pair_recomputes_from_source_over_stale_post_id() {
    let env = Env::new(&[
        // Same source, one with a stale stored id, unrelated bodies: identity pair.
        (
            "_blog_metadata/m.md",
            note(
                "source: \"https://blog.example/p/one?utm_source=x\"\npost-id: \"stale-id\"\n",
                &words("a", 40),
            ),
        ),
        (
            "Clippings/c.md",
            note("source: \"https://blog.example/p/one\"\n", &words("b", 40)),
        ),
        // Same stored id but different sources: not a pair.
        (
            "notes/p.md",
            note(
                "source: \"https://o.example/1\"\npost-id: \"same\"\n",
                &words("c", 40),
            ),
        ),
        (
            "notes/q.md",
            note(
                "source: \"https://o.example/2\"\npost-id: \"same\"\n",
                &words("d", 40),
            ),
        ),
    ]);
    let o = env.run();
    assert_eq!(var(&o, "pairs"), 1, "{:?}", o.vars);
    assert_eq!(var(&o, "pairs_identity"), 1);
    assert_eq!(env.labels("_blog_metadata/m.md"), ["[[Clippings/c]]"]);
    assert_eq!(env.labels("Clippings/c.md"), ["[[_blog_metadata/m]]"]);
    assert!(env.labels("notes/p.md").is_empty());
}

#[test]
fn containment_pair_is_directional_and_idempotent() {
    let small = words("s", 50);
    let large = format!("{small} {}", words("l", 150));
    let env = Env::new(&[
        ("_blog_metadata/s.md", note("", &small)),
        ("daily/day/l.md", note("", &large)),
        ("other/z.md", note("", &words("z", 80))),
    ]);
    let o = env.run();
    assert_eq!(var(&o, "pairs_containment"), 1, "{:?}", o.vars);
    assert_eq!(var(&o, "written"), 2);
    assert_eq!(env.labels("daily/day/l.md"), ["[[_blog_metadata/s]]"]);
    assert_eq!(o.vars["by_folder"]["_blog_metadata × daily"], 1);
    let again = env.run();
    assert_eq!(var(&again, "written"), 0);
    assert_eq!(var(&again, "already_labelled"), 1);
    assert_eq!(env.labels("_blog_metadata/s.md"), ["[[daily/day/l]]"]);

    let opts = post_id_options(&env.0.config);
    let s = score(&small, &large, &opts);
    assert!(!s.identity_match);
    assert!((s.containment_a_in_b - 1.0).abs() < 1e-9);
    assert!(s.containment_b_in_a < 0.3 && s.keep());
}

#[test]
fn not_duplicate_of_suppresses_the_pair() {
    let body = words("n", 60);
    let env = Env::new(&[
        (
            "a/x.md",
            note("not-duplicate-of:\n  - \"[[b/y]]\"\n", &body),
        ),
        ("b/y.md", note("", &body)),
    ]);
    let o = env.run();
    assert_eq!(var(&o, "suppressed"), 1, "{:?}", o.vars);
    assert_eq!(var(&o, "pairs"), 0);
    assert!(env.labels("b/y.md").is_empty());
}

#[test]
fn dry_run_counts_but_writes_nothing() {
    let body = words("d", 60);
    let files = [("a/x.md", note("", &body)), ("b/y.md", note("", &body))];
    let env = Env::new(&files);
    let o = env.run_with(true);
    assert_eq!(var(&o, "pairs"), 1);
    assert_eq!(var(&o, "written"), 2);
    assert_eq!(o.vars["dry_run"], Value::Bool(true));
    for (p, t) in &files {
        assert_eq!(&env.0.read(p).unwrap(), t);
    }
}

#[test]
fn job_dry_run_wins_over_the_step_param_in_both_directions() {
    let body = words("j", 60);
    let files = [("a/x.md", note("", &body)), ("b/y.md", note("", &body))];
    let env = Env::new(&files);
    // Step dry_run = true, job "false" → a real run.
    let o = env.run_job(true, Some(Value::String("false".into())));
    assert_eq!(o.vars["dry_run"], Value::Bool(false));
    assert!(!env.labels("a/x.md").is_empty());
    // Step dry_run = false, job true → dry.
    let env = Env::new(&files);
    let o = env.run_job(false, Some(Value::Bool(true)));
    assert_eq!(o.vars["dry_run"], Value::Bool(true));
    assert!(env.labels("a/x.md").is_empty());
}

/// Scratch-copy measurement driver (never the live vault): `CV_DUPES_SCRATCH=/tmp/r12-wp6`
/// holding `vault/` and `config/`; prints the outcome vars plus the scan's RSS (VmHWM reset via
/// `/proc/self/clear_refs` after the index build, so it measures the scan alone).
#[test]
#[ignore = "manual measurement on a scratch copy"]
fn scratch_dry_run() {
    let root = std::path::PathBuf::from(std::env::var("CV_DUPES_SCRATCH").unwrap());
    assert!(root.starts_with("/tmp/r12-wp6"), "scratch only");
    let vault = root.join("vault");
    let index_db = root.join("index").join("index.db");
    std::fs::create_dir_all(root.join("index")).unwrap();
    let mut files = std::collections::BTreeMap::new();
    for e in std::fs::read_dir(root.join("config")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "toml") {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            files.insert(name, std::fs::read_to_string(&p).ok());
        }
    }
    let mut config = cv_actions::ConfigSet::new();
    config.reload(&files, &crate::steps::validate);
    let fs = cv_vault::VaultFs::live(&vault).unwrap();
    let t0 = std::time::Instant::now();
    let w = cv_vault::VaultWriter::open(fs, &index_db).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    eprintln!("index build {:?}", t0.elapsed());
    let status = |k: &str| {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|l| l.starts_with(k))
            .unwrap_or_default()
            .to_owned()
    };
    std::fs::write("/proc/self/clear_refs", "5").unwrap();
    eprintln!("before: {} | {}", status("VmRSS"), status("VmHWM"));
    let state = std::sync::Arc::new(std::sync::Mutex::new(
        cv_state::StateDb::open(
            &root.join("scratch-state.db"),
            1,
            cv_state::QueueConfig::default(),
        )
        .unwrap(),
    ));
    let action = config_of(&[("actions.toml", ACTIONS)])
        .armed("dupes-scan")
        .unwrap()
        .clone();
    let (params, vars) = (Map::new(), Map::new());
    let context = cv_protocol::ActionContext::default();
    let mut ctx = super::StepCtx {
        action: &action,
        step_index: 0,
        params: &params,
        context: &context,
        vars: &vars,
        config: &config,
        vault_root: &vault,
        index_db: &index_db,
        search: None,
        system_trash: None,
        state: &state,
        writer: Some(writer),
        children: &super::testkit::NoChildren,
        written: Vec::new(),
        guard: None,
    };
    let t1 = std::time::Instant::now();
    let o = <super::dupes::DupesScan as super::StepModule>::run(true, &mut ctx).unwrap();
    eprintln!("scan wall {:?}", t1.elapsed());
    eprintln!("after: {} | {}", status("VmRSS"), status("VmHWM"));
    eprintln!("{}", serde_json::to_string_pretty(&o.vars).unwrap());
}

#[test]
fn custom_and_crucible_folders_are_not_scanned() {
    // User decision 2026-10-23: vault machinery under `_custom/` and `_crucible/` is excluded.
    let src = "source: \"https://blog.example/p/one\"\n";
    let env = Env::new(&[
        ("_custom/templates/t.md", note(src, &words("a", 60))),
        ("daily/day/d.md", note(src, &words("b", 60))),
        ("_crucible/x.md", note("", &words("c", 60))),
        ("_crucible/y.md", note("", &words("c", 60))),
    ]);
    let o = env.run();
    assert_eq!(var(&o, "pairs"), 0, "{:?}", o.vars);
    assert_eq!(
        var(&o, "notes"),
        1,
        "_custom/ and _crucible/ are not scanned"
    );
    assert!(env.labels("_custom/templates/t.md").is_empty());
    assert!(env.labels("daily/day/d.md").is_empty());
}
