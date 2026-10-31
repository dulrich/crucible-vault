//! WP-3 `link_scan` tests: a synthetic temp vault and a temp `state.db` (never the live vault).

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::ActionContext;
use cv_state::StateDb;
use cv_vault::test_support::LiveFixture;
use serde_json::json;

use super::{is_excluded, normalize_exclusions};
use crate::steps::StepOutcome;
use crate::steps::testkit::StepEnv;
use crate::steps::{execute, validate};

type Env = StepEnv;

fn env(files: &[(&str, &str)]) -> Env {
    let action = Action {
        id: "link-scan".into(),
        label: "Link scan".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        hold: false,
        fast: false,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec {
            note: false,
            keys: vec![],
        },
    };
    StepEnv::new(
        LiveFixture::new(files.iter().copied()),
        ConfigSet::new(),
        action,
    )
}

fn step(src: &str) -> Step {
    let t: toml::Table = format!("type = \"link_scan\"\n{src}").parse().unwrap();
    Step::from_table(t).unwrap()
}

fn run(e: &Env, src: &str, active: Option<&str>) -> Result<StepOutcome, String> {
    let context = ActionContext {
        active_path: active.map(str::to_owned),
        ..ActionContext::default()
    };
    let mut ctx = e.ctx();
    ctx.context = &context;
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
