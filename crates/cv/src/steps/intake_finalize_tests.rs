//! `intake_finalize` tests (plan intake-followups-mail-youtube WP-4): synthetic temp vaults and a
//! temp `state.db` (never the live vault).

use cv_actions::{Action, LaneChoice, ResourceSpec, Step};
use cv_protocol::{ActionContext, TrackerOutcome};
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value, json};

use crate::steps::testkit::{StepEnv, config_of};
use crate::steps::{StepOutcome, validate};

const VID: &str = "dQw4w9WgXcQ";

fn action() -> Action {
    Action {
        id: "file-as-kind".into(),
        label: "File".into(),
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

fn env(files: &[(&str, String)]) -> StepEnv {
    let fx = LiveFixture::new(files.iter().map(|(p, t)| (*p, t.clone())));
    let config = config_of(&[(
        "kinds.toml",
        "[kinds.post]\ncleanup_transcript = true\n[kinds.article]\n",
    )]);
    StepEnv::new(fx, config, action())
}

fn words(n: usize) -> String {
    (0..n)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn stamps() -> String {
    "## Transcript\n\n**0:01** one\n\n**0:05** two\n\n**1:10** three\n".to_owned()
}

fn add(e: &StepEnv, kind: &str, id: &str, url: &str, path: Option<&str>, status: Option<char>) {
    let mut db = e.state.lock().unwrap();
    db.insert_tracker_item(&TrackerItem {
        source_kind: kind.into(),
        source_key: "k".into(),
        item_id: id.into(),
        title: "T".into(),
        url: url.into(),
        first_seen_ms: 1,
        day: "2026-10-21".into(),
        outcome: TrackerOutcome::Note,
        score: None,
        full_body: None,
        note_path: path.map(str::to_owned),
        source_name: None,
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    })
    .unwrap();
    if status.is_some() {
        db.set_tracker_item_status(kind, "k", id, status).unwrap();
    }
}

fn row(e: &StepEnv, kind: &str, id: &str) -> (TrackerItem, Option<char>) {
    let db = e.state.lock().unwrap();
    (
        db.tracker_item(kind, "k", id).unwrap().unwrap(),
        db.tracker_item_status(kind, "k", id).unwrap().unwrap(),
    )
}

fn run_on(e: &StepEnv, path: &str) -> StepOutcome {
    let step = Step::from_table("type = \"intake_finalize\"".parse().unwrap()).unwrap();
    let mut vars = Map::new();
    vars.insert("moved_path".into(), json!(path));
    e.exec_with(&step, &Map::new(), &ActionContext::default(), &vars)
        .unwrap()
}

fn counts(o: &StepOutcome) -> [Value; 4] {
    ["rows", "linked", "ticked", "no_evidence"].map(|k| o.vars[k].clone())
}

#[test]
fn params_validate() {
    let c = cv_actions::ConfigSet::new();
    let s = |src: &str| {
        Step::from_table(
            format!("type = \"intake_finalize\"\n{src}")
                .parse()
                .unwrap(),
        )
        .unwrap()
    };
    assert!(validate(&s(""), &c).is_ok());
    assert!(validate(&s("path = \"{vars.moved_path}\""), &c).is_ok());
    assert!(validate(&s("nope = 1"), &c).is_err());
}

#[test]
fn filed_clip_links_and_ticks_and_rerun_is_idempotent() {
    let clip = format!(
        "---\nsource: https://youtu.be/{VID}\ntags: [post]\n---\n![](https://youtu.be/{VID})\n\nA description.\n\n{}",
        stamps()
    );
    let e = env(&[("stack/clip.md", clip)]);
    // The row URL is the watch form; it resolves by video id.
    add(
        &e,
        "youtube",
        VID,
        &format!("https://www.youtube.com/watch?v={VID}&t=1"),
        None,
        None,
    );
    let o = run_on(&e, "stack/clip.md");
    assert_eq!(counts(&o), [json!(1), json!(1), json!(1), json!(0)]);
    let (r, s) = row(&e, "youtube", VID);
    assert_eq!(
        (r.note_path.as_deref(), r.complete, s),
        (Some("stack/clip.md"), Some(true), Some('x'))
    );
    let o2 = run_on(&e, "stack/clip.md");
    assert_eq!(counts(&o2), [json!(1), json!(0), json!(0), json!(0)]);
    assert_eq!(row(&e, "youtube", VID).1, Some('x'));
    assert!(!serde_json::to_string(&o.vars).unwrap().contains("stack/"));
}

#[test]
fn note_that_only_embeds_the_video_never_ticks() {
    let n = format!(
        "---\nsource: https://ex.test/article\n---\n![](https://youtu.be/{VID})\n\n{}",
        stamps()
    );
    let e = env(&[("notes/a.md", n)]);
    add(&e, "youtube", VID, "u", Some("notes/a.md"), None);
    let o = run_on(&e, "notes/a.md");
    assert_eq!(counts(&o), [json!(1), json!(0), json!(0), json!(1)]);
    let (r, s) = row(&e, "youtube", VID);
    assert_eq!((r.complete, s), (None, None));
}

#[test]
fn clip_without_transcript_links_only() {
    let n = format!(
        "---\nsource: https://youtu.be/{VID}\ntags: [post]\n---\n![](https://youtu.be/{VID})\n\n{}\n",
        words(300)
    );
    let e = env(&[("stack/c.md", n)]);
    add(&e, "youtube", VID, "u", None, None);
    let o = run_on(&e, "stack/c.md");
    assert_eq!(counts(&o), [json!(1), json!(1), json!(0), json!(1)]);
    let (r, s) = row(&e, "youtube", VID);
    assert_eq!((r.note_path.as_deref(), s), (Some("stack/c.md"), None));
}

#[test]
fn legacy_cleaned_post_ticks_but_not_without_the_cleanup_kind() {
    let body = words(80);
    let e = env(&[
        (
            "posts/p.md",
            format!("---\nsource: https://youtu.be/{VID}\ntags: [post]\n---\n{body}\n"),
        ),
        (
            "notes/a.md",
            format!(
                "---\nyt-video-id: other0000ab\nsource: https://youtu.be/other0000ab\ntags: [article]\n---\n{body}\n"
            ),
        ),
    ]);
    add(&e, "youtube", VID, "u", None, None);
    add(&e, "youtube", "other0000ab", "u2", None, None);
    assert_eq!(counts(&run_on(&e, "posts/p.md"))[2], json!(1));
    assert_eq!(row(&e, "youtube", VID).1, Some('x'));
    assert_eq!(counts(&run_on(&e, "notes/a.md"))[3], json!(1));
    assert_eq!(row(&e, "youtube", "other0000ab").1, None);
}

#[test]
fn blog_rows_go_through_finalize_and_set_rows_stay_untouched() {
    let e = env(&[
        (
            "notes/free.md",
            format!("---\nsource: https://ex.test/p/free\n---\n{}\n", words(30)),
        ),
        (
            "notes/paid.md",
            format!(
                "---\nsource: https://ex.test/p/paid\naccess: paid\n---\n{}\n",
                words(30)
            ),
        ),
        (
            "notes/skip.md",
            format!("---\nsource: https://ex.test/p/skip\n---\n{}\n", words(30)),
        ),
    ]);
    add(&e, "blog", "free", "https://ex.test/p/free", None, None);
    add(&e, "blog", "paid", "https://ex.test/p/paid", None, None);
    add(
        &e,
        "blog",
        "skip",
        "https://ex.test/p/skip",
        None,
        Some('-'),
    );
    assert_eq!(
        counts(&run_on(&e, "notes/free.md")),
        [json!(1), json!(1), json!(1), json!(0)]
    );
    let (r, s) = row(&e, "blog", "free");
    assert_eq!((r.complete, s), (Some(true), Some('x')));
    assert_eq!(counts(&run_on(&e, "notes/paid.md"))[3], json!(1));
    let (r, s) = row(&e, "blog", "paid");
    assert_eq!(
        (r.access.as_deref(), r.complete, s),
        (Some("paid"), Some(false), None)
    );
    // An already-skipped row is linked but its status and evidence are untouched.
    let o = run_on(&e, "notes/skip.md");
    assert_eq!(counts(&o), [json!(1), json!(1), json!(0), json!(0)]);
    let (r, s) = row(&e, "blog", "skip");
    assert_eq!((r.complete, s), (None, Some('-')));
    // A note with no row is a no-op success.
    let e2 = env(&[("notes/x.md", "---\ntitle: x\n---\nbody\n".to_owned())]);
    assert_eq!(
        counts(&run_on(&e2, "notes/x.md")),
        [json!(0), json!(0), json!(0), json!(0)]
    );
}

/// D11: a missing finalize target is `NoteGone`, not a hard failure.
#[test]
fn missing_note_is_note_gone() {
    let e = env(&[("notes/a.md", "x\n".to_owned())]);
    let step = Step::from_table("type = \"intake_finalize\"".parse().unwrap()).unwrap();
    let mut vars = Map::new();
    vars.insert("moved_path".into(), json!("stack/gone.md"));
    let r = e.exec_with(&step, &Map::new(), &ActionContext::default(), &vars);
    assert_eq!(super::testkit::note_gone(r), "stack/gone.md");
}
