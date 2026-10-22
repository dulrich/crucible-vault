//! Reconcile backfill tests (plan intake-followups-mail-youtube WP-5): synthetic temp vaults,
//! synthetic `.eml` files (invented senders and text) in a temp Maildir and a temp `state.db`;
//! never the network, never the live vault.

use cv_actions::{Action, ConfigSet, LaneChoice, ResourceSpec, Step};
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::Value;

use super::reconcile_backfill::{Counts, recleaned};
use crate::steps::mail::{KIND, UNKNOWN};
use crate::steps::testkit::{StepEnv, config_of};
use crate::steps::{StepOutcome, execute};

const VID: &str = "dQw4w9WgXcQ";
const KINDS: &str = "[kinds.fanfic]\ntags = [\"fanfic\"]\nfolder = \"Fanfic\"\n[kinds.blog]\ntags = [\"blog\"]\nfolder = \"Letters\"\n[kinds.news]\ntags = [\"news\"]\nfolder = \"News\"\n[kinds.post]\ncleanup_transcript = true\n";
const CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\n";
/// Newsbit: an explicit row for the sender the first run did not know.
const HEATMAP: &str = "[[newsletter]]\nname = \"Newsbit\"\nfrom = [\"hello@plain.example\"]\nkind = \"news\"\ntags = [\"extra\"]\n";
const CHROME: &str = "Real prose paragraph one.\n\nReal prose paragraph two.\n\n[Unsubscribe](https://news.example/unsubscribe?u=1)\n";

struct World {
    env: StepEnv,
    mail: tempfile::TempDir,
}

