//! `blog_metadata_backfill` tests (plan `round-12-feedback-todos` WP-8): synthetic temp vaults
//! through the real writer thread.

use std::collections::BTreeMap;

use cv_protocol::TrackerOutcome;
use cv_state::{IntakeQuery, TrackerItem};
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value};

use super::StepOutcome;
use super::blog_metadata_backfill::{Stop, run};
use super::dupes::POSSIBLE_KEY;
use super::testkit::{StepEnv, config_of};

const ACTIONS: &str = "[[action]]\nid = \"blog-metadata-backfill\"\n[[action.steps]]\ntype = \"blog_metadata_backfill\"\n";
const CAPTURES: &str = "[periods.day]\nfolder = \"daily/day\"\nformat = \"YYYY-MM-DD\"\n";
const KINDS: &str =
    "[kinds.blog]\nlabel = \"Blog\"\ntags = [\"blog\"]\nfolder = \"{period.day}\"\n";
const SOURCES: &str = "[[blog]]\nname = \"Known\"\nlink = \"https://known.example/feed\"\n";

fn env(files: &[(&str, String)]) -> StepEnv {
    let config = config_of(&[
        ("actions.toml", ACTIONS),
        ("captures.toml", CAPTURES),
        ("kinds.toml", KINDS),
        ("sources.toml", SOURCES),
    ]);
    let action = config.armed("blog-metadata-backfill").unwrap().clone();
    StepEnv::new(
        LiveFixture::new(files.iter().map(|(p, t)| (*p, t.as_str()))),
        config,
        action,
    )
}

/// Runs the step as the action does; `dry_run` as a job param (`None` = absent).
fn exec(e: &StepEnv, dry_run: Option<bool>) -> StepOutcome {
    let mut t = toml::Table::new();
    t.insert("type".into(), "blog_metadata_backfill".into());
    let step = cv_actions::Step::from_table(t).unwrap();
    let mut p = Map::new();
    if let Some(d) = dry_run {
        p.insert("dry_run".into(), Value::Bool(d));
    }
    e.exec_with(&step, &p, &e.context, &e.vars).unwrap()
}

fn apply(e: &StepEnv) -> StepOutcome {
    exec(e, Some(false))
}

fn n(o: &StepOutcome, k: &str) -> u64 {
    o.vars
        .get(k)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{k} missing: {:?}", o.vars))
}

fn words(seed: &str, k: usize) -> String {
    (0..k)
        .map(|i| format!("{seed}w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn note(source: &str, extra: &str, body: &str) -> String {
    format!("---\ntitle: T\nsource: \"{source}\"\n{extra}---\n{body}\n")
}

fn src(slug: &str) -> String {
    format!("https://blog.example/p/{slug}")
}

fn post_id(e: &StepEnv, slug: &str) -> String {
    cv_text::url_canon::post_id_from_url(&src(slug), &super::dupes::post_id_options(&e.config))
}

fn row(e: &StepEnv, id: &str, url: &str, day: &str, outcome: TrackerOutcome) {
    e.state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: "feed".into(),
            item_id: id.into(),
            title: "T".into(),
            url: url.into(),
            first_seen_ms: 1,
            day: day.into(),
            outcome,
            score: None,
            full_body: None,
            note_path: None,
            source_name: None,
            duration_s: None,
            word_count: None,
            access: None,
            complete: None,
            full_word_count: None,
        })
        .unwrap();
}

/// Every blog row, keyed, without `first_seen_ms` (compared across runs).
fn rows(e: &StepEnv) -> BTreeMap<String, (String, String, Option<String>, String)> {
    let r = cv_state::StateReader::open(&e.fx.root().join("state.db")).unwrap();
    r.tracker_items(&IntakeQuery::default())
        .unwrap()
        .into_iter()
        .map(|t| {
            (
                format!("{}|{}", t.source_key, t.item_id),
                (t.day, t.outcome.as_str().to_owned(), t.note_path, t.title),
            )
        })
        .collect()
}

