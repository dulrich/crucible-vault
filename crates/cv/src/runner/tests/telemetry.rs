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
