//! Job, queue, intake, tracker and status RPC handlers.

use super::*;

pub(super) fn map_job(j: &cv_state::Job) -> JobStatus {
    JobStatus {
        job_id: j.id.clone(),
        status: crate::jobs::job_state(j, now_ms()),
        message: j.error.clone(),
        output_paths: Vec::new(),
        open_path: None,
        held_note: None,
        action_label: None,
    }
}

impl Shared {
    /// The one read path of the list and status handlers: a read-only `state.db` handle, so
    /// listing never contends for the writer's lock.
    fn reader(&self) -> Result<StateReader, RpcError> {
        StateReader::open(&self.config.state_db).map_err(rpc_from_state)
    }

    pub(super) fn job(&self, id: &str) -> Result<JobStatus, RpcError> {
        let job = lock(&self.state).job(id).map_err(rpc_from_state)?;
        job.map(|j| map_job(&j))
            .ok_or_else(|| rpc_from_state(StateError::NoSuchJob(id.to_owned())))
    }

    /// `cancel`: a queued job settles now; a running (claimed) one gets the durable
    /// `cancel_requested` flag and stops before its next step (no claim→running race, SR10).
    pub(super) fn cancel(&self, id: &str) -> Result<JobStatus, RpcError> {
        lock(&self.state)
            .cancel(id, now_ms())
            .map_err(rpc_from_state)
            .and_then(|()| self.job(id))
    }

    pub(super) fn jobs_list(
        &self,
        p: &cv_protocol::JobsListParams,
    ) -> Result<JobsListResult, RpcError> {
        let now = now_ms();
        let q =
            crate::jobs::list_query(p, now).map_err(|m| rpc_err(error_codes::INVALID_PARAMS, m))?;
        let jobs = self.reader()?.list(&q).map_err(rpc_from_state)?;
        Ok(JobsListResult {
            jobs: jobs
                .iter()
                .map(|j| crate::jobs::job_summary(j, now))
                .collect(),
        })
    }

    /// `jobs_stats`: per-type bucket percentiles over the settled jobs, via the read-only handle.
    pub(super) fn jobs_stats(
        &self,
        p: &cv_protocol::JobsStatsParams,
    ) -> Result<cv_protocol::JobsStatsResult, RpcError> {
        let jobs = self
            .reader()?
            .telemetry_since(p.since_ms, p.job_type.as_deref())
            .map_err(rpc_from_state)?;
        Ok(crate::jobs::jobs_stats_result(&cv_state::job_stats(&jobs)))
    }

    /// `job_timeline`: one job's stored telemetry; an unknown id is `NoSuchJob` (like `job`).
    pub(super) fn job_timeline(
        &self,
        id: &str,
    ) -> Result<cv_protocol::JobTimelineResult, RpcError> {
        let t = self.reader()?.job_telemetry(id).map_err(rpc_from_state)?;
        t.map(|t| crate::jobs::job_timeline(&t))
            .ok_or_else(|| rpc_from_state(StateError::NoSuchJob(id.to_owned())))
    }

    pub(super) fn job_retry(&self, id: &str) -> Result<JobStatus, RpcError> {
        let outcome = lock(&self.state)
            .retry_job(id, now_ms())
            .map_err(rpc_from_state)?;
        match outcome {
            RetryOutcome::Retried => self.job(id),
            RetryOutcome::Duplicate(other) => Err(rpc_err(
                error_codes::CONFLICT,
                format!("job {other} with the same dedupe key is already pending"),
            )),
        }
    }

    pub(super) fn intake_list(
        &self,
        p: &cv_protocol::IntakeListParams,
    ) -> Result<IntakeListResult, RpcError> {
        let q =
            crate::jobs::intake_query(p).map_err(|m| rpc_err(error_codes::INVALID_PARAMS, m))?;
        let items = self.reader()?.tracker_items(&q).map_err(rpc_from_state)?;
        Ok(IntakeListResult {
            items: items.into_iter().map(crate::jobs::intake_item).collect(),
        })
    }

    /// `tracker_item_set_status`.
    pub(super) fn set_tracker_status(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
        status: Option<char>,
    ) -> Result<(), RpcError> {
        let matched = lock(&self.state)
            .set_tracker_item_status(source_kind, source_key, item_id, status)
            .map_err(internal)?;
        if !matched {
            return Err(rpc_err(
                error_codes::NOT_FOUND,
                format!("no tracker item {source_kind}/{source_key}/{item_id}"),
            ));
        }
        Ok(())
    }

