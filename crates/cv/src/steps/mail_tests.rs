//! `track_mail` tests (plan auto-intake-sprint WP-4): synthetic `.eml` files (invented senders
//! and text, CRLF) in a temp Maildir beside a synthetic temp vault; never the network, never the
//! live vault.

use std::path::{Path, PathBuf};

use cv_actions::{Action, LaneChoice, ResourceSpec, Step};
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::{Value, json};

use crate::steps::mail::KIND;
use crate::steps::testkit::{StepEnv, config_of};
use crate::steps::{StepOutcome, execute};

const FEED: &str = "https://paid.example/feed";
const STUB_URL: &str = "https://paid.example/p/deep-dive";
const SOURCES: &str = r#"
[[blog]]
name = "Paid Blog"
link = "https://paid.example/feed"

[[newsletter]]
name = "Paid Letter"
from = ["@paid.example"]
blog = "Paid Blog"
tags = ["letters"]
paywall_markers = ["Subscribe to keep reading", "Upgrade to paid"]

[[newsletter]]
name = "Plain Letter"
from = ["hello@plain.example"]
"#;
const KINDS: &str = "[kinds.blog]\nlabel = \"Blog\"\ntags = [\"blog\"]\nfolder = \"Letters\"\n";
const CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n";

struct World {
    env: StepEnv,
    mail: tempfile::TempDir,
}

impl World {
    fn root(&self) -> &Path {
        self.mail.path()
    }
    fn put(&self, rel: &str, bytes: &[u8]) {
        let p = self.root().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    fn exists(&self, rel: &str) -> bool {
        self.root().join(rel).exists()
    }
    fn run(&self) -> StepOutcome {
        let t: toml::Table = "type = \"track_mail\"".parse().unwrap();
        execute(&Step::from_table(t).unwrap(), &mut self.env.ctx()).unwrap()
    }
    fn row(&self, key: &str, mid: &str) -> Option<(TrackerItem, Option<char>)> {
        let db = self.env.state.lock().unwrap();
        db.tracker_item(KIND, key, mid).unwrap().map(|r| {
            let s = db.tracker_item_status(KIND, key, mid).unwrap().flatten();
            (r, s)
        })
    }
    fn notes(&self, folder: &str) -> Vec<PathBuf> {
        std::fs::read_dir(self.env.vault().join(folder))
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }
}

fn world(files: &[(&str, String)]) -> World {
    world_with(SOURCES, files)
}

fn world_with(sources: &str, files: &[(&str, String)]) -> World {
    let mail = tempfile::tempdir().unwrap();
    let pipelines = format!(
        "[mail]\nenabled = true\nmaildir = {:?}\n",
        mail.path().to_str().unwrap()
    );
    let config = config_of(&[
        ("pipelines.toml", &pipelines),
        ("sources.toml", sources),
        ("kinds.toml", KINDS),
        ("captures.toml", CAPTURES),
    ]);
    let action = Action {
        id: "track-mail".into(),
        label: "Track mail".into(),
        group: None,
        keys: vec![],
        params: vec![],
        needs: vec![],
        lane: LaneChoice::Auto,
        durable: true,
        hold: false,
        fast: false,
        steps: vec![],
        starters: vec![],
        resources: ResourceSpec::default(),
    };
    let fx = LiveFixture::new(files.iter().map(|(p, t)| (*p, t.clone())));
    World {
        env: StepEnv::new(fx, config, action),
        mail,
    }
}

/// A CRLF `text/html` message.
fn eml(mid: &str, from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Sender <{from}>\nSubject: {subject}\nDate: Tue, 14 Oct 2026 10:00:00 +0000\nMessage-ID: <{mid}>\nMIME-Version: 1.0\nContent-Type: text/html; charset=utf-8\n\n{body}\n"
    )
    .replace('\n', "\r\n")
    .into_bytes()
}

