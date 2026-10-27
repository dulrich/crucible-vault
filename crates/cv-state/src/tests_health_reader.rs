use super::*;

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
