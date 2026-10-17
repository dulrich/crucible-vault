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
        EnqueueOutcome::Suppressed => panic!("suppressed"),
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
fn v4_database_migrates_to_v5_and_tracker_accessors_work() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    {
        let mut c = rusqlite::Connection::open(&p).unwrap();
        for (i, sql) in migrate::MIGRATIONS.iter().take(4).enumerate() {
            let tx = c.transaction().unwrap();
            tx.execute_batch(sql).unwrap();
            tx.pragma_update(None, "user_version", i64::try_from(i).unwrap() + 1)
                .unwrap();
            tx.commit().unwrap();
        }
        c.execute(
            "INSERT INTO cutover_marker (id, vault, created_at_ms, created_by) VALUES (1, 'v', 1, 't')",
            [],
        )
        .unwrap();
    }
    let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(s.schema_version().unwrap(), migrate::latest_version());
    let n: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM cutover_marker", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "v4 data survives");
    s.conn()
        .execute(
            "INSERT INTO link_records (canonical_url, url, yt_video_id) VALUES ('c', 'u', 'y')",
            [],
        )
        .unwrap();
    let item = TrackerItem {
        source_kind: "blog".into(),
        source_key: "https://f.example/feed".into(),
        item_id: "p1".into(),
        title: "T".into(),
        url: "https://f.example/p1".into(),
        first_seen_ms: 10,
        day: "2026-10-15".into(),
        outcome: TrackerOutcome::Note,
        score: None,
        full_body: Some(true),
        note_path: None,
    };
    assert!(s.insert_tracker_item(&item).unwrap());
    assert!(
        !s.insert_tracker_item(&item).unwrap(),
        "first sighting wins"
    );
    assert_eq!(
        s.tracker_item("blog", "https://f.example/feed", "p1")
            .unwrap(),
        Some(item.clone())
    );
    assert_eq!(
        s.tracker_items_for_source("blog", "https://f.example/feed", 5)
            .unwrap()
            .len(),
        1
    );
    assert!(s.insert_ignored_id("yt", "abc", "short", 1).unwrap());
    assert!(!s.insert_ignored_id("yt", "abc", "short", 2).unwrap());
    assert!(s.is_ignored("yt", "abc").unwrap());
    assert_eq!(s.ignored_ids("yt").unwrap(), vec!["abc".to_owned()]);
    assert_eq!(s.add_api_usage("youtube", "2026-10-15", 3).unwrap(), 3);
    assert_eq!(s.add_api_usage("youtube", "2026-10-15", 2).unwrap(), 5);
    assert_eq!(s.api_usage("youtube", "2026-10-16").unwrap(), 0);
}

#[test]
fn v5_database_migrates_to_v6_preserving_tracker_rows() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    {
        let mut c = rusqlite::Connection::open(&p).unwrap();
        for (i, sql) in migrate::MIGRATIONS.iter().take(5).enumerate() {
            let tx = c.transaction().unwrap();
            tx.execute_batch(sql).unwrap();
            tx.pragma_update(None, "user_version", i64::try_from(i).unwrap() + 1)
                .unwrap();
            tx.commit().unwrap();
        }
        c.execute(
            "INSERT INTO tracker_items (source_kind, source_key, item_id, title, url,
                first_seen_ms, day, outcome) VALUES ('youtube', 'UC1', 'v1', 'T', 'u', 1, 'd', 'line')",
            [],
        )
        .unwrap();
    }
    let s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(s.schema_version().unwrap(), 6);
    let it = s.tracker_item("youtube", "UC1", "v1").unwrap().unwrap();
    assert_eq!((it.title.as_str(), it.note_path), ("T", None));
    assert_eq!(
        s.tracker_item_done("youtube", "UC1", "v1").unwrap(),
        Some(false)
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

// ---- dedupe / re-seed -------------------------------------------------------

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
    assert!(s.has_pending("t", "note:x").unwrap());
    assert!(!s.has_pending("t", "note:").unwrap(), "never prefix");
    assert!(!s.has_pending("t", "note:x2").unwrap());
    assert_eq!(
        s.job(&a).unwrap().unwrap().dedupe_key.as_deref(),
        Some("t::note:x")
    );
    // running still counts as pending
    s.claim(3).unwrap();
    assert!(s.has_pending("t", "note:x").unwrap());
    s.settle(&a, &JobResult::Done, 4).unwrap();
    assert!(!s.has_pending("t", "note:x").unwrap());
    // empty keys never collapse
    let e1 = id_of(s.enqueue(job("t", Some(""), &[]), 5).unwrap());
    let e2 = id_of(s.enqueue(job("t", Some(""), &[]), 5).unwrap());
    assert_ne!(e1, e2);
    assert_eq!(s.job(&e1).unwrap().unwrap().dedupe_key, None);
}

