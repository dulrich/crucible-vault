//! Store tests. Vectors ported from `https://github.com/dulrich/obsidian-crucible/blob/master/tests/{sqliteJobStore,dbJobBackend,
//! jobClaimOrder,failedJobRepairRequeue,serviceHealth,queueControl}.test.mjs` where their
//! subject is not Obsidian-induced. Synthetic data only (I1).

use crate::health::{MAX_OPEN_WINDOW_MS, OPEN_WINDOW_MS, PROBE_STALE_MS};
use crate::*;
use cv_protocol::{BreakerState, FailureKind, Lane, TrackerOutcome, TrashLocation, TrashReason};
use serde_json::json;
use tempfile::TempDir;

fn db() -> (TempDir, StateDb) {
    db_with(QueueConfig::default())
}

fn db_with(cfg: QueueConfig) -> (TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let s = StateDb::open(&dir.path().join("state.db"), 1_000, cfg).expect("open");
    (dir, s)
}

fn job(t: &str, key: Option<&str>, keys: &[&str]) -> NewJob {
    NewJob {
        job_type: t.into(),
        lane: Lane::Background,
        dedupe_key: key.map(Into::into),
        params: json!({"k": key}),
        resource_keys: keys.iter().map(|s| (*s).to_owned()).collect(),
        service: None,
    }
}

fn id_of(o: EnqueueOutcome) -> String {
    match o {
        EnqueueOutcome::Enqueued(id) | EnqueueOutcome::Duplicate(id) => id,
    }
}

// ---- migrations -------------------------------------------------------------

#[test]
fn migrations_reach_latest_and_reopen_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(s.schema_version().unwrap(), migrate::latest_version());
    let id = id_of(s.enqueue(job("t", Some("a"), &[]), 1).unwrap());
    drop(s);
    let s = StateDb::open(&p, 2, QueueConfig::default()).unwrap();
    assert!(
        s.job(&id).unwrap().is_some(),
        "primary data survives reopen"
    );
}

/// WP-R1 / D-a: the finalize op ticks only NULL rows and only when `tick` is set; evidence
/// always writes.
#[test]
fn finalize_tracker_item_ticks_only_open_rows_when_authorized() {
    let (_d, mut s) = db();
    let mk = |id: &str| TrackerItem {
        source_kind: "blog".into(),
        source_key: "f".into(),
        item_id: id.into(),
        title: "T".into(),
        url: format!("u/{id}"),
        first_seen_ms: 1,
        day: "2026-10-17".into(),
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
    };
    for id in ["open", "done", "cancel", "untick"] {
        assert!(s.insert_tracker_item(&mk(id)).unwrap());
    }
    s.set_tracker_item_status("blog", "f", "done", Some('x'))
        .unwrap();
    s.set_tracker_item_status("blog", "f", "cancel", Some('-'))
        .unwrap();
    let ev = TrackerEvidence {
        access: Some("free"),
        complete: Some(true),
        full_word_count: Some(42),
    };
    assert!(
        s.finalize_tracker_item("blog", "f", "open", Some(ev), true)
            .unwrap()
    );
    assert!(
        !s.finalize_tracker_item("blog", "f", "done", Some(ev), true)
            .unwrap()
    );
    assert!(
        !s.finalize_tracker_item("blog", "f", "cancel", Some(ev), true)
            .unwrap()
    );
    assert!(
        !s.finalize_tracker_item("blog", "f", "untick", Some(ev), false)
            .unwrap()
    );
    for (id, want) in [
        ("open", Some(Some('x'))),
        ("done", Some(Some('x'))),
        ("cancel", Some(Some('-'))),
        ("untick", Some(None)),
    ] {
        assert_eq!(
            s.tracker_item_status("blog", "f", id).unwrap(),
            want,
            "{id}"
        );
        let got = s.tracker_item("blog", "f", id).unwrap().unwrap();
        assert_eq!(
            (got.access.as_deref(), got.complete, got.full_word_count),
            (Some("free"), Some(true), Some(42)),
            "{id} evidence"
        );
    }
}

