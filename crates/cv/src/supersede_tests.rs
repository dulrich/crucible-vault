//! Supersede engine tests (plan auto-intake-sprint WP-3): synthetic temp vaults through
//! [`StepEnv`] (never the live vault, never the network).

use cv_actions::pipelines::SupersedeConfig;
use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec};
use cv_ingest::access::Access;
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;

use super::{Action as Verdict, Incoming, Origin, SupersedeIndex, resolve};
use crate::steps::testkit::StepEnv;

fn action() -> Action {
    Action {
        id: "intake".into(),
        label: "Intake".into(),
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
    StepEnv::new(
        LiveFixture::new(files.iter().map(|(p, t)| (*p, t.clone()))),
        ConfigSet::new(),
        action(),
    )
}

/// `n` distinct words starting at `w{from}` (prefix-shaped stubs share their shingles).
fn words(prefix: &str, from: usize, n: usize) -> String {
    (from..from + n)
        .map(|i| format!("{prefix}{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn note(source: &str, extra: &str, body: &str) -> String {
    format!(
        "---\ntitle: T\nsource: {source}\nauthor:\n  - \"[[Ann]]\"\npublished: 2026-10-01\n{extra}---\n\n{body}\n"
    )
}

fn incoming<'a>(urls: &[&str], body: &'a str) -> Incoming<'a> {
    Incoming {
        own_path: None,
        urls: urls.iter().map(|u| (*u).to_owned()).collect(),
        source: None,
        authors: vec!["Ann".into()],
        host: None,
        published: chrono::NaiveDate::from_ymd_opt(2026, 10, 3),
        body_md: body,
        access: None,
        complete: None,
        publication: None,
        origin: Origin::Mail,
    }
}

fn run(e: &StepEnv, inc: &Incoming<'_>) -> super::Resolved {
    let mut ctx = e.ctx();
    let idx = SupersedeIndex::build(&ctx).unwrap();
    resolve(&mut ctx, &idx, &SupersedeConfig::default(), inc).unwrap()
}

fn row(e: &StepEnv, path: &str, url: &str) {
    e.state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: "feed".into(),
            item_id: url.into(),
            title: "T".into(),
            url: url.into(),
            first_seen_ms: 1,
            day: "2026-10-01".into(),
            outcome: TrackerOutcome::Note,
            score: None,
            full_body: Some(true),
            note_path: Some(path.into()),
            source_name: None,
            duration_s: None,
            word_count: None,
            access: Some("paid".into()),
            complete: Some(false),
            full_word_count: Some(100),
        })
        .unwrap();
}

fn row_state(e: &StepEnv, url: &str) -> (Option<bool>, Option<char>) {
    let db = e.state.lock().unwrap();
    let r = db.tracker_item("blog", "feed", url).unwrap().unwrap();
    let s = db
        .tracker_item_status("blog", "feed", url)
        .unwrap()
        .unwrap();
    (r.complete, s)
}