struct Spec;
impl JobTypeSpec for Spec {
    fn job_type(&self) -> &str {
        "enrich"
    }
    fn dedupe_key(&self, p: &serde_json::Value) -> Option<String> {
        p["path"].as_str().map(|s| format!("note:{s}"))
    }
    fn resource_keys(&self, p: &serde_json::Value) -> Vec<String> {
        p["path"]
            .as_str()
            .map(|s| vec![s.to_owned()])
            .unwrap_or_default()
    }
}

#[test]
fn settled_key_suppresses_auto_reseed_for_the_window_only() {
    let (_d, mut s) = db_with(QueueConfig {
        reseed_window_ms: 1_000,
        ..QueueConfig::default()
    });
    let seed = || JobSeed {
        params: json!({"path": "a.md"}),
        lane: None,
    };
    let id = id_of(s.enqueue_seed(&Spec, seed(), 0).unwrap());
    let j = s.job(&id).unwrap().unwrap();
    assert_eq!(j.dedupe_key.as_deref(), Some("enrich::note:a.md"));
    assert_eq!(j.resource_keys, vec!["a.md".to_owned()]);
    assert_eq!(
        s.enqueue_seed(&Spec, seed(), 1).unwrap(),
        EnqueueOutcome::Duplicate(id.clone())
    );
    s.cancel(&id, 100).unwrap();
    assert_eq!(
        s.enqueue_seed(&Spec, seed(), 500).unwrap(),
        EnqueueOutcome::Suppressed
    );
    // explicit (user) enqueue is never suppressed
    let mut user = job("enrich", Some("note:a.md"), &[]);
    user.lane = Lane::User;
    let uid = id_of(s.enqueue(user, 600).unwrap());
    s.cancel(&uid, 600).unwrap();
    // window expires by design (measured from the latest settle)
    assert!(matches!(
        s.enqueue_seed(&Spec, seed(), 1_601).unwrap(),
        EnqueueOutcome::Enqueued(_)
    ));
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

// ---- queue control (queueControl, sqliteJobStore) -----------------------------

#[test]
fn clear_queued_cancels_only_queued_and_appends_notes() {
    let (_d, mut s) = db();
    let run = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    s.claim(2).unwrap();
    let q = id_of(s.enqueue(job("t", None, &[]), 3).unwrap());
    s.append_notes(&q, "first").unwrap();
    s.enqueue(job("t", None, &[]), 4).unwrap();
    assert_eq!(s.clear_queued(5).unwrap(), 2);
    assert_eq!(s.job(&run).unwrap().unwrap().status, JobStatus::Running);
    let jq = s.job(&q).unwrap().unwrap();
    assert_eq!(jq.status, JobStatus::Cancelled);
    assert_eq!(jq.notes.as_deref(), Some("first\ncleared"));
}

#[test]
fn promote_changes_lane_of_queued_only() {
    let (_d, mut s) = db();
    let a = id_of(s.enqueue(job("t", None, &[]), 1).unwrap());
    let b = id_of(s.enqueue(job("t", None, &[]), 2).unwrap());
    assert!(s.promote(&b, Lane::User).unwrap());
    assert_eq!(s.claim(3).unwrap().unwrap().id, b);
    assert!(!s.promote(&b, Lane::Background).unwrap());
    assert_eq!(s.claim(3).unwrap().unwrap().id, a);
}

// ---- recovery / retention -----------------------------------------------------

#[test]
fn dead_token_leases_recover_regardless_of_age() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut dead = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let id = id_of(dead.enqueue(job("t", None, &["x"]), 1).unwrap());
    dead.claim(1_000).unwrap();
    let token = dead.claim_token().to_owned();
    drop(dead);
    let mut live = StateDb::open(&p, 1_001, QueueConfig::default()).unwrap();
    assert_eq!(live.recover_own_leases(&token).unwrap(), 1);
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
    assert_eq!(s.recover_stale_leases(1_100).unwrap(), 0, "boundary kept");
    assert_eq!(s.recover_stale_leases(1_101).unwrap(), 1);
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

// ---- shadow writes / read-only client ---------------------------------------------

#[test]
fn shadow_writes_append_and_list() {
    let (_d, mut s) = db();
    let w = |op: &str, at| NewShadowWrite {
        op: op.into(),
        path: "synthetic/a.md".into(),
        before_hash: None,
        after_hash: Some("h1".into()),
        job_id: None,
        at_ms: at,
    };
    let a = s.append_shadow_write(&w("create", 1)).unwrap();
    let b = s.append_shadow_write(&w("modify", 2)).unwrap();
    assert!(b > a);
    let all = s.shadow_writes(0, 0).unwrap();
    assert_eq!(
        all.iter().map(|x| x.op.as_str()).collect::<Vec<_>>(),
        ["create", "modify"]
    );
    assert_eq!(s.shadow_writes(a, 0).unwrap().len(), 1);
}

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
    assert!(r.shadow_writes(0, 0).unwrap().is_empty());
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
fn v3_to_v4_migration_keeps_rows_and_sets_no_marker() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        for sql in &migrate::MIGRATIONS[..3] {
            c.execute_batch(sql).unwrap();
        }
        c.pragma_update(None, "user_version", 3).unwrap();
        c.execute(
            "INSERT INTO jobs (id, job_type, lane, status, params, resource_keys, created_ms)
             VALUES ('j1', 't', 1, 'queued', '{}', '[]', 1)",
            [],
        )
        .unwrap();
    }
    assert_eq!(StateReader::open(&p).unwrap().cutover().unwrap(), None);
    let s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(s.schema_version().unwrap(), migrate::latest_version());
    assert!(s.job("j1").unwrap().is_some());
    assert_eq!(s.cutover().unwrap(), None);
}

