//! `fanfic_title` tests (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-5): synthetic notes
//! over the shipped example config.

use serde_json::{Value, json};

use super::fanfic_title::{Plan, plan};
use super::testkit::{StepEnv, active, example_env};

const FM: &str = "---\ntags:\n  - fanfic\n---\n";
const NOTE_STEP: &str = "type = \"fanfic_title\"\nmatch_kind = \"fanfic\"\nkind = \"{param.kind}\"\npath = \"{vars.moved_path}\"";
const BACKFILL: &str = "type = \"fanfic_title\"\nscope = \"backfill\"\nkind = \"fanfic\"";

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
    assert_eq!(
        plan("F/A - Chapter 2.md", "x", &[], &|_| true),
        Plan::Already
    );
    assert_eq!(
        plan("F/A - Chapter 2 - W - Fandom.md", "x", &[], &|_| true),
        Plan::Rename("F/A - Chapter 2.md".into())
    );
    assert_eq!(
        plan(
            "F/A | Site.md",
            "---\nk: v\n---\n\n# Chapter 4\n",
            &[],
            &|_| true
        ),
        Plan::Rename("F/A - Chapter 4.md".into())
    );
    assert_eq!(plan("F/A.md", "prose", &[], &|_| true), Plan::Undetected);
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

/// Plan public-release WP-4 pin: backfill selects by its `kind` param. `kind = "fanfic"` is
/// today's selection (the counts above); another kind selects none of these notes.
#[test]
fn backfill_selects_by_its_kind_param() {
    let e = corpus();
    let other = "type = \"fanfic_title\"\nscope = \"backfill\"\nkind = \"serial\"";
    let o = e
        .run(
            other,
            json!({"dry_run": false}),
            active("F/A - Chapter 1.md"),
        )
        .unwrap();
    assert_eq!((count(&o, "scanned"), count(&o, "renamed")), (0, 0));
    assert!(e.read("F/B - Chapter 2 - W - Fandom.md").is_some());
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
        // Plan public-release WP-4: no built-in kind; backfill takes `kind`, note `match_kind`.
        "type = \"fanfic_title\"\nscope = \"backfill\"",
        "type = \"fanfic_title\"\nkind = \"{param.kind}\"",
        "type = \"fanfic_title\"\nscope = \"backfill\"\nkind = \"fanfic\"\nmatch_kind = \"fanfic\"",
        "type = \"fanfic_title\"\nscope = \"backfill\"\nkind = \"{param.kind}\"",
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
    assert!(t.contains(
        "Scanned 6, already 2, would rename 2, would retitle 0, collision 1, undetected 1."
    ));
    assert!(t.contains("Apply: run this backfill again with Dry run off."));
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

// ---- WP-8: target-based plan, serial publication, chain-safe backfill ----------------------

fn serial_rows() -> Vec<cv_actions::pipelines::SerialSource> {
    #[derive(serde::Deserialize)]
    struct Rows {
        serial: Vec<cv_actions::pipelines::SerialSource>,
    }
    let src = "[[serial]]\nname = \"Zeta Long Name\"\nshort = \"Zeta:  Tale\"\nprefix = [\"https://fic.example/s/1/\"]\n";
    toml::from_str::<Rows>(src).unwrap().serial
}

#[test]
fn publication_from_serial_row_names_the_story() {
    let rows = serial_rows();
    let text = "---\nsource: https://fic.example/s/1/3\npublication: Other\n---\nChapter 3\n";
    assert_eq!(
        plan("F/Site Title | Fic Site.md", text, &rows, &|_| true),
        Plan::Rename("F/Zeta Tale - Chapter 3.md".into())
    );
    assert_eq!(
        plan("F/Zeta Tale - Chapter 3.md", text, &rows, &|_| true),
        Plan::Already
    );
    // No serial match: the `publication` frontmatter names the story.
    let text = "---\nsource: https://elsewhere.example/\npublication: Eta Saga\n---\nChapter 2\n";
    assert_eq!(
        plan("F/Eta - Chapter 2.md", text, &rows, &|_| true),
        Plan::Rename("F/Eta Saga - Chapter 2.md".into())
    );
}

#[test]
fn target_shaped_note_with_wrong_story_is_renamed() {
    let fm = "---\ntags:\n  - fanfic\npublication: Theta Arc\n---\n";
    let p = "F/Wrong Name - Chapter 4.md";
    let e = example_env(&[(p, &format!("{fm}x\n"))]);
    let o = e
        .run(BACKFILL, json!({"dry_run": false}), active(p))
        .unwrap();
    assert_eq!(count(&o, "renamed"), 1);
    assert!(e.read("F/Theta Arc - Chapter 4.md").is_some());
}

/// Three notes numbered one too high by the site; the body carries the logical number.
fn chain() -> StepEnv {
    example_env(&[
        ("F/S - Chapter 1.md", &note("Chapter 0\n")),
        ("F/S - Chapter 2.md", &note("Chapter 1\n")),
        ("F/S - Chapter 3.md", &note("Chapter 2\n")),
    ])
}

#[test]
fn down_shift_chain_renames_holders_first() {
    let e = chain();
    let o = e
        .run(BACKFILL, json!({}), active("F/S - Chapter 1.md"))
        .unwrap();
    assert_eq!(count(&o, "would_rename"), 3);
    assert_eq!(count(&o, "collision"), 0);
    let o = e
        .run(
            BACKFILL,
            json!({"dry_run": false}),
            active("F/S - Chapter 1.md"),
        )
        .unwrap();
    assert_eq!(count(&o, "renamed"), 3);
    assert_eq!(count(&o, "collision"), 0);
    for (n, body) in [(0, "Chapter 0\n"), (1, "Chapter 1\n"), (2, "Chapter 2\n")] {
        assert_eq!(
            e.read(&format!("F/S - Chapter {n}.md")).unwrap(),
            note(body)
        );
    }
    assert!(e.read("F/S - Chapter 3.md").is_none());
}

#[test]
fn duplicate_cycle_and_static_holder_are_collisions() {
    let e = example_env(&[
        // Two notes claim `Dup - Chapter 1`.
        ("F/Dup - Chapter 5 - W.md", &note("Chapter 1\n")),
        ("F/Dup - Chapter 6 - W.md", &note("Chapter 1\n")),
        // A swap: each target held by the other mover.
        ("F/Cy - Chapter 1.md", &note("Chapter 2\n")),
        ("F/Cy - Chapter 2.md", &note("Chapter 1\n")),
        // The holder stays (already its target).
        ("F/Hold - Chapter 9 - W.md", &note("Chapter 8\n")),
        ("F/Hold - Chapter 8.md", &note("Chapter 8\n")),
    ]);
    let o = e
        .run(BACKFILL, json!({}), active("F/Cy - Chapter 1.md"))
        .unwrap();
    assert_eq!(count(&o, "collision"), 5);
    assert_eq!(count(&o, "would_rename"), 0);
    let t = e.read(&review_path()).unwrap();
    assert!(t.contains("Dup - Chapter 1 (duplicate target)"), "{t}");
    assert!(t.contains("Cy - Chapter 1 (cycle)"), "{t}");
    assert!(t.contains("Hold - Chapter 8 (exists)"), "{t}");
    let o = e
        .run(
            BACKFILL,
            json!({"dry_run": false}),
            active("F/Cy - Chapter 1.md"),
        )
        .unwrap();
    assert_eq!(count(&o, "renamed"), 0);
    assert!(e.read("F/Dup - Chapter 5 - W.md").is_some());
    assert!(e.read("F/Hold - Chapter 9 - W.md").is_some());
}

#[test]
fn order_renames_withholds_a_chain_behind_a_withheld_holder() {
    use super::fanfic_title::{Withheld, order_renames};
    let r = |s: &str, d: &str| (s.to_owned(), d.to_owned());
    // c → b is held by b, whose own target a is held by a non-mover.
    let renames = [r("b", "a"), r("c", "b")];
    let (order, held) = order_renames(&renames, &|p| p == "a" || p == "b");
    assert!(order.is_empty());
    assert_eq!(held.len(), 2);
    assert!(held.iter().all(|(_, _, w)| *w == Withheld::Exists));
    let (order, held) = order_renames(&renames, &|p| p == "b");
    assert_eq!(order, vec![r("b", "a"), r("c", "b")]);
    assert!(held.is_empty());
}

// ---- 2026-11-10: the note's `title` follows the chapter name ----------------------------------

fn titled(title: &str, body: &str) -> String {
    format!("---\ntitle: {title}\ntags:\n  - fanfic\n---\n{body}")
}

#[test]
fn filing_sets_a_stale_title_to_the_new_name() {
    let src = "Clips/Alpha - Chapter 3 - Writer - Fandom.md";
    let e = example_env(&[(src, &titled("Alpha (crossover, long clip title)", "text\n"))]);
    let o = e
        .run_vars(
            NOTE_STEP,
            json!({"kind": "fanfic"}),
            active(src),
            &moved(src),
        )
        .unwrap();
    assert_eq!(var(&o, "fanfic_renamed"), json!(1));
    assert_eq!(
        var(&o, "fanfic_retitled"),
        json!(0),
        "a rename is not counted as a retitle"
    );
    assert_eq!(
        e.read("Clips/Alpha - Chapter 3.md").unwrap(),
        titled("Alpha - Chapter 3", "text\n")
    );
}

#[test]
fn a_note_without_a_title_is_not_given_one() {
    let src = "Clips/Alpha - Chapter 3 - Writer - Fandom.md";
    let e = example_env(&[(src, &note("text\n"))]);
    e.run_vars(
        NOTE_STEP,
        json!({"kind": "fanfic"}),
        active(src),
        &moved(src),
    )
    .unwrap();
    assert_eq!(
        e.read("Clips/Alpha - Chapter 3.md").unwrap(),
        note("text\n")
    );
}

#[test]
fn already_named_note_with_a_stale_title_is_retitled_once() {
    let p = "Clips/Alpha - Chapter 3.md";
    let e = example_env(&[(p, &titled("Old clip title", "text\n"))]);
    let o = e
        .run_vars(NOTE_STEP, json!({"kind": "fanfic"}), active(p), &moved(p))
        .unwrap();
    assert_eq!(var(&o, "fanfic_already"), json!(1));
    assert_eq!(var(&o, "fanfic_retitled"), json!(1));
    assert_eq!(e.read(p).unwrap(), titled("Alpha - Chapter 3", "text\n"));
    let o = e
        .run_vars(NOTE_STEP, json!({"kind": "fanfic"}), active(p), &moved(p))
        .unwrap();
    assert_eq!(var(&o, "fanfic_retitled"), json!(0));
    assert!(o.output_paths.is_empty());
}

#[test]
fn backfill_retitles_stale_titles_after_a_dry_run_lists_them() {
    let e = example_env(&[
        ("F/A - Chapter 1.md", &titled("A clip title", "x\n")),
        ("F/B - Chapter 2.md", &titled("B - Chapter 2", "x\n")),
        ("F/C - Chapter 3 - W.md", &titled("C clip title", "x\n")),
    ]);
    let ctx = || active("F/A - Chapter 1.md");
    let o = e.run(BACKFILL, json!({}), ctx()).unwrap();
    assert_eq!(count(&o, "already"), 2);
    assert_eq!(count(&o, "would_retitle"), 1);
    let t = e.read(&review_path()).unwrap();
    assert!(
        t.contains("## Would retitle (1)\n\n- [[F/A - Chapter 1|A - Chapter 1]]\n"),
        "{t}"
    );
    assert_eq!(
        e.read("F/A - Chapter 1.md").unwrap(),
        titled("A clip title", "x\n")
    );
    let o = e.run(BACKFILL, json!({"dry_run": false}), ctx()).unwrap();
    assert_eq!(count(&o, "retitled"), 1);
    assert_eq!(count(&o, "renamed"), 1);
    assert_eq!(
        e.read("F/A - Chapter 1.md").unwrap(),
        titled("A - Chapter 1", "x\n")
    );
    assert_eq!(
        e.read("F/C - Chapter 3.md").unwrap(),
        titled("C - Chapter 3", "x\n")
    );
    let o = e.run(BACKFILL, json!({"dry_run": false}), ctx()).unwrap();
    assert_eq!((count(&o, "retitled"), count(&o, "renamed")), (0, 0));
}

#[test]
fn retitled_text_is_pure() {
    use super::fanfic_title::retitled_text;
    assert_eq!(retitled_text("no frontmatter\n", "X"), None);
    assert_eq!(retitled_text(&titled("X", ""), "X"), None);
    assert_eq!(
        retitled_text(&titled("Y", ""), "X").as_deref(),
        Some(titled("X", "").as_str())
    );
}