fn superseded(e: &StepEnv) -> Vec<String> {
    let dir = e.fx.root().join("superseded");
    std::fs::read_dir(dir)
        .map(|d| {
            d.map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn same_post_with_other_date_and_url_form_resolves_to_one_note() {
    let stub = words("w", 0, 30);
    let e = env(&[(
        "notes/a.md",
        note("https://ex.substack.com/p/slug", "", &stub),
    )]);
    row(&e, "notes/a.md", "https://ex.substack.com/p/slug");
    let mut inc = incoming(&["https://open.substack.com/pub/ex/p/slug?r=1"], &stub);
    inc.published = chrono::NaiveDate::from_ymd_opt(2025, 1, 1);
    let r = run(&e, &inc);
    assert_eq!(
        (r.path.as_deref(), r.action),
        (Some("notes/a.md"), Verdict::Kept)
    );
    let inc = incoming(&["https://ex.substack.com/p/slug/?utm_source=email"], &stub);
    assert_eq!(run(&e, &inc).path.as_deref(), Some("notes/a.md"));
    // The kept note has no `complete` key: the row's known value survives, no tick.
    assert_eq!(
        row_state(&e, "https://ex.substack.com/p/slug"),
        (Some(false), None)
    );
}

#[test]
fn stub_is_replaced_in_place_keeping_user_frontmatter() {
    let (stub, full) = (words("w", 0, 30), words("w", 0, 100));
    let url = "https://ex.substack.com/p/s";
    let e = env(&[(
        "notes/a.md",
        note(
            url,
            "word-count: 30\naccess: paid\ncomplete: false\ntags:\n  - blog\nread: true\nmine: kept\n",
            &stub,
        ),
    )]);
    row(&e, "notes/a.md", url);
    let mut inc = incoming(&[url], &full);
    inc.access = Some(Access::Free);
    inc.complete = Some(true);
    let r = run(&e, &inc);
    assert_eq!(
        (r.path.as_deref(), r.action),
        (Some("notes/a.md"), Verdict::Replaced)
    );
    let t = e.read("notes/a.md").unwrap();
    assert!(
        t.contains("read: true\n") && t.contains("mine: kept\n"),
        "{t}"
    );
    assert!(
        t.contains("word-count: 100\naccess: free\ncomplete: true\nvia: email\n"),
        "{t}"
    );
    assert!(t.contains(&full) && t.contains("title: T\n"), "{t}");
    let saved = superseded(&e);
    assert_eq!(saved.len(), 1);
    assert!(saved[0].contains(&stub) && !saved[0].contains(&full));
    assert_eq!(row_state(&e, url), (Some(true), Some('x')));
}

#[test]
fn complete_existing_is_kept_against_an_incoming_stub() {
    let full = words("w", 0, 100);
    let src = note("https://ex.com/p/a", "complete: true\n", &full);
    let e = env(&[("notes/a.md", src.clone())]);
    let stub = words("w", 0, 30);
    let mut inc = incoming(&["https://ex.com/p/a"], &stub);
    inc.complete = Some(false);
    let r = run(&e, &inc);
    assert_eq!(r.action, Verdict::Kept);
    assert_eq!(r.complete, Some(true));
    assert_eq!(e.read("notes/a.md").unwrap(), src);
    assert!(superseded(&e).is_empty());
}

#[test]
fn unrelated_same_source_post_is_new() {
    let e = env(&[(
        "notes/a.md",
        note("https://ex.com/p/a", "", &words("z", 0, 80)),
    )]);
    let body = words("w", 0, 80);
    let r = run(&e, &incoming(&["https://ex.com/p/b"], &body));
    assert_eq!((r.path, r.action), (None, Verdict::New));
}

#[test]
fn sole_clip_resolves_new_never_itself() {
    let body = words("w", 0, 80);
    let e = env(&[("Clippings/c.md", note("https://ex.com/p/c", "", &body))]);
    let mut inc = incoming(&["https://ex.com/p/c"], &body);
    inc.own_path = Some("Clippings/c.md");
    inc.origin = Origin::Clip;
    let r = run(&e, &inc);
    assert_eq!((r.path, r.action), (None, Verdict::New));
}

#[test]
fn larger_partial_content_match_replaces_but_stays_open() {
    let (stub, larger) = (words("w", 0, 30), words("w", 0, 60));
    let url = "https://ex.com/p/a";
    let e = env(&[("notes/a.md", note(url, "complete: false\n", &stub))]);
    row(&e, "notes/a.md", url);
    // A different URL (no identity): matched by same-host containment.
    let mut inc = incoming(&["https://ex.com/newsletter/a-mail"], &larger);
    inc.complete = Some(false);
    let r = run(&e, &inc);
    assert_eq!(
        (r.path.as_deref(), r.action),
        (Some("notes/a.md"), Verdict::Replaced)
    );
    assert!(e.read("notes/a.md").unwrap().contains(&larger));
    assert_eq!(row_state(&e, url), (Some(false), None));
}

// --- plan intake-round-2-clean-dedupe-reading WP-3: `publication` ---

fn replace_with_publication(extra: &str) -> String {
    let (stub, full) = (words("w", 0, 30), words("w", 0, 100));
    let url = "https://ex.substack.com/p/s";
    let e = env(&[(
        "notes/a.md",
        note(
            url,
            &format!("{extra}word-count: 30\ncomplete: false\n"),
            &stub,
        ),
    )]);
    row(&e, "notes/a.md", url);
    let mut inc = incoming(&[url], &full);
    inc.complete = Some(true);
    inc.publication = Some("Synth Pub");
    let r = run(&e, &inc);
    assert_eq!(r.action, Verdict::Replaced);
    e.read("notes/a.md").unwrap()
}

#[test]
fn replaced_fills_an_absent_publication_after_author() {
    let t = replace_with_publication("");
    assert!(
        t.contains("author:\n  - \"[[Ann]]\"\npublication: Synth Pub\npublished:")
            || t.contains("author:\n  - \"[[Ann]]\"\npublication: \"Synth Pub\"\npublished:"),
        "{t}"
    );
}

#[test]
fn replaced_fills_an_empty_blank_or_null_publication() {
    for empty in [
        "publication: \"\"\n",
        "publication: \"  \"\n",
        "publication:\n",
    ] {
        let t = replace_with_publication(empty);
        assert!(
            t.contains("publication: \"Synth Pub\"") || t.contains("publication: Synth Pub"),
            "{empty}: {t}"
        );
        assert_eq!(t.matches("publication").count(), 1, "{t}");
    }
}

#[test]
fn replaced_keeps_an_existing_publication() {
    let t = replace_with_publication("publication: Old Pub\n");
    assert!(t.contains("publication: Old Pub\n"), "{t}");
    assert!(!t.contains("Synth Pub"), "{t}");
}

/// WP-R1 / D-a: a complete supersede resolve writes the evidence but never overwrites a
/// cancelled `-` row's status.
#[test]
fn cancelled_row_survives_a_complete_resolve() {
    let (stub, full) = (words("w", 0, 30), words("w", 0, 100));
    let url = "https://ex.substack.com/p/c";
    let e = env(&[(
        "notes/a.md",
        note(
            url,
            "word-count: 30\naccess: paid\ncomplete: false\n",
            &stub,
        ),
    )]);
    row(&e, "notes/a.md", url);
    e.state
        .lock()
        .unwrap()
        .set_tracker_item_status("blog", "feed", url, Some('-'))
        .unwrap();
    let mut inc = incoming(&[url], &full);
    inc.access = Some(Access::Free);
    inc.complete = Some(true);
    let r = run(&e, &inc);
    assert_eq!(r.action, Verdict::Replaced);
    assert_eq!(row_state(&e, url), (Some(true), Some('-')));
}
