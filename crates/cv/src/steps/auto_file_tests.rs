//! `auto_file` tests (plan auto-intake-sprint WP-5): synthetic temp vaults (`*.example` hosts,
//! invented text) through [`StepEnv`]; never the network, never the live vault.

use std::sync::Mutex;

use cv_actions::Step;
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value};

use super::auto_file::host_row;
use crate::steps::testkit::{StepEnv, active, config_of, example_config};
use crate::steps::{ChildSpawner, StepOutcome, execute};

const SOURCES: &str = r#"
[[blog]]
name = "Paid Blog"
link = "https://www.paid.example/feed"
paywall_markers = ["Subscribe to keep reading"]

[[blog]]
name = "Plain Blog"
link = "https://plain.example/feed"

[[blog]]
name = "Letter Blog"
link = "https://letter.example/feed"

[[newsletter]]
name = "Letter"
from = ["@letter.example"]
blog = "Letter Blog"
kind = "news"

[[newsletter]]
name = "Beehive"
short = "Bee"
hosts = ["bee.example"]
kind = "news"

[[serial]]
name = "Odd"
prefix = ["https://saga.example/odd/"]
kind = "not-a-kind"

[[serial]]
name = "The Saga"
prefix = ["https://saga.example/"]

[[serial]]
name = "Ann Posts"
short = "Ann P"
author = "Ann Poster"
prefix = ["https://posts.example/"]
kind = "post"
"#;

/// Records the children a step spawns.
#[derive(Default)]
struct Rec(Mutex<Vec<(String, Map<String, Value>)>>);

impl ChildSpawner for Rec {
    fn spawn_child(&self, id: &str, params: Map<String, Value>) -> Result<String, String> {
        let mut v = self.0.lock().unwrap();
        v.push((id.to_owned(), params));
        Ok(format!("job-{}", v.len()))
    }
}

impl Rec {
    fn kinds(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| format!("{id}:{}", p["kind"].as_str().unwrap_or("")))
            .collect()
    }
}