fn action() -> Action {
    Action {
        id: "intake-reconcile".into(),
        label: "Reconcile".into(),
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

fn config(mail: &tempfile::TempDir, sources: &str) -> ConfigSet {
    let pipelines = format!(
        "[mail]\nenabled = true\nmaildir = {:?}\n",
        mail.path().to_str().unwrap()
    );
    config_of(&[
        ("pipelines.toml", &pipelines),
        ("sources.toml", sources),
        ("kinds.toml", KINDS),
        ("captures.toml", CAPTURES),
    ])
}

fn world(files: &[(&str, String)]) -> World {
    let mail = tempfile::tempdir().unwrap();
    let env = StepEnv::new(
        LiveFixture::new(files.iter().map(|(p, t)| (*p, t.clone()))),
        config(&mail, ""),
        action(),
    );
    World { env, mail }
}

impl World {
    fn put(&self, rel: &str, bytes: &[u8]) {
        let p = self.mail.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    fn sources(&mut self, sources: &str) {
        self.env.config = config(&self.mail, sources);
    }
    /// `track_mail` once (so rows and notes exist exactly as a fresh run makes them).
    fn track(&self) {
        let t: toml::Table = "type = \"track_mail\"".parse().unwrap();
        execute(&Step::from_table(t).unwrap(), &mut self.env.ctx()).unwrap();
    }
    fn reconcile(&self, dry: bool) -> StepOutcome {
        let t: toml::Table = format!("type = \"intake_reconcile\"\ndry_run = {dry}")
            .parse()
            .unwrap();
        execute(&Step::from_table(t).unwrap(), &mut self.env.ctx()).unwrap()
    }
    fn row(&self, kind: &str, key: &str, id: &str) -> Option<(TrackerItem, Option<char>)> {
        let db = self.env.state.lock().unwrap();
        db.tracker_item(kind, key, id).unwrap().map(|r| {
            let s = db.tracker_item_status(kind, key, id).unwrap().flatten();
            (r, s)
        })
    }
    fn notes_in(&self, folder: &str) -> usize {
        std::fs::read_dir(self.env.vault().join(folder)).map_or(0, Iterator::count)
    }
}

fn eml(mid: &str, from: &str, body: &str) -> Vec<u8> {
    format!(
        "From: Sender <{from}>\nSubject: Weekly\nDate: Tue, 14 Oct 2026 10:00:00 +0000\nMessage-ID: <{mid}>\nMIME-Version: 1.0\nContent-Type: text/html; charset=utf-8\n\n{body}\n"
    )
    .replace('\n', "\r\n")
    .into_bytes()
}

fn n(o: &StepOutcome, k: &str) -> u64 {
    o.vars[k].as_u64().unwrap_or_else(|| panic!("var {k}"))
}

/// The backfill vars of `o` as a sorted `key=value` list (everything the phases add).
fn backfill_vars(o: &StepOutcome) -> Vec<(String, Value)> {
    let names = serde_json::to_value(Counts::default()).unwrap();
    let mut v: Vec<(String, Value)> = names
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), o.vars[k].clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn add_row(
    w: &World,
    kind: &str,
    key: &str,
    id: &str,
    outcome: TrackerOutcome,
    path: Option<&str>,
) {
    w.env
        .state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: kind.into(),
            source_key: key.into(),
            item_id: id.into(),
            title: "T".into(),
            url: format!("https://www.youtube.com/watch?v={id}"),
            first_seen_ms: 1,
            day: "2026-10-21".into(),
            outcome,
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
}

fn clip(source: &str) -> String {
    format!(
        "---\nsource: {source}\ntags: [post]\n---\n![](https://youtu.be/{VID})\n\nA description.\n\n## Transcript\n\n**0:01** one\n\n**0:05** two\n\n**1:10** three\n"
    )
}

/// A world with one unknown-sender mail note (made by `track_mail`), then `sources` configured.
fn rematch_world(sources: &str, body: &str) -> World {
    let mut w = world(&[]);
    w.put(
        "new/m1",
        &eml("m1@plain.example", "hello@plain.example", body),
    );
    w.track();
    assert!(w.row(KIND, UNKNOWN, "m1@plain.example").is_some());
    w.sources(sources);
    w
}

#[test]
fn dry_run_reports_each_phase_and_writes_nothing_then_the_real_run_matches() {
    let mut w = world(&[
        ("stack/clip.md", clip(&format!("https://youtu.be/{VID}"))),
        (
            "mailnotes/c.md",
            format!("---\nvia: email\n---\n\n{CHROME}"),
        ),
    ]);
    w.put(
        "new/m1",
        &eml(
            "m1@plain.example",
            "hello@plain.example",
            "<p>alpha beta gamma</p>",
        ),
    );
    w.track();
    add_row(&w, "youtube", "UC1", VID, TrackerOutcome::Line, None);
    w.sources(HEATMAP);
    let (before_row, _) = w.row(KIND, UNKNOWN, "m1@plain.example").unwrap();
    let note_path = before_row.note_path.clone().unwrap();
    let note_before = w.env.read(&note_path).unwrap();
    let chrome_before = w.env.read("mailnotes/c.md").unwrap();

    let dry = w.reconcile(true);
    assert_eq!(
        (
            n(&dry, "yt_ticked"),
            n(&dry, "yt_linked"),
            n(&dry, "mail_rematched"),
            n(&dry, "mail_ticked"),
            n(&dry, "mail_retagged"),
            n(&dry, "mail_recleaned_r2"),
            n(&dry, "x_ticked")
        ),
        (1, 1, 1, 1, 1, 1, 0)
    );
    // Dry run: nothing written to state.db or the vault.
    assert!(w.row(KIND, UNKNOWN, "m1@plain.example").is_some());
    assert!(w.row(KIND, "Newsbit", "m1@plain.example").is_none());
    assert_eq!(w.row("youtube", "UC1", VID).unwrap().1, None);
    assert_eq!(w.row("youtube", "UC1", VID).unwrap().0.note_path, None);
    assert_eq!(w.env.read(&note_path).unwrap(), note_before);
    assert_eq!(w.env.read("mailnotes/c.md").unwrap(), chrome_before);

    let real = w.reconcile(false);
    assert_eq!(backfill_vars(&dry), backfill_vars(&real));
    assert_eq!(w.row("youtube", "UC1", VID).unwrap().1, Some('x'));
    assert!(w.row(KIND, UNKNOWN, "m1@plain.example").is_none());
    assert_eq!(
        w.row(KIND, "Newsbit", "m1@plain.example").unwrap().1,
        Some('x')
    );
    assert!(
        !w.env
            .read("mailnotes/c.md")
            .unwrap()
            .contains("Unsubscribe")
    );

    // Re-run: nothing left to do.
    let again = w.reconcile(false);
    for k in [
        "yt_ticked",
        "mail_rematched",
        "mail_ticked",
        "mail_retagged",
        "mail_recleaned_r2",
    ] {
        assert_eq!(n(&again, k), 0, "{k}");
    }
    assert!(
        !serde_json::to_string(&real.vars)
            .unwrap()
            .contains("stack/")
    );
}

#[test]
fn rematch_rekeys_in_place_retags_and_ticks_without_a_second_note() {
    let w = rematch_world(HEATMAP, "<p>alpha beta gamma</p>");
    let (r0, _) = w.row(KIND, UNKNOWN, "m1@plain.example").unwrap();
    let path = r0.note_path.unwrap();
    let folder = path.rsplit_once('/').unwrap().0.to_owned();
    let notes_before = w.notes_in(&folder);
    let o = w.reconcile(false);
    assert_eq!(
        (
            n(&o, "mail_rematched"),
            n(&o, "mail_collisions"),
            n(&o, "mail_ticked")
        ),
        (1, 0, 1)
    );
    // The kind folder differs (News vs Letters): counted and left.
    assert_eq!(n(&o, "mail_folder_mismatch"), 1);
    assert_eq!(
        w.notes_in(&folder),
        notes_before,
        "no second note, none moved"
    );
    let (r, status) = w.row(KIND, "Newsbit", "m1@plain.example").unwrap();
    assert_eq!(
        (r.note_path.as_deref(), r.source_name.as_deref(), status),
        (Some(path.as_str()), Some("Newsbit"), Some('x'))
    );
    let text = w.env.read(&path).unwrap();
    assert!(
        text.contains("  - news") && text.contains("  - extra"),
        "{text}"
    );
}

#[test]
fn rekey_collision_merges_and_keeps_the_new_rows_status() {
    let w = rematch_world(HEATMAP, "<p>alpha beta gamma</p>");
    // A row already sits at the new key (a fresh run recorded it), still open and note-less.
    add_row(
        &w,
        KIND,
        "Newsbit",
        "m1@plain.example",
        TrackerOutcome::Skipped,
        None,
    );
    let o = w.reconcile(false);
    assert_eq!((n(&o, "mail_rematched"), n(&o, "mail_collisions")), (1, 1));
    assert!(w.row(KIND, UNKNOWN, "m1@plain.example").is_none());
    let (r, status) = w.row(KIND, "Newsbit", "m1@plain.example").unwrap();
    assert_eq!(
        r.outcome,
        TrackerOutcome::Skipped,
        "the new-key row is kept"
    );
    assert!(
        r.note_path.is_some(),
        "NULL note_path filled from the old row"
    );
    assert_eq!(status, Some('x'));
}

#[test]
fn a_paywall_marker_in_the_original_message_keeps_the_row_open() {
    let sources = format!("{HEATMAP}paywall_markers = [\"Upgrade to read\"]\n");
    let w = rematch_world(
        &sources,
        "<p>alpha beta gamma</p><p>Upgrade to read the rest</p>",
    );
    let o = w.reconcile(false);
    assert_eq!(
        (
            n(&o, "mail_rematched"),
            n(&o, "mail_marker_incomplete"),
            n(&o, "mail_ticked")
        ),
        (1, 1, 0)
    );
    let (r, status) = w.row(KIND, "Newsbit", "m1@plain.example").unwrap();
    assert_eq!(status, None, "partial mail is never ticked");
    assert!(
        w.env
            .read(&r.note_path.unwrap())
            .unwrap()
            .contains("complete: false")
    );
}

#[test]
fn missing_messages_and_unmatched_senders_are_counted_and_left_alone() {
    let w = rematch_world(HEATMAP, "<p>alpha beta gamma</p>");
    // Unmatched: a message from a sender no row knows.
    w.put(
        "processed/other.eml",
        &eml(
            "m2@stranger.example",
            "who@stranger.example",
            "<p>x y z</p>",
        ),
    );
    add_row(
        &w,
        KIND,
        UNKNOWN,
        "m2@stranger.example",
        TrackerOutcome::Skipped,
        None,
    );
    // Missing: no file for this Message-ID.
    add_row(
        &w,
        KIND,
        UNKNOWN,
        "gone@x.example",
        TrackerOutcome::Skipped,
        None,
    );
    let o = w.reconcile(false);
    assert_eq!(
        (
            n(&o, "mail_rematched"),
            n(&o, "mail_unmatched"),
            n(&o, "mail_missing")
        ),
        (1, 1, 1)
    );
    assert!(w.row(KIND, UNKNOWN, "m2@stranger.example").is_some());
    assert!(w.row(KIND, UNKNOWN, "gone@x.example").is_some());
}

#[test]
fn youtube_clip_with_a_transcript_ticks_and_a_bare_embed_does_not() {
    let w = world(&[
        ("stack/clip.md", clip(&format!("https://youtu.be/{VID}"))),
        (
            "stack/embed.md",
            "---\nsource: https://example.com/essay\ntags: [post]\n---\n![](https://youtu.be/ZZZZZZZZZZZ)\n\nProse about it.\n".to_owned(),
        ),
    ]);
    add_row(&w, "youtube", "UC1", VID, TrackerOutcome::Line, None);
    add_row(
        &w,
        "youtube",
        "UC1",
        "ZZZZZZZZZZZ",
        TrackerOutcome::Note,
        Some("stack/embed.md"),
    );
    add_row(
        &w,
        "youtube",
        "UC1",
        "NOFILE000000",
        TrackerOutcome::Line,
        None,
    );
    let o = w.reconcile(false);
    assert_eq!(
        (
            n(&o, "yt_ticked"),
            n(&o, "yt_linked"),
            n(&o, "yt_no_evidence"),
            n(&o, "yt_no_note")
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(w.row("youtube", "UC1", VID).unwrap().1, Some('x'));
    assert_eq!(w.row("youtube", "UC1", "ZZZZZZZZZZZ").unwrap().1, None);
    assert_eq!(
        n(&o, "skipped_kind"),
        0,
        "youtube rows no longer count as skipped"
    );
}

#[test]
fn reclean_rewrites_a_chrome_note_once_and_keeps_the_frontmatter() {
    let fm = "---\ntitle: \"T\"\nvia: email\ntags:\n  - blog\n---\n";
    let note = format!("{fm}\n{CHROME}");
    let w = world(&[
        ("mailnotes/c.md", note.clone()),
        ("mailnotes/clean.md", format!("{fm}\nJust prose.\n")),
        (
            "mailnotes/other.md",
            format!("---\nvia: rss\n---\n\n{CHROME}"),
        ),
    ]);
    let o = w.reconcile(false);
    assert_eq!(
        (n(&o, "mail_recleaned_r2"), n(&o, "mail_clean_unchanged")),
        (1, 1)
    );
    let after = w.env.read("mailnotes/c.md").unwrap();
    assert!(after.starts_with(fm), "frontmatter byte-for-byte");
    assert!(after.contains("Real prose paragraph two.") && !after.contains("Unsubscribe"));
    assert!(
        w.env
            .read("mailnotes/other.md")
            .unwrap()
            .contains("Unsubscribe"),
        "non-email notes are untouched"
    );
    let again = w.reconcile(false);
    assert_eq!(
        (
            n(&again, "mail_recleaned_r2"),
            n(&again, "mail_clean_unchanged")
        ),
        (0, 2)
    );
    assert_eq!(w.env.read("mailnotes/c.md").unwrap(), after);
}

#[test]
fn recleaned_is_pure_and_idempotent() {
    let text = format!("---\nvia: email\n---\n\n{CHROME}");
    let once = recleaned(&text, &|_| None).expect("changes");
    assert!(once.starts_with("---\nvia: email\n---\n"));
    assert_eq!(recleaned(&once, &|_| None), None);
    assert_eq!(
        recleaned("---\nvia: email\n---\n", &|_| None),
        None,
        "empty body"
    );
}

#[test]
fn intake_reconcile_is_a_note_mutating_step() {
    assert!(cv_actions::StepKind::IntakeReconcile.mutates_note());
    assert!(!cv_actions::StepKind::IntakeFinalize.mutates_note());
}

// ---- round-2 repair (plan intake-round-2-clean-dedupe-reading WP-6) ---------------------------

const ZED: &str = "https://zed.substack.com/p/slug-one";
const ZED_BLOG: &str =
    "[[blog]]\nname = \"Zed\"\nshort = \"ZZ\"\nlink = \"https://zed.substack.com/feed\"\n";

fn zed_eml() -> Vec<u8> {
    eml(
        "m1@zed.example",
        "zed@substack.com",
        &format!("<a href=\"{ZED}?token=abc\">Slug One</a><p>body</p>"),
    )
}

/// A mail note as `track_mail` writes it, but with a token `source` the way round 1 left it.
fn mail_note(complete: bool, body: &str) -> String {
    format!(
        "---\ntitle: \"Slug One\"\nsource: {ZED}?token=abc\nmessage-id: \"m1@zed.example\"\nauthor:\n  - \"[[Pub]]\"\ncomplete: {complete}\nvia: email\ntags:\n  - blog\n---\n\n{body}\n"
    )
}

fn rss_note(complete: bool, body: &str) -> String {
    format!(
        "---\ntitle: \"Slug One\"\nsource: {ZED}\nauthor:\n  - \"[[Pub]]\"\ncomplete: {complete}\nvia: rss\n---\n\n{body}\n"
    )
}

const LONG: &str = "The quick brown fox jumps over the lazy dog while seven wizards quietly box jars of old pickled plums for the winter market stalls.";

fn zed_world(rss: String, mail: String) -> World {
    let w = world(&[("Letters/rss.md", rss), ("mailnotes/m.md", mail)]);
    w.put("processed/m1", &zed_eml());
    add_row(
        &w,
        "mail",
        "Zed",
        "m1@zed.example",
        TrackerOutcome::Note,
        Some("mailnotes/m.md"),
    );
    w
}

fn repair_vars(o: &StepOutcome) -> Vec<(String, Value)> {
    let names = serde_json::to_value(super::reconcile_repair::Counts::default()).unwrap();
    names
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), o.vars[k].clone()))
        .collect()
}

fn trashed(w: &World) -> usize {
    fn count(p: &std::path::Path) -> usize {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() { count(&p) } else { 1 }
            })
            .sum()
    }
    count(&w.env.vault().join(".trash"))
}

