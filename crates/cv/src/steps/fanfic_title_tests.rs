//! `fanfic_title` tests (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-5): synthetic notes
//! over the shipped example config.

use serde_json::{Value, json};

use super::fanfic_title::{Plan, plan};
use super::testkit::{StepEnv, active, example_env};

const FM: &str = "---\ntags:\n  - fanfic\n---\n";
const NOTE_STEP: &str =
    "type = \"fanfic_title\"\nkind = \"{param.kind}\"\npath = \"{vars.moved_path}\"";
const BACKFILL: &str = "type = \"fanfic_title\"\nscope = \"backfill\"";

fn note(body: &str) -> String {
    format!("{FM}{body}")
}

fn var(o: &super::StepOutcome, k: &str) -> Value {
    o.vars.get(k).cloned().unwrap_or(Value::Null)
}

fn moved(path: &str) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("moved_path".into(), json!(path));
    m
}

#[test]
fn plan_is_pure_per_shape() {
    assert_eq!(plan("F/A - Chapter 2.md", "x"), Plan::Already);
    assert_eq!(
        plan("F/A - Chapter 2 - W - Fandom.md", "x"),
        Plan::Rename("F/A - Chapter 2.md".into())
    );
    assert_eq!(
        plan("F/A | Site.md", "---\nk: v\n---\n\n# Chapter 4\n"),
        Plan::Rename("F/A - Chapter 4.md".into())
    );
    assert_eq!(plan("F/A.md", "prose"), Plan::Undetected);
}

#[test]
fn filing_a_fanfic_renames_it_and_rewrites_links() {
    let e = example_env(&[
        (
            "Clips/Alpha - Chapter 3 - Writer - Fandom.md",
            &note("text\n"),
        ),
        ("Hub.md", "see [[Alpha - Chapter 3 - Writer - Fandom]]\n"),
    ]);
    let o = e
        .run_vars(
            NOTE_STEP,
            json!({"kind": "fanfic"}),
            active("Clips/Alpha - Chapter 3 - Writer - Fandom.md"),
            &moved("Clips/Alpha - Chapter 3 - Writer - Fandom.md"),
        )
        .unwrap();
    assert_eq!(var(&o, "fanfic_renamed"), json!(1));
    assert_eq!(var(&o, "moved_path"), json!("Clips/Alpha - Chapter 3.md"));
    assert_eq!(
        var(&o, "moved_from"),
        json!("Clips/Alpha - Chapter 3 - Writer - Fandom.md")
    );
    assert!(e.read("Clips/Alpha - Chapter 3.md").is_some());
    assert!(
        e.read("Clips/Alpha - Chapter 3 - Writer - Fandom.md")
            .is_none()
    );
    let hub = e.read("Hub.md").unwrap();
    assert!(hub.contains("[[Alpha - Chapter 3]]"), "{hub}");
}

#[test]
fn other_kinds_are_a_no_op() {
    let p = "Clips/Alpha - Chapter 3 - Writer.md";
    let e = example_env(&[(p, &note("t\n"))]);
    let o = e
        .run_vars(NOTE_STEP, json!({"kind": "blog"}), active(p), &moved(p))
        .unwrap();
    assert!(o.vars.is_empty() && o.output_paths.is_empty());
    assert!(e.read(p).is_some());
}

#[test]
fn collision_and_undetected_leave_the_note() {
    let (a, b) = (
        "Clips/Alpha - Chapter 3 - Writer.md",
        "Clips/Alpha - Chapter 3.md",
    );
    let e = example_env(&[
        (a, &note("t\n")),
        (b, &note("other\n")),
        ("Clips/Plain.md", &note("t\n")),
    ]);
    let o = e
        .run_vars(NOTE_STEP, json!({"kind": "fanfic"}), active(a), &moved(a))
        .unwrap();
    assert_eq!(var(&o, "fanfic_skipped_collision"), json!(1));
    assert_eq!(var(&o, "fanfic_renamed"), json!(0));
    assert!(o.vars.get("moved_path").is_none());
    assert!(e.read(a).is_some());
    assert_eq!(e.read(b).unwrap(), note("other\n"));
    let o = e
        .run_vars(
            NOTE_STEP,
            json!({"kind": "fanfic"}),
            active("Clips/Plain.md"),
            &moved("Clips/Plain.md"),
        )
        .unwrap();
    assert_eq!(var(&o, "fanfic_undetected"), json!(1));
}

#[test]
fn file_as_kind_renames_then_later_steps_follow() {
    let src = "Clips/Beta - Chapter 8 - Writer.md";
    let e = example_env(&[(src, &note("t\n"))]);
    let action = e.config.armed("file-as-kind").unwrap().clone();
    // move, fanfic_title, upsert_tags, lint (the last step enqueues the child enrich).
    let (open, vars) = e
        .run_steps(&action.steps[..4], json!({"kind": "fanfic"}), active(src))
        .unwrap();
    let to = vars.get("moved_path").and_then(Value::as_str).unwrap();
    assert!(to.ends_with("/Beta - Chapter 8.md"), "{to}");
    assert_eq!(open.as_deref(), Some(to));
    assert!(e.read(to).is_some());
    assert!(e.read(src).is_none());
}

fn corpus() -> StepEnv {
    example_env(&[
        ("F/A - Chapter 1.md", &note("x\n")),
        ("F/B - Chapter 2 - W - Fandom.md", &note("x\n")),
        ("F/C - Chapter 5 - W.md", &note("x\n")),
        ("F/C - Chapter 5.md", &note("x\n")),
        ("F/D | Site.md", &note("Chapter 7\ntext\n")),
        ("F/E.md", &note("prose\n")),
        ("F/Untagged - Chapter 2 - W.md", "no tag\n"),
        ("Hub.md", "[[B - Chapter 2 - W - Fandom]]\n"),
    ])
}