fn world(files: &[(&str, String)]) -> StepEnv {
    let ex = |n: &str| -> String {
        std::fs::read_to_string(format!(
            "{}/../../config/examples/{n}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    };
    let (actions, kinds, captures, agents, providers) = (
        ex("actions.toml"),
        ex("kinds.toml"),
        ex("captures.toml"),
        ex("agents.toml"),
        ex("providers.toml"),
    );
    let config = config_of(&[
        ("actions.toml", &actions),
        ("kinds.toml", &kinds),
        ("captures.toml", &captures),
        ("agents.toml", &agents),
        ("providers.toml", &providers),
        ("sources.toml", SOURCES),
    ]);
    let action = config.action("clip-auto-file").unwrap().clone();
    let fx = LiveFixture::new(files.iter().map(|(p, t)| (*p, t.clone())));
    StepEnv::new(fx, config, action)
}

fn run(e: &StepEnv, clip: &str, rec: &Rec) -> StepOutcome {
    let step = Step::from_table("type = \"auto_file\"".parse().unwrap()).unwrap();
    let mut ctx = e.ctx();
    let context = active(clip);
    ctx.context = &context;
    ctx.children = rec;
    execute(&step, &mut ctx).unwrap()
}

fn words(prefix: &str, from: usize, n: usize) -> String {
    (from..from + n)
        .map(|i| format!("{prefix}{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn doc(fm: &str, body: &str) -> String {
    format!("---\n{fm}---\n\n{body}\n")
}

fn var<'a>(o: &'a StepOutcome, k: &str) -> Option<&'a str> {
    o.vars.get(k).and_then(Value::as_str)
}

fn trashed(e: &StepEnv) -> bool {
    e.vault().join(".trash").exists()
}

fn row(e: &StepEnv, key: &str, path: &str, url: &str) {
    e.state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: key.into(),
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
            access: None,
            complete: Some(false),
            full_word_count: None,
        })
        .unwrap();
}

#[test]
fn sole_clip_is_new_and_never_self_matched() {
    let clip = doc("source: https://solo.example/a\n", &words("s", 0, 40));
    let e = world(&[("Clippings/Solo.md", clip)]);
    let rec = Rec::default();
    let o = run(&e, "Clippings/Solo.md", &rec);
    assert_eq!(var(&o, "action"), Some("new"));
    assert!(var(&o, "target").is_none() && var(&o, "kind").is_none());
    assert!(e.read("Clippings/Solo.md").is_some());
    assert!(rec.kinds().is_empty());
}

#[test]
fn contained_clip_of_an_indexed_note_is_trashed_and_the_target_untouched() {
    let body = words("c", 0, 60);
    let orig = doc("source: https://dup.example/post\n", &body);
    let clip = doc("source: https://dup.example/post?utm_source=feed\n", &body);
    let e = world(&[("Notes/Orig.md", orig.clone()), ("Clippings/Dup.md", clip)]);
    let rec = Rec::default();
    let o = run(&e, "Clippings/Dup.md", &rec);
    assert_eq!(var(&o, "action"), Some("merged"));
    assert_eq!(var(&o, "target"), Some("Notes/Orig.md"));
    assert!(e.read("Clippings/Dup.md").is_none());
    assert!(trashed(&e));
    assert_eq!(e.read("Notes/Orig.md").unwrap(), orig);
    assert!(rec.kinds().is_empty());
}

#[test]
fn fuller_clip_replaces_a_stub_and_ticks_only_when_complete() {
    for (marker, ticked) in [(false, true), (true, false)] {
        let stub = doc(
            "source: https://www.paid.example/p/deep\ncomplete: false\n",
            &words("p", 0, 30),
        );
        let mut full = words("p", 0, 120);
        if marker {
            full.push_str(" Subscribe to keep reading");
        }
        let clip = doc("source: https://www.paid.example/p/deep?ref=x\n", &full);
        let e = world(&[("Notes/Stub.md", stub), ("Clippings/Full.md", clip)]);
        row(
            &e,
            "https://www.paid.example/feed",
            "Notes/Stub.md",
            "https://www.paid.example/p/deep",
        );
        let rec = Rec::default();
        let o = run(&e, "Clippings/Full.md", &rec);
        assert_eq!(var(&o, "action"), Some("replaced"), "marker={marker}");
        let t = e.read("Notes/Stub.md").unwrap();
        assert!(t.contains("p119") && t.contains("complete:"), "{t}");
        assert!(e.read("Clippings/Full.md").is_none());
        assert!(rec.kinds().is_empty(), "replaced files nothing");
        let db = e.state.lock().unwrap();
        let st = db
            .tracker_item_status(
                "blog",
                "https://www.paid.example/feed",
                "https://www.paid.example/p/deep",
            )
            .unwrap()
            .flatten();
        assert_eq!(st == Some('x'), ticked, "marker={marker}");
    }
}

#[test]
fn identity_match_without_containment_keeps_the_clip_with_duplicate_of() {
    let orig = doc("source: https://other.example/p\n", &words("a", 0, 50));
    let clip = doc("source: https://other.example/p#frag\n", &words("z", 0, 50));
    let e = world(&[("Notes/Orig.md", orig), ("Clippings/Other.md", clip)]);
    let rec = Rec::default();
    let o = run(&e, "Clippings/Other.md", &rec);
    assert_eq!(var(&o, "action"), Some("duplicate"));
    let t = e.read("Clippings/Other.md").expect("kept");
    assert!(t.contains("duplicate-of: \"[[Notes/Orig]]\""), "{t}");
    assert!(rec.kinds().is_empty(), "no kind matches other.example");
}

/// Round 11 Decision 4: `[[serial]]` rows (config order) file a matching clip as their `kind`
/// with their publication (any kind); a row whose kind is undefined is skipped and matching
/// continues; a clip matching no row files by the later rules or not at all.
#[test]
fn serial_rows_file_the_clip_with_their_kind_and_publication() {
    type Case = (
        &'static str,
        &'static str,
        Option<(&'static str, &'static str)>,
    );
    let cases: [Case; 5] = [
        (
            "https://saga.example/ch1",
            "Ann",
            Some(("fanfic", "The Saga")),
        ),
        // The undefined-kind row matches first and is skipped; The Saga still matches.
        (
            "https://saga.example/odd/1",
            "Ann",
            Some(("fanfic", "The Saga")),
        ),
        (
            "https://posts.example/a",
            "\"[[Ann Poster|AP]]\"",
            Some(("post", "Ann P")),
        ),
        ("https://posts.example/a", "Bob", None),
        ("https://nowhere.example/a", "\"[[Ann Poster]]\"", None),
    ];
    for (url, author, want) in cases {
        let clip = doc(
            &format!("source: {url}\nauthor:\n  - {author}\n"),
            &words("k", 0, 30),
        );
        let e = world(&[("Clippings/Ch1.md", clip)]);
        let rec = Rec::default();
        let o = run(&e, "Clippings/Ch1.md", &rec);
        let t = e.read("Clippings/Ch1.md").expect("never moved by the step");
        match want {
            Some((kind, publication)) => {
                assert_eq!(rec.kinds(), [format!("file-as-kind:{kind}")], "{url}");
                assert_eq!(var(&o, "kind"), Some(kind), "{url}");
                assert!(t.contains(&format!("publication: {publication}\n")), "{t}");
            }
            None => {
                assert!(rec.kinds().is_empty(), "{url} {author}");
                assert!(!t.contains("publication"), "{t}");
            }
        }
    }
}

/// Round 11 Decision 7 (step 2b): a `[[newsletter]]` row's `hosts` files a clip as that row's
/// kind with its publication, with no `[[blog]]` row.
#[test]
fn newsletter_hosts_file_the_clip_as_that_rows_kind() {
    let clip = doc("source: https://www.bee.example/p/1\n", &words("b", 0, 30));
    let e = world(&[("Clippings/B.md", clip)]);
    let rec = Rec::default();
    run(&e, "Clippings/B.md", &rec);
    assert_eq!(rec.kinds(), ["file-as-kind:news"]);
    let t = e.read("Clippings/B.md").unwrap();
    assert!(t.contains("publication: Bee\n"), "{t}");
}

#[test]
fn blog_newsletter_and_hosts_rules_pick_the_kind() {
    let cases = [
        ("https://plain.example/p/1", Some("file-as-kind:blog")),
        ("https://letter.example/p/1", Some("file-as-kind:news")),
        (
            "https://archiveofourown.org/works/1",
            Some("file-as-kind:fanfic"),
        ),
        (
            "https://www.fanfiction.net/s/1",
            Some("file-as-kind:fanfic"),
        ),
        ("https://nowhere.example/x", None),
    ];
    for (url, want) in cases {
        let clip = doc(&format!("source: {url}\n"), &words("h", 0, 30));
        let e = world(&[("Clippings/H.md", clip)]);
        let rec = Rec::default();
        run(&e, "Clippings/H.md", &rec);
        assert_eq!(
            rec.kinds(),
            want.map(str::to_owned).into_iter().collect::<Vec<_>>(),
            "{url}"
        );
        assert!(e.read("Clippings/H.md").is_some());
    }
}

#[test]
fn clip_without_source_or_frontmatter_stays_and_missing_note_succeeds() {
    let e = world(&[("Clippings/Bare.md", "just text\n".to_owned())]);
    let rec = Rec::default();
    assert_eq!(
        var(&run(&e, "Clippings/Bare.md", &rec), "action"),
        Some("new")
    );
    assert_eq!(
        var(&run(&e, "Clippings/Gone.md", &rec), "action"),
        Some("missing")
    );
    assert!(rec.kinds().is_empty());
}

#[test]
fn host_row_matches_blog_and_newsletter() {
    let c = config_of(&[("sources.toml", SOURCES)]);
    let s = c.sources();
    let r = host_row("letter.example", s).unwrap();
    assert_eq!(r.kind.as_deref(), Some("news"));
    assert_eq!(
        host_row("paid.example", s).unwrap().markers,
        ["Subscribe to keep reading"]
    );
    assert!(host_row("nope.example", s).is_none());
}

#[test]
fn e1_clip_auto_file_has_no_event_starter_and_runs_last_in_clip_images() {
    use cv_actions::{Starter, StepKind};
    let c = example_config();
    let auto = c.armed("clip-auto-file").expect("armed");
    assert!(
        auto.starters
            .iter()
            .all(|s| !matches!(s, Starter::Event { .. })),
        "an independent event starter could run before localize"
    );
    assert_eq!(auto.steps.len(), 1);
    assert_eq!(auto.steps[0].kind, StepKind::AutoFile);
    let images = c.armed("clip-images").expect("armed");
    let kinds: Vec<StepKind> = images.steps.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        [StepKind::Localize, StepKind::RunAction, StepKind::RunAction]
    );
    let last = images.steps.last().unwrap().run_action().unwrap();
    assert_eq!(last.action_id, "clip-auto-file");
    assert!(
        StepKind::AutoFile
            .intrinsic_resource_keys()
            .contains(&cv_actions::INTAKE_IDENTITY_KEY)
    );
}