#[test]
fn dry_run_repair_writes_nothing_and_the_real_run_merges_the_pair() {
    let w = zed_world(rss_note(false, "A teaser."), mail_note(true, LONG));
    let (mail0, rss0) = (
        w.env.read("mailnotes/m.md").unwrap(),
        w.env.read("Letters/rss.md").unwrap(),
    );
    let dry = w.reconcile(true);
    assert_eq!(
        (
            n(&dry, "mail_rekeyed"),
            n(&dry, "dup_merged"),
            n(&dry, "dup_marked")
        ),
        (1, 1, 0)
    );
    assert_eq!(w.env.read("mailnotes/m.md").unwrap(), mail0);
    assert_eq!(w.env.read("Letters/rss.md").unwrap(), rss0);
    assert_eq!(trashed(&w), 0);
    let row = w.row("mail", "Zed", "m1@zed.example").unwrap();
    assert_eq!(
        (row.0.note_path.as_deref(), row.1),
        (Some("mailnotes/m.md"), None)
    );

    let real = w.reconcile(false);
    assert_eq!(repair_vars(&dry), repair_vars(&real));
    assert!(
        !w.env.vault().join("mailnotes/m.md").exists(),
        "loser trashed"
    );
    assert_eq!(trashed(&w), 1);
    let survivor = w.env.read("Letters/rss.md").unwrap();
    assert!(
        survivor.contains(LONG),
        "the fuller mail body replaced the stub"
    );
    let row = w.row("mail", "Zed", "m1@zed.example").unwrap();
    assert_eq!(
        (row.0.note_path.as_deref(), row.1),
        (Some("Letters/rss.md"), Some('x')),
        "re-pointed to the survivor and finalized"
    );

    let again = w.reconcile(false);
    for k in [
        "mail_rekeyed",
        "mail_token_stripped",
        "dup_merged",
        "dup_marked",
        "dup_other",
        "publication_backfilled",
    ] {
        assert_eq!(n(&again, k), 0, "{k}");
    }
    assert_eq!(trashed(&w), 1);
}