fn words(prefix: &str, from: usize, n: usize) -> String {
    (from..from + n)
        .map(|i| format!("{prefix}{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn n(o: &StepOutcome, k: &str) -> Value {
    o.vars[k].clone()
}

#[test]
fn unknown_sender_is_captured_and_left_open() {
    let w = world(&[]);
    w.put(
        "new/1.eml",
        &eml(
            "u1@x.example",
            "who@stranger.example",
            "Odd letter",
            "<p>alpha beta gamma</p>",
        ),
    );
    let o = w.run();
    assert_eq!(
        (n(&o, "scanned"), n(&o, "new"), n(&o, "invalid")),
        (json!(1), json!(1), json!(0))
    );
    let (r, status) = w.row("unknown-sender", "u1@x.example").unwrap();
    assert_eq!(r.outcome, TrackerOutcome::Note);
    assert_eq!(r.complete, Some(true));
    assert_eq!(status, None, "an unknown sender stays open");
    let path = r.note_path.unwrap();
    let text = w.env.read(&path).unwrap();
    assert!(
        text.contains("via: email\n") && text.contains("message-id: \"u1@x.example\""),
        "{text}"
    );
    assert!(
        !text.contains("publication:"),
        "an unknown sender has no publication: {text}"
    );
    assert!(text.contains("alpha beta gamma"), "{text}");
}

#[test]
fn duplicate_message_id_gives_one_note_and_files_move_on() {
    let w = world(&[]);
    let m = eml(
        "d1@paid.example",
        "news@paid.example",
        "Weekly",
        "<p>one two three four</p>",
    );
    w.put("new/a", &m);
    w.put("loose.eml", &m);
    w.put("keep.txt", b"not mail");
    w.put("new/bad", b"just some bytes without headers\n");
    let o = w.run();
    assert_eq!(
        (
            n(&o, "scanned"),
            n(&o, "new"),
            n(&o, "invalid"),
            n(&o, "move_failed")
        ),
        (json!(3), json!(1), json!(1), json!(0))
    );
    assert!(
        w.exists("new/bad"),
        "an unparseable message is left in place"
    );
    assert_eq!(w.notes("Letters").len(), 1);
    let (r, status) = w.row("Paid Letter", "d1@paid.example").unwrap();
    assert_eq!(
        status,
        Some('x'),
        "a complete email from a known sender is done"
    );
    assert_eq!(r.source_name.as_deref(), Some("Paid Letter"));
    let text = w.env.read(&r.note_path.unwrap()).unwrap();
    assert!(text.contains("  - blog\n  - letters\n"), "{text}");
    assert!(!w.exists("new/a") && w.exists("cur/a:2,S"));
    assert!(!w.exists("loose.eml") && w.exists("processed/loose.eml"));
    assert!(w.exists("keep.txt"), "non-.eml loose files are not touched");
}

#[test]
fn rerun_on_the_same_mail_is_a_noop() {
    let w = world(&[]);
    let m = eml(
        "r1@plain.example",
        "hello@plain.example",
        "Again",
        "<p>same words here</p>",
    );
    w.put("new/a", &m);
    w.run();
    let before = w.notes("Letters");
    w.put("new/a", &m); // the fetcher re-delivers
    let o = w.run();
    assert_eq!(
        (n(&o, "new"), n(&o, "kept"), n(&o, "replaced")),
        (json!(0), json!(0), json!(0))
    );
    assert_eq!(w.notes("Letters"), before);
    assert!(!w.exists("new/a"), "the re-delivery is moved on");
}

/// An RSS-captured paid stub, its tracker row and the matching blog evidence.
fn stub_world() -> World {
    let stub = format!(
        "---\ntitle: \"Deep dive\"\nsource: {STUB_URL}\npost-id: \"deep-dive\"\nauthor:\n  - \"[[Sender]]\"\npublished: 2026-10-14\nword-count: 30\naccess: paid\ncomplete: false\ntags:\n  - blog\nread: true\n---\n\n{}\n",
        words("w", 0, 30)
    );
    let w = world(&[("Letters/Deep dive.md", stub)]);
    w.env
        .state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: FEED.into(),
            item_id: STUB_URL.into(),
            title: "Deep dive".into(),
            url: STUB_URL.into(),
            first_seen_ms: 1,
            day: "2026-10-14".into(),
            outcome: TrackerOutcome::Note,
            score: None,
            full_body: Some(true),
            note_path: Some("Letters/Deep dive.md".into()),
            source_name: Some("Paid Blog".into()),
            duration_s: None,
            word_count: Some(30),
            access: Some("paid".into()),
            complete: Some(false),
            full_word_count: Some(120),
        })
        .unwrap();
    w
}

