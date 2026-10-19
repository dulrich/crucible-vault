//! WP-2: unattended queue — breaker feeding, bounded retry, requeue on close, maintenance.

use super::*;

/// Executor whose behaviour is switched by `mode`: 0 = transient defer on `S`, 1 = done,
/// 2 = permanent failure.
fn switch_exec(mode: Arc<AtomicUsize>) -> Arc<dyn StepExecutor> {
    Arc::new(FnExec(Box::new(move |_, _| {
        match mode.load(Ordering::SeqCst) {
            0 => Err(StepError::Deferred {
                retry_after_ms: 1_000,
                service: Some("S".into()),
                cause: DeferCause::Transient(cv_state::ServiceFailureKind::Timeout),
            }),
            1 => Ok(StepOutcome::default()),
            _ => Err(StepError::failed("bad request")),
        }
    })))
}

fn raw_job(service: Option<&str>) -> cv_state::NewJob {
    cv_state::NewJob {
        job_type: "raw".into(),
        lane: Lane::Background,
        dedupe_key: None,
        params: serde_json::json!({}),
        resource_keys: Vec::new(),
        service: service.map(str::to_owned),
    }
}

fn raw_id(o: cv_state::EnqueueOutcome) -> String {
    match o {
        cv_state::EnqueueOutcome::Enqueued(id) => id,
        other => panic!("{other:?}"),
    }
}

/// Claims the next job at `clock`'s now and runs it through the runner's `run_job`.
async fn claim_and_run(r: &Runner, f: &Fixture, clock: &ManualClock) -> Option<String> {
    let job = f.state.lock().unwrap().claim(clock.now_ms()).unwrap()?;
    let id = job.id.clone();
    run_job(Arc::clone(r.inner()), job).await;
    Some(id)
}

fn health(f: &Fixture) -> cv_state::ServiceHealth {
    f.state.lock().unwrap().service_health("S").unwrap()
}