#[test]
fn a_contained_kept_mail_note_is_trashed() {
    let w = zed_world(
        rss_note(true, &format!("{LONG} And more text after it.")),
        mail_note(true, LONG),
    );
    let o = w.reconcile(false);
    assert_eq!((n(&o, "dup_merged"), n(&o, "dup_marked")), (1, 0));
    assert!(!w.env.vault().join("mailnotes/m.md").exists());
    assert_eq!(
        w.row("mail", "Zed", "m1@zed.example")
            .unwrap()
            .0
            .note_path
            .as_deref(),
        Some("Letters/rss.md")
    );
}

#[test]
fn a_kept_mail_note_that_is_not_contained_is_marked_and_nothing_is_trashed() {
    let other = "Entirely different words appear here describing harbors lighthouses and migrating geese over cold northern water at dawn.";
    let w = zed_world(rss_note(true, other), mail_note(true, LONG));
    let rss0 = w.env.read("Letters/rss.md").unwrap();
    let o = w.reconcile(false);
    assert_eq!((n(&o, "dup_merged"), n(&o, "dup_marked")), (0, 1));
    assert_eq!(trashed(&w), 0);
    let mail = w.env.read("mailnotes/m.md").unwrap();
    assert!(
        mail.contains("duplicate-of: \"[[Letters/rss]]\"")
            || mail.contains("duplicate-of: [[Letters/rss]]"),
        "{mail}"
    );
    assert!(
        mail.contains(&format!("source: {ZED}\n")),
        "re-keyed: {mail}"
    );
    assert_eq!(w.env.read("Letters/rss.md").unwrap(), rss0);
    let again = w.reconcile(false);
    assert_eq!((n(&again, "dup_marked"), n(&again, "dup_merged")), (0, 0));
}

