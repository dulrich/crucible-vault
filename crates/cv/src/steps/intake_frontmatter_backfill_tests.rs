//! `intake_frontmatter_backfill` tests (plan sources-publication-model WP-4): synthetic temp
//! vaults with invented notes and a temp `state.db`; never the network, never the live vault.

use cv_actions::{Action, ResourceSpec, Step};
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;

use crate::steps::testkit::{ActionBuilder, StepEnv, config_of};
use crate::steps::{StepOutcome, execute};

const SOURCES: &str = "[[publication]]\nname = \"Pref\"\nshort = \"PrefShort\"\nprefix = [\"https://pref.example/\"]\n\n[[publication]]\nname = \"Blog\"\nshort = \"BlogShort\"\nfeeds = [\"https://feed.example/rss\"]\n";
const CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n";

fn action() -> Action {
    ActionBuilder::new(
        "intake-frontmatter-backfill",
        vec![],
        ResourceSpec::default(),
    )
    .build()
}

fn env(files: &[(&str, &str)]) -> StepEnv {
    StepEnv::new(
        LiveFixture::new(files.iter().map(|(p, t)| (*p, (*t).to_owned()))),
        config_of(&[("sources.toml", SOURCES), ("captures.toml", CAPTURES)]),
        action(),
    )
}

fn run(e: &StepEnv, dry: bool) -> StepOutcome {
    let t: toml::Table = format!("type = \"intake_frontmatter_backfill\"\ndry_run = {dry}")
        .parse()
        .unwrap();
    execute(&Step::from_table(t).unwrap(), &mut e.ctx()).unwrap()
}

fn n(o: &StepOutcome, k: &str) -> u64 {
    o.vars[k].as_u64().unwrap_or_else(|| panic!("var {k}"))
}

fn counts(o: &StepOutcome) -> [u64; 5] {
    [
        "candidates",
        "read_set",
        "publication_set",
        "unchanged",
        "conflicts",
    ]
    .map(|k| n(o, k))
}

fn row(e: &StepEnv, id: &str, path: &str) {
    e.state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: "https://feed.example/rss".into(),
            item_id: id.into(),
            title: "T".into(),
            url: format!("https://feed.example/p/{id}"),
            first_seen_ms: 1,
            day: "2026-10-21".into(),
            outcome: TrackerOutcome::Note,
            score: None,
            full_body: None,
            note_path: Some(path.to_owned()),
            source_name: None,
            duration_s: None,
            word_count: None,
            access: None,
            complete: None,
            full_word_count: None,
        })
        .unwrap();
}

fn fm(extra: &str) -> String {
    format!("---\ntitle: T\nauthor: Someone\n{extra}---\n\nBody.\n")
}

#[test]
fn read_is_set_false_only_where_absent_and_candidates_are_day_folder_intake_notes() {
    let e = env(&[
        ("daily/absent.md", &fm("post-id: a\n")),
        ("daily/mail.md", &fm("message-id: m\n")),
        ("daily/true.md", &fm("post-id: b\nread: true\n")),
        ("daily/false.md", &fm("post-id: c\nread: false\n")),
        ("daily/plain.md", &fm("")),
        ("elsewhere/outside.md", &fm("post-id: d\n")),
    ]);
    let o = run(&e, false);
    assert_eq!(counts(&o), [4, 2, 0, 2, 0]);
    for p in ["daily/absent.md", "daily/mail.md"] {
        assert!(e.read(p).unwrap().contains("read: false\n"), "{p}");
    }
    assert!(e.read("daily/true.md").unwrap().contains("read: true\n"));
    assert_eq!(
        e.read("daily/false.md").unwrap(),
        fm("post-id: c\nread: false\n")
    );
    assert_eq!(e.read("daily/plain.md").unwrap(), fm(""));
    assert_eq!(e.read("elsewhere/outside.md").unwrap(), fm("post-id: d\n"));
}

#[test]
fn publication_comes_from_the_source_then_the_tracker_row_and_is_never_overwritten() {
    let e = env(&[
        (
            "daily/by-source.md",
            &fm("post-id: a\nread: true\nsource: https://pref.example/p/1\n"),
        ),
        (
            "daily/by-row.md",
            &fm("post-id: b\nread: true\nsource: https://elsewhere.example/p/2\n"),
        ),
        (
            "daily/neither.md",
            &fm("post-id: c\nread: true\nsource: https://elsewhere.example/p/3\n"),
        ),
        (
            "daily/blank.md",
            &fm("post-id: d\nread: true\npublication: \"\"\nsource: https://pref.example/p/4\n"),
        ),
        (
            "daily/has.md",
            &fm("post-id: e\nread: true\npublication: Keep\nsource: https://pref.example/p/5\n"),
        ),
    ]);
    row(&e, "b", "daily/by-row.md");
    row(&e, "c2", "daily/has.md");
    let o = run(&e, false);
    assert_eq!(counts(&o), [5, 0, 3, 2, 0]);
    assert!(
        e.read("daily/by-source.md")
            .unwrap()
            .contains("publication: PrefShort\n")
    );
    assert!(
        e.read("daily/by-row.md")
            .unwrap()
            .contains("publication: BlogShort\n")
    );
    assert!(
        e.read("daily/blank.md")
            .unwrap()
            .contains("publication: PrefShort\n")
    );
    assert!(!e.read("daily/neither.md").unwrap().contains("publication"));
    let has = e.read("daily/has.md").unwrap();
    assert!(has.contains("publication: Keep\n") && !has.contains("BlogShort"));
}

#[test]
fn both_edits_share_one_write_dry_run_writes_nothing_and_a_second_run_changes_nothing() {
    let note = fm("post-id: a\nsource: https://pref.example/p/1\n");
    let e = env(&[("daily/a.md", &note)]);
    let dry = run(&e, true);
    assert_eq!(counts(&dry), [1, 1, 1, 0, 0]);
    assert_eq!(dry.vars["dry_run"], true);
    assert_eq!(e.read("daily/a.md").unwrap(), note);
    let real = run(&e, false);
    assert_eq!(counts(&real), [1, 1, 1, 0, 0]);
    let text = e.read("daily/a.md").unwrap();
    assert!(text.contains("read: false\n") && text.contains("publication: PrefShort\n"));
    assert_eq!(counts(&run(&e, false)), [1, 0, 0, 1, 0]);
    assert_eq!(e.read("daily/a.md").unwrap(), text);
}

#[test]
fn a_job_param_dry_run_wins_and_unknown_keys_are_rejected() {
    let mut e = env(&[("daily/a.md", &fm("post-id: a\n"))]);
    e.params.insert("dry_run".into(), serde_json::json!(true));
    let o = run(&e, false);
    assert_eq!(o.vars["dry_run"], true);
    assert!(!e.read("daily/a.md").unwrap().contains("read:"));
    let t: toml::Table = "type = \"intake_frontmatter_backfill\"\nnope = 1"
        .parse()
        .unwrap();
    assert!(crate::steps::validate(&Step::from_table(t).unwrap(), &e.config).is_err());
}