#[test]
fn kinds_hosts_key_parses_and_rejects_non_strings() {
    let c = example_config();
    let fanfic = c.table("kinds.toml")["kinds"]["fanfic"].as_table().unwrap();
    let hosts = crate::steps::params::get_str_list(fanfic, "hosts").unwrap();
    assert!(hosts.contains(&"www.fanfiction.net".to_owned()));
    let step = Step::from_table("type = \"move\"\nkind = \"x\"".parse().unwrap()).unwrap();
    let bad = config_of(&[(
        "kinds.toml",
        "[kinds.x]\ntags = [\"x\"]\nfolder = \"f\"\nhosts = [\"a.example\"]\n",
    )]);
    assert!(crate::steps::validate(&step, &bad).is_ok());
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "kinds.toml".to_owned(),
        Some("[kinds.x]\nhosts = 5\n".to_owned()),
    );
    let mut c2 = cv_actions::ConfigSet::new();
    c2.reload(&files, &crate::steps::validate);
    assert!(crate::steps::validate(&step, &c2).is_err());
}

fn day_clip_path(name: &str) -> String {
    format!("daily/day/{}/{name}", crate::steps::testkit::today())
}

/// WP-5b: a clip already in its kind's folder with the kind's tags is not filed again, and a
/// re-run changes nothing (no child, no trash, identical bytes).
#[test]
fn clip_already_in_its_kind_folder_is_a_noop_and_rerun_is_stable() {
    let p = day_clip_path("Ch1.md");
    let clip = doc(
        "source: https://www.fanfiction.net/s/1\ntags:\n  - clippings\n  - fanfic\n",
        &words("k", 0, 30),
    );
    let e = world(&[(p.as_str(), clip.clone())]);
    for _ in 0..2 {
        let rec = Rec::default();
        let o = run(&e, &p, &rec);
        assert!(rec.kinds().is_empty(), "no child");
        assert_eq!(var(&o, "kind"), Some("fanfic"));
        assert_eq!(var(&o, "filed"), Some("already"));
        assert_eq!(var(&o, "action"), Some("new"));
        assert!(var(&o, "child_job_id").is_none());
        assert!(!trashed(&e));
        assert_eq!(e.read(&p).as_deref(), Some(clip.as_str()));
    }
}

