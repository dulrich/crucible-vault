//! Learned publication hosts (plan `intake-foundations` WP-3). Synthetic hosts and ids only.

use super::*;
use cv_ingest::identity::{intersects, value_keys};
use cv_protocol::TrackerOutcome;
use cv_state::{QueueConfig, TrackerItem};

const FEED: &str = "https://feeds.example/letter/rss";

fn sources(extra: &str) -> SourcesConfig {
    let t = format!(
        "[[publication]]\nname = \"Letter\"\nfeeds = [\"{FEED}\"]\n[[publication]]\nname = \"Other\"\nfeeds = [\"https://other.example/feed\"]\n{extra}"
    );
    SourcesConfig::from_table(&t.parse::<toml::Table>().unwrap()).unwrap()
}

fn db() -> (tempfile::TempDir, Mutex<StateDb>) {
    let dir = tempfile::tempdir().unwrap();
    let s = StateDb::open(&dir.path().join("state.db"), 1, QueueConfig::default()).unwrap();
    (dir, Mutex::new(s))
}

fn track(s: &Mutex<StateDb>, feed: &str, item_id: &str, url: &str) {
    lock(s)
        .insert_tracker_item(&TrackerItem {
            source_kind: "blog".into(),
            source_key: feed.into(),
            item_id: item_id.into(),
            title: "T".into(),
            url: url.into(),
            first_seen_ms: 1,
            day: "2026-10-01".into(),
            outcome: TrackerOutcome::Line,
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

fn learned(s: &Mutex<StateDb>) -> Vec<(String, String)> {
    rows(&lock(s))
        .into_iter()
        .map(|r| (r.host, r.publication))
        .collect()
}

#[test]
fn seed_learns_proved_hosts_once_and_skips_guids_and_ambiguity() {
    let src = sources("");
    let (_d, s) = db();
    // A link on a custom domain with a distinct GUID: proof.
    track(&s, FEED, "guid-1", "https://letter.example/p/one");
    // E1: a GUID-as-URL on a foreign host (url == item_id): not proof.
    track(
        &s,
        FEED,
        "https://guid.example/x/2",
        "https://guid.example/x/2",
    );
    // The feed's own host: nothing to learn.
    track(&s, FEED, "guid-3", "https://feeds.example/letter/p/3");
    // A host observed under two publications: skipped.
    track(&s, FEED, "guid-4", "https://shared.example/p/4");
    track(
        &s,
        "https://other.example/feed",
        "guid-5",
        "https://shared.example/p/5",
    );
    // A host Other configures (its feed host) is reserved.
    track(&s, FEED, "guid-6", "https://other.example/p/6");
    assert_eq!(seed(&s, &src, 10), 1);
    assert_eq!(
        learned(&s),
        [("letter.example".to_owned(), "Letter".to_owned())]
    );
    // Idempotent.
    assert_eq!(seed(&s, &src, 11), 0);
    assert_eq!(learned(&s).len(), 1);
}

#[test]
fn a_host_configured_on_b_is_never_learned_for_a_on_learning_or_reload() {
    let (_d, s) = db();
    let before = sources("");
    // On learning: Other's configured host.
    assert!(!observe_feed_item(
        &mut lock(&s),
        &before,
        FEED,
        "https://other.example/p/1",
        "g1",
        1
    ));
    assert!(observe_feed_item(
        &mut lock(&s),
        &before,
        FEED,
        "https://letter.example/p/1",
        "g2",
        1
    ));
    // First proof wins: Other cannot take it.
    assert!(!observe(
        &mut lock(&s),
        &before,
        "Other",
        "letter.example",
        EVIDENCE_RSS,
        2
    ));
    // On reload: B (a new record) now configures the host; the learned row goes.
    let after = sources("[[publication]]\nname = \"B\"\nhosts = [\"www.letter.example\"]\n");
    assert_eq!(prune(&mut lock(&s), &before), 0);
    assert_eq!(prune(&mut lock(&s), &after), 1);
    assert!(learned(&s).is_empty());
    assert!(!observe(
        &mut lock(&s),
        &after,
        "Letter",
        "letter.example",
        EVIDENCE_RSS,
        3
    ));
    // A row whose publication is gone goes too.
    assert!(observe(
        &mut lock(&s),
        &before,
        "Other",
        "gone.example",
        EVIDENCE_MAIL,
        4
    ));
    let renamed = SourcesConfig::from_table(
        &format!("[[publication]]\nname = \"Letter\"\nfeeds = [\"{FEED}\"]\n")
            .parse::<toml::Table>()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(prune(&mut lock(&s), &renamed), 1);
}

/// A mail copy on `<pub>.substack.com` and an RSS copy on the custom domain match in both arrival
/// orders once the Substack host is learned.
#[test]
fn learned_host_joins_mail_and_rss_copies_in_both_orders() {
    let src = SourcesConfig::from_table(
        &"[[publication]]\nname = \"Letter\"\nfeeds = [\"https://letter.example/feed\"]\n"
            .parse::<toml::Table>()
            .unwrap(),
    )
    .unwrap();
    let mail = "https://letter.substack.com/p/a-post";
    let rss = "https://letter.example/p/a-post";
    let keys = |learned: &[LearnedRow]| {
        let pubs = crate::supersede::publication_hosts(&src, learned);
        (value_keys(mail, &pubs, None), value_keys(rss, &pubs, None))
    };
    let (m, r) = keys(&[]);
    assert!(!intersects(&m, &r));
    let (_d, s) = db();
    assert!(observe(
        &mut lock(&s),
        &src,
        "Letter",
        "letter.substack.com",
        EVIDENCE_MAIL,
        1
    ));
    let (m, r) = keys(&rows(&lock(&s)));
    assert!(intersects(&m, &r) && intersects(&r, &m));
}