#[test]
fn an_identity_match_with_a_clip_is_counted_and_left_alone() {
    let clip =
        format!("---\ntitle: \"C\"\nsource: {ZED}\nvia: clip\ncomplete: false\n---\n\nA clip.\n");
    let w = zed_world(clip, mail_note(true, LONG));
    let o = w.reconcile(false);
    assert_eq!(
        (n(&o, "dup_other"), n(&o, "dup_merged"), n(&o, "dup_marked")),
        (1, 0, 0)
    );
    assert_eq!(trashed(&w), 0);
    assert!(
        w.env
            .read("mailnotes/m.md")
            .unwrap()
            .contains(&format!("source: {ZED}\n"))
    );
}

#[test]
fn a_token_source_with_no_canonical_url_is_stripped_and_a_missing_message_is_skipped() {
    let tracker = "https://track.example/v2/c/abc?token=zzz";
    let note = |mid: &str| {
        format!(
            "---\ntitle: \"T\"\nsource: {tracker}\npost-id: {tracker}\nmessage-id: \"{mid}\"\nvia: email\n---\n\nSome prose here.\n"
        )
    };
    let w = world(&[
        ("mailnotes/a.md", note("a@x.example")),
        ("mailnotes/b.md", note("gone@x.example")),
    ]);
    w.put(
        "processed/a",
        &eml(
            "a@x.example",
            "n@plain.example",
            "<a href=\"https://track.example/v2/c/abc\">x</a>",
        ),
    );
    let dry = w.reconcile(true);
    assert_eq!(
        (n(&dry, "mail_token_stripped"), n(&dry, "rekey_skipped")),
        (1, 1)
    );
    assert!(w.env.read("mailnotes/a.md").unwrap().contains("token=zzz"));
    let o = w.reconcile(false);
    assert_eq!(repair_vars(&dry), repair_vars(&o));
    let a = w.env.read("mailnotes/a.md").unwrap();
    assert!(
        !a.contains("source:") && !a.contains("post-id:") && !a.contains("token="),
        "{a}"
    );
    assert!(
        w.env.read("mailnotes/b.md").unwrap().contains("token=zzz"),
        "skipped note unchanged"
    );
    assert_eq!(n(&w.reconcile(false), "mail_token_stripped"), 0);
}