/// WP-5b: in the folder but missing a kind tag (or tagged but elsewhere) still files.
#[test]
fn clip_in_folder_without_the_kind_tag_or_tagged_elsewhere_still_files() {
    let p = day_clip_path("Ch2.md");
    let untagged = doc(
        "source: https://www.fanfiction.net/s/2\ntags:\n  - clippings\n",
        &words("m", 0, 30),
    );
    let e = world(&[(p.as_str(), untagged)]);
    let rec = Rec::default();
    let o = run(&e, &p, &rec);
    assert_eq!(rec.kinds(), ["file-as-kind:fanfic"]);
    assert!(var(&o, "filed").is_none());
    let tagged = doc(
        "source: https://www.fanfiction.net/s/3\ntags:\n  - clippings\n  - fanfic\n",
        &words("n", 0, 30),
    );
    let e = world(&[("Clippings/Ch3.md", tagged)]);
    let rec = Rec::default();
    run(&e, "Clippings/Ch3.md", &rec);
    assert_eq!(rec.kinds(), ["file-as-kind:fanfic"]);
}

/// WP-5b: a blog-row match writes `publication` once (never overwrites); a kinds.toml
/// hosts-only match gets none.
#[test]
fn host_row_match_sets_publication_only_when_absent() {
    let body = words("p", 0, 30);
    let clip = doc("source: https://plain.example/p/1\nauthor: Ann\n", &body);
    let e = world(&[("Clippings/A.md", clip)]);
    let rec = Rec::default();
    run(&e, "Clippings/A.md", &rec);
    assert_eq!(rec.kinds(), ["file-as-kind:blog"]);
    let t = e.read("Clippings/A.md").unwrap();
    assert!(t.contains("author: Ann\npublication: Plain Blog\n"), "{t}");
    // Existing value kept; a re-run is byte-stable.
    let have = doc(
        "source: https://plain.example/p/2\npublication: \"Mine\"\n",
        &body,
    );
    let e = world(&[("Clippings/B.md", have.clone())]);
    run(&e, "Clippings/B.md", &Rec::default());
    assert_eq!(e.read("Clippings/B.md").as_deref(), Some(have.as_str()));
    // A kinds.toml hosts-only match: no publication.
    let h = doc("source: https://www.fanfiction.net/s/9\n", &body);
    let e = world(&[("Clippings/D.md", h)]);
    run(&e, "Clippings/D.md", &Rec::default());
    assert!(!e.read("Clippings/D.md").unwrap().contains("publication"));
}