    /// The `status` result.
    pub fn status(&self) -> Result<StatusResult, RpcError> {
        let counts = lock(&self.state).counts().map_err(internal)?;
        let mut queue: BTreeMap<String, u64> = BTreeMap::new();
        for c in counts {
            *queue.entry(c.status.as_str().to_owned()).or_default() +=
                u64::try_from(c.count).unwrap_or(0);
        }
        let reader = self.reader()?;
        let services = reader
            .service_health()
            .map_err(internal)?
            .into_iter()
            .map(crate::jobs::service_health)
            .collect();
        let (tracker_items, link_records) = crate::jobs::store_counts(&reader);
        Ok(StatusResult {
            generation: self.generation(),
            queue,
            config_errors: self.runner().map(Runner::config_errors).unwrap_or_default(),
            services,
            tracker_items,
            link_records,
            memory: None,
        })
    }
}

/// Longest `reconcile_intake` waits for its job (plan auto-intake-sprint WP-6).
pub(super) const RECONCILE_WAIT: Duration = Duration::from_secs(600);

/// The step outcome vars of a job's checkpoints, merged in step order (later keys win).
pub(crate) fn checkpoint_vars(
    cps: &[cv_state::StepCheckpoint],
) -> serde_json::Map<String, serde_json::Value> {
    let mut vars = serde_json::Map::new();
    for cp in cps {
        if let Ok(serde_json::Value::Object(o)) =
            serde_json::from_str::<serde_json::Value>(&cp.outcome)
                .map(|v| v.get("vars").cloned().unwrap_or(serde_json::Value::Null))
        {
            vars.extend(o);
        }
    }
    vars
}

impl Shared {
    /// `reconcile_intake`: enqueues the `intake-reconcile` action, waits (at most
    /// [`RECONCILE_WAIT`]) for it to settle and returns its outcome vars, read from the job's
    /// step checkpoints (where the runner persists each step's outcome).
    pub(super) async fn reconcile_intake(
        &self,
        p: cv_protocol::ReconcileIntakeParams,
    ) -> Result<cv_protocol::ReconcileIntakeResult, RpcError> {
        let mut params = serde_json::Map::new();
        params.insert("dry_run".into(), Value::Bool(p.dry_run));
        let started = crate::runner::run_action(
            self,
            self.runner_or_err()?,
            cv_protocol::RunActionParams {
                action_id: "intake-reconcile".into(),
                params,
                context: cv_protocol::ActionContext::default(),
            },
        )
        .await?;
        let deadline = std::time::Instant::now() + RECONCILE_WAIT;
        let status = loop {
            let s = self.job(&started.job_id)?;
            let settled = matches!(
                s.status,
                cv_protocol::JobState::Done
                    | cv_protocol::JobState::Failed
                    | cv_protocol::JobState::Cancelled
            );
            if settled || std::time::Instant::now() >= deadline {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        let counts = if status.status == cv_protocol::JobState::Done {
            let cps = lock(&self.state)
                .checkpoints(&started.job_id)
                .map_err(rpc_from_state)?;
            checkpoint_vars(&cps)
        } else {
            serde_json::Map::new()
        };
        Ok(cv_protocol::ReconcileIntakeResult {
            job_id: started.job_id,
            status: status.status,
            message: status.message,
            counts,
        })
    }
}

#[cfg(test)]
mod reconcile_tests {
    use cv_state::StepCheckpoint;

    use super::checkpoint_vars;

    #[test]
    fn checkpoint_vars_merge_in_step_order_and_skip_garbage() {
        let cp = |i, o: &str| StepCheckpoint {
            step_index: i,
            outcome: o.to_owned(),
            at_ms: 0,
        };
        let vars = checkpoint_vars(&[
            cp(0, r#"{"vars":{"linked":1,"ticked":9}}"#),
            cp(1, "not json"),
            cp(2, r#"{"output_paths":[]}"#),
            cp(3, r#"{"vars":{"ticked":2}}"#),
        ]);
        assert_eq!(vars["linked"], 1);
        assert_eq!(vars["ticked"], 2);
        assert_eq!(vars.len(), 2);
    }
}
