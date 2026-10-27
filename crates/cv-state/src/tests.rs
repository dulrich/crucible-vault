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

/// D27: a fresh database gets exactly the schema the retired v11 baseline + v12–v15 chain produced.
#[test]
fn fresh_baseline_matches_the_v15_chain_schema() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(migrate::BASELINE).unwrap();
    c.pragma_update(None, "user_version", migrate::BASELINE_VERSION)
        .unwrap();
    assert_eq!(
        schema_dump(&c),
        include_str!("../tests/fixtures/baseline-v15-schema.txt")
    );
}

/// A database already at the latest version opens with no write.
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

/// D27: versions 1..=14 predate the 0.2.0 baseline and are refused, by writer and reader.
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
            matches!(err, StateError::PreBaseline { found, baseline: 15 } if found == v),
            "{err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("version {v} ")) && msg.contains("predates the 0.2.0 baseline"),
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

// ---- settle / result union ----------------------------------------------------

#[test]
fn settle_stamps_failure_kind_and_clears_claim() {
    let (_d, mut s) = db();
    let a = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    let b = id_of(s.enqueue(job("t", None, &[]), 2).unwrap());
    s.claim(3).unwrap();
    s.claim(3).unwrap();
    s.settle(
        &a,
        &JobResult::Failed {
            error: "down".into(),
            reason: Some(FailureReason::ServiceOutage),
        },
        4,
    )
    .unwrap();
    s.settle(
        &b,
        &JobResult::Failed {
            error: "bad".into(),
            reason: Some(FailureReason::NoApiKey),
        },
        4,
    )
    .unwrap();
    let ja = s.job(&a).unwrap().unwrap();
    assert_eq!(
        (ja.status, ja.failure_kind, ja.settled_at_ms),
        (JobStatus::Failed, Some(FailureKind::Service), Some(4))
    );
    assert_eq!((ja.claim_token, ja.claimed_at_ms), (None, None));
    let jb = s.job(&b).unwrap().unwrap();
    assert_eq!(
        (jb.failure_kind, jb.failure_reason),
        (Some(FailureKind::Job), Some(FailureReason::NoApiKey))
    );
    // settling a non-running job is refused
    assert!(matches!(
        s.settle(&a, &JobResult::Done, 5),
        Err(StateError::WrongStatus { .. })
    ));
    assert!(matches!(
        s.settle("nope", &JobResult::Done, 5),
        Err(StateError::NoSuchJob(_))
    ));
}

#[test]
fn result_union_round_trips_as_tagged_json() {
    let r = JobResult::Deferred {
        retry_after_ms: 5,
        service_unhealthy: None,
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["status"], "deferred");
    assert_eq!(serde_json::from_value::<JobResult>(v).unwrap(), r);
}

#[test]
fn settle_requires_this_handles_token() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut a = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let mut b = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let id = id_of(a.enqueue(job("t", None, &[]), 1).unwrap());
    a.claim(2).unwrap();
    assert!(b.settle(&id, &JobResult::Done, 3).is_err());
    a.settle(&id, &JobResult::Done, 3).unwrap();
}

// ---- recovery / retention -----------------------------------------------------

#[test]
fn dead_token_leases_recover_regardless_of_age() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut dead = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let id = id_of(dead.enqueue(job("t", None, &["x"]), 1).unwrap());
    dead.claim(1_000).unwrap();
    drop(dead);
    let mut live = StateDb::open(&p, 1_001, QueueConfig::default()).unwrap();
    assert_eq!(live.recover_foreign_leases().unwrap(), 1);
    assert_eq!(live.job(&id).unwrap().unwrap().status, JobStatus::Queued);
    live.claim(1_002).unwrap();
    // foreign recovery leaves this handle's own lease alone
    assert_eq!(live.recover_foreign_leases().unwrap(), 0);
}

#[test]
fn live_leases_recover_only_after_timeout() {
    let (_d, mut s) = db_with(QueueConfig {
        stale_lease_ms: 100,
        ..QueueConfig::default()
    });
    let id = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.claim(1_000).unwrap();
    let none = std::collections::HashSet::new();
    assert_eq!(
        s.recover_stale_leases_except(1_100, &none).unwrap(),
        0,
        "boundary kept"
    );
    assert_eq!(s.recover_stale_leases_except(1_101, &none).unwrap(), 1);
    assert_eq!(s.job(&id).unwrap().unwrap().status, JobStatus::Queued);
}

