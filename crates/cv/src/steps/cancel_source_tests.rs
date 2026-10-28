//! `cancel_source_intake` tests (plan `cancel-source-intake` WP-1): a synthetic temp vault and a
//! temp `state.db` (never the network, never the live vault or state).

use std::collections::BTreeMap;

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value, json};

use super::cancel_source::{cancel_orphaned, mail_keys};
use crate::steps::testkit::StepEnv;
use crate::steps::{StepError, StepOutcome, execute, validate};

fn config(sources: &str) -> ConfigSet {
    let files = BTreeMap::from([("sources.toml".to_owned(), Some(sources.to_owned()))]);
    let mut c = ConfigSet::new();
    c.reload(&files, &validate);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    c
}

fn action() -> Action {
    Action {
        id: "cancel-source-intake".into(),
        label: "Cancel".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec::default(),
    }
}

fn env(sources: &str) -> StepEnv {
    StepEnv::new(
        LiveFixture::new(Vec::<(&str, &str)>::new()),
        config(sources),
        action(),
    )
}

fn row(kind: &str, key: &str, id: &str, name: &str) -> TrackerItem {
    TrackerItem {
        source_kind: kind.into(),
        source_key: key.into(),
        item_id: id.into(),
        title: "T".into(),
        url: "u".into(),
        first_seen_ms: 1,
        day: "2026-10-28".into(),
        outcome: TrackerOutcome::Line,
        score: None,
        full_body: None,
        note_path: None,
        source_name: Some(name.into()),
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    }
}

fn add(e: &StepEnv, kind: &str, key: &str, id: &str, name: &str) {
    e.state
        .lock()
        .unwrap()
        .insert_tracker_item(&row(kind, key, id, name))
        .unwrap();
}

fn status(e: &StepEnv, kind: &str, key: &str, id: &str) -> Option<char> {
    e.state
        .lock()
        .unwrap()
        .tracker_item_status(kind, key, id)
        .unwrap()
        .unwrap()
}

fn manual(e: &StepEnv, source: &str) -> Result<StepOutcome, StepError> {
    let t: toml::Table = "type = \"cancel_source_intake\"\nsource = \"{param.source}\""
        .parse()
        .unwrap();
    let step = Step::from_table(t).unwrap();
    let params = Map::from_iter([("source".to_owned(), Value::String(source.to_owned()))]);
    e.exec_with(&step, &params, &Default::default(), &Map::new())
}

fn n(o: &StepOutcome, k: &str) -> Value {
    o.vars.get(k).cloned().unwrap_or(Value::Null)
}

#[test]
fn manual_cancels_by_key_or_name_case_insensitively() {
    let e = env("");
    add(&e, "youtube", "UCa", "v1", "Chan A");
    add(&e, "youtube", "UCa", "v2", "Chan A");
    add(&e, "youtube", "UCb", "v3", "Chan B");
    let o = manual(&e, "  chan a ").unwrap();
    assert_eq!(n(&o, "cancelled"), json!(2));
    assert_eq!(n(&o, "kind"), json!("youtube"));
    assert_eq!(n(&o, "source_key"), json!("UCa"));
    assert_eq!(status(&e, "youtube", "UCa", "v1"), Some('-'));
    assert_eq!(status(&e, "youtube", "UCb", "v3"), None);
    let o = manual(&e, "ucb").unwrap();
    assert_eq!(n(&o, "cancelled"), json!(1));
    assert_eq!(status(&e, "youtube", "UCb", "v3"), Some('-'));
}

#[test]
fn manual_fails_without_change_on_no_match_near_match_empty_or_ambiguous() {
    let e = env("");
    add(&e, "youtube", "UCa", "v1", "Chan A");
    add(&e, "blog", "https://x.example/feed", "p1", "Chan A");
    for q in ["", "   ", "Chan", "Chan AA", "nothing"] {
        assert!(manual(&e, q).is_err(), "{q:?}");
    }
    let err = manual(&e, "chan a").unwrap_err().to_string();
    assert!(err.contains("youtube:UCa (Chan A)"), "{err}");
    assert!(
        err.contains("blog:https://x.example/feed (Chan A)"),
        "{err}"
    );
    assert_eq!(status(&e, "youtube", "UCa", "v1"), None);
    assert_eq!(status(&e, "blog", "https://x.example/feed", "p1"), None);
}

#[test]
fn orphans_cancel_only_removed_sources() {
    let e = env("");
    for (key, id) in [("UCa", "a1"), ("UCb", "b1"), ("UCc", "c1"), ("UCd", "d1")] {
        add(&e, "youtube", key, id, key);
    }
    // UCb is a `skip` row: still present.
    let present = ["UCa", "UCb", "UCc"].map(str::to_owned);
    assert_eq!(cancel_orphaned(&e.ctx(), "youtube", &present).unwrap(), 1);
    assert_eq!(status(&e, "youtube", "UCd", "d1"), Some('-'));
    for (key, id) in [("UCa", "a1"), ("UCb", "b1"), ("UCc", "c1")] {
        assert_eq!(status(&e, "youtube", key, id), None);
    }
    assert!(e.state.lock().unwrap().is_ignored("youtube", "d1").unwrap());
}

