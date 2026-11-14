//! Reconcile backfill tests (plan intake-followups-mail-youtube WP-5): synthetic temp vaults,
//! synthetic `.eml` files (invented senders and text) in a temp Maildir and a temp `state.db`;
//! never the network, never the live vault.

use cv_actions::{Action, ConfigSet, ResourceSpec, Step};
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_vault::test_support::LiveFixture;
use serde_json::Value;

use super::reconcile_backfill::{Counts, recleaned};
use super::{reconcile_backfill, reconcile_repair};
use crate::steps::mail::{KIND, UNKNOWN};
use crate::steps::testkit::{ActionBuilder, StepEnv, config_of};
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
    ActionBuilder::new("intake-reconcile", vec![], ResourceSpec::default()).build()
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

/// Round 21 WP-3 (audit M1): the outcome migration 0033 relies on. A `[[newsletter]]` row added
/// after the mail was captured under `unknown-sender` makes ONE non-dry reconcile re-key the row
/// (phase B) and then write the row's name as the note's `publication` (phase F reads the
/// persisted, re-keyed rows, so B must run first); a second run changes nothing.
#[test]
fn one_non_dry_run_rekeys_unknown_sender_mail_and_backfills_its_publication() {
    const NEWSLETTER_ROW: &str = "[[newsletter]]\nname = \"Sample Letter\"\nfrom = [\"hello@plain.example\"]\nkind = \"blog\"\n";
    let w = rematch_world(NEWSLETTER_ROW, "<p>alpha beta gamma</p>");
    let (r0, _) = w.row(KIND, UNKNOWN, "m1@plain.example").unwrap();
    let path = r0.note_path.unwrap();
    let before = w.env.read(&path).unwrap();
    assert!(
        !before.contains("publication"),
        "the unknown-sender note starts without a publication: {before}"
    );

    let first = w.reconcile(false);
    assert_eq!(n(&first, "mail_rematched"), 1);
    assert_eq!(n(&first, "publication_backfilled"), 1);
    assert!(w.row(KIND, UNKNOWN, "m1@plain.example").is_none());
    let (r, _) = w.row(KIND, "Sample Letter", "m1@plain.example").unwrap();
    assert_eq!(r.note_path.as_deref(), Some(path.as_str()));
    let after = w.env.read(&path).unwrap();
    assert!(after.contains("publication: Sample Letter\n"), "{after}");

    let second = w.reconcile(false);
    assert_eq!(n(&second, "mail_rematched"), 0);
    assert_eq!(n(&second, "publication_backfilled"), 0);
    assert_eq!(w.env.read(&path).unwrap(), after, "note bytes unchanged");
}

#[test]
fn intake_reconcile_is_a_note_mutating_step() {
    assert!(cv_actions::StepKind::IntakeReconcile.mutates_note());
    assert!(!cv_actions::StepKind::IntakeFinalize.mutates_note());
}

#[path = "reconcile_backfill_boilerplate_tests.rs"]
mod boilerplate;

#[path = "reconcile_backfill_repair_tests.rs"]
mod repair;

/// Plan sources-publication-model WP-2: a `blog = ` link folds a newsletter into a record of
/// another name; the reconcile re-keys that newsletter's mail rows and learned boilerplate to
/// the record name, once.
#[test]
fn folded_newsletter_name_rekeys_mail_rows_and_boilerplate() {
    let mut w = world(&[]);
    w.sources("[[newsletter]]\nname = \"Old Letter\"\nfrom = [\"hello@plain.example\"]\n");
    w.put(
        "new/r1",
        &eml(
            "r1@plain.example",
            "hello@plain.example",
            "<p>Body text here.</p>",
        ),
    );
    w.track();
    assert!(w.row(KIND, "Old Letter", "r1@plain.example").is_some());
    let seen = |w: &World, k: &str| {
        w.env
            .state
            .lock()
            .unwrap()
            .mail_boilerplate_seen(k, "r1@plain.example")
            .unwrap()
    };
    assert!(seen(&w, "Old Letter"));
    w.sources(
        "[[blog]]\nname = \"New Pub\"\nlink = \"https://plain.example/feed\"\n\n[[newsletter]]\nname = \"Old Letter\"\nfrom = [\"hello@plain.example\"]\nblog = \"New Pub\"\n",
    );
    let dry = w.reconcile(true);
    assert_eq!(n(&dry, "mail_renamed"), 1);
    assert!(w.row(KIND, "Old Letter", "r1@plain.example").is_some());
    let o = w.reconcile(false);
    assert_eq!(n(&o, "mail_renamed"), 1);
    assert!(w.row(KIND, "Old Letter", "r1@plain.example").is_none());
    let (r, _) = w.row(KIND, "New Pub", "r1@plain.example").unwrap();
    assert_eq!(r.source_name.as_deref(), Some("New Pub"));
    assert!(!seen(&w, "Old Letter") && seen(&w, "New Pub"));
    assert_eq!(n(&w.reconcile(false), "mail_renamed"), 0);
}

/// Plan sources-publication-model WP-4: `only = "sources"` runs the re-match (and publication)
/// phases and nothing else.
#[test]
fn only_sources_rematches_and_skips_the_other_phases() {
    let mut w = rematch_world(HEATMAP, "<p>alpha beta gamma</p>");
    // An open, never-probed Substack blog row: the full run would probe it (and count it unlinked).
    w.env
        .state
        .lock()
        .unwrap()
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: "feed".into(),
            item_id: "slug".into(),
            title: "T".into(),
            url: "https://x.substack.com/p/slug".into(),
            first_seen_ms: 1,
            day: "2026-10-21".into(),
            outcome: TrackerOutcome::Note,
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
    w.env
        .params
        .insert("only".into(), Value::String("sources".into()));
    w.env.params.insert("dry_run".into(), Value::Bool(true));
    let dry = w.reconcile(false);
    assert_eq!((n(&dry, "mail_rematched"), n(&dry, "null_paths")), (1, 0));
    assert!(
        w.row(KIND, UNKNOWN, "m1@plain.example").is_some(),
        "dry run writes nothing"
    );
    w.env.params.remove("dry_run");
    let o = w.reconcile(false);
    assert_eq!(o.vars["only"], "sources");
    for k in [
        "null_paths",
        "probed",
        "probe_failed",
        "probe_skipped",
        "ticked",
        "left_open",
        "skipped_kind",
        "mail_recleaned_r2",
        "bp_learned",
    ] {
        assert_eq!(n(&o, k), 0, "{k}");
    }
    assert_eq!(n(&o, "mail_rematched"), 1);
    assert!(w.row(KIND, UNKNOWN, "m1@plain.example").is_none());
    assert!(w.row(KIND, "Newsbit", "m1@plain.example").is_some());
    let (b, status) = w.row("blog", "feed", "slug").unwrap();
    assert_eq!(
        (b.access, status),
        (None, None),
        "the open blog row is untouched"
    );
}

#[test]
fn an_unknown_only_value_is_a_step_error_naming_the_accepted_one() {
    let mut w = world(&[]);
    for bad in [
        Value::String("all".into()),
        Value::String(String::new()),
        Value::Null,
        Value::Bool(true),
    ] {
        w.env.params.insert("only".into(), bad.clone());
        let t: toml::Table = "type = \"intake_reconcile\"".parse().unwrap();
        let e = execute(&Step::from_table(t).unwrap(), &mut w.env.ctx()).unwrap_err();
        assert!(e.to_string().contains("\"sources\""), "{bad}: {e}");
    }
}
