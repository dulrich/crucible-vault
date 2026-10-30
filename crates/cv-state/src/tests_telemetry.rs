//! Job telemetry (plan job-telemetry WP-1, state.db v16): attempt/step/wait rows and the
//! bucket split. Synthetic data only.

use super::*;

/// The invariant: every millisecond of `settled − created` lands in exactly one bucket.
fn split(s: &StateDb, id: &str) -> BucketSplit {
    let t = s.job_telemetry(id).unwrap().unwrap();
    let b = split_buckets(&t).expect("settled, fully instrumented");
    assert!(b.residual_ms >= 0, "{b:?}");
    assert_eq!(
        b.deferral_ms + b.permit_ms + b.key_ms + b.breaker_ms + b.run_ms + b.residual_ms,
        b.total_ms,
        "{b:?}"
    );
    assert_eq!(b.run_ms, b.attempts.iter().map(|a| a.run_ms).sum::<i64>());
    let waits: i64 = b.blockers.iter().map(|(_, _, ms)| ms).sum();
    assert_eq!(waits, b.deferral_ms + b.permit_ms + b.key_ms + b.breaker_ms);
    b
}

fn fail_now(s: &mut StateDb, id: &str, now: i64) {
    let r = JobResult::Failed {
        error: "x".into(),
        reason: None,
    };
    s.settle(id, &r, now).unwrap();
}

#[test]
fn v15_database_migrates_to_v16_and_marks_existing_rows_partial() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch(migrate::BASELINE).unwrap();
        c.pragma_update(None, "user_version", migrate::BASELINE_VERSION)
            .unwrap();
        c.execute(
            "INSERT INTO jobs (id, job_type, lane, status, params, resource_keys, created_ms, settled_at_ms)
             VALUES ('old', 't', 1, 'done', '{}', '[]', 5, 9)",
            [],
        )
        .unwrap();
    }
    let mut s = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    assert_eq!(s.schema_version().unwrap(), 16);
    let t = s.job_telemetry("old").unwrap().unwrap();
    assert!(t.partial);
    assert_eq!(split_buckets(&t), None, "pre-v16 intervals are unavailable");
    // A row created after the step is fully instrumented.
    let id = id_of(s.enqueue(job("t", None, &[]), 10).unwrap());
    s.claim(12).unwrap();
    s.settle(&id, &JobResult::Done, 15).unwrap();
    let b = split(&s, &id);
    assert_eq!((b.total_ms, b.run_ms, b.residual_ms), (5, 3, 2));
}

#[test]
fn failed_then_retried_puts_the_terminal_gap_in_the_residual() {
    let (_d, mut s) = db();
    let id = id_of(s.enqueue(job("t", None, &[]), 10).unwrap());
    s.claim(20).unwrap();
    fail_now(&mut s, &id, 30);
    assert_eq!(s.retry_job(&id, 100).unwrap(), RetryOutcome::Retried);
    s.claim(110).unwrap();
    s.settle(&id, &JobResult::Done, 150).unwrap();
    // E1: `attempts` was reset by retry; the telemetry attempt is not.
    assert_eq!(s.job(&id).unwrap().unwrap().attempts, 1);
    let b = split(&s, &id);
    let attempts: Vec<(i64, i64)> = b.attempts.iter().map(|a| (a.attempt, a.run_ms)).collect();
    assert_eq!(attempts, vec![(1, 10), (2, 40)]);
    assert_eq!((b.total_ms, b.run_ms, b.residual_ms), (140, 50, 90));
}

#[test]
fn key_wait_is_attributed_to_the_blocking_action() {
    let (_d, mut s) = db();
    let a = id_of(s.enqueue(job("holder", None, &["n"]), 10).unwrap());
    let b = id_of(s.enqueue(job("waiter", None, &["n"]), 11).unwrap());
    assert_eq!(s.claim(20).unwrap().unwrap().id, a);
    assert!(s.claim(25).unwrap().is_none());
    assert!(s.claim(30).unwrap().is_none(), "same cause: no new row");
    s.settle(&a, &JobResult::Done, 50).unwrap();
    assert_eq!(s.claim(50).unwrap().unwrap().id, b);
    s.settle(&b, &JobResult::Done, 60).unwrap();
    let t = s.job_telemetry(&b).unwrap().unwrap();
    assert_eq!(t.waits.len(), 1, "one row per cause change");
    let sb = split(&s, &b);
    assert_eq!((sb.key_ms, sb.run_ms, sb.residual_ms), (25, 10, 14));
    assert_eq!(sb.blockers, vec![(WaitCause::Key, "holder".to_owned(), 25)]);
}