#[test]
fn publication_is_backfilled_once_and_never_overwritten() {
    let mut w = world(&[
        ("Letters/a.md", "---\ntitle: \"A\"\nauthor:\n  - \"[[P]]\"\nvia: rss\n---\n\nBody a.\n".to_owned()),
        ("Letters/b.md", "---\ntitle: \"B\"\nauthor:\n  - \"[[P]]\"\npublication: Mine\nvia: rss\n---\n\nBody b.\n".to_owned()),
        ("Letters/c.md", "---\ntitle: \"C\"\nvia: rss\n---\n\nBody c.\n".to_owned()),
    ]);
    w.sources(ZED_BLOG);
    for (id, p) in [("a", "Letters/a.md"), ("b", "Letters/b.md")] {
        add_row(
            &w,
            "blog",
            "https://zed.substack.com/feed",
            id,
            TrackerOutcome::Note,
            Some(p),
        );
    }
    add_row(
        &w,
        "blog",
        "https://other.example/feed",
        "c",
        TrackerOutcome::Note,
        Some("Letters/c.md"),
    );
    let b0 = w.env.read("Letters/b.md").unwrap();
    let dry = w.reconcile(true);
    assert_eq!(
        (
            n(&dry, "publication_backfilled"),
            n(&dry, "publication_no_row")
        ),
        (1, 1)
    );
    assert!(!w.env.read("Letters/a.md").unwrap().contains("publication"));
    let o = w.reconcile(false);
    assert_eq!(repair_vars(&dry), repair_vars(&o));
    let a = w.env.read("Letters/a.md").unwrap();
    assert!(
        a.contains("author:\n  - \"[[P]]\"\npublication: ZZ\n"),
        "{a}"
    );
    assert_eq!(w.env.read("Letters/b.md").unwrap(), b0, "never overwritten");
    let again = w.reconcile(false);
    assert_eq!(n(&again, "publication_backfilled"), 0);
    assert_eq!(w.env.read("Letters/a.md").unwrap(), a);
}