fn blog_row(w: &World) -> (TrackerItem, Option<char>) {
    let db = w.env.state.lock().unwrap();
    let r = db.tracker_item("blog", FEED, STUB_URL).unwrap().unwrap();
    let s = db
        .tracker_item_status("blog", FEED, STUB_URL)
        .unwrap()
        .flatten();
    (r, s)
}

#[test]
fn full_subscriber_email_replaces_the_stub_and_ticks_it() {
    let w = stub_world();
    let body = format!(
        "<p>{}</p><a href=\"{STUB_URL}\">Read online</a>",
        words("w", 0, 120)
    );
    w.put(
        "new/full",
        &eml("f1@paid.example", "news@paid.example", "Deep dive", &body),
    );
    let o = w.run();
    assert_eq!(
        (n(&o, "replaced"), n(&o, "new"), n(&o, "kept")),
        (json!(1), json!(0), json!(0))
    );
    let text = w.env.read("Letters/Deep dive.md").unwrap();
    assert!(
        text.contains("w119") && text.contains("read: true\n"),
        "{text}"
    );
    assert!(
        text.contains("complete: true\n") && text.contains("via: email\n"),
        "{text}"
    );
    let (r, status) = blog_row(&w);
    assert_eq!(
        (r.access.as_deref(), r.complete, status),
        (Some("paid"), Some(true), Some('x'))
    );
    let (mail, mstatus) = w.row("Paid Letter", "f1@paid.example").unwrap();
    assert_eq!(mail.outcome, TrackerOutcome::Skipped);
    assert_eq!(mail.note_path.as_deref(), Some("Letters/Deep dive.md"));
    assert_eq!(mstatus, None);
    assert_eq!(w.notes("Letters").len(), 1, "no second note");
}

#[test]
fn truncated_email_does_not_replace_or_tick_the_stub() {
    let w = stub_world();
    let body = format!(
        "<p>{}</p><p>Subscribe to keep reading</p><a href=\"{STUB_URL}\">Read online</a>",
        words("w", 0, 25)
    );
    w.put(
        "new/cut",
        &eml("t1@paid.example", "news@paid.example", "Deep dive", &body),
    );
    let o = w.run();
    assert_eq!(
        (n(&o, "replaced"), n(&o, "kept"), n(&o, "new")),
        (json!(0), json!(1), json!(0))
    );
    let text = w.env.read("Letters/Deep dive.md").unwrap();
    assert!(
        !text.contains("Subscribe") && !text.contains("Subscribe"),
        "{text}"
    );
    let (r, status) = blog_row(&w);
    assert_eq!((r.complete, status), (Some(false), None));
    assert_eq!(
        w.row("Paid Letter", "t1@paid.example").unwrap().0.outcome,
        TrackerOutcome::Skipped
    );
}

#[test]
fn marker_only_in_removed_chrome_still_blocks_the_replace() {
    let w = stub_world();
    // Without the marker this same-size email is complete and replaces the stub; "Upgrade to paid" (a configured marker) sits only in a chrome
    // line that `clean_body` removes, so the verdict must come from the pre-cleanup body.
    let body = format!(
        "<p>{}</p><p><a href=\"https://substack.com/redirect/2/bad.sig\">Upgrade to paid</a></p><a href=\"{STUB_URL}\">Read online</a>",
        words("w", 0, 30)
    );
    w.put(
        "new/chrome",
        &eml("c1@paid.example", "news@paid.example", "Deep dive", &body),
    );
    let o = w.run();
    assert_eq!(
        (n(&o, "replaced"), n(&o, "kept"), n(&o, "new")),
        (json!(0), json!(1), json!(0))
    );
    let (r, status) = blog_row(&w);
    assert_eq!((r.complete, status), (Some(false), None));
}