fn count(o: &super::StepOutcome, k: &str) -> u64 {
    var(o, k).as_u64().unwrap()
}

#[test]
fn backfill_dry_run_counts_only() {
    let e = corpus();
    let o = e
        .run(BACKFILL, json!({}), active("F/A - Chapter 1.md"))
        .unwrap();
    assert_eq!(var(&o, "dry_run"), json!(true));
    assert_eq!(count(&o, "scanned"), 6);
    assert_eq!(count(&o, "already"), 2);
    assert_eq!(count(&o, "would_rename"), 2);
    assert_eq!(count(&o, "collision"), 1);
    assert_eq!(count(&o, "undetected"), 1);
    assert_eq!(count(&o, "renamed"), 0);
    assert!(e.read("F/B - Chapter 2 - W - Fandom.md").is_some());
    assert!(o.output_paths.is_empty());
}

#[test]
fn backfill_renames_then_is_idempotent() {
    let e = corpus();
    let ctx = || active("F/A - Chapter 1.md");
    let o = e.run(BACKFILL, json!({"dry_run": false}), ctx()).unwrap();
    assert_eq!(count(&o, "renamed"), 2);
    assert_eq!(count(&o, "collision"), 1);
    assert!(e.read("F/B - Chapter 2.md").is_some());
    assert!(e.read("F/D - Chapter 7.md").is_some());
    assert!(e.read("F/C - Chapter 5 - W.md").is_some());
    assert!(e.read("F/Untagged - Chapter 2 - W.md").is_some());
    assert!(e.read("Hub.md").unwrap().contains("[[B - Chapter 2]]"));
    // A fresh index read sees the renames: the second run has nothing to rename.
    let o = e.run(BACKFILL, json!({"dry_run": false}), ctx()).unwrap();
    assert_eq!(count(&o, "renamed"), 0);
    assert_eq!(count(&o, "would_rename"), 0);
}

#[test]
fn bad_params_are_rejected() {
    for src in [
        "type = \"fanfic_title\"\ndry_run = true",
        "type = \"fanfic_title\"\nscope = \"backfill\"\npath = \"x.md\"",
        "type = \"fanfic_title\"\nscope = \"all\"",
        "type = \"fanfic_title\"\nx = 1",
    ] {
        let step = cv_actions::Step::from_table(src.parse::<toml::Table>().unwrap()).unwrap();
        assert!(
            super::validate(&step, &super::testkit::example_config()).is_err(),
            "{src}"
        );
    }
}

fn review_path() -> String {
    let date = super::materialize::now().date().format("%Y-%m-%d");
    format!("_crucible/reviews/fanfic-titles-{date}.md")
}

#[test]
fn dry_run_writes_review_note() {
    let e = corpus();
    let o = e
        .run(BACKFILL, json!({}), active("F/A - Chapter 1.md"))
        .unwrap();
    let rp = review_path();
    assert_eq!(var(&o, "review_path"), json!(rp));
    assert_eq!(o.open_path.as_deref(), Some(rp.as_str()));
    let t = e.read(&rp).unwrap();
    assert!(
        t.starts_with("# Fanfic title backfill \u{2014} dry run "),
        "{t}"
    );
    assert!(t.contains("Scanned 6, already 2, would rename 2, collision 1, undetected 1."));
    assert!(t.contains("Apply: run \u{201c}Fanfic: chapter titles\u{201d} with Dry run off."));
    assert!(t.contains("## Would rename (2)"));
    assert!(t.contains(
        "- [[F/B - Chapter 2 - W - Fandom|B - Chapter 2 - W - Fandom]] \u{2192} B - Chapter 2\n"
    ));
    // The `|` in the name cannot sit in a wikilink: plain backticked path.
    assert!(t.contains("- `F/D | Site.md` \u{2192} D - Chapter 7\n"));
    assert!(t.contains(
        "## Collision (1)\n\n- [[F/C - Chapter 5 - W|C - Chapter 5 - W]] \u{2192} C - Chapter 5 (exists)\n"
    ));
    assert!(t.contains("## Undetected (1)\n\n- [[F/E|E]]\n"));
}

#[test]
fn second_dry_run_overwrites_and_empty_sections_say_none() {
    let e = corpus();
    for _ in 0..2 {
        e.run(BACKFILL, json!({}), active("F/A - Chapter 1.md"))
            .unwrap();
    }
    assert!(
        e.read(&review_path())
            .unwrap()
            .contains("## Would rename (2)")
    );
    let e = example_env(&[("F/A - Chapter 1.md", &note("x\n"))]);
    e.run(BACKFILL, json!({}), active("F/A - Chapter 1.md"))
        .unwrap();
    let t = e.read(&review_path()).unwrap();
    assert!(t.contains("## Would rename (0)\n\n- (none)\n"), "{t}");
    assert!(t.contains("## Undetected (0)\n\n- (none)\n"), "{t}");
}

#[test]
fn real_run_writes_no_review_note() {
    let e = corpus();
    let o = e
        .run(
            BACKFILL,
            json!({"dry_run": false}),
            active("F/A - Chapter 1.md"),
        )
        .unwrap();
    assert!(e.read(&review_path()).is_none());
    assert!(o.open_path.is_none());
    assert!(o.vars.get("review_path").is_none());
}