fn job(f: &Fixture, id: &str) -> cv_state::Job {
    f.state.lock().unwrap().job(id).unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn breaker_opens_after_three_transient_deferrals_gates_claim_and_requeues_on_close() {
    let f = fixture();
    let mode = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(ManualClock::new(1_000_000));
    let r = runner(
        &f,
        switch_exec(Arc::clone(&mode)),
        clock.clone(),
        1,
        ONE_STEP,
    );
    // An earlier outage failure, eligible for requeue once S closes.
    let failed = {
        let mut db = f.state.lock().unwrap();
        let id = raw_id(db.enqueue(raw_job(None), 1).unwrap());
        db.claim(1).unwrap().unwrap();
        db.settle(
            &id,
            &JobResult::Failed {
                error: "outage".into(),
                reason: Some(cv_state::FailureReason::ServiceOutage),
            },
            2,
        )
        .unwrap();
        id
    };
    let a = enqueue(&r, "look", None, Lane::Background);
    let mut now = clock.now_ms();
    for n in 1..=3_u32 {
        assert_eq!(
            claim_and_run(&r, &f, &clock).await.as_deref(),
            Some(a.as_str())
        );
        let j = job(&f, &a);
        assert_eq!(
            (j.status, j.service.as_deref()),
            (DbStatus::Queued, Some("S")),
            "deferral stamps the service (E1)"
        );
        // backoff: max(step 1 s, 30 s * 2^(n-1))
        assert_eq!(j.defer_until_ms, Some(now + 30_000 * (1_i64 << (n - 1))));
        if n < 3 {
            now = j.defer_until_ms.unwrap();
            clock.set(now);
        }
    }
    assert_eq!(health(&f).state, cv_protocol::BreakerState::Open);
    // A ready job stamped S is gated while Open.
    let b = raw_id(
        f.state
            .lock()
            .unwrap()
            .enqueue(raw_job(Some("S")), now)
            .unwrap(),
    );
    clock.set(now + 1_000);
    assert!(
        f.state
            .lock()
            .unwrap()
            .claim(clock.now_ms())
            .unwrap()
            .is_none()
    );
    // Window elapsed and A ready: exactly one probe is admitted — A, the oldest.
    clock.set(now + 120_000);
    mode.store(1, Ordering::SeqCst);
    let probe = f
        .state
        .lock()
        .unwrap()
        .claim(clock.now_ms())
        .unwrap()
        .unwrap();
    assert_eq!(probe.id, a);
    assert!(
        f.state
            .lock()
            .unwrap()
            .claim(clock.now_ms())
            .unwrap()
            .is_none(),
        "one probe only"
    );
    run_job(Arc::clone(r.inner()), probe).await;
    assert_eq!(status(&f, &a), DbStatus::Done);
    assert_eq!(health(&f).state, cv_protocol::BreakerState::Closed);
    let fj = job(&f, &failed);
    assert_eq!(
        (fj.status, fj.attempts),
        (DbStatus::Queued, 0),
        "requeued on close"
    );
    assert_eq!(status(&f, &b), DbStatus::Queued);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_exhaustion_fails_service_and_permanent_never_retries() {
    let f = fixture();
    let mode = Arc::new(AtomicUsize::new(0));
    let clock = Arc::new(ManualClock::new(1_000_000));
    let r = runner(
        &f,
        switch_exec(Arc::clone(&mode)),
        clock.clone(),
        1,
        ONE_STEP,
    );
    let a = enqueue(&r, "look", None, Lane::Background);
    for n in 1..=TRANSIENT_MAX_ATTEMPTS {
        // Past every backoff and breaker window: claimed (as the probe once the breaker opened).
        clock.set(1_000_000 + n * 3_600_000);
        assert_eq!(
            claim_and_run(&r, &f, &clock).await.as_deref(),
            Some(a.as_str())
        );
    }
    let j = job(&f, &a);
    assert_eq!(j.status, DbStatus::Failed);
    assert_eq!(j.failure_kind, Some(cv_protocol::FailureKind::Service));
    assert_eq!(j.attempts, TRANSIENT_MAX_ATTEMPTS);

    mode.store(2, Ordering::SeqCst);
    let p = enqueue(&r, "look", None, Lane::Background);
    clock.set(clock.now_ms() + 3_600_000);
    assert_eq!(
        claim_and_run(&r, &f, &clock).await.as_deref(),
        Some(p.as_str())
    );
    let j = job(&f, &p);
    assert_eq!((j.status, j.attempts), (DbStatus::Failed, 1));
    assert_ne!(j.failure_kind, Some(cv_protocol::FailureKind::Service));
    clock.set(clock.now_ms() + 3_600_000);
    assert!(
        f.state
            .lock()
            .unwrap()
            .claim(clock.now_ms())
            .unwrap()
            .is_none()
    );
}

#[test]
fn maintenance_skips_live_ids_and_prunes_by_configured_retention() {
    let f = fixture();
    let clock = Arc::new(ManualClock::new(1_000_000));
    let exec = Arc::new(FnExec(Box::new(|_, _| Ok(StepOutcome::default()))));
    let r = runner(&f, exec, clock.clone(), 1, ONE_STEP);
    let (live, stale, old) = {
        let mut db = f.state.lock().unwrap();
        let live = raw_id(db.enqueue(raw_job(None), 1).unwrap());
        let stale = raw_id(db.enqueue(raw_job(None), 1).unwrap());
        let old = raw_id(db.enqueue(raw_job(None), 1).unwrap());
        db.claim(1_000_000).unwrap().unwrap();
        db.claim(1_000_000).unwrap().unwrap();
        db.cancel(&old, 1_000_000).unwrap();
        (live, stale, old)
    };
    lock(&r.inner().running).insert(live.clone());
    let mut m = Maintenance::default();
    // 31 days later: both leases are stale and the default 30-day retention has passed.
    clock.set(1_000_000 + 31 * 86_400_000);
    r.inner().maintenance_tick(&mut m);
    assert_eq!(
        status(&f, &live),
        DbStatus::Running,
        "live job never recovered"
    );
    assert_eq!(status(&f, &stale), DbStatus::Queued);
    assert!(
        f.state.lock().unwrap().job(&old).unwrap().is_none(),
        "pruned"
    );
}