#[test]
fn replaced_note_body_is_clean() {
    let w = stub_world();
    let body = format!(
        "<p>{}</p><p><a href=\"https://substack.com/redirect/2/bad.sig\">Share this post</a></p><p>Read <a href=\"{STUB_URL}?utm_source=email\">online</a></p>",
        words("w", 0, 120)
    );
    w.put(
        "new/clean",
        &eml("c2@paid.example", "news@paid.example", "Deep dive", &body),
    );
    let o = w.run();
    assert_eq!(n(&o, "replaced"), json!(1));
    let text = w.env.read("Letters/Deep dive.md").unwrap();
    assert!(
        text.contains("w119")
            && !text.contains("Share this post")
            && !text.contains("substack.com/redirect")
            && !text.contains("utm_"),
        "{text}"
    );
}

const SUBSTACK_FEED: &str = "https://examplepub.substack.com/feed";
const SUBSTACK_STUB: &str = "https://examplepub.substack.com/p/deep-dive";
const SUBSTACK_SOURCES: &str = r#"
[[blog]]
name = "Example Pub"
link = "https://examplepub.substack.com/feed"
paywall_markers = ["Upgrade to paid"]
"#;

/// A Substack message: `From: <pub>@substack.com`, `List-Id: <pub>.substack.com`.
fn substack_eml(mid: &str, pubname: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Pub <{pubname}@substack.com>\nList-Id: <{pubname}.substack.com>\nSubject: Deep dive\nDate: Tue, 14 Oct 2026 10:00:00 +0000\nMessage-ID: <{mid}>\nMIME-Version: 1.0\nContent-Type: text/html; charset=utf-8\n\n{body}\n"
    )
    .replace('\n', "\r\n")
    .into_bytes()
}

fn substack_world(sources: &str) -> World {
    let stub = format!(
        "---\ntitle: \"Deep dive\"\nsource: {SUBSTACK_STUB}\npost-id: \"deep-dive\"\npublished: 2026-10-14\nword-count: 30\naccess: paid\ncomplete: false\ntags:\n  - blog\nread: true\n---\n\n{}\n",
        words("w", 0, 30)
    );
    let w = world_with(sources, &[("Letters/Deep dive.md", stub)]);
    w.env
        .state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: SUBSTACK_FEED.into(),
            item_id: SUBSTACK_STUB.into(),
            title: "Deep dive".into(),
            url: SUBSTACK_STUB.into(),
            first_seen_ms: 1,
            day: "2026-10-14".into(),
            outcome: TrackerOutcome::Note,
            score: None,
            full_body: Some(true),
            note_path: Some("Letters/Deep dive.md".into()),
            source_name: Some("Example Pub".into()),
            duration_s: None,
            word_count: Some(30),
            access: Some("paid".into()),
            complete: Some(false),
            full_word_count: Some(120),
        })
        .unwrap();
    w
}

fn substack_body(n_words: usize) -> String {
    format!(
        "<p>{}</p><a href=\"{SUBSTACK_STUB}\">Read online</a>",
        words("w", 0, n_words)
    )
}

#[test]
fn substack_mail_mirrors_its_blog_row_and_ticks_the_stub() {
    let w = substack_world(SUBSTACK_SOURCES);
    w.put(
        "new/s",
        &substack_eml("s1@substack.com", "examplepub", &substack_body(120)),
    );
    let o = w.run();
    assert_eq!(
        (n(&o, "replaced"), n(&o, "new"), n(&o, "kept")),
        (json!(1), json!(0), json!(0))
    );
    let (mail, mstatus) = w.row("Example Pub", "s1@substack.com").unwrap();
    assert_eq!(mail.outcome, TrackerOutcome::Skipped);
    assert_eq!(mstatus, None);
    assert!(w.row("unknown-sender", "s1@substack.com").is_none());
    let db = w.env.state.lock().unwrap();
    let r = db
        .tracker_item("blog", SUBSTACK_FEED, SUBSTACK_STUB)
        .unwrap()
        .unwrap();
    let st = db
        .tracker_item_status("blog", SUBSTACK_FEED, SUBSTACK_STUB)
        .unwrap()
        .flatten();
    assert_eq!((r.complete, st), (Some(true), Some('x')));
}

