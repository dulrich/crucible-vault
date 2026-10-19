//! Job, queue, intake, tracker and status RPC handlers.

use super::*;

pub(super) fn map_job(j: &cv_state::Job) -> JobStatus {
    JobStatus {
        job_id: j.id.clone(),
        status: crate::jobs::job_state(j, now_ms()),
        message: j.error.clone(),
        output_paths: Vec::new(),
        open_path: None,
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
        })
    }
}
