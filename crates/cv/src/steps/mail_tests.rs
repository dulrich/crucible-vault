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
paywall_markers = ["Subscribe to keep reading"]

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
    let mail = tempfile::tempdir().unwrap();
    let pipelines = format!(
        "[mail]\nenabled = true\nmaildir = {:?}\n",
        mail.path().to_str().unwrap()
    );
    let config = config_of(&[
        ("pipelines.toml", &pipelines),
        ("sources.toml", SOURCES),
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