#[test]
fn stale_recovery_skips_live_ids() {
    let (_d, mut s) = db_with(QueueConfig {
        stale_lease_ms: 100,
        ..QueueConfig::default()
    });
    let a = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    let b = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.claim(1_000).unwrap();
    s.claim(1_000).unwrap();
    let live: std::collections::HashSet<String> = [a.clone()].into_iter().collect();
    assert_eq!(s.recover_stale_leases_except(5_000, &live).unwrap(), 1);
    assert_eq!(s.job(&a).unwrap().unwrap().status, JobStatus::Running);
    assert_eq!(s.job(&b).unwrap().unwrap().status, JobStatus::Queued);
}

#[test]
fn set_retention_changes_prune_cutoff() {
    let (_d, mut s) = db_with(QueueConfig {
        retention_ms: 0,
        ..QueueConfig::default()
    });
    let x = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.cancel(&x, 10).unwrap();
    assert_eq!(s.prune_terminal(1_000).unwrap(), 0, "0 keeps forever");
    s.set_retention_ms(100);
    assert_eq!(s.prune_terminal(1_000).unwrap(), 1);
}

#[test]
fn deferred_settle_stamps_service() {
    let (_d, mut s) = db();
    let id = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.claim(1).unwrap();
    s.settle(
        &id,
        &JobResult::Deferred {
            retry_after_ms: 10,
            service_unhealthy: Some("svc".into()),
        },
        2,
    )
    .unwrap();
    assert_eq!(s.job(&id).unwrap().unwrap().service.as_deref(), Some("svc"));
}

#[test]
fn prune_terminal_respects_cutoff_boundary_and_keep_forever() {
    let (_d, mut s) = db_with(QueueConfig {
        retention_ms: 100,
        ..QueueConfig::default()
    });
    let old = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    let edge = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    let live = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.cancel(&old, 10).unwrap();
    s.cancel(&edge, 100).unwrap();
    assert_eq!(s.prune_terminal(200).unwrap(), 1);
    assert!(s.job(&old).unwrap().is_none());
    assert!(s.job(&edge).unwrap().is_some());
    assert!(s.job(&live).unwrap().is_some());
    assert_eq!(DEFAULT_RETENTION_MS, 30 * 86_400_000);
    let (_d2, mut forever) = db_with(QueueConfig {
        retention_ms: 0,
        ..QueueConfig::default()
    });
    let x = id_of(forever.enqueue(job("t", None, &[]), 1).unwrap());
    forever.cancel(&x, 1).unwrap();
    assert_eq!(forever.prune_terminal(i64::MAX / 2).unwrap(), 0);
}

// ---- outage requeue (failedJobRepairRequeue) ------------------------------------

fn fail(
    s: &mut StateDb,
    t: &str,
    key: Option<&str>,
    reason: Option<FailureReason>,
    now: i64,
) -> String {
    let id = id_of(s.enqueue(job(t, key, &[]), now).unwrap());
    let got = s.claim(now).unwrap().unwrap();
    assert_eq!(got.id, id);
    s.settle(
        &id,
        &JobResult::Failed {
            error: "connection refused".into(),
            reason,
        },
        now,
    )
    .unwrap();
    id
}