#[test]
fn reclean_r2_drops_a_small_localized_image_by_its_decoded_size() {
    let note = |name: &str| {
        format!(
            "---\nvia: email\n---\n\n![](_attachments/{name}.png)\n\nReal prose paragraph one.\n"
        )
    };
    let w = world(&[
        ("mailnotes/s.md", note("small")),
        ("mailnotes/l.md", note("large")),
    ]);
    let dir = w.env.vault().join("_attachments");
    std::fs::create_dir_all(&dir).unwrap();
    image::RgbImage::new(36, 36)
        .save(dir.join("small.png"))
        .unwrap();
    image::RgbImage::new(300, 200)
        .save(dir.join("large.png"))
        .unwrap();
    let o = w.reconcile(false);
    assert_eq!(n(&o, "mail_recleaned_r2"), 1);
    assert!(!w.env.read("mailnotes/s.md").unwrap().contains("small.png"));
    assert!(w.env.read("mailnotes/l.md").unwrap().contains("large.png"));
    assert!(
        w.env
            .read("mailnotes/s.md")
            .unwrap()
            .contains("Real prose paragraph one.")
    );
    assert_eq!(n(&w.reconcile(false), "mail_recleaned_r2"), 0);
}

// ---- round-9 repair (plan round-9-feedback-todos WP-2) ----------------------------------------

const PAGE: &str = "https://forum.example/threads/saga.1/page-2";

fn identity_vars(o: &StepOutcome) -> Vec<(String, Value)> {
    let names = serde_json::to_value(super::reconcile_identity::Counts::default()).unwrap();
    names
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), o.vars[k].clone()))
        .collect()
}

fn chapter(post: u32, dup: Option<&str>, body: &str) -> String {
    let dup = dup.map_or(String::new(), |d| format!("duplicate-of: \"[[{d}]]\"\n"));
    format!(
        "---\ntitle: \"Ch {post}\"\nsource: {PAGE}#post-{post}\npost-id: {PAGE}\n{dup}---\n\n{body}\n"
    )
}

#[test]
fn forum_chapters_are_rekeyed_and_only_the_spurious_duplicate_of_goes() {
    let other = "Entirely different words appear here describing harbors lighthouses and migrating geese over cold northern water at dawn.";
    let w = world(&[
        ("Fic/b.md", chapter(12, None, LONG)),
        ("Fic/a.md", chapter(11, Some("Fic/b"), other)),
        // Same page, but the content matches: the marker is real evidence and stays.
        ("Fic/c.md", chapter(13, Some("Fic/b"), LONG)),
        // A non-forum duplicate-of is never touched.
        (
            "Fic/d.md",
            format!("---\nsource: {ZED}\nduplicate-of: \"[[Fic/b]]\"\n---\n\n{other}\n"),
        ),
        // A stored post-id that is not the obsolete page key is left alone.
        (
            "Fic/e.md",
            format!("---\nsource: {PAGE}#post-14\npost-id: custom\n---\n\nx\n"),
        ),
    ]);
    let before: Vec<String> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|f| w.env.read(&format!("Fic/{f}.md")).unwrap())
        .collect();
    let dry = w.reconcile(true);
    assert_eq!(
        (
            n(&dry, "forum_rekeyed"),
            n(&dry, "dup_of_removed"),
            n(&dry, "dup_of_kept")
        ),
        (3, 1, 1)
    );
    for (f, t) in ["a", "b", "c", "d", "e"].iter().zip(&before) {
        assert_eq!(&w.env.read(&format!("Fic/{f}.md")).unwrap(), t, "dry: {f}");
    }
    let real = w.reconcile(false);
    assert_eq!(identity_vars(&dry), identity_vars(&real));
    let a = w.env.read("Fic/a.md").unwrap();
    assert!(
        a.contains("post-id: https://forum.example/posts/11\n"),
        "{a}"
    );
    assert!(!a.contains("duplicate-of"), "{a}");
    assert!(w.env.read("Fic/c.md").unwrap().contains("duplicate-of"));
    assert_eq!(w.env.read("Fic/d.md").unwrap(), before[3]);
    assert_eq!(w.env.read("Fic/e.md").unwrap(), before[4]);
    assert!(
        w.env
            .read("Fic/b.md")
            .unwrap()
            .contains("post-id: https://forum.example/posts/12\n")
    );
    let again = w.reconcile(false);
    assert_eq!(
        (n(&again, "forum_rekeyed"), n(&again, "dup_of_removed")),
        (0, 0)
    );
}