#[test]
fn truncated_substack_mail_keeps_the_stub_by_the_blog_rows_markers() {
    let w = substack_world(SUBSTACK_SOURCES);
    let body = format!(
        "<p>{}</p><p>Upgrade to paid</p><a href=\"{SUBSTACK_STUB}\">Read online</a>",
        words("w", 0, 25)
    );
    w.put(
        "new/c",
        &substack_eml("s2@substack.com", "examplepub", &body),
    );
    let o = w.run();
    assert_eq!((n(&o, "replaced"), n(&o, "kept")), (json!(0), json!(1)));
    assert!(w.row("Example Pub", "s2@substack.com").is_some());
}

#[test]
fn explicit_newsletter_row_wins_over_the_substack_match() {
    let sources = format!(
        "{SUBSTACK_SOURCES}\n[[newsletter]]\nname = \"Explicit Pub\"\nlist_id = \"examplepub.substack.com\"\nblog = \"Example Pub\"\n"
    );
    let w = substack_world(&sources);
    w.put(
        "new/e",
        &substack_eml("s3@substack.com", "examplepub", &substack_body(120)),
    );
    w.run();
    assert!(w.row("Explicit Pub", "s3@substack.com").is_some());
    assert!(w.row("Example Pub", "s3@substack.com").is_none());
}

#[test]
fn non_substack_unknown_sender_stays_unknown() {
    let w = substack_world(SUBSTACK_SOURCES);
    w.put(
        "new/u",
        &eml(
            "u1@elsewhere.example",
            "x@elsewhere.example",
            "Hi",
            "<p>hello</p>",
        ),
    );
    w.run();
    let (item, status) = w.row("unknown-sender", "u1@elsewhere.example").unwrap();
    assert_eq!((item.outcome, status), (TrackerOutcome::Note, None));
}

#[test]
fn substack_host_without_a_blog_row_stays_unknown() {
    let w = substack_world(SUBSTACK_SOURCES);
    w.put(
        "new/o",
        &substack_eml("s4@substack.com", "otherpub", "<p>hello there</p>"),
    );
    w.run();
    assert!(w.row("unknown-sender", "s4@substack.com").is_some());
    assert!(w.row("Example Pub", "s4@substack.com").is_none());
}

// --- plan intake-round-2-clean-dedupe-reading WP-2: canonical post URL ---

const CUSTOM_FEED: &str = "https://custom.example/feed";
const CUSTOM_POST: &str = "https://custom.example/p/deep-dive";

#[test]
fn restack_only_mail_meets_its_custom_domain_rss_note() {
    let sources = format!(
        "[[blog]]\nname = \"Custom Blog\"\nlink = \"{CUSTOM_FEED}\"\n\n[[newsletter]]\nname = \"Custom Letter\"\nfrom = [\"custompub@substack.com\"]\nblog = \"Custom Blog\"\n"
    );
    let note = format!(
        "---\ntitle: \"Deep dive\"\nsource: {CUSTOM_POST}\npost-id: \"deep-dive\"\npublished: 2026-10-14\nword-count: 120\ncomplete: true\ntags:\n  - blog\nread: false\n---\n\n{}\n",
        words("w", 0, 120)
    );
    let w = world_with(&sources, &[("Letters/Deep dive.md", note)]);
    w.env
        .state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: CUSTOM_FEED.into(),
            item_id: CUSTOM_POST.into(),
            title: "Deep dive".into(),
            url: CUSTOM_POST.into(),
            first_seen_ms: 1,
            day: "2026-10-14".into(),
            outcome: TrackerOutcome::Note,
            score: None,
            full_body: Some(true),
            note_path: Some("Letters/Deep dive.md".into()),
            source_name: Some("Custom Blog".into()),
            duration_s: None,
            word_count: Some(120),
            access: None,
            complete: Some(true),
            full_word_count: None,
        })
        .unwrap();
    let body = format!(
        "<p>{}</p><a href=\"https://open.substack.com/pub/custompub/p/deep-dive?action=restack-comment&amp;r=abc&amp;token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJyZWFkZXIifQ.c2lnbmF0dXJlX29mX2p3dA\">Restack</a>",
        words("m", 0, 120)
    );
    w.put(
        "new/k",
        &eml(
            "k1@substack.com",
            "custompub@substack.com",
            "Deep dive",
            &body,
        ),
    );
    let o = w.run();
    assert_eq!(n(&o, "new"), json!(0), "{o:?}");
    assert_eq!(
        n(&o, "kept").as_u64().unwrap() + n(&o, "replaced").as_u64().unwrap(),
        1
    );
    assert_eq!(w.notes("Letters").len(), 1, "no second note");
    let (mail, _) = w.row("Custom Letter", "k1@substack.com").unwrap();
    assert_eq!(mail.note_path.as_deref(), Some("Letters/Deep dive.md"));
    let text = w.env.read("Letters/Deep dive.md").unwrap();
    assert!(
        !text.contains("token=") && !text.contains("open.substack.com"),
        "{text}"
    );
}