#[test]
fn set_tracker_item_done_round_trips_and_signals() {
    use std::sync::{Arc, Mutex};
    let (_d, mut s) = db();
    let item = TrackerItem {
        source_kind: "blog".into(),
        source_key: "f".into(),
        item_id: "p1".into(),
        title: "T".into(),
        url: "u".into(),
        first_seen_ms: 1,
        day: "2026-10-17".into(),
        outcome: TrackerOutcome::Note,
        score: None,
        full_body: None,
        note_path: Some("notes/T.md".into()),
        source_name: None,
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    };
    assert!(s.insert_tracker_item(&item).unwrap());
    assert_eq!(
        s.tracker_item("blog", "f", "p1").unwrap(),
        Some(item.clone())
    );
    let ev = TrackerEvidence {
        access: Some("paid"),
        complete: Some(false),
        full_word_count: Some(900),
    };
    assert!(
        !s.finalize_tracker_item("blog", "f", "p1", Some(ev), false)
            .unwrap()
    );
    let got = s.tracker_item("blog", "f", "p1").unwrap().unwrap();
    assert_eq!(
        (got.access.as_deref(), got.complete, got.full_word_count),
        (Some("paid"), Some(false), Some(900))
    );
    assert!(
        !s.finalize_tracker_item("blog", "f", "p1", Some(TrackerEvidence::default()), false)
            .unwrap()
    );
    assert_eq!(s.tracker_item("blog", "f", "p1").unwrap(), Some(item));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    s.set_change_hook(Box::new(move |t| sink.lock().unwrap().push(t)));
    assert!(s.set_tracker_item_done("blog", "f", "p1", true).unwrap());
    assert_eq!(s.tracker_item_done("blog", "f", "p1").unwrap(), Some(true));
    assert!(s.set_tracker_item_done("blog", "f", "p1", false).unwrap());
    assert_eq!(s.tracker_item_done("blog", "f", "p1").unwrap(), Some(false));
    assert!(!s.set_tracker_item_done("blog", "f", "nope", true).unwrap());
    assert_eq!(s.tracker_item_done("blog", "f", "nope").unwrap(), None);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["tracker_items", "tracker_items"]
    );
}

#[test]
fn tracker_items_for_item_matches_kind_and_id_across_source_keys() {
    let (_d, mut s) = db();
    let mk = |kind: &str, key: &str, id: &str, ms: i64| TrackerItem {
        source_kind: kind.into(),
        source_key: key.into(),
        item_id: id.into(),
        title: "T".into(),
        url: "u".into(),
        first_seen_ms: ms,
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
    };
    for it in [
        mk("youtube", "UC2", "vid", 2),
        mk("youtube", "UC1", "vid", 1),
        mk("youtube", "UC1", "other", 3),
        mk("blog", "f", "vid", 4),
    ] {
        assert!(s.insert_tracker_item(&it).unwrap());
    }
    let got: Vec<String> = s
        .tracker_items_for_item("youtube", "vid")
        .unwrap()
        .into_iter()
        .map(|r| r.source_key)
        .collect();
    assert_eq!(got, vec!["UC1", "UC2"]);
    assert!(
        s.tracker_items_for_item("youtube", "none")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn fill_tracker_source_name_fills_only_null_rows_of_that_source() {
    use std::sync::{Arc, Mutex};
    let (_d, mut s) = db();
    let mk = |key: &str, id: &str, name: Option<&str>| TrackerItem {
        source_kind: "blog".into(),
        source_key: key.into(),
        item_id: id.into(),
        title: "T".into(),
        url: "u".into(),
        first_seen_ms: 1,
        day: "2026-10-17".into(),
        outcome: TrackerOutcome::Line,
        score: None,
        full_body: None,
        note_path: None,
        source_name: name.map(str::to_owned),
        duration_s: Some(5),
        word_count: Some(7),
        access: None,
        complete: None,
        full_word_count: None,
    };
    for it in [
        mk("f", "p1", None),
        mk("f", "p2", Some("Kept")),
        mk("g", "p3", None),
    ] {
        assert!(s.insert_tracker_item(&it).unwrap());
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    s.set_change_hook(Box::new(move |t| sink.lock().unwrap().push(t)));
    assert_eq!(s.fill_tracker_source_name("blog", "f", "Feed").unwrap(), 1);
    assert_eq!(s.fill_tracker_source_name("blog", "f", "Feed").unwrap(), 0);
    let name = |k: &str, i: &str| s.tracker_item("blog", k, i).unwrap().unwrap().source_name;
    assert_eq!(name("f", "p1").as_deref(), Some("Feed"));
    assert_eq!(name("f", "p2").as_deref(), Some("Kept"));
    assert_eq!(name("g", "p3"), None);
    let it = s.tracker_item("blog", "f", "p1").unwrap().unwrap();
    assert_eq!((it.duration_s, it.word_count), (Some(5), Some(7)));
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["tracker_items"],
        "signal only on change"
    );
}

/// `sqlite_master` + `user_version`, in the fixture's format.
fn schema_dump(c: &rusqlite::Connection) -> String {
    let mut st = c
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
        .unwrap();
    let rows: Vec<String> = st
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?
                    .unwrap_or_else(|| "NULL".into())
            ))
        })
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    let v = migrate::current_version(c).unwrap();
    format!("user_version={v}\n{}\n", rows.join("\n"))
}