#[test]
fn fanfic_notes_get_the_matched_work_title_once() {
    let work = "---\ntitle: \"The Saga\"\nsource-prefix-filter: [\"https://saga.example/\"]\ntags: [\"fanfic\"]\n---\n\nA work.\n";
    let w = world(&[
        ("core/works/Saga.md", work.to_owned()),
        (
            "Fanfic/a.md",
            "---\nsource: https://saga.example/ch1\nauthor:\n  - \"[[Ann]]\"\ntags:\n  - fanfic\n---\n\nx\n".to_owned(),
        ),
        (
            "Fanfic/b.md",
            "---\nsource: https://saga.example/ch2\npublication: Mine\ntags:\n  - fanfic\n---\n\nx\n".to_owned(),
        ),
        (
            "Fanfic/c.md",
            "---\nsource: https://elsewhere.example/1\ntags:\n  - fanfic\n---\n\nx\n".to_owned(),
        ),
    ]);
    let b0 = w.env.read("Fanfic/b.md").unwrap();
    let dry = w.reconcile(true);
    assert_eq!(
        (
            n(&dry, "fanfic_publication_backfilled"),
            n(&dry, "fanfic_publication_no_work")
        ),
        (1, 1)
    );
    let o = w.reconcile(false);
    assert_eq!(identity_vars(&dry), identity_vars(&o));
    let a = w.env.read("Fanfic/a.md").unwrap();
    assert!(
        a.contains("author:\n  - \"[[Ann]]\"\npublication: The Saga\n"),
        "{a}"
    );
    assert_eq!(w.env.read("Fanfic/b.md").unwrap(), b0);
    assert!(!w.env.read("Fanfic/c.md").unwrap().contains("publication"));
    assert_eq!(n(&w.reconcile(false), "fanfic_publication_backfilled"), 0);
}

/// A pre-D25 RSS note carries no `via`; its `blog` tracker row makes it RSS-origin, so its mail
/// twin merges.
#[test]
fn a_pre_d25_rss_note_with_a_blog_row_merges_with_its_mail_twin() {
    let rss =
        format!("---\ntitle: \"Slug One\"\nsource: {ZED}\ncomplete: false\n---\n\nA teaser.\n");
    let w = zed_world(rss, mail_note(true, LONG));
    add_row(
        &w,
        "blog",
        "https://zed.substack.com/feed",
        "z1",
        TrackerOutcome::Note,
        Some("Letters/rss.md"),
    );
    let o = w.reconcile(false);
    assert_eq!((n(&o, "dup_merged"), n(&o, "dup_other")), (1, 0));
    assert!(!w.env.vault().join("mailnotes/m.md").exists());
    assert!(w.env.read("Letters/rss.md").unwrap().contains(LONG));
}

#[test]
fn description_is_recomputed_on_reclean_and_css_only_when_it_carries_css() {
    let css = "---\ntitle: \"T\"\ndescription: \"@media (max-width:600px){.x{display:none}} Hello\"\nvia: email\n---\n\nHello there reader.\n";
    let plain = "---\ntitle: \"T\"\ndescription: \"My own words\"\nvia: email\n---\n\nHello there reader.\n";
    let chrome = format!("---\ntitle: \"T\"\ndescription: \"stale\"\nvia: email\n---\n\n{CHROME}");
    let w = world(&[
        ("mailnotes/css.md", css.to_owned()),
        ("mailnotes/plain.md", plain.to_owned()),
        ("mailnotes/chrome.md", chrome),
    ]);
    let o = w.reconcile(false);
    assert_eq!(
        (
            n(&o, "mail_description_repaired"),
            n(&o, "mail_recleaned_r2")
        ),
        (2, 1)
    );
    let c = w.env.read("mailnotes/css.md").unwrap();
    assert!(
        c.contains("description: \"Hello there reader.\"\n")
            || c.contains("description: Hello there reader.\n"),
        "{c}"
    );
    assert_eq!(w.env.read("mailnotes/plain.md").unwrap(), plain);
    let ch = w.env.read("mailnotes/chrome.md").unwrap();
    assert!(
        !ch.contains("stale") && ch.contains("Real prose paragraph one."),
        "{ch}"
    );
    let again = w.reconcile(false);
    assert_eq!(
        (
            n(&again, "mail_description_repaired"),
            n(&again, "mail_recleaned_r2")
        ),
        (0, 0)
    );
}

#[test]
fn css_predicate_is_narrow() {
    use super::reconcile_backfill::has_css;
    assert!(has_css("@media screen { a { color: red } } hi"));
    assert!(has_css(".btn{display:none} Read more"));
    assert!(has_css("p { margin-top: 0; }"));
    assert!(!has_css("Hello {name}, welcome { to the list }"));
    assert!(!has_css("A note about JSON {\"Key\": 1} values"));
    assert!(!has_css("Plain prose: no braces at all."));
}