#[test]
fn requeue_moves_service_failures_only_and_dry_run_writes_nothing() {
    let (_d, mut s) = db();
    let s1 = fail(&mut s, "a", None, Some(FailureReason::ServiceOutage), 1);
    let _s2 = fail(&mut s, "b", None, Some(FailureReason::ServiceOutage), 2);
    // genuine failure with outage-sounding text: selection reads the column, not the text
    let g = fail(&mut s, "a", None, None, 3);
    let q = id_of(s.enqueue(job("a", None, &[]), 4).unwrap());

    let dry = s.requeue_service_failures(true).unwrap();
    assert_eq!((dry.total, dry.requeued), (2, 0));
    assert_eq!(dry.by_type.get("a"), Some(&1));
    assert_eq!(s.job(&s1).unwrap().unwrap().status, JobStatus::Failed);

    let real = s.requeue_service_failures(false).unwrap();
    assert_eq!(real.requeued, 2);
    let j = s.job(&s1).unwrap().unwrap();
    assert_eq!(
        (
            j.status,
            j.error,
            j.failure_kind,
            j.settled_at_ms,
            j.attempts
        ),
        (JobStatus::Queued, None, None, None, 0),
        "requeued rows get a fresh retry budget"
    );
    assert_eq!(s.job(&g).unwrap().unwrap().status, JobStatus::Failed);
    assert_eq!(s.job(&q).unwrap().unwrap().status, JobStatus::Queued);
    assert!(
        s.claim(5).unwrap().is_some(),
        "requeued job is immediately claimable"
    );
    let empty = s.requeue_service_failures(false).unwrap();
    assert_eq!(empty, RequeueBreakdown::default());
}

#[test]
fn requeue_skips_a_service_failure_whose_key_is_pending_again() {
    let (_d, mut s) = db();
    let f = fail(
        &mut s,
        "a",
        Some("k"),
        Some(FailureReason::ServiceOutage),
        1,
    );
    s.enqueue(job("a", Some("k"), &[]), 2).unwrap();
    assert_eq!(s.requeue_service_failures(false).unwrap().requeued, 0);
    assert_eq!(s.job(&f).unwrap().unwrap().status, JobStatus::Failed);
}

// ---- service health breaker (serviceHealth) -------------------------------------

#[test]
fn breaker_pure_transitions() {
    let mut h = ServiceHealth::fresh("svc");
    h.report_failure(ServiceFailureKind::Timeout, "t", None, 0);
    h.report_failure(ServiceFailureKind::Timeout, "t", None, 0);
    assert_eq!(h.state, BreakerState::Closed, "two do not open");
    h.report_success();
    h.report_failure(ServiceFailureKind::Timeout, "t", None, 0);
    h.report_failure(ServiceFailureKind::Timeout, "t", None, 0);
    assert_eq!(
        h.state,
        BreakerState::Closed,
        "failures must be consecutive"
    );
    h.report_failure(ServiceFailureKind::ServerError, "t", None, 0);
    assert_eq!(
        (h.state, h.retry_at_ms),
        (BreakerState::Open, Some(OPEN_WINDOW_MS))
    );
    // failure while open does not push recovery away
    h.report_failure(ServiceFailureKind::Timeout, "later", None, 10);
    assert_eq!(
        (h.retry_at_ms, h.last_reason.as_deref()),
        (Some(OPEN_WINDOW_MS), Some("later"))
    );
    h.tick(OPEN_WINDOW_MS - 1);
    assert_eq!(h.state, BreakerState::Open);
    h.tick(OPEN_WINDOW_MS);
    assert_eq!(h.state, BreakerState::HalfOpen);
    assert!(h.try_acquire_probe(OPEN_WINDOW_MS));
    assert!(!h.try_acquire_probe(OPEN_WINDOW_MS), "exactly one probe");
    // failed probe re-opens with a doubled window
    h.report_failure(ServiceFailureKind::Timeout, "t", None, 100_000);
    assert_eq!(
        (h.state, h.retry_at_ms),
        (BreakerState::Open, Some(100_000 + 2 * OPEN_WINDOW_MS))
    );
    for _ in 0..10 {
        h.tick(i64::MAX / 4);
        h.report_failure(ServiceFailureKind::Timeout, "t", None, 0);
    }
    assert_eq!(
        h.retry_at_ms,
        Some(MAX_OPEN_WINDOW_MS),
        "capped at ten minutes"
    );
    h.tick(i64::MAX / 4);
    h.report_success();
    assert_eq!(
        (h.state, h.open_count),
        (BreakerState::Closed, 0),
        "success closes outright and resets backoff"
    );
}

