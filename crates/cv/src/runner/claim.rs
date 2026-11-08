//! The permit and scheduling layer (moved from `runner/mod.rs`, plan tn-review-2026-11-04-remediation WP-R4, finding P3): the two permit pools, the claim loop, and one job's run and settle.

use super::*;

impl Inner {
    /// Breaker feeding + bounded transient retry (plan WP-2), run after the steps. Pure: returns
    /// the final result plus the service failure to report; both are committed together with the
    /// settle by [`Self::settle`] (SR10).
    pub(crate) fn apply_service_policy(
        job: &cv_state::Job,
        result: JobResult,
        cause: Option<DeferCause>,
    ) -> (JobResult, Option<BreakerFailure>) {
        match (result, cause) {
            (
                JobResult::Deferred {
                    retry_after_ms,
                    service_unhealthy: Some(svc),
                },
                Some(DeferCause::Transient(kind)),
            ) => {
                // Transient: the breaker computes its own doubling window (no retry_after).
                let failure = BreakerFailure {
                    service: svc.clone(),
                    kind,
                    reason: format!("transient {kind:?} (attempt {})", job.attempts),
                    retry_after_ms: None,
                };
                let result = if job.attempts >= TRANSIENT_MAX_ATTEMPTS {
                    JobResult::Failed {
                        error: format!(
                            "service {svc} still failing after {} attempts",
                            job.attempts
                        ),
                        reason: Some(cv_state::FailureReason::ServiceOutage),
                    }
                } else {
                    let shift = u32::try_from((job.attempts - 1).clamp(0, 20)).unwrap_or(20);
                    let backoff = TRANSIENT_BASE_MS
                        .saturating_mul(1_i64 << shift)
                        .min(TRANSIENT_CAP_MS);
                    JobResult::Deferred {
                        retry_after_ms: retry_after_ms.max(backoff),
                        service_unhealthy: Some(svc),
                    }
                };
                (result, Some(failure))
            }
            (
                JobResult::Deferred {
                    retry_after_ms,
                    service_unhealthy: Some(svc),
                },
                Some(DeferCause::Quota),
            ) => {
                let failure = BreakerFailure {
                    service: svc.clone(),
                    kind: cv_state::ServiceFailureKind::RateLimited,
                    reason: "quota exhausted".into(),
                    retry_after_ms: Some(retry_after_ms),
                };
                (
                    JobResult::Deferred {
                        retry_after_ms,
                        service_unhealthy: Some(svc),
                    },
                    Some(failure),
                )
            }
            (r, _) => (r, None),
        }
    }

    /// Settles `job` with `result` and its breaker update in one state.db transaction.
    fn settle(&self, job: &cv_state::Job, result: &JobResult, failure: Option<&BreakerFailure>) {
        let now = self.clock.now_ms();
        match lock(&self.state).settle_with_breaker(&job.id, result, failure, now) {
            Ok(Some(requeued)) => tracing::info!(
                service = job.service.as_deref().unwrap_or_default(),
                requeued,
                "breaker closed; requeued service failures"
            ),
            Ok(None) => {}
            Err(e) => tracing::error!(job_id = %job.id, error = %e, "settle failed"),
        }
    }
}

/// A runner permit pool (plan job-slots-fast-lane WP-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pool {
    /// Claims any job (`RunnerDeps::concurrency` permits).
    General,
    /// Claims only jobs of armed `fast` actions (`RunnerDeps::fast_slots` permits).
    Fast,
}

impl Inner {
    fn config_snapshot(&self) -> Arc<ConfigSet> {
        Arc::clone(&read(&self.config))
    }

    /// The permit-wait view: `fast_types` from this pass's snapshot, each pool's free state and
    /// its holders from the claim-time record (sorted by the store).
    fn pool_view(
        &self,
        fast_types: HashSet<String>,
        general_free: bool,
        fast_free: bool,
    ) -> cv_state::PoolView {
        let mut view = cv_state::PoolView {
            fast_types,
            general_free,
            fast_free,
            ..cv_state::PoolView::default()
        };
        for j in lock(&self.running).values() {
            match j.pool {
                Pool::General => view.general_holders.push(j.job_type.clone()),
                Pool::Fast => view.fast_holders.push(j.job_type.clone()),
            }
        }
        view
    }
}

/// `action:<id>` of every armed `fast` action (plan job-slots-fast-lane WP-2).
fn fast_job_types(config: &ConfigSet) -> HashSet<String> {
    config
        .armed_actions()
        .filter(|a| a.fast)
        .map(|a| format!("action:{}", a.id))
        .collect()
}

/// Claims into `pool` while it has a free permit (plan job-slots-fast-lane WP-2), restricted to
/// `only` job types when set. Returns whether the pool ended exhausted (a permit was wanted
/// and none was free). The permit and the job's pool record travel with the job until settle.
fn claim_pool(
    inner: &Arc<Inner>,
    permits: &Arc<tokio::sync::Semaphore>,
    pool: Pool,
    only: Option<&HashSet<String>>,
) -> bool {
    loop {
        let Ok(permit) = Arc::clone(permits).try_acquire_owned() else {
            return true;
        };
        let claimed = lock(&inner.state).claim_filtered(inner.clock.now_ms(), only);
        match claimed {
            Ok(Some(job)) => {
                let entry = RunningJob {
                    pool,
                    job_type: job.job_type.clone(),
                    hold: None,
                    _permit: permit,
                };
                lock(&inner.running).insert(job.id.clone(), entry);
                let inner2 = Arc::clone(inner);
                tokio::spawn(async move {
                    run_job(Arc::clone(&inner2), job).await;
                    inner2.wake.notify_one();
                });
            }
            Ok(None) => return false,
            Err(e) => {
                tracing::error!(error = %e, "claim failed");
                return false;
            }
        }
    }
}