/// Every vault file and its content (excluding the fixture's own state files).
fn files(e: &StepEnv) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![e.vault().to_path_buf()];
    while let Some(d) = stack.pop() {
        for ent in std::fs::read_dir(&d).unwrap() {
            let p = ent.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                let rel = p
                    .strip_prefix(e.vault())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    out
}

/// The mixed corpus: certain, uncertain (capture shorter), two no-twin forms, no date.
fn corpus() -> Vec<(&'static str, String)> {
    let a = words("a", 60);
    vec![
        (
            "_blog_metadata/a.md",
            note(&src("a"), "published: 2026-01-02\n", &a),
        ),
        (
            "daily/day/2026-01-05/cap-a.md",
            note(&src("a"), "", &format!("{a} {}", words("extra", 10))),
        ),
        (
            "_blog_metadata/b.md",
            note(&src("b"), "published: 2026-01-03\n", &words("b", 60)),
        ),
        (
            "daily/day/2026-01-06/cap-b.md",
            note(&src("b"), "", &words("b", 30)),
        ),
        (
            "_blog_metadata/c.md",
            note(
                &src("c"),
                "published: \"2026-02-03T23:30:00-08:00\"\n",
                &words("c", 40),
            ),
        ),
        (
            "_blog_metadata/d.md",
            note(&src("d"), "published: 2026-03-04\n", &words("d", 40)),
        ),
        ("_blog_metadata/e.md", note(&src("e"), "", &words("e", 40))),
        ("notes/links-c.md", "see [[c]]\n".to_owned()),
    ]
}

#[test]
fn buckets_published_forms_and_created_row() {
    let e = env(&corpus());
    let o = apply(&e);
    assert_eq!(n(&o, "certain"), 1, "{:?}", o.vars);
    assert_eq!(n(&o, "uncertain"), 1);
    assert_eq!(n(&o, "no_twin"), 2);
    assert_eq!(n(&o, "skipped_no_date"), 1);
    assert_eq!(n(&o, "rows_created"), 2);
    // Certain: trashed (recoverable), capture untouched.
    assert!(e.read("_blog_metadata/a.md").is_none());
    assert!(e.vault().join(".trash/_blog_metadata/a.md").exists());
    // Uncertain (the capture is the shorter body): labelled both ways, stays put.
    let cap_b = e.read("daily/day/2026-01-06/cap-b.md").unwrap();
    assert!(cap_b.contains("[[_blog_metadata/b]]"), "{cap_b}");
    let meta_b = cv_note::FrontmatterDoc::new(e.read("_blog_metadata/b.md").unwrap());
    assert_eq!(
        meta_b.get_strings(POSSIBLE_KEY),
        vec!["[[daily/day/2026-01-06/cap-b]]"]
    );
    // No twin: ISO date part (no timezone shift) and plain date; tagged blog, trim-label false.
    let c = e
        .read("daily/day/2026-02-03/c.md")
        .expect("moved by ISO date");
    assert!(
        c.contains("- blog") && c.contains("trim-label: false"),
        "{c}"
    );
    assert!(e.read("daily/day/2026-03-04/d.md").is_some());
    // No date: untouched.
    assert!(e.read("_blog_metadata/e.md").is_some());
    let r = rows(&e);
    let id_c = post_id(&e, "c");
    let (day, outcome, path, _) = &r[&format!("backfill:blog.example|{id_c}")];
    assert_eq!(
        (day.as_str(), outcome.as_str(), path.as_deref()),
        ("2026-02-03", "note", Some("daily/day/2026-02-03/c.md"))
    );
}

#[test]
fn skipped_row_on_the_backfill_day_becomes_a_visible_item_on_published() {
    let e = env(&corpus());
    let id_a = post_id(&e, "a");
    row(&e, &id_a, &src("a"), "2026-10-16", TrackerOutcome::Skipped);
    row(
        &e,
        "other-id",
        &src("d"),
        "2026-10-16",
        TrackerOutcome::Skipped,
    );
    let o = apply(&e);
    assert_eq!(n(&o, "rows_visible"), 2, "{:?}", o.vars);
    assert_eq!(n(&o, "rows_created"), 1, "only c gets a created row");
    let reader = cv_state::StateReader::open(&e.fx.root().join("state.db")).unwrap();
    let day_list = |day: &str| {
        reader
            .tracker_items(&IntakeQuery {
                day: Some(day.into()),
                outcome: Some(TrackerOutcome::Note),
                ..IntakeQuery::default()
            })
            .unwrap()
    };
    let a = day_list("2026-01-02");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].item_id, id_a);
    assert_eq!(
        a[0].note_path.as_deref(),
        Some("daily/day/2026-01-05/cap-a.md")
    );
    let d = day_list("2026-03-04");
    assert_eq!(d.len(), 1, "matched by url");
    assert_eq!(d[0].note_path.as_deref(), Some("daily/day/2026-03-04/d.md"));
    assert!(day_list("2026-10-16").is_empty());
    assert!(
        reader
            .tracker_items(&IntakeQuery {
                day: Some("2026-10-16".into()),
                ..IntakeQuery::default()
            })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stale_post_ids_are_rekeyed_on_metadata_and_twins() {
    let a = words("s", 60);
    let e = env(&[
        (
            "_blog_metadata/s.md",
            note(&src("s"), "post-id: stale-1\npublished: 2026-01-02\n", &a),
        ),
        (
            "daily/day/2026-01-05/cap-s.md",
            note(&src("s"), "post-id: stale-2\n", &words("t", 20)),
        ),
    ]);
    let o = apply(&e);
    assert_eq!(n(&o, "rekeyed_notes"), 2, "{:?}", o.vars);
    assert_eq!(n(&o, "uncertain"), 1);
    let id = post_id(&e, "s");
    for p in ["_blog_metadata/s.md", "daily/day/2026-01-05/cap-s.md"] {
        let d = cv_note::FrontmatterDoc::new(e.read(p).unwrap());
        assert_eq!(d.get_str("post-id").as_deref(), Some(id.as_str()), "{p}");
    }
}

#[test]
fn inbound_guard_ignores_label_links_but_not_body_links() {
    let body = words("g", 60);
    let pair = |extra: (&'static str, String)| {
        env(&[
            (
                "_blog_metadata/g.md",
                note(&src("g"), "published: 2026-01-02\n", &body),
            ),
            ("daily/day/2026-01-05/cap-g.md", note(&src("g"), "", &body)),
            extra,
        ])
    };
    let label_only = pair((
        "notes/x.md",
        format!("---\n{POSSIBLE_KEY}:\n  - \"[[_blog_metadata/g]]\"\n---\nx\n"),
    ));
    let o = apply(&label_only);
    assert_eq!(n(&o, "certain"), 1, "label entries are not user links");
    let body_link = pair(("notes/x.md", "see [[_blog_metadata/g]]\n".to_owned()));
    let o = apply(&body_link);
    assert_eq!(n(&o, "certain"), 0);
    assert_eq!(n(&o, "uncertain"), 1);
    assert_eq!(n(&o, "inbound_guarded"), 1);
    assert!(body_link.read("_blog_metadata/g.md").is_some());
}

#[test]
fn dry_run_is_the_default_and_writes_nothing() {
    let e = env(&corpus());
    row(
        &e,
        &post_id(&e, "a"),
        &src("a"),
        "2026-10-16",
        TrackerOutcome::Skipped,
    );
    let (before_files, before_rows) = (files(&e), rows(&e));
    for dry in [None, Some(true)] {
        let o = exec(&e, dry);
        assert_eq!(o.vars["dry_run"], Value::Bool(true));
        assert_eq!(n(&o, "certain"), 1, "{:?}", o.vars);
        assert_eq!(n(&o, "uncertain"), 1);
        assert_eq!(n(&o, "no_twin"), 2);
        assert_eq!(n(&o, "rows_created"), 2);
        assert_eq!(n(&o, "rows_visible"), 1);
        assert_eq!(files(&e), before_files);
        assert_eq!(rows(&e), before_rows);
    }
}

#[test]
fn interrupted_apply_reruns_to_the_same_state_and_a_second_apply_is_a_no_op() {
    let mk = || {
        let mut c = corpus();
        c.push((
            "_blog_metadata/k.md",
            note(
                &src("k"),
                "post-id: stale\npublished: 2026-04-01\n",
                &words("k", 40),
            ),
        ));
        let e = env(&c);
        row(
            &e,
            &post_id(&e, "a"),
            &src("a"),
            "2026-10-16",
            TrackerOutcome::Skipped,
        );
        e
    };
    let full = mk();
    apply(&full);
    // Interrupted after phase 1 (the re-key), then rerun.
    let partial = mk();
    run(false, Stop::AfterRekey, &mut partial.ctx()).unwrap();
    apply(&partial);
    assert_eq!(files(&full), files(&partial));
    assert_eq!(rows(&full), rows(&partial));
    // A second full apply changes nothing.
    let (f, r) = (files(&full), rows(&full));
    let o = apply(&full);
    for k in [
        "certain",
        "no_twin",
        "rows_created",
        "rows_updated",
        "rekeyed_notes",
        "labels_added",
    ] {
        assert_eq!(n(&o, k), 0, "{k}: {:?}", o.vars);
    }
    assert_eq!(files(&full), f);
    assert_eq!(rows(&full), r);
}

/// Scratch-copy measurement driver (never the live vault): `CV_BACKFILL_SCRATCH=/tmp/r12-wp8`
/// holding `vault/`, `config/` and `state.db`; runs dry, apply, apply and prints the vars.
#[test]
#[ignore = "manual measurement on a scratch copy"]
fn scratch_dry_apply_apply() {
    let root = std::path::PathBuf::from(std::env::var("CV_BACKFILL_SCRATCH").unwrap());
    assert!(root.starts_with("/tmp/r12-wp8"), "scratch only");
    let vault = root.join("vault");
    let index_db = root.join("index").join("index.db");
    std::fs::create_dir_all(root.join("index")).unwrap();
    let mut cfg = BTreeMap::new();
    for ent in std::fs::read_dir(root.join("config")).unwrap() {
        let p = ent.unwrap().path();
        if p.extension().is_some_and(|x| x == "toml") {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            cfg.insert(name, std::fs::read_to_string(&p).ok());
        }
    }
    let mut config = cv_actions::ConfigSet::new();
    config.reload(&cfg, &crate::steps::validate);
    let fs = cv_vault::VaultFs::live(&vault).unwrap();
    let w = cv_vault::VaultWriter::open(fs, &index_db).unwrap();
    let (writer, _join) = cv_vault::spawn_writer(w).unwrap();
    let state = std::sync::Arc::new(std::sync::Mutex::new(
        cv_state::StateDb::open(&root.join("state.db"), 1, cv_state::QueueConfig::default())
            .unwrap(),
    ));
    let action = config_of(&[("actions.toml", ACTIONS)])
        .armed("blog-metadata-backfill")
        .unwrap()
        .clone();
    let (params, vars) = (Map::new(), Map::new());
    let context = cv_protocol::ActionContext::default();
    let skipped_1016 = || {
        let r = cv_state::StateReader::open(&root.join("state.db")).unwrap();
        r.tracker_items(&IntakeQuery {
            source_kind: Some("blog".into()),
            day: Some("2026-10-16".into()),
            outcome: Some(TrackerOutcome::Skipped),
            ..IntakeQuery::default()
        })
        .unwrap()
        .len()
    };
    eprintln!("skipped blog rows on 2026-10-16 before: {}", skipped_1016());
    for dry in [true, false, false] {
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
            state: &state,
            writer: Some(writer.clone()),
            children: &super::testkit::NoChildren,
            written: Vec::new(),
        };
        let t = std::time::Instant::now();
        let o = run(dry, Stop::Full, &mut ctx).unwrap();
        eprintln!("dry={dry} wall {:?}", t.elapsed());
        eprintln!("{}", serde_json::to_string(&o.vars).unwrap());
    }
    eprintln!("skipped blog rows on 2026-10-16 after: {}", skipped_1016());
}
