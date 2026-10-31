//! Plan job-telemetry WP-1: permit-wait attribution and step timing.

use super::*;

/// Plan job-telemetry WP-1: with one permit held by a long job, the queued job's wait is
/// attributed to the permit (naming the holder's action), and each step is timed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permit_starved_job_attributes_permit_wait_and_times_steps() {
    let f = fixture();
    let exec = FnExec(Box::new(move |_, ctx| {
        if ctx.context.active_path.as_deref() == Some("a.md") {
            std::thread::sleep(Duration::from_millis(150));
        }
        Ok(StepOutcome::default())
    }));
    let r = runner(&f, Arc::new(exec), Arc::new(SystemClock), 1, ONE_STEP);
    let long = enqueue(&r, "touch", Some("a.md"), Lane::User);
    let quick = enqueue(&r, "touch", Some("b.md"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    assert_eq!(wait_terminal(&f, &long).await, DbStatus::Done);
    assert_eq!(wait_terminal(&f, &quick).await, DbStatus::Done);
    let db = f.state.lock().unwrap();
    let t = db.job_telemetry(&quick).unwrap().unwrap();
    let b = cv_state::split_buckets(&t).unwrap();
    assert_eq!(
        b.deferral_ms + b.permit_ms + b.key_ms + b.breaker_ms + b.run_ms + b.residual_ms,
        b.total_ms
    );
    assert!(b.permit_ms >= 100, "{b:?}");
    assert_eq!(b.key_ms, 0);
    let (cause, holder, _) = &b.blockers[0];
    assert_eq!(*cause, cv_state::WaitCause::Permit);
    assert!(holder.contains("touch"), "{holder}");
    let lt = db.job_telemetry(&long).unwrap().unwrap();
    assert_eq!(lt.steps.len(), 1);
    let s = &lt.steps[0];
    assert_eq!(
        (s.attempt, s.step_index, s.step_kind.as_str()),
        (1, 0, "set_props")
    );
    assert!(s.end_ms.unwrap() - s.start_ms >= 150);
}

// ---- plan job-slots-fast-lane WP-2: general + fast pools ----------------------------------

const POOLS: &str = r#"
[[action]]
id = "slow"
[[action.steps]]
type = "set_props"

[[action]]
id = "quick"
fast = true
[[action.steps]]
type = "set_props"

[[action]]
id = "other"
[[action.steps]]
type = "set_props"
"#;

/// After reload: `quick` is no longer fast, `other` is.
const POOLS_RELOADED: &str = r#"
[[action]]
id = "slow"
[[action.steps]]
type = "set_props"

[[action]]
id = "quick"
[[action.steps]]
type = "set_props"

[[action]]
id = "other"
fast = true
[[action.steps]]
type = "set_props"
"#;

/// An executor whose `hold-*` paths block until released.
fn gated() -> (Arc<Mutex<HashSet<String>>>, FnExec) {
    let released = Arc::new(Mutex::new(HashSet::<String>::new()));
    let r2 = Arc::clone(&released);
    let exec = FnExec(Box::new(move |_, ctx| {
        let path = ctx.context.active_path.clone().unwrap_or_default();
        if path.starts_with("hold-") {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !r2.lock().unwrap().contains(&path) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(StepOutcome::default())
    }));
    (released, exec)
}

async fn wait_status(f: &Fixture, id: &str, want: DbStatus) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while status(f, id) != want {
        assert!(Instant::now() < deadline, "job {id} never reached {want:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The job's last recorded wait `(cause, blocker)`, polled until one exists.
async fn last_wait(f: &Fixture, id: &str) -> (cv_state::WaitCause, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let t = f.state.lock().unwrap().job_telemetry(id).unwrap().unwrap();
        if let Some(w) = t.waits.last() {
            return (w.cause, w.blocker.clone().unwrap_or_default());
        }
        assert!(Instant::now() < deadline, "job {id} recorded no wait");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn release(r: &Mutex<HashSet<String>>, paths: &[&str]) {
    r.lock()
        .unwrap()
        .extend(paths.iter().map(|p| (*p).to_owned()));
}

/// A fast job claims the fast slot while three slow jobs hold the general pool, and records no
/// permit wait (the fast permit was free).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_job_takes_the_fast_slot_while_general_is_full() {
    let f = fixture();
    let (released, exec) = gated();
    let r = runner_pools(&f, Arc::new(exec), Arc::new(SystemClock), 3, 1, POOLS);
    let slows: Vec<String> = ["hold-1", "hold-2", "hold-3"]
        .iter()
        .map(|p| enqueue(&r, "slow", Some(p), Lane::User))
        .collect();
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    for s in &slows {
        wait_status(&f, s, DbStatus::Running).await;
    }
    let q = enqueue(&r, "quick", Some("q.md"), Lane::User);
    r.inner.wake.notify_one();
    assert_eq!(wait_terminal(&f, &q).await, DbStatus::Done);
    for s in &slows {
        assert_eq!(status(&f, s), DbStatus::Running);
    }
    let t = f.state.lock().unwrap().job_telemetry(&q).unwrap().unwrap();
    assert!(
        t.waits
            .iter()
            .all(|w| w.cause != cv_state::WaitCause::Permit),
        "{:?}",
        t.waits
    );
    release(&released, &["hold-1", "hold-2", "hold-3"]);
    for s in &slows {
        assert_eq!(wait_terminal(&f, s).await, DbStatus::Done);
    }
}

/// A non-fast job never takes the free fast slot: it waits on the general pool, recorded as a
/// permit wait naming the general holders only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn non_fast_job_waits_for_general_while_fast_slot_is_free() {
    let f = fixture();
    let (released, exec) = gated();
    let r = runner_pools(&f, Arc::new(exec), Arc::new(SystemClock), 1, 1, POOLS);
    let s1 = enqueue(&r, "slow", Some("hold-1"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    wait_status(&f, &s1, DbStatus::Running).await;
    let s2 = enqueue(&r, "other", Some("b.md"), Lane::User);
    r.inner.wake.notify_one();
    let (cause, blocker) = last_wait(&f, &s2).await;
    assert_eq!(
        (cause, blocker.as_str()),
        (cv_state::WaitCause::Permit, "action:slow")
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(status(&f, &s2), DbStatus::Queued, "took the fast slot");
    release(&released, &["hold-1"]);
    assert_eq!(wait_terminal(&f, &s2).await, DbStatus::Done);
}

/// A fast job uses a general slot when the fast slot is busy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_job_uses_a_general_slot_when_the_fast_slot_is_busy() {
    let f = fixture();
    let (released, exec) = gated();
    let r = runner_pools(&f, Arc::new(exec), Arc::new(SystemClock), 1, 1, POOLS);
    let q1 = enqueue(&r, "quick", Some("hold-1"), Lane::User);
    let q2 = enqueue(&r, "quick", Some("hold-2"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    wait_status(&f, &q1, DbStatus::Running).await;
    wait_status(&f, &q2, DbStatus::Running).await;
    let pools: Vec<Pool> = {
        let m = lock(&r.inner.pools);
        vec![m[&q1].0, m[&q2].0]
    };
    assert_eq!(pools, vec![Pool::Fast, Pool::General]);
    release(&released, &["hold-1", "hold-2"]);
    assert_eq!(wait_terminal(&f, &q1).await, DbStatus::Done);
    assert_eq!(wait_terminal(&f, &q2).await, DbStatus::Done);
}

/// A reload changes the fast set for later claims, while a job already on the fast permit keeps
/// its pool identity (still named as a fast holder) until it settles (audit E1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_changes_the_fast_set_and_running_jobs_keep_their_pool() {
    let f = fixture();
    let (released, exec) = gated();
    let r = runner_pools(&f, Arc::new(exec), Arc::new(SystemClock), 1, 1, POOLS);
    let q1 = enqueue(&r, "quick", Some("hold-q"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    wait_status(&f, &q1, DbStatus::Running).await;
    assert_eq!(lock(&r.inner.pools)[&q1].0, Pool::Fast);
    r.set_config(config(POOLS_RELOADED));
    let s1 = enqueue(&r, "slow", Some("hold-s"), Lane::User);
    r.inner.wake.notify_one();
    wait_status(&f, &s1, DbStatus::Running).await;
    // `other` is fast now: both its pools are full; the fast holder is still `quick`.
    let o = enqueue(&r, "other", Some("o.md"), Lane::User);
    // `quick` is no longer fast: it may only use the general pool.
    let q2 = enqueue(&r, "quick", Some("q2.md"), Lane::User);
    r.inner.wake.notify_one();
    let (cause, blocker) = last_wait(&f, &o).await;
    assert_eq!(
        (cause, blocker.as_str()),
        (cv_state::WaitCause::Permit, "action:quick,action:slow")
    );
    let (cause, blocker) = last_wait(&f, &q2).await;
    assert_eq!(
        (cause, blocker.as_str()),
        (cv_state::WaitCause::Permit, "action:slow")
    );
    release(&released, &["hold-q"]);
    assert_eq!(wait_terminal(&f, &o).await, DbStatus::Done);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        status(&f, &q2),
        DbStatus::Queued,
        "non-fast took the fast slot"
    );
    release(&released, &["hold-s"]);
    assert_eq!(wait_terminal(&f, &q2).await, DbStatus::Done);
}