pub(super) async fn claim_loop(inner: Arc<Inner>, mut stop: watch::Receiver<bool>) {
    inner.recover();
    let mut maintenance = Maintenance::default();
    let general = Arc::new(tokio::sync::Semaphore::new(inner.concurrency));
    let fast = Arc::new(tokio::sync::Semaphore::new(inner.fast_slots));
    loop {
        if *stop.borrow() {
            return;
        }
        inner.maintenance_tick(&mut maintenance);
        // Plan job-slots-fast-lane WP-2 (audit E1): one config snapshot per pass; its fast set
        // drives this pass's fast claims and its permit-wait observation. A fast job takes the
        // fast slot first, keeping general slots for slow work; the general pool claims any job.
        let fast_types = fast_job_types(&inner.config_snapshot());
        // The fast pool's exhaustion alone never triggers an observation: every candidate may
        // use the general pool, so none waits on a permit while a general permit is free.
        claim_pool(&inner, &fast, Pool::Fast, Some(&fast_types));
        if claim_pool(&inner, &general, Pool::General, None) {
            // The general pool is full: queued claimable work whose every usable pool is
            // exhausted waits for a permit (plan job-telemetry Decision 3; written only on a
            // cause change).
            let view = inner.pool_view(
                fast_types,
                general.available_permits() > 0,
                fast.available_permits() > 0,
            );
            if let Err(e) = lock(&inner.state).observe_pool_wait(inner.clock.now_ms(), &view) {
                tracing::warn!(error = %e, "telemetry: permit-wait observation failed");
            }
        }
        let now = inner.clock.now_ms();
        let wake_in = lock(&inner.state)
            .next_wake(now)
            .ok()
            .flatten()
            .map(|t| Duration::from_millis(u64::try_from(t - now).unwrap_or(0)))
            .unwrap_or(IDLE_POLL)
            .min(IDLE_POLL);
        tokio::select! {
            _ = stop.changed() => return,
            () = inner.wake.notified() => {}
            () = tokio::time::sleep(wake_in) => {}
        }
    }
}

/// A failure with no typed reason (classified `job`, never a service outage).
pub(super) fn failed(error: String) -> JobResult {
    JobResult::Failed {
        error,
        reason: None,
    }
}

/// Returns freed heap pages to the OS when anonymous RSS exceeds live allocator bytes by more than
/// [`cv_malloc::TRIM_RETAINED_THRESHOLD`] (the free-resident figure, which falls after a trim).
/// Runs on the blocking pool, never a worker.
fn trim_after_job() {
    if let Some((free_resident, trimmed)) = cv_malloc::trim_if_free_resident() {
        tracing::debug!(target: "cv::memory", free_resident, trimmed, "trim after job");
    }
}

/// Executes one claimed job end to end and settles it.
/// The job's `running` entry (pool record, note hold and permit) is removed right after the
/// settle, the one removal site, so all three clear together.
pub(super) async fn run_job(inner: Arc<Inner>, job: cv_state::Job) {
    let id = job.id.clone();
    let config = Arc::clone(&read(&inner.config));
    let parsed: Result<JobParams, _> = serde_json::from_value(job.params.clone());
    let (result, summary) = match parsed {
        Err(e) => (
            failed(format!("bad job params: {e}")),
            RunSummary::default(),
        ),
        Ok(jp) => match config.armed(&jp.action_id).cloned() {
            None => (
                failed(format!(
                    "action {:?} is not armed: {}",
                    jp.action_id,
                    config
                        .disarm_reason(&jp.action_id)
                        .unwrap_or("unknown action")
                )),
                RunSummary::default(),
            ),
            Some(action) => {
                inner.start_running(&id, &action, &jp.context, config.action_holds_note(&action));
                let inner2 = Arc::clone(&inner);
                let job_id = id.clone();
                let lane = job.lane;
                let retry = job.attempts > 1;
                match tokio::task::spawn_blocking(move || {
                    let r = execute_steps(&inner2, &job_id, lane, retry, &action, &jp, &config);
                    trim_after_job();
                    r
                })
                .await
                {
                    Ok(r) => r,
                    Err(e) => (failed(format!("step panicked: {e}")), RunSummary::default()),
                }
            }
        },
    };
    let (result, failure) = Inner::apply_service_policy(&job, result, summary.defer_cause);
    inner.settle(&job, &result, failure.as_ref());
    // Dropping the entry releases its permit.
    let hold = lock(&inner.running).remove(&id).and_then(|j| j.hold);
    let (state, message) = match &result {
        JobResult::Done => (JobState::Done, None),
        JobResult::Failed { error, .. } => (JobState::Failed, Some(error.clone())),
        JobResult::Deferred { .. } => (JobState::Deferred, None),
        JobResult::Cancelled => (JobState::Cancelled, None),
    };
    {
        let mut f = lock(&inner.finished);
        f.insert(id.clone(), summary.clone());
        while f.len() > FINISHED_CAP {
            let Some(k) = f.keys().next().cloned() else {
                break;
            };
            f.remove(&k);
        }
    }
    inner.publish(
        &id,
        state,
        message,
        summary.output_paths,
        summary.open_path,
        hold.as_ref(),
    );
}