#[test]
fn breaker_refused_counts_double_and_retry_after_overrides() {
    let mut h = ServiceHealth::fresh("svc");
    h.report_failure(ServiceFailureKind::Refused, "r", None, 0);
    h.report_failure(ServiceFailureKind::Refused, "r", Some(20 * 60_000), 0);
    assert_eq!(
        (h.state, h.retry_at_ms),
        (BreakerState::Open, Some(20 * 60_000)),
        "long retry-after honoured past the cap"
    );
    let mut g = ServiceHealth::fresh("svc");
    for _ in 0..3 {
        g.report_failure(ServiceFailureKind::RateLimited, "q", Some(-5), 0);
    }
    assert_eq!(
        g.retry_at_ms,
        Some(OPEN_WINDOW_MS),
        "nonsense retry-after ignored"
    );
}

#[test]
fn stale_probe_token_is_reclaimed() {
    let mut h = ServiceHealth::fresh("svc");
    for _ in 0..3 {
        h.report_failure(ServiceFailureKind::Timeout, "t", None, 0);
    }
    h.tick(OPEN_WINDOW_MS);
    assert!(h.try_acquire_probe(OPEN_WINDOW_MS));
    h.tick(OPEN_WINDOW_MS + PROBE_STALE_MS);
    assert!(h.probe_in_flight());
    h.tick(OPEN_WINDOW_MS + PROBE_STALE_MS + 1);
    assert!(!h.probe_in_flight());
}

#[test]
fn breaker_persists_and_gates_claims_per_service() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    for _ in 0..3 {
        s.report_service_failure("svc", ServiceFailureKind::Timeout, "t", None, 0)
            .unwrap();
    }
    let mut gated = job("t", None, &[]);
    gated.service = Some("svc".into());
    let g1 = id_of(s.enqueue(gated.clone(), 1).unwrap());
    let _g2 = id_of(s.enqueue(gated, 2).unwrap());
    let mut other = job("t", None, &[]);
    other.service = Some("other".into());
    let o = id_of(s.enqueue(other, 3).unwrap());
    // independent services: the open one is skipped, the other claimed
    assert_eq!(s.claim(10).unwrap().unwrap().id, o);
    assert!(s.claim(10).unwrap().is_none());
    assert_eq!(
        s.service_health("other").unwrap(),
        ServiceHealth::fresh("other")
    );
    drop(s);
    // persisted across reopen; half-open admits exactly one probe
    let mut s = StateDb::open(&p, 2, QueueConfig::default()).unwrap();
    assert_eq!(s.service_health("svc").unwrap().state, BreakerState::Open);
    assert_eq!(s.claim(OPEN_WINDOW_MS).unwrap().unwrap().id, g1);
    assert!(s.claim(OPEN_WINDOW_MS).unwrap().is_none());
    s.settle(&g1, &JobResult::Done, OPEN_WINDOW_MS + 1).unwrap();
    s.report_service_success("svc").unwrap();
    assert!(s.claim(OPEN_WINDOW_MS + 2).unwrap().is_some());
    let r = StateReader::open(&p).unwrap();
    assert_eq!(r.service_health().unwrap()[0].state, BreakerState::Closed);
}

// ---- read-only client ---------------------------------------------

#[test]
fn reader_is_read_only_and_sees_queue_status() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    assert!(
        StateReader::open(&p).is_err(),
        "reader never creates the db"
    );
    let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let a = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.enqueue(job("t", None, &[]), 2).unwrap();
    s.claim(3).unwrap();
    let r = StateReader::open(&p).unwrap();
    let counts = r.counts().unwrap();
    assert!(counts.contains(&StatusCount {
        job_type: "t".into(),
        status: JobStatus::Running,
        count: 1
    }));
    assert!(counts.contains(&StatusCount {
        job_type: "t".into(),
        status: JobStatus::Queued,
        count: 1
    }));
    assert_eq!(r.job(&a).unwrap().unwrap().status, JobStatus::Running);
    assert_eq!(r.list(&ListQuery::default()).unwrap().len(), 2);
    // writes through the read-only handle fail
    assert!(r.conn_for_test().execute("DELETE FROM jobs", []).is_err());
}

#[test]
fn corrupt_json_columns_read_as_safe_defaults() {
    let (_d, mut s) = db();
    let id = id_of(s.enqueue(job("t", None, &["x"]), 1).unwrap());
    s.conn()
        .execute(
            "UPDATE jobs SET params = '{bad', resource_keys = 'nope' WHERE id = ?1",
            [&id],
        )
        .unwrap();
    let j = s.job(&id).unwrap().unwrap();
    assert_eq!(
        (j.params, j.resource_keys),
        (serde_json::Value::Null, Vec::<String>::new())
    );
    assert_eq!(s.list(&ListQuery::default()).unwrap().len(), 1);
}

