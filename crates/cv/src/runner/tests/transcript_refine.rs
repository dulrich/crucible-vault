//! Plan youtube-transcript-auto-refine WP-1 (audit E1): the `transcript_refined` outcome var that
//! forces `intake_enrich` survives the runner's checkpoint replay of a deferred later step.

use super::*;

const REFINE_THEN_ENRICH: &str = r#"
[[action]]
id = "refine-enrich"
needs = ["active_path"]
[[action.steps]]
type = "transcript_refine"
[[action.steps]]
type = "intake_enrich"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_refine_flag_reaches_enrich_after_a_deferral_replays_the_checkpoint() {
    let f = fixture();
    let refine_runs = Arc::new(AtomicUsize::new(0));
    let enrich_calls = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<Option<bool>>>> = Arc::default();
    let (rr, ec, sn) = (
        Arc::clone(&refine_runs),
        Arc::clone(&enrich_calls),
        Arc::clone(&seen),
    );
    let exec = FnExec(Box::new(move |_, ctx| {
        if ctx.step_index == 0 {
            rr.fetch_add(1, Ordering::SeqCst);
            let mut o = StepOutcome::default();
            o.vars.insert(
                crate::steps::transcript_refine::REFINED_VAR.into(),
                serde_json::json!(true),
            );
            return Ok(o);
        }
        sn.lock().unwrap().push(
            ctx.vars
                .get(crate::steps::transcript_refine::REFINED_VAR)
                .and_then(serde_json::Value::as_bool),
        );
        if ec.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(StepError::Deferred {
                retry_after_ms: 20,
                service: None,
                cause: crate::steps::DeferCause::Wait,
            });
        }
        Ok(StepOutcome::default())
    }));
    let r = runner(
        &f,
        Arc::new(exec),
        Arc::new(SystemClock),
        1,
        REFINE_THEN_ENRICH,
    );
    let id = enqueue(&r, "refine-enrich", Some("a.md"), Lane::User);
    let (_stop_tx, stop) = watch::channel(false);
    let _h = r.spawn_claim_loop(stop);
    assert_eq!(wait_terminal(&f, &id).await, DbStatus::Done);
    assert_eq!(
        refine_runs.load(Ordering::SeqCst),
        1,
        "step 0 is replayed, not re-run"
    );
    assert_eq!(*seen.lock().unwrap(), vec![Some(true), Some(true)]);
}