/// Round 9 Decision 2 / round 11 Decision 4: a clip filed by a serial row takes the row's
/// publication; an existing value is never overwritten.
#[test]
fn fanfic_clip_takes_the_matched_serial_publication() {
    let body = words("q", 0, 30);
    let c = doc("source: https://saga.example/ch2\nauthor: Ann\n", &body);
    let e = world(&[("Clippings/F.md", c)]);
    let rec = Rec::default();
    run(&e, "Clippings/F.md", &rec);
    assert_eq!(rec.kinds(), ["file-as-kind:fanfic"]);
    let t = e.read("Clippings/F.md").unwrap();
    assert!(t.contains("author: Ann\npublication: The Saga\n"), "{t}");
    let have = doc(
        "source: https://saga.example/ch3\npublication: \"Mine\"\n",
        &body,
    );
    let e = world(&[("Clippings/G.md", have.clone())]);
    run(&e, "Clippings/G.md", &Rec::default());
    assert_eq!(e.read("Clippings/G.md").as_deref(), Some(have.as_str()));
}

/// WP-5b acceptance: a `clippings`-tagged clip created in a day folder whose source matches a
/// fanfic serial row spawns `file-as-kind { fanfic }`, and that child files it with no move error.
#[test]
fn fanfic_clip_in_a_day_folder_files_without_a_move_error() {
    use serde_json::json;
    let p = day_clip_path("Ch9.md");
    let clip = doc(
        "source: https://saga.example/ch9\ntags:\n  - clippings\n",
        &words("f", 0, 30),
    );
    let e = world(&[(p.as_str(), clip)]);
    let rec = Rec::default();
    run(&e, &p, &rec);
    assert_eq!(rec.kinds(), ["file-as-kind:fanfic"]);
    let (open, vars) = e
        .run_action("file-as-kind", json!({"kind": "fanfic"}), active(&p))
        .expect("child files with no move error");
    assert_eq!(open.as_deref(), Some(p.as_str()));
    assert_eq!(vars["moved_path"], json!(p));
    let t = e.read(&p).unwrap();
    assert!(t.contains("fanfic"), "{t}");
    // Now filed in place: a second auto_file run is the no-op.
    let rec = Rec::default();
    let o = run(&e, &p, &rec);
    assert!(rec.kinds().is_empty());
    assert_eq!(var(&o, "filed"), Some("already"));
}

/// WP-5b: the tag starter lives on `clip-images` next to the folder starter.
#[test]
fn clip_images_has_folder_and_tag_starters() {
    use cv_actions::Starter;
    let c = example_config();
    let images = c.armed("clip-images").expect("armed");
    let sels: Vec<(Option<&str>, Option<&str>)> = images
        .starters
        .iter()
        .filter_map(|s| match s {
            Starter::Event { selector, .. } => {
                Some((selector.folder.as_deref(), selector.tag.as_deref()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(sels, [(Some("Clippings"), None), (None, Some("clippings"))]);
}