#[test]
fn unparseable_resource_keys_fail_closed_at_claim() {
    // I3: a candidate whose keys cannot be read is never claimed; a disjoint job still is.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let bad = id_of(s.enqueue(job("t", Some("bad"), &["note.md"]), 1).unwrap());
    let good = id_of(s.enqueue(job("t", Some("good"), &["other.md"]), 2).unwrap());
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute(
            "UPDATE jobs SET resource_keys = 'not json' WHERE id = ?1",
            [&bad],
        )
        .unwrap();
    let claimed = s.claim(3).unwrap().expect("disjoint job claimable");
    assert_eq!(claimed.id, good);
    assert!(
        s.claim(4).unwrap().is_none(),
        "corrupt-keys job not claimed"
    );
    assert!(s.job(&bad).unwrap().is_some(), "still listed");
}

/// SR10: a scheduled enqueue writes the job and the `schedule_runs` row in one transaction —
/// both on success, neither when the transaction fails (the "crash between them" window is gone).
#[test]
fn enqueue_scheduled_writes_job_and_schedule_run_atomically() {
    let (dir, mut s) = db();
    let id = id_of(
        s.enqueue_scheduled(job("action:a", Some("a::schedule"), &[]), "a", 5)
            .unwrap(),
    );
    assert!(s.job(&id).unwrap().is_some());
    assert_eq!(s.schedule_last_run("a").unwrap(), Some(5));
    // Make the schedule_runs write fail: the job insert must roll back with it.
    rusqlite::Connection::open(dir.path().join("state.db"))
        .unwrap()
        .execute_batch("DROP TABLE schedule_runs;")
        .unwrap();
    let before = s.list(&ListQuery::default()).unwrap().len();
    assert!(
        s.enqueue_scheduled(job("action:b", Some("b::schedule"), &[]), "b", 6)
            .is_err()
    );
    assert_eq!(s.list(&ListQuery::default()).unwrap().len(), before);
}

/// SR10: a cancel arriving right after the claim (before the runner starts the steps) is
/// accepted — no WrongStatus/INVALID_PARAMS — and is durable: the runner sees the flag, and
/// even a deferral settles the job as cancelled.
#[test]
fn cancel_right_after_claim_is_durable_and_ends_cancelled() {
    let (_d, mut s) = db();
    let id = id_of(s.enqueue(job("t", None, &[]), 1_000).unwrap());
    let claimed = s.claim(1_001).unwrap().expect("claimed");
    assert_eq!(claimed.id, id);
    s.cancel(&id, 1_002).unwrap();
    assert!(s.cancel_requested(&id).unwrap());
    s.settle(
        &id,
        &JobResult::Deferred {
            retry_after_ms: 10,
            service_unhealthy: None,
        },
        1_003,
    )
    .unwrap();
    let j = s.job(&id).unwrap().unwrap();
    assert_eq!(j.status, JobStatus::Cancelled);
    assert!(!s.cancel_requested(&id).unwrap(), "flag cleared on settle");
    assert!(matches!(
        s.cancel(&id, 1_004),
        Err(StateError::WrongStatus { .. })
    ));
}

// ---- stored forms of the protocol enums (WP-X3: byte-identical to the retired mirrors) --------

