//! Job telemetry (plan job-telemetry, state.db v16): the write side inside the store's
//! transactions ([`write`]), the one loader ([`load`]) and the read model with its pure bucket
//! split and per-type stats ([`split`]).

mod load;
mod split;
mod write;

pub use split::{
    AttemptTime, BucketSplit, JobTelemetry, Pcts, StepTime, TypeStats, WaitCause, WaitEvent,
    job_stats, nearest_rank, split_buckets, wait_durations,
};
pub(crate) use write::{insert_wait, note_wait, record_claim, record_settle};

use crate::claim::{self, Eligibility, ProbeMode};
use crate::error::Result;
use crate::health;
use crate::store::{StateDb, StateReader};
use load::Select;
use rusqlite::{TransactionBehavior, params};

impl StateDb {
    /// Permit-wait observation (plan job-telemetry Decision 3): the claim loop calls this when
    /// no runner permit is free. Every claimable queued candidate records why it waits — a busy
    /// key or breaker when one applies (classified read-only; never takes the half-open probe,
    /// audit E2), else the permit, naming the running jobs' actions. One transaction; a
    /// candidate is written only when its cause changed (Decision 4).
    pub fn observe_permit_wait(&mut self, now_ms: i64) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let busy = claim::running_keys(&tx)?;
        let holders = claim::running_actions(&tx)?;
        for (id, raw_keys, service) in claim::candidates(&tx, now_ms)? {
            let Ok(keys) = serde_json::from_str::<Vec<String>>(&raw_keys) else {
                continue;
            };
            let mut h = match &service {
                Some(svc) => health::load(&tx, svc)?,
                None => None,
            };
            let elig = claim::classify(
                &keys,
                service.as_deref(),
                &busy,
                h.as_mut(),
                ProbeMode::Peek,
                now_ms,
            );
            let (cause, blocker) = match elig {
                Eligibility::Wait { cause, blocker } => (cause, blocker),
                Eligibility::Claimable { .. } => (WaitCause::Permit, holders.clone()),
            };
            note_wait(&tx, &id, cause, Some(&blocker), now_ms)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Stamps the start of step `step_index` (`step_kind`) of `job_id`'s current telemetry attempt.
    pub fn step_started(
        &mut self,
        job_id: &str,
        step_index: i64,
        step_kind: &str,
        now_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO job_step_times (job_id, attempt, step_index, step_kind, start_ms, end_ms)
             SELECT id, tel_attempt, ?2, ?3, ?4, NULL FROM jobs WHERE id = ?1",
            params![job_id, step_index, step_kind, now_ms],
        )?;
        Ok(())
    }

    /// Stamps the end of step `step_index` of `job_id`'s current telemetry attempt.
    pub fn step_ended(&mut self, job_id: &str, step_index: i64, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE job_step_times SET end_ms = ?3 WHERE job_id = ?1 AND step_index = ?2
               AND attempt = (SELECT tel_attempt FROM jobs WHERE id = ?1)",
            params![job_id, step_index, now_ms],
        )?;
        Ok(())
    }

    /// Every telemetry row of job `id` (input of [`split_buckets`]); `None` when it does not exist.
    pub fn job_telemetry(&self, id: &str) -> Result<Option<JobTelemetry>> {
        Ok(load::load(&self.conn, Select::Id(id))?.pop())
    }
}

impl StateReader {
    /// Every telemetry row of job `id` (input of [`split_buckets`]); `None` when it does not exist.
    pub fn job_telemetry(&self, id: &str) -> Result<Option<JobTelemetry>> {
        Ok(load::load(&self.conn, Select::Id(id))?.pop())
    }

    /// Telemetry of every job settled at or after `since_ms` (all settled jobs when `None`),
    /// optionally one `job_type`. Batched: one query for the jobs plus one per child table.
    pub fn telemetry_since(
        &self,
        since_ms: Option<i64>,
        job_type: Option<&str>,
    ) -> Result<Vec<JobTelemetry>> {
        load::load(&self.conn, Select::Settled { since_ms, job_type })
    }
}
