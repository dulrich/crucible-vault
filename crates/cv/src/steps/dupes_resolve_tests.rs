//! `dupes_resolve` tests (plan `round-12-feedback-todos` WP-7): synthetic temp vaults through the
//! real writer thread.

use cv_actions::Step;
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value};

use super::StepOutcome;
use super::dupes::{NOT_DUP_KEY, POSSIBLE_KEY};
use super::testkit::{StepEnv, config_of};

const ACTIONS: &str = "[[action]]\nid = \"dupes-resolve\"\n[[action.steps]]\ntype = \"dupes_resolve\"\n\
[[action]]\nid = \"dupes-scan\"\n[[action.steps]]\ntype = \"dupes_scan\"\n";

fn env(files: &[(&str, String)]) -> StepEnv {
    let config = config_of(&[("actions.toml", ACTIONS)]);
    let action = config.armed("dupes-resolve").unwrap().clone();
    StepEnv::new(
        LiveFixture::new(files.iter().map(|(p, t)| (*p, t.as_str()))),
        config,
        action,
    )
}

fn step(kind: &str) -> Step {
    let mut t = toml::Table::new();
    t.insert("type".into(), kind.into());
    Step::from_table(t).unwrap()
}

fn resolve(e: &StepEnv, keep: &str, other: &str, verdict: &str) -> StepOutcome {
    let mut p = Map::new();
    p.insert("keep".into(), keep.into());
    p.insert("other".into(), other.into());
    p.insert("verdict".into(), verdict.into());
    e.exec_with(&step("dupes_resolve"), &p, &e.context, &e.vars)
        .unwrap()
}

fn scan(e: &StepEnv) -> StepOutcome {
    crate::steps::execute(&step("dupes_scan"), &mut e.ctx()).unwrap()
}

fn keys(e: &StepEnv, rel: &str, key: &str) -> Vec<String> {
    cv_note::FrontmatterDoc::new(e.read(rel).unwrap()).get_strings(key)
}

fn s<'a>(o: &'a StepOutcome, k: &str) -> &'a str {
    o.vars.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn words(seed: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("{seed}w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn labelled(other: &str, body: &str) -> String {
    format!("---\ntitle: T\n{POSSIBLE_KEY}:\n  - \"[[{other}]]\"\n---\n{body}\n")
}

fn tracker_row(e: &StepEnv, id: &str, path: &str) {
    e.state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: "feed".into(),
            item_id: id.into(),
            title: "T".into(),
            url: format!("https://x.example/{id}"),
            first_seen_ms: 1,
            day: "2026-10-01".into(),
            outcome: TrackerOutcome::Note,
            score: None,
            full_body: Some(true),
            note_path: Some(path.into()),
            source_name: None,
            duration_s: None,
            word_count: None,
            access: None,
            complete: Some(false),
            full_word_count: None,
        })
        .unwrap();
}

#[test]
fn not_duplicates_relabels_both_records_one_row_and_suppresses_rescan() {
    let body = words("x", 60);
    let e = env(&[
        ("a/one.md", labelled("b/two", &body)),
        ("b/two.md", labelled("a/one", &body)),
    ]);
    let o = resolve(&e, "a/one.md", "b/two.md", "not-duplicates");
    assert_eq!(s(&o, "action"), "not-duplicates");
    assert!(keys(&e, "a/one.md", POSSIBLE_KEY).is_empty());
    assert!(keys(&e, "b/two.md", POSSIBLE_KEY).is_empty());
    assert_eq!(keys(&e, "a/one.md", NOT_DUP_KEY), vec!["[[b/two]]"]);
    assert_eq!(keys(&e, "b/two.md", NOT_DUP_KEY), vec!["[[a/one]]"]);
    assert!(e.read("a/one.md").unwrap().contains("title: T"));
    let rows = e.state.lock().unwrap().dup_labels().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, "not-duplicates");
    assert!((rows[0].containment_keep_in_other - 1.0).abs() < 1e-9);
    // A rescan suppresses the pair instead of relabelling it.
    let o = scan(&e);
    assert_eq!(o.vars["pairs"], 0, "{:?}", o.vars);
    assert_eq!(o.vars["suppressed"], 1);
    assert!(keys(&e, "a/one.md", POSSIBLE_KEY).is_empty());
    // A second verdict keeps set semantics.
    resolve(&e, "a/one.md", "b/two.md", "not-duplicates");
    assert_eq!(keys(&e, "a/one.md", NOT_DUP_KEY).len(), 1);
}

#[test]
fn keep_repoints_rows_trashes_other_and_counts_user_inbound_only() {
    let body = words("y", 60);
    let e = env(&[
        ("a/keep.md", labelled("b/dup", &body)),
        ("b/dup.md", labelled("a/keep", &body)),
        // Links `b/dup` only through a label: not a user link.
        ("c/label.md", labelled("b/dup", &words("z", 5))),
        // A body link: a user link.
        ("c/body.md", "see [[b/dup]]\n".to_owned()),
    ]);
    tracker_row(&e, "1", "b/dup.md");
    tracker_row(&e, "2", "b/dup.md");
    let o = resolve(&e, "a/keep.md", "b/dup.md", "keep");
    assert_eq!(s(&o, "action"), "keep");
    assert_eq!(o.vars["inbound_links"], 1, "{:?}", o.vars);
    assert_eq!(o.vars["tracker_repointed"], 2);
    {
        let st = e.state.lock().unwrap();
        assert!(st.tracker_items_for_note("b/dup.md").unwrap().is_empty());
        assert_eq!(st.tracker_items_for_note("a/keep.md").unwrap().len(), 2);
        let rows = st.dup_labels().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verdict, "keep");
        assert_eq!(rows[0].other_path, "b/dup.md");
    }
    assert!(e.read("b/dup.md").is_none());
    assert!(e.vault().join(".trash/b/dup.md").exists());
    assert!(keys(&e, "a/keep.md", POSSIBLE_KEY).is_empty());
    assert!(keys(&e, "a/keep.md", NOT_DUP_KEY).is_empty());
    // Idempotent rerun: stale, no new row.
    let o = resolve(&e, "a/keep.md", "b/dup.md", "keep");
    assert_eq!(s(&o, "action"), "stale");
    assert_eq!(e.state.lock().unwrap().dup_labels().unwrap().len(), 1);
}

#[test]
fn missing_note_cleans_the_dangling_entry_and_writes_no_row() {
    let e = env(&[("a/keep.md", labelled("b/gone", &words("q", 30)))]);
    let o = resolve(&e, "a/keep.md", "b/gone.md", "not-duplicates");
    assert_eq!(s(&o, "action"), "stale");
    assert!(keys(&e, "a/keep.md", POSSIBLE_KEY).is_empty());
    assert!(keys(&e, "a/keep.md", NOT_DUP_KEY).is_empty());
    assert!(e.state.lock().unwrap().dup_labels().unwrap().is_empty());
}