/// D31: a fresh database gets exactly the schema the retired v15 baseline + v16 + v17 ladder produced.
#[test]
fn fresh_baseline_matches_the_v17_chain_schema() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(migrate::BASELINE).unwrap();
    c.pragma_update(None, "user_version", migrate::BASELINE_VERSION)
        .unwrap();
    assert_eq!(
        schema_dump(&c),
        include_str!("../tests/fixtures/baseline-v17-schema.txt")
    );
}

/// Plan note-identity WP-6: a v18 database (baseline + the note-id step) migrates to v19, which
/// drops both path-history tables and leaves the note-id table (and its rows) alone.
#[test]
fn v18_database_migrates_to_v19_without_the_path_history_tables() {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(migrate::BASELINE).unwrap();
    c.execute_batch(migrate::MIGRATIONS[0]).unwrap();
    c.pragma_update(None, "user_version", 18).unwrap();
    c.execute("INSERT INTO note_ids (path) VALUES ('a.md')", [])
        .unwrap();
    let tables = |c: &rusqlite::Connection| -> Vec<String> {
        c.prepare(
            r"SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'path\_%' ESCAPE '\'",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect()
    };
    assert_eq!(tables(&c).len(), 2, "the v18 database still has both");
    assert_eq!(migrate::migrate(&mut c).unwrap(), 19);
    assert!(tables(&c).is_empty(), "v19 drops both path-history tables");
    let ids: i64 = c
        .query_row("SELECT COUNT(*) FROM note_ids", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ids, 1, "note-id rows survive");
}

/// A database at the latest version opens with no write.
#[test]
fn baseline_database_opens_without_a_schema_write() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    {
        let mut c = rusqlite::Connection::open(&p).unwrap();
        migrate::migrate(&mut c).unwrap();
    }
    let mut c = rusqlite::Connection::open(&p).unwrap();
    let before = schema_dump(&c);
    assert_eq!(migrate::migrate(&mut c).unwrap(), migrate::latest_version());
    assert_eq!(c.total_changes(), 0, "no row written");
    assert_eq!(schema_dump(&c), before, "no schema written");
    drop(c);
    let s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(s.schema_version().unwrap(), migrate::latest_version());
}

/// D31: versions 1..=16 predate the 1.0.0 baseline and are refused, by writer and reader.
#[test]
fn pre_baseline_versions_are_refused() {
    for v in 1..migrate::BASELINE_VERSION {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.db");
        {
            let c = rusqlite::Connection::open(&p).unwrap();
            c.pragma_update(None, "user_version", v).unwrap();
        }
        let err = StateDb::open(&p, 1, QueueConfig::default()).unwrap_err();
        assert!(
            matches!(err, StateError::PreBaseline { found, baseline: 17 } if found == v),
            "{err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("version {v} ")) && msg.contains("predates the 1.0.0 baseline"),
            "{msg}"
        );
        assert!(matches!(
            StateReader::open(&p),
            Err(StateError::PreBaseline { .. })
        ));
    }
}

#[test]
fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(matches!(
        StateDb::open(&p, 1, QueueConfig::default()),
        Err(StateError::NewerSchema { found: 99, .. })
    ));
    assert!(matches!(
        StateReader::open(&p),
        Err(StateError::NewerSchema { .. })
    ));
}

// ---- claim / ordering (jobClaimOrder, sqliteJobStore) -----------------------

#[test]
fn user_lane_claimed_before_older_background_then_oldest_id() {
    let (_d, mut s) = db();
    let bg = id_of(s.enqueue(job("t", None, &[]), 10).unwrap());
    let mut u = job("t", None, &[]);
    u.lane = Lane::User;
    let user = id_of(s.enqueue(u, 20).unwrap());
    let bg2 = id_of(s.enqueue(job("t", None, &[]), 30).unwrap());
    assert_eq!(s.claim(100).unwrap().unwrap().id, user);
    assert_eq!(s.claim(100).unwrap().unwrap().id, bg);
    assert_eq!(s.claim(100).unwrap().unwrap().id, bg2);
    assert!(s.claim(100).unwrap().is_none());
}