#[test]
fn mail_unknown_sender_is_never_orphaned() {
    let e = env("");
    add(&e, "mail", "unknown-sender", "m1", "Unknown");
    add(&e, "mail", "Gone", "m2", "Gone");
    for k in ["A", "B", "C"] {
        add(&e, "mail", k, &format!("m{k}"), k);
    }
    let present = ["A", "B", "C"].map(str::to_owned);
    assert_eq!(cancel_orphaned(&e.ctx(), "mail", &present).unwrap(), 1);
    assert_eq!(status(&e, "mail", "unknown-sender", "m1"), None);
    assert_eq!(status(&e, "mail", "Gone", "m2"), Some('-'));
}

#[test]
fn mail_keys_cover_newsletters_and_blog_names() {
    let c = config(
        "[[newsletter]]\nname = \"NL\"\nhosts = [\"nl.example\"]\n[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\n",
    );
    assert_eq!(mail_keys(&c), vec!["NL", "B"]);
}

#[test]
fn guards_cancel_nothing() {
    // Empty present list (also a missing sources.toml).
    let e = env("");
    add(&e, "youtube", "UCa", "a1", "A");
    assert_eq!(cancel_orphaned(&e.ctx(), "youtube", &[]).unwrap(), 0);
    // More than half orphaned: 2 of 3.
    add(&e, "youtube", "UCb", "b1", "B");
    add(&e, "youtube", "UCc", "c1", "C");
    let present = ["UCa".to_owned()];
    assert_eq!(cancel_orphaned(&e.ctx(), "youtube", &present).unwrap(), 0);
    assert_eq!(status(&e, "youtube", "UCb", "b1"), None);
    // Exactly half is allowed (1 of 2 -> strictly-more-than-half guard does not fire).
    let e2 = env("");
    add(&e2, "youtube", "UCa", "a1", "A");
    add(&e2, "youtube", "UCb", "b1", "B");
    let present = ["UCa".to_owned()];
    assert_eq!(cancel_orphaned(&e2.ctx(), "youtube", &present).unwrap(), 1);
    // A sources.toml error (last valid copy kept) cancels nothing.
    let mut broken = config("[[youtube]]\nchannel = \"A\"\nid = \"UCa\"\n");
    let files = BTreeMap::from([("sources.toml".to_owned(), Some("[[youtube".to_owned()))]);
    broken.reload(&files, &validate);
    assert!(!broken.errors().is_empty());
    let e3 = StepEnv::new(
        LiveFixture::new(Vec::<(&str, &str)>::new()),
        broken,
        action(),
    );
    add(&e3, "youtube", "UCa", "a1", "A");
    add(&e3, "youtube", "UCz", "z1", "Z");
    add(&e3, "youtube", "UCy", "y1", "Y");
    add(&e3, "youtube", "UCx", "x1", "X");
    let present = ["UCa".to_owned(), "UCy".to_owned(), "UCx".to_owned()];
    assert_eq!(cancel_orphaned(&e3.ctx(), "youtube", &present).unwrap(), 0);
    assert_eq!(status(&e3, "youtube", "UCz", "z1"), None);
}

/// Audit E1: the pass runs on a tracker's early-return paths (disabled pipeline).
#[test]
fn trackers_run_the_orphans_pass_when_disabled() {
    for (step_type, kind, present_src, key) in [
        (
            "track_youtube",
            "youtube",
            "[[youtube]]\nchannel = \"A\"\nid = \"UCa\"\n",
            "UCgone",
        ),
        (
            "track_blogs",
            "blog",
            "[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\n",
            "https://gone.example/feed",
        ),
    ] {
        let e = env(present_src);
        let present = if kind == "youtube" {
            "UCa"
        } else {
            "https://b.example/feed"
        };
        add(&e, kind, present, "p1", "Present");
        add(&e, kind, present, "p2", "Present");
        add(&e, kind, key, "g1", "Gone");
        let t: toml::Table = format!("type = \"{step_type}\"").parse().unwrap();
        let o = execute(&Step::from_table(t).unwrap(), &mut e.ctx()).unwrap();
        assert_eq!(n(&o, "disabled"), json!(true), "{step_type}");
        assert_eq!(status(&e, kind, key, "g1"), Some('-'), "{step_type}");
        assert_eq!(status(&e, kind, present, "p1"), None, "{step_type}");
    }
}