#[test]
fn protocol_enums_keep_stored_forms_and_corrupt_handling() {
    let (_d, mut s) = db();
    let raw_str = |s: &StateDb, sql: &str, key: &str| -> String {
        s.conn().query_row(sql, [key], |r| r.get(0)).unwrap()
    };
    // lane: INTEGER rank, User=0 / Background=1.
    for (lane, rank) in [(Lane::User, 0_i64), (Lane::Background, 1)] {
        let id = id_of(
            s.enqueue(
                NewJob {
                    lane,
                    ..job("t", None, &[])
                },
                1,
            )
            .unwrap(),
        );
        let got: i64 = s
            .conn()
            .query_row("SELECT lane FROM jobs WHERE id = ?1", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(got, rank);
        assert_eq!(s.job(&id).unwrap().unwrap().lane, lane);
        s.cancel(&id, 1).unwrap();
    }
    // failure_kind: settled through the store.
    for (reason, kind, text) in [
        (
            Some(FailureReason::ServiceOutage),
            FailureKind::Service,
            "service",
        ),
        (None, FailureKind::Job, "job"),
    ] {
        let id = id_of(s.enqueue(job("f", None, &[]), 2).unwrap());
        assert_eq!(s.claim(2).unwrap().unwrap().id, id);
        let r = JobResult::Failed {
            error: "e".into(),
            reason,
        };
        s.settle(&id, &r, 3).unwrap();
        let sql = "SELECT failure_kind FROM jobs WHERE id = ?1";
        assert_eq!(raw_str(&s, sql, &id), text);
        assert_eq!(s.job(&id).unwrap().unwrap().failure_kind, Some(kind));
    }
    // tracker outcome.
    for (o, text) in [
        (TrackerOutcome::Line, "line"),
        (TrackerOutcome::Note, "note"),
        (TrackerOutcome::Ignored, "ignored"),
        (TrackerOutcome::Skipped, "skipped"),
    ] {
        let item = TrackerItem {
            source_kind: "blog".into(),
            source_key: "k".into(),
            item_id: text.into(),
            title: "T".into(),
            url: "u".into(),
            first_seen_ms: 1,
            day: "2026-10-17".into(),
            outcome: o,
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
        assert!(s.insert_tracker_item(&item).unwrap());
        let sql = "SELECT outcome FROM tracker_items WHERE item_id = ?1";
        assert_eq!(raw_str(&s, sql, text), text);
        let back = s.tracker_item("blog", "k", text).unwrap().unwrap();
        assert_eq!(back.outcome, o);
    }
    // breaker state.
    for (st, text) in [
        (BreakerState::Closed, "closed"),
        (BreakerState::Open, "open"),
        (BreakerState::HalfOpen, "half_open"),
    ] {
        let mut h = ServiceHealth::fresh(text);
        h.state = st;
        crate::health::store(s.conn(), &h).unwrap();
        let sql = "SELECT state FROM service_health WHERE service = ?1";
        assert_eq!(raw_str(&s, sql, text), text);
        assert_eq!(s.service_health(text).unwrap().state, st);
    }
    // Pre-existing bad raw values: lane / failure_kind / breaker state are Corrupt; an unknown
    // outcome string parses to `None` (the read path then falls back to `Skipped`).
    let bad = id_of(s.enqueue(job("b", None, &[]), 4).unwrap());
    let c = s.conn();
    c.execute("UPDATE jobs SET lane = 7 WHERE id = ?1", [&bad])
        .unwrap();
    assert!(matches!(
        s.job(&bad),
        Err(StateError::Corrupt { column: "lane", .. })
    ));
    c.execute(
        "UPDATE jobs SET lane = 1, failure_kind = 'x' WHERE id = ?1",
        [&bad],
    )
    .unwrap();
    assert!(matches!(
        s.job(&bad),
        Err(StateError::Corrupt {
            column: "failure_kind",
            ..
        })
    ));
    c.execute(
        "UPDATE service_health SET state = 'x' WHERE service = 'open'",
        [],
    )
    .unwrap();
    assert!(matches!(
        s.service_health("open"),
        Err(StateError::Corrupt {
            column: "service_health.state",
            ..
        })
    ));
    // `tracker_items.outcome` has a CHECK constraint, so a bad stored outcome cannot exist.
    assert_eq!(TrackerOutcome::parse("x"), None);
}

fn mail_row(key: &str, id: &str) -> TrackerItem {
    TrackerItem {
        source_kind: "mail".into(),
        source_key: key.into(),
        item_id: id.into(),
        title: "T".into(),
        url: "u".into(),
        first_seen_ms: 1,
        day: "2026-10-21".into(),
        outcome: TrackerOutcome::Note,
        score: None,
        full_body: None,
        note_path: None,
        source_name: Some("Unknown sender a@b.example".into()),
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    }
}

/// Plan intake-followups-mail-youtube WP-5 (audit E2): a re-key without a row at the new key
/// updates it in place, keeping status, `note_path` and evidence; a second call is a no-op.
#[test]
fn rekey_moves_a_row_in_place_and_is_idempotent() {
    let (_d, mut s) = db();
    let mut it = mail_row("unknown-sender", "m1");
    it.note_path = Some("n/a.md".into());
    it.complete = Some(true);
    s.insert_tracker_item(&it).unwrap();
    s.set_tracker_item_status("mail", "unknown-sender", "m1", Some('x'))
        .unwrap();
    let r = s
        .rekey_tracker_item("mail", "unknown-sender", "m1", "Newsbit", "Newsbit")
        .unwrap();
    assert_eq!(r, Rekey::Moved);
    assert!(
        s.tracker_item("mail", "unknown-sender", "m1")
            .unwrap()
            .is_none()
    );
    let got = s.tracker_item("mail", "Newsbit", "m1").unwrap().unwrap();
    assert_eq!(
        (
            got.note_path.as_deref(),
            got.complete,
            got.source_name.as_deref()
        ),
        (Some("n/a.md"), Some(true), Some("Newsbit"))
    );
    assert_eq!(
        s.tracker_item_status("mail", "Newsbit", "m1").unwrap(),
        Some(Some('x'))
    );
    let again = s
        .rekey_tracker_item("mail", "unknown-sender", "m1", "Newsbit", "Newsbit")
        .unwrap();
    assert_eq!(again, Rekey::Absent);
    assert_eq!(
        s.tracker_rows_of_source("mail", "Newsbit").unwrap().len(),
        1
    );
}

/// A collision keeps the new-key row, fills its NULL fields (and a NULL status) from the old
/// row, never overwrites a set field, and deletes the old row.
#[test]
fn rekey_collision_merges_into_the_existing_row() {
    let (_d, mut s) = db();
    let mut old = mail_row("unknown-sender", "m1");
    old.note_path = Some("n/old.md".into());
    old.access = Some("free".into());
    old.complete = Some(true);
    old.full_word_count = Some(900);
    let mut new = mail_row("Newsbit", "m1");
    new.note_path = Some("n/new.md".into());
    new.complete = Some(false);
    s.insert_tracker_item(&old).unwrap();
    s.insert_tracker_item(&new).unwrap();
    s.set_tracker_item_status("mail", "unknown-sender", "m1", Some('x'))
        .unwrap();
    let r = s
        .rekey_tracker_item("mail", "unknown-sender", "m1", "Newsbit", "Newsbit")
        .unwrap();
    assert_eq!(r, Rekey::Merged);
    assert!(
        s.tracker_item("mail", "unknown-sender", "m1")
            .unwrap()
            .is_none()
    );
    let got = s.tracker_item("mail", "Newsbit", "m1").unwrap().unwrap();
    assert_eq!(got.note_path.as_deref(), Some("n/new.md"), "set field kept");
    assert_eq!(got.complete, Some(false), "set field kept");
    assert_eq!(got.access.as_deref(), Some("free"), "NULL filled");
    assert_eq!(got.full_word_count, Some(900), "NULL filled");
    assert_eq!(
        s.tracker_item_status("mail", "Newsbit", "m1").unwrap(),
        Some(Some('x')),
        "NULL status taken from the old row"
    );
}

#[test]
fn tracker_open_rows_and_rows_of_source_ignore_outcome() {
    let (_d, mut s) = db();
    for (id, outcome) in [
        ("a", TrackerOutcome::Line),
        ("b", TrackerOutcome::Note),
        ("c", TrackerOutcome::Skipped),
    ] {
        let mut it = mail_row("unknown-sender", id);
        it.source_kind = "youtube".into();
        it.outcome = outcome;
        s.insert_tracker_item(&it).unwrap();
    }
    s.set_tracker_item_status("youtube", "unknown-sender", "b", Some('x'))
        .unwrap();
    let open: Vec<String> = s
        .tracker_open_rows("youtube")
        .unwrap()
        .into_iter()
        .map(|r| r.item_id)
        .collect();
    assert_eq!(open, vec!["a", "c"]);
    assert_eq!(
        s.tracker_rows_of_source("youtube", "unknown-sender")
            .unwrap()
            .len(),
        3
    );
}

/// Plan intake-round-2-clean-dedupe-reading WP-6: the note-linked rows, and nothing else.
#[test]
fn tracker_items_with_note_lists_only_linked_rows() {
    let (_d, mut s) = db();
    for (id, path) in [("a", Some("n/a.md")), ("b", None), ("c", Some("n/c.md"))] {
        let mut it = mail_row("Newsbit", id);
        it.note_path = path.map(str::to_owned);
        s.insert_tracker_item(&it).unwrap();
    }
    let got: Vec<String> = s
        .tracker_items_with_note()
        .unwrap()
        .into_iter()
        .map(|r| r.item_id)
        .collect();
    assert_eq!(got, vec!["a", "c"]);
}

/// `dup_labels` round-trips a row.
#[test]
fn dup_labels_roundtrip() {
    let (_d, mut s) = db();
    let l = crate::DupLabel {
        keep_path: "a.md".into(),
        other_path: "b.md".into(),
        keep_identity: Some("id".into()),
        other_identity: None,
        containment_keep_in_other: 0.5,
        containment_other_in_keep: 0.75,
        identity_match: false,
        verdict: "keep".into(),
        at_ms: 7,
    };
    s.insert_dup_label(&l).unwrap();
    assert_eq!(s.dup_labels().unwrap(), vec![l]);
}

fn trash_entry(path: &str, reason: TrashReason, at: i64) -> NewTrashEntry {
    NewTrashEntry {
        location: TrashLocation::Vault,
        trash_path: format!(".trash/{path}"),
        original_path: path.into(),
        trashed_ms: at,
        bytes: 3,
        reason,
        mtime_ms: Some(10),
        inode: Some(42),
        info_name: None,
    }
}

/// A fresh database is created at the v15 baseline with the ledger and label tables present and empty.
#[test]
fn fresh_database_is_created_at_v15() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(migrate::BASELINE_VERSION, 15);
    assert_eq!(migrate::latest_version(), 15);
    assert_eq!(s.schema_version().unwrap(), 15);
    assert!(s.trash_entries().unwrap().is_empty());
    drop(s);
    let c = rusqlite::Connection::open(&p).unwrap();
    for t in [
        "mail_boilerplate",
        "mail_boilerplate_seen",
        "dup_labels",
        "trash_entries",
    ] {
        let n: i64 = c
            .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{t}");
    }
}

/// The ledger accessors round-trip, refuse unknown spellings, and every mutation signals.
#[test]
fn trash_entries_roundtrip_and_signal() {
    use std::sync::{Arc, Mutex};
    let (_d, mut s) = db();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sn = Arc::clone(&seen);
    s.set_change_hook(Box::new(move |t| sn.lock().unwrap().push(t)));
    let a = trash_entry("a.md", TrashReason::Deleted, 5);
    let mut b = trash_entry("b.png", TrashReason::NoteAttachment, 2);
    b.location = TrashLocation::System;
    b.trash_path = "b.png".into();
    b.info_name = Some("b.png.trashinfo".into());
    let ia = s.insert_trash_entry(&a).unwrap();
    let ib = s.insert_trash_entry(&b).unwrap();
    let all = s.trash_entries().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].id, &all[0].entry), (ib, &b), "oldest first");
    assert_eq!(
        s.trash_entry_by_path(TrashLocation::Vault, ".trash/a.md")
            .unwrap()
            .map(|e| e.id),
        Some(ia)
    );
    assert!(
        s.trash_entry_by_path(TrashLocation::System, ".trash/a.md")
            .unwrap()
            .is_none()
    );
    // The CHECKs still refuse unknown spellings (reachable by raw SQL only now).
    for (loc, reason) in [("vault", "lost"), ("bin", "deleted")] {
        assert!(
            s.conn()
                .execute(
                    "INSERT INTO trash_entries (location, trash_path, original_path, trashed_ms,
                         bytes, reason) VALUES (?1, 'x', 'x', 1, 1, ?2)",
                    rusqlite::params![loc, reason],
                )
                .is_err()
        );
    }
    assert!(s.delete_trash_entry(ia).unwrap());
    assert!(!s.delete_trash_entry(ia).unwrap());
    assert_eq!(s.trash_entries().unwrap().len(), 1);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["trash_entries", "trash_entries", "trash_entries"]
    );
}