#[test]
fn permit_wait_names_the_running_actions_and_writes_on_change_only() {
    let (_d, mut s) = db();
    let a = id_of(s.enqueue(job("long", None, &[]), 10).unwrap());
    let b = id_of(s.enqueue(job("quick", None, &[]), 11).unwrap());
    assert_eq!(s.claim(12).unwrap().unwrap().id, a);
    s.observe_permit_wait(15).unwrap();
    s.observe_permit_wait(16).unwrap();
    assert_eq!(s.job_telemetry(&b).unwrap().unwrap().waits.len(), 1);
    s.settle(&a, &JobResult::Done, 40).unwrap();
    assert_eq!(s.claim(40).unwrap().unwrap().id, b);
    s.settle(&b, &JobResult::Done, 45).unwrap();
    let sb = split(&s, &b);
    assert_eq!((sb.permit_ms, sb.run_ms, sb.residual_ms), (25, 5, 4));
    assert_eq!(
        sb.blockers,
        vec![(WaitCause::Permit, "long".to_owned(), 25)]
    );
}

#[test]
fn deferral_is_attributed_up_to_its_due_time() {
    let (_d, mut s) = db();
    let id = id_of(s.enqueue(job("t", None, &[]), 0).unwrap());
    s.claim(10).unwrap();
    let r = JobResult::Deferred {
        retry_after_ms: 100,
        service_unhealthy: None,
    };
    s.settle(&id, &r, 20).unwrap();
    assert!(s.claim(50).unwrap().is_none(), "still deferred");
    s.claim(200).unwrap();
    s.settle(&id, &JobResult::Done, 210).unwrap();
    let b = split(&s, &id);
    // 20..120 deferred; 120..200 unobserved; runs 10..20 and 200..210.
    assert_eq!((b.deferral_ms, b.run_ms, b.residual_ms), (100, 20, 90));
}

#[test]
fn step_rows_are_kept_per_telemetry_attempt_and_pruned_with_the_job() {
    let (_d, mut s) = db_with(QueueConfig {
        retention_ms: 1_000,
        ..QueueConfig::default()
    });
    let id = id_of(s.enqueue(job("t", None, &[]), 0).unwrap());
    s.claim(1).unwrap();
    s.step_started(&id, 0, "guard", 1).unwrap();
    s.step_ended(&id, 0, 3).unwrap();
    fail_now(&mut s, &id, 4);
    s.retry_job(&id, 5).unwrap();
    s.claim(6).unwrap();
    s.step_started(&id, 0, "guard", 6).unwrap();
    s.step_ended(&id, 0, 7).unwrap();
    s.step_started(&id, 1, "set_props", 7).unwrap();
    s.step_ended(&id, 1, 10).unwrap();
    s.settle(&id, &JobResult::Done, 10).unwrap();
    let b = split(&s, &id);
    type Steps = Vec<(i64, String, i64)>;
    let steps: Vec<(i64, Steps)> = b
        .attempts
        .into_iter()
        .map(|a| (a.attempt, a.steps))
        .collect();
    assert_eq!(
        steps,
        vec![
            (1, vec![(0, "guard".to_owned(), 2)]),
            (
                2,
                vec![(0, "guard".to_owned(), 1), (1, "set_props".to_owned(), 3)]
            ),
        ]
    );
    assert_eq!(s.prune_terminal(5_000).unwrap(), 1);
    let c = rusqlite::Connection::open(_d.path().join("state.db")).unwrap();
    for t in ["job_step_times", "job_attempt_times", "job_wait_events"] {
        let n: i64 = c
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{t} pruned with its job");
    }
}

#[test]
fn recovery_and_queued_cancel_mark_the_row_partial() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.db");
    let mut dead = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
    let rec = id_of(dead.enqueue(job("t", None, &[]), 1).unwrap());
    dead.claim(2).unwrap();
    drop(dead);
    let mut live = StateDb::open(&p, 3, QueueConfig::default()).unwrap();
    assert_eq!(live.recover_foreign_leases().unwrap(), 1);
    live.claim(4).unwrap();
    live.settle(&rec, &JobResult::Done, 5).unwrap();
    let t = live.job_telemetry(&rec).unwrap().unwrap();
    assert!(t.partial);
    let attempts: Vec<i64> = t.attempts.iter().map(|a| a.attempt).collect();
    assert_eq!(
        attempts,
        vec![1, 2],
        "recovery keeps the telemetry attempt monotonic"
    );
    assert_eq!(t.attempts[0].settled_ms, None);
    assert_eq!(split_buckets(&t), None);

    let mut stale = StateDb::open(
        &dir.path().join("s2.db"),
        1,
        QueueConfig {
            stale_lease_ms: 10,
            ..QueueConfig::default()
        },
    )
    .unwrap();
    let st = id_of(stale.enqueue(job("t", None, &[]), 1).unwrap());
    stale.claim(2).unwrap();
    assert_eq!(
        stale
            .recover_stale_leases_except(100, &std::collections::HashSet::new())
            .unwrap(),
        1
    );
    assert!(stale.job_telemetry(&st).unwrap().unwrap().partial);

    let q = id_of(live.enqueue(job("t", None, &[]), 10).unwrap());
    live.cancel(&q, 20).unwrap();
    let t = live.job_telemetry(&q).unwrap().unwrap();
    assert!(t.partial);
    assert_eq!(split_buckets(&t), None);
}