#[test]
fn tracker_only_mail_note_has_no_source_line() {
    let w = world(&[]);
    let body = "<p>some words here</p><a href=\"https://link.mail.beehiiv.com/ss/c/u001.opaque/abc\">View in browser</a><a href=\"https://elsewhere.example/p/linked\">a linked post</a>";
    w.put(
        "new/t",
        &eml("t1@plain.example", "hello@plain.example", "Tracked", body),
    );
    w.run();
    let (r, _) = w.row("Plain Letter", "t1@plain.example").unwrap();
    assert_eq!(r.url, "");
    let text = w.env.read(&r.note_path.unwrap()).unwrap();
    assert!(
        !text.contains("source:") && !text.contains("post-id:"),
        "{text}"
    );
}

// --- plan intake-round-2-clean-dedupe-reading WP-3: `publication` ---

#[test]
fn explicit_row_note_carries_its_short_publication_after_author() {
    let sources = SOURCES.replace(
        "name = \"Plain Letter\"\n",
        "name = \"Plain Letter\"\nshort = \"  Plain  \"\n",
    );
    let w = world_with(&sources, &[]);
    w.put(
        "new/p",
        &eml(
            "p1@plain.example",
            "hello@plain.example",
            "Plainly",
            "<p>one two three</p>",
        ),
    );
    w.run();
    let (r, _) = w.row("Plain Letter", "p1@plain.example").unwrap();
    let text = w.env.read(&r.note_path.unwrap()).unwrap();
    assert!(
        text.contains("author:\n  - \"[[Sender]]\"\npublication: \"Plain\"\npublished:"),
        "{text}"
    );
}

#[test]
fn explicit_row_without_short_publishes_its_name() {
    let w = world(&[]);
    w.put(
        "new/q",
        &eml(
            "q1@plain.example",
            "hello@plain.example",
            "Named",
            "<p>one two three</p>",
        ),
    );
    w.run();
    let (r, _) = w.row("Plain Letter", "q1@plain.example").unwrap();
    let text = w.env.read(&r.note_path.unwrap()).unwrap();
    assert!(text.contains("publication: \"Plain Letter\"\n"), "{text}");
}

#[test]
fn substack_auto_match_note_publishes_the_blog_rows_short() {
    let sources = SUBSTACK_SOURCES.replace(
        "name = \"Example Pub\"\n",
        "name = \"Example Pub\"\nshort = \"EP\"\n",
    );
    let w = world_with(&sources, &[]);
    w.put(
        "new/n",
        &substack_eml("s9@substack.com", "examplepub", "<p>fresh words here</p>"),
    );
    w.run();
    let (r, _) = w.row("Example Pub", "s9@substack.com").unwrap();
    let text = w.env.read(&r.note_path.unwrap()).unwrap();
    assert!(text.contains("publication: \"EP\"\n"), "{text}");
    assert!(!text.contains("Example Pub"), "{text}");
}

