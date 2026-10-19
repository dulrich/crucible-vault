//! Store tests. Vectors ported from `https://github.com/dulrich/obsidian-crucible/blob/master/tests/{sqliteJobStore,dbJobBackend,
//! jobClaimOrder,failedJobRepairRequeue,serviceHealth,queueControl}.test.mjs` where their
//! subject is not Obsidian-induced. Synthetic data only (I1).

use crate::health::{MAX_OPEN_WINDOW_MS, OPEN_WINDOW_MS, PROBE_STALE_MS};
use crate::*;
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
    };
    assert!(s.insert_tracker_item(&item).unwrap());
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

/// D23: a fresh database gets exactly the schema the retired v1→v10 chain produced.
#[test]
fn fresh_baseline_matches_the_v10_chain_schema() {
    let mut c = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(migrate::migrate(&mut c).unwrap(), migrate::BASELINE_VERSION);
    assert_eq!(
        schema_dump(&c),
        include_str!("../tests/fixtures/baseline-v10-schema.txt")
    );
}

/// D23: a database already at the baseline (the live state.db) opens with no write.
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
    assert_eq!(s.schema_version().unwrap(), migrate::BASELINE_VERSION);
}

/// D23: versions 1..=9 predate the 0.1.0 baseline and are refused, by writer and reader.
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
            matches!(err, StateError::PreBaseline { found, baseline: 10 } if found == v),
            "{err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("version {v} ")) && msg.contains("predates the 0.1.0 baseline"),
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

// ---- cut-over marker (plan cut-over-now WP-1) -------------------------------

#[test]
fn set_cutover_is_idempotent() {
    let (_d, mut s) = db();
    let a = s.set_cutover("/v", "me", 10).unwrap();
    let b = s.set_cutover("/other", "you", 20).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.vault, "/v");
    assert_eq!(s.cutover().unwrap(), Some(a));
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
