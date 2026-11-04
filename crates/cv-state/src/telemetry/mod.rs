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
use std::collections::HashSet;

/// The runner's two permit pools at the end of a claim pass (plan job-slots-fast-lane WP-2,
/// audit E4): which job types may use the fast pool, whether each pool has a free permit, and
/// each pool's holders (running jobs' `job_type`s, as the runner recorded them at claim).
#[derive(Debug, Clone, Default)]
pub struct PoolView {
    /// `job_type`s that may use the fast pool (`action:<id>` of every armed `fast` action).
    pub fast_types: HashSet<String>,
    /// The general pool (any job) has a free permit.
    pub general_free: bool,
    /// The fast pool (fast jobs only) has a free permit.
    pub fast_free: bool,
    /// The general pool's holders.
    pub general_holders: Vec<String>,
    /// The fast pool's holders.
    pub fast_holders: Vec<String>,
}

impl StateDb {
    /// Permit-wait observation (plan job-telemetry Decision 3; two pools by plan
    /// job-slots-fast-lane WP-2, audit E4): the claim loop calls this when the general pool is
    /// exhausted. A candidate is observed only when every pool it may use is exhausted
    /// (non-fast: general; fast: general and fast); a candidate with a usable free pool writes
    /// nothing. Every observed candidate records why it waits — a busy key or breaker when one
    /// applies (classified read-only; never takes the half-open probe, audit E2), else `Permit`
    /// naming the holders of exactly its pools (sorted, comma-joined). One transaction; a
    /// candidate is written only when its cause changed (Decision 4). The single-pool case is an
    /// empty `fast_types`.
    pub fn observe_pool_wait(&mut self, now_ms: i64, pools: &PoolView) -> Result<()> {
        self.observe_waits(now_ms, pools)
    }

    fn observe_waits(&mut self, now_ms: i64, p: &PoolView) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let busy = claim::running_keys(&tx)?;
        for claim::Candidate {
            id,
            raw_keys,
            service,
            job_type,
        } in claim::candidates(&tx, now_ms)?
        {
            let fast = p.fast_types.contains(&job_type);
            if p.general_free || (fast && p.fast_free) {
                continue;
            }
            let mut v = p.general_holders.clone();
            if fast {
                v.extend(p.fast_holders.iter().cloned());
            }
            v.sort_unstable();
            let holders = v.join(",");
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
                Eligibility::Claimable { .. } => (WaitCause::Permit, holders),
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