#[test]
fn list_in_claim_order_matches_claim_head() {
    let (_d, mut s) = db();
    for i in 0..5 {
        s.enqueue(job("t", None, &[]), i).unwrap();
    }
    let head = s
        .list(&ListQuery {
            status: Some(JobStatus::Queued),
            ..Default::default()
        })
        .unwrap()[0]
        .id
        .clone();
    assert_eq!(s.claim(10).unwrap().unwrap().id, head);
    let page = s
        .list(&ListQuery {
            status: Some(JobStatus::Queued),
            limit: 2,
            offset: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.len(), 2);
}

#[test]
fn two_claimers_never_win_the_same_job() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut a = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let mut b = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_ne!(a.claim_token(), b.claim_token());
    a.enqueue(job("t", None, &[]), 1).unwrap();
    a.enqueue(job("t", None, &[]), 2).unwrap();
    let ja = a.claim(5).unwrap().unwrap();
    let jb = b.claim(5).unwrap().unwrap();
    assert_ne!(ja.id, jb.id);
    assert!(a.claim(5).unwrap().is_none());
    assert_eq!(ja.claim_token.as_deref(), Some(a.claim_token()));
}

#[test]
fn i3_overlapping_resource_keys_never_run_concurrently() {
    let (_d, mut s) = db();
    let a = id_of(
        s.enqueue(job("t", None, &["notes/a.md", "yt-video::1"]), 1)
            .unwrap(),
    );
    let b = id_of(s.enqueue(job("t", None, &["notes/a.md"]), 2).unwrap());
    let c = id_of(s.enqueue(job("t", None, &["notes/c.md"]), 3).unwrap());
    assert_eq!(s.claim(10).unwrap().unwrap().id, a);
    // b overlaps the running a: the disjoint c is claimed instead.
    assert_eq!(s.claim(10).unwrap().unwrap().id, c);
    assert!(s.claim(10).unwrap().is_none());
    s.settle(&a, &JobResult::Done, 11).unwrap();
    assert_eq!(s.claim(12).unwrap().unwrap().id, b);
}

#[test]
fn deferral_skips_until_due_and_reports_wake() {
    let (_d, mut s) = db();
    let id = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.claim(2).unwrap().unwrap();
    s.settle(
        &id,
        &JobResult::Deferred {
            retry_after_ms: 100,
            service_unhealthy: Some("svc".into()),
        },
        10,
    )
    .unwrap();
    assert_eq!(s.next_wake(10).unwrap(), Some(110));
    assert!(s.claim(109).unwrap().is_none());
    let j = s.claim(110).unwrap().unwrap();
    assert_eq!(
        j.defer_until_ms, None,
        "transition out of queued clears defer_until"
    );
    assert_eq!(j.attempts, 2);
}

// ---- dedupe --------------------------------------------------------------

#[test]
fn dedupe_is_type_namespaced_exact_and_pending_only() {
    let (_d, mut s) = db();
    let a = id_of(s.enqueue(job("t", Some("note:x"), &[]), 1).unwrap());
    assert_eq!(
        s.enqueue(job("t", Some("note:x"), &[]), 2).unwrap(),
        EnqueueOutcome::Duplicate(a.clone())
    );
    assert!(matches!(
        s.enqueue(job("u", Some("note:x"), &[]), 2).unwrap(),
        EnqueueOutcome::Enqueued(_)
    ));
    assert_eq!(
        s.job(&a).unwrap().unwrap().dedupe_key.as_deref(),
        Some("t::note:x")
    );
    // running still counts as pending
    s.claim(3).unwrap();
    assert_eq!(
        s.enqueue(job("t", Some("note:x"), &[]), 3).unwrap(),
        EnqueueOutcome::Duplicate(a.clone())
    );
    s.settle(&a, &JobResult::Done, 4).unwrap();
    assert!(matches!(
        s.enqueue(job("t", Some("note:x"), &[]), 4).unwrap(),
        EnqueueOutcome::Enqueued(_)
    ));
    // empty keys never collapse
    let e1 = id_of(s.enqueue(job("t", Some(""), &[]), 5).unwrap());
    let e2 = id_of(s.enqueue(job("t", Some(""), &[]), 5).unwrap());
    assert_ne!(e1, e2);
    assert_eq!(s.job(&e1).unwrap().unwrap().dedupe_key, None);
}

#[path = "tests_tracker_trash.rs"]
mod tracker_trash;

#[path = "tests_enums.rs"]
mod enums;

#[path = "tests_health_reader.rs"]
mod health_reader;

#[path = "tests_settle_recovery.rs"]
mod settle_recovery;

#[path = "tests_telemetry.rs"]
mod telemetry;