#[test]
fn substack_replace_fills_the_stubs_absent_publication() {
    let w = substack_world(SUBSTACK_SOURCES);
    w.put(
        "new/s",
        &substack_eml("s1@substack.com", "examplepub", &substack_body(120)),
    );
    w.run();
    let text = w.env.read("Letters/Deep dive.md").unwrap();
    assert!(text.contains("\npublication: Example Pub\n"), "{text}");
}

/// Plan round-12 WP-5: a newly captured blog-kind mail note starts with `trim-label: false`.
#[test]
fn new_mail_note_of_blog_kind_has_an_unticked_trim_label() {
    let w = world(&[]);
    w.put(
        "new/1.eml",
        &eml(
            "t1@x.example",
            "who@stranger.example",
            "Odd letter",
            "<p>alpha beta gamma</p>",
        ),
    );
    w.run();
    let (r, _) = w.row("unknown-sender", "t1@x.example").unwrap();
    let text = w.env.read(&r.note_path.unwrap()).unwrap();
    assert!(text.contains("\ntrim-label: false\n"), "{text}");
}

/// Plan round-16 WP-6 consumer gate: complete message B triggers a scan while multipart message A
/// is still being written. B is processed; A is neither handled nor moved.
#[test]
fn incomplete_multipart_is_left_untouched_while_a_complete_message_is_processed() {
    let w = world(&[]);
    let a_open = "From: Sender <news@paid.example>\nSubject: Partial\nMessage-ID: <a1@paid.example>\nContent-Type: multipart/alternative; boundary=\"b1\"\n\n--b1\nContent-Type: text/plain\n\nfirst half of the bod"
        .replace('\n', "\r\n");
    w.put("new/a", a_open.as_bytes());
    w.put(
        "new/b",
        &eml(
            "b1@paid.example",
            "news@paid.example",
            "Done",
            "<p>one two three four</p>",
        ),
    );
    let o = w.run();
    assert_eq!(
        (
            n(&o, "scanned"),
            n(&o, "new"),
            n(&o, "incomplete"),
            n(&o, "invalid")
        ),
        (json!(2), json!(1), json!(1), json!(0))
    );
    assert!(w.exists("new/a"), "the incomplete message stays in new/");
    assert_eq!(
        std::fs::read(w.root().join("new/a")).unwrap(),
        a_open.as_bytes()
    );
    assert!(!w.exists("new/b"), "the complete message moved on");
    assert!(
        w.row("Paid Letter", "a1@paid.example").is_none(),
        "A not recorded"
    );
    // Once A's closing boundary lands, a later scan handles it.
    let done = format!("{a_open}y\r\n--b1--\r\n");
    w.put("new/a", done.as_bytes());
    let o2 = w.run();
    assert_eq!((n(&o2, "new"), n(&o2, "incomplete")), (json!(1), json!(0)));
    assert!(!w.exists("new/a"));
}

/// Plan intake-auto-enrich WP-3: a new mail note enqueues exactly one `intake-enrich` child on
/// its path; a second scan (nothing new) adds none.
#[test]
fn new_mail_note_enqueues_one_enrich_job() {
    let w = world(&[]);
    w.put(
        "new/1.eml",
        &eml(
            "e1@x.example",
            "who@stranger.example",
            "Odd letter",
            "<p>alpha beta gamma</p>",
        ),
    );
    let rec = crate::steps::testkit::AtRecorder::default();
    let mut ctx = w.env.ctx();
    ctx.children = &rec;
    let t: toml::Table = "type = \"track_mail\"".parse().unwrap();
    let st = Step::from_table(t).unwrap();
    execute(&st, &mut ctx).unwrap();
    let (r, _) = w.row("unknown-sender", "e1@x.example").unwrap();
    assert_eq!(
        rec.calls(),
        [format!("intake-enrich@{}", r.note_path.unwrap())]
    );
    execute(&st, &mut ctx).unwrap();
    assert_eq!(rec.calls().len(), 1);
}