#[test]
fn set_cutover_is_idempotent() {
    let (_d, mut s) = db();
    let a = s.set_cutover("/v", "me", 10).unwrap();
    let b = s.set_cutover("/other", "you", 20).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.vault, "/v");
    assert_eq!(s.cutover().unwrap(), Some(a));
}

#[test]
fn cancel_unfinished_covers_queued_deferred_running_and_keeps_rows() {
    let (_d, mut s) = db();
    let deferred = id_of(s.enqueue(job("t", Some("d"), &[]), 1).unwrap());
    let c = s.claim(2).unwrap().unwrap();
    assert_eq!(c.id, deferred);
    s.settle(
        &deferred,
        &JobResult::Deferred {
            retry_after_ms: 1_000_000,
            service_unhealthy: None,
        },
        3,
    )
    .unwrap();
    let running = id_of(s.enqueue(job("t", Some("r"), &[]), 4).unwrap());
    assert_eq!(s.claim(5).unwrap().unwrap().id, running);
    let queued = id_of(s.enqueue(job("t", Some("q"), &[]), 6).unwrap());
    let done = s.cancel_unfinished(7).unwrap();
    assert_eq!(done.len(), 3);
    assert!(s.unfinished_jobs().unwrap().is_empty());
    for id in [&deferred, &running, &queued] {
        let j = s.job(id).unwrap().expect("row kept");
        assert_eq!(j.status, JobStatus::Cancelled, "{id}");
    }
    assert_eq!(s.cutover().unwrap(), None, "cancelling never marks");
}
