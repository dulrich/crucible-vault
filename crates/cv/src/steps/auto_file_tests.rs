//! `auto_file` tests (plan auto-intake-sprint WP-5): synthetic temp vaults (`*.example` hosts,
//! invented text) through [`StepEnv`]; never the network, never the live vault.

use std::sync::Mutex;

use cv_actions::Step;
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::{Map, Value};

use super::auto_file::{Clip, Work, host_row, works_kind, works_match};
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

fn work(author: Option<&str>, prefixes: &[&str], tags: &[&str]) -> String {
    let mut fm = String::new();
    if let Some(a) = author {
        fm.push_str(&format!("author-filter: \"{a}\"\n"));
    }
    if !prefixes.is_empty() {
        fm.push_str(&format!(
            "source-prefix-filter: {}\n",
            serde_json::to_string(prefixes).unwrap()
        ));
    }
    fm.push_str(&format!("tags: {}\n", serde_json::to_string(tags).unwrap()));
    doc(&fm, "A work.")
}

#[test]
fn works_match_files_the_clip_under_the_works_kind() {
    let cases: [(&str, String, bool); 4] = [
        (
            "author and prefix",
            work(
                Some("[[Ann Writer]]"),
                &["https://saga.example/"],
                &["fanfic"],
            ),
            true,
        ),
        (
            "prefix only",
            work(None, &["https://saga.example/"], &["#fanfic"]),
            true,
        ),
        (
            "author mismatch",
            work(Some("[[Bob]]"), &["https://saga.example/"], &["fanfic"]),
            false,
        ),
        ("no filters", work(None, &[], &["fanfic"]), false),
    ];
    for (name, w, hit) in cases {
        let clip = doc(
            "source: https://saga.example/ch1\nauthor:\n  - \"[[Ann Writer]]\"\n",
            &words("k", 0, 30),
        );
        let e = world(&[("core/works/Saga.md", w), ("Clippings/Ch1.md", clip)]);
        let rec = Rec::default();
        let o = run(&e, "Clippings/Ch1.md", &rec);
        if hit {
            assert_eq!(rec.kinds(), ["file-as-kind:fanfic"], "{name}");
            assert_eq!(var(&o, "kind"), Some("fanfic"), "{name}");
            assert_eq!(var(&o, "child_job_id"), Some("job-1"), "{name}");
        } else {
            assert!(rec.kinds().is_empty(), "{name}");
        }
        assert!(
            e.read("Clippings/Ch1.md").is_some(),
            "{name}: never moved by the step"
        );
    }
}

#[test]
fn works_pure_rules() {
    let clip = Clip {
        source: "https://saga.example/ch1".into(),
        authors: vec![" [[Ann]] ".into(), "#tagged".into()],
        host: Some("saga.example".into()),
    };
    let w = |a: Option<&str>, p: &[&str], t: &[&str]| Work {
        path: "core/works/W.md".into(),
        author_filter: a.map(str::to_owned),
        source_prefixes: p.iter().map(|s| (*s).to_owned()).collect(),
        tags: t.iter().map(|s| (*s).to_owned()).collect(),
    };
    assert!(works_match(&w(Some("[[Ann]]"), &[], &[]), &clip));
    assert!(
        works_match(&w(Some("tagged"), &[], &[]), &clip),
        "# removed"
    );
    assert!(!works_match(
        &w(Some("[[Ann]]"), &["https://elsewhere.example"], &[]),
        &clip
    ));
    assert!(works_match(
        &w(None, &["", " https://saga.example "], &[]),
        &clip
    ));
    assert!(
        !works_match(&w(Some("  "), &["  "], &[]), &clip),
        "empty filters match nothing"
    );
    // The first matching work with a kind tag wins, in path order.
    let mut a = w(None, &["https://saga.example"], &["notakind"]);
    a.path = "core/works/A.md".into();
    let mut b = w(None, &["https://saga.example"], &["fanfic"]);
    b.path = "core/works/B.md".into();
    assert_eq!(
        works_kind(&[b, a], &clip, |k| k == "fanfic").as_deref(),
        Some("fanfic")
    );
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
