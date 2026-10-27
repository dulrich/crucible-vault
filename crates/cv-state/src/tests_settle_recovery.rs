use super::*;

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
