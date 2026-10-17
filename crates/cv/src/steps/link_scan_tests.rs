//! WP-3 `link_scan` tests: a synthetic temp vault and a temp `state.db` (never the live vault).

use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_state::{QueueConfig, StateDb};
use serde_json::{Map, Value, json};

use super::{execute, is_excluded, normalize_exclusions, validate};
use crate::steps::{ChildSpawner, StepCtx, StepOutcome};

struct NoChildren;
impl ChildSpawner for NoChildren {
    fn spawn_child(&self, _: &str, _: Map<String, Value>) -> Result<String, String> {
        Err("no children".into())
    }
}

struct Tmp(std::path::PathBuf);
impl Tmp {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cv-wp3-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Env {
    _tmp: Tmp,
    vault: std::path::PathBuf,
    index: std::path::PathBuf,
    writer: cv_vault::WriterHandle,
    state: Arc<Mutex<StateDb>>,
}

fn env(files: &[(&str, &str)]) -> Env {
    let tmp = Tmp::new();
    let vault = tmp.path().join("vault");
    let protected = tmp.path().join("protected");
    std::fs::create_dir_all(&protected).unwrap();
    for (rel, text) in files {
        let p = vault.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let fs = cv_vault::VaultFs::live(&vault, &protected, &cv_vault::NoCutover).unwrap();
    let index = tmp.path().join("index.db");
    let w = cv_vault::VaultWriter::open(fs, &index).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = StateDb::open(&tmp.path().join("state.db"), 1, QueueConfig::default()).unwrap();
    Env {
        vault: std::fs::canonicalize(&vault).unwrap(),
        index,
        _tmp: tmp,
        writer,
        state: Arc::new(Mutex::new(state)),
    }
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"link_scan\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn run(e: &Env, src: &str, active: Option<&str>) -> Result<StepOutcome, String> {
    let action = Action {
        id: "link-scan".into(),
        label: "Link scan".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec {
            note: false,
            keys: vec![],
        },
    };
    let context = ActionContext {
        active_path: active.map(str::to_owned),
        ..ActionContext::default()
    };
    let (params, vars) = (Map::new(), Map::new());
    let config = ConfigSet::new();
    let mut ctx = StepCtx {
        job_id: "job-1",
        action: &action,
        step_index: 0,
        params: &params,
        context: &context,
        vars: &vars,
        config: &config,
        vault_root: &e.vault,
        index_db: &e.index,
        state: &e.state,
        writer: Some(e.writer.clone()),
        children: &NoChildren,
        written: Vec::new(),
    };
    execute(&step(src), &mut ctx).map_err(|e| format!("{e:?}"))
}

fn n(o: &StepOutcome, k: &str) -> u64 {
    o.vars[k].as_u64().unwrap()
}

const A: &str = "See https://example.com/a?utm_source=x and [x](https://www.youtube.com/watch?v=dQw4w9WgXcQ).\n`https://code.example/skip`\n";
const B: &str = "Again https://example.com/a and https://x.com/someone/status/1234567890 and https://www.youtube.com/@chan\n";

#[test]
fn params_validate_and_normalize() {
    let c = ConfigSet::new();
    assert!(validate(&step(""), &c).is_ok());
    assert!(validate(&step("scope = \"vault\"\nexclude = [\"a/\", \" b \"]"), &c).is_ok());
    assert!(validate(&step("scope = \"all\""), &c).is_err());
    assert!(validate(&step("bogus = 1"), &c).is_err());
    let ex = normalize_exclusions(&["a/".into(), " a".into(), "".into(), "b//".into()]);
    assert_eq!(ex, vec!["a", "b"]);
    assert!(is_excluded("a", &ex) && is_excluded("a/x.md", &ex));
    assert!(!is_excluded("ab/x.md", &ex));
}

#[test]
fn vault_scan_aggregates_skips_and_rescan_writes_nothing() {
    let e = env(&[
        ("n/a.md", A),
        ("n/b.md", B),
        ("_crucible/c.md", "https://excluded.example/z\n"),
        (
            "r.md",
            "---\ntype: link-record\n---\nhttps://record.example/z\n",
        ),
    ]);
    let o = run(&e, "scope = \"vault\"", None).unwrap();
    assert_eq!(n(&o, "scanned"), 2);
    assert_eq!(n(&o, "records"), 4);
    assert_eq!(n(&o, "created"), 4);
    assert_eq!(n(&o, "candidates"), 1);
    {
        let db = e.state.lock().unwrap();
        assert_eq!(db.link_record_count().unwrap(), 4);
        let r = db.link_record("https://example.com/a").unwrap().unwrap();
        assert_eq!(r.source_notes, vec!["[[n/a]]", "[[n/b]]"]);
        assert_eq!(r.state, "pending");
        assert_eq!(db.link_records_by_yt("dQw4w9WgXcQ").unwrap().len(), 1);
        assert_eq!(db.link_records_by_x("1234567890").unwrap().len(), 1);
    }
    let snapshot = |db: &StateDb| {
        (
            db.link_record_count().unwrap(),
            db.link_record("https://example.com/a").unwrap(),
        )
    };
    let before = snapshot(&e.state.lock().unwrap());
    let o2 = run(&e, "scope = \"vault\"", None).unwrap();
    assert_eq!(
        (n(&o2, "created"), n(&o2, "updated"), n(&o2, "unchanged")),
        (0, 0, 4)
    );
    assert_eq!(snapshot(&e.state.lock().unwrap()), before);
}

#[test]
fn note_scope_scans_the_active_note_only() {
    let e = env(&[("n/a.md", A), ("n/b.md", B)]);
    assert!(run(&e, "", None).is_err(), "no active note");
    let o = run(&e, "", Some("n/b.md")).unwrap();
    assert_eq!((n(&o, "scanned"), n(&o, "created")), (1, 3));
    let o = run(&e, "", Some("n/a.md")).unwrap();
    assert_eq!(
        (n(&o, "created"), n(&o, "updated"), n(&o, "unchanged")),
        (1, 1, 0)
    );
    let o = run(&e, "", Some("n/a.md")).unwrap();
    assert_eq!((n(&o, "created"), n(&o, "updated")), (0, 0));
    let db = e.state.lock().unwrap();
    let r = db.link_record("https://example.com/a").unwrap().unwrap();
    assert_eq!(r.source_notes, vec!["[[n/b]]", "[[n/a]]"]);
    assert_eq!(r.tracked_source, json!(false));
}

#[test]
fn note_scope_skips_non_markdown_active_path() {
    let e = env(&[(
        "n/_attachments/x.png",
        "xmp http://ns.adobe.com/xap/1.0/ https://example.com/a",
    )]);
    let o = run(&e, "", Some("n/_attachments/x.png")).unwrap();
    assert_eq!((n(&o, "scanned"), n(&o, "created")), (0, 0));
}
