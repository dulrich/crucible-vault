//! Runner bookkeeping (plan `v0-core-daemon` WP-9, migration v2): per-step checkpoints (audit
//! item E2 — applied steps are never re-run on recovery) and schedule last-run times.

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// One committed step of a job: the step's serialized outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCheckpoint {
    /// 0-based step index within the action.
    pub step_index: i64,
    /// The step's outcome (JSON text, opaque to the store).
    pub outcome: String,
    /// Commit time.
    pub at_ms: i64,
}

impl StateDb {
    /// Records that step `step_index` of `job_id` completed with `outcome`. Idempotent: a repeat
    /// for the same step replaces the row.
    pub fn put_checkpoint(
        &mut self,
        job_id: &str,
        step_index: i64,
        outcome: &str,
        at_ms: i64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO job_step_checkpoints (job_id, step_index, outcome, at_ms) VALUES (?1, ?2, ?3, ?4)",
            params![job_id, step_index, outcome, at_ms],
        )?;
        Ok(())
    }

    /// Every checkpoint of `job_id`, in step order.
    pub fn checkpoints(&self, job_id: &str) -> Result<Vec<StepCheckpoint>> {
        let mut stmt = self.conn().prepare(
            "SELECT step_index, outcome, at_ms FROM job_step_checkpoints WHERE job_id = ?1 ORDER BY step_index",
        )?;
        let rows = stmt.query_map(params![job_id], |r| {
            Ok(StepCheckpoint {
                step_index: r.get(0)?,
                outcome: r.get(1)?,
                at_ms: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The last time `action_id`'s schedule fired, if ever.
    pub fn schedule_last_run(&self, action_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT last_run_ms FROM schedule_runs WHERE action_id = ?1",
                params![action_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Records that `action_id`'s schedule fired at `at_ms` (set at enqueue time).
    pub fn set_schedule_run(&mut self, action_id: &str, at_ms: i64) -> Result<()> {
        record_schedule_run(self.conn(), action_id, at_ms)
    }
}

/// Upserts `schedule_runs` (shared by [`StateDb::set_schedule_run`] and
/// [`StateDb::enqueue_scheduled`]'s transaction).
pub(crate) fn record_schedule_run(
    conn: &rusqlite::Connection,
    action_id: &str,
    at_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO schedule_runs (action_id, last_run_ms) VALUES (?1, ?2) ON CONFLICT(action_id) DO UPDATE SET last_run_ms = excluded.last_run_ms",
        params![action_id, at_ms],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::*;
    use cv_protocol::Lane;
    use serde_json::json;

    fn new_job() -> NewJob {
        NewJob {
            job_type: "action:a".into(),
            lane: Lane::User,
            dedupe_key: None,
            params: json!({}),
            resource_keys: vec!["n.md".into()],
            service: None,
        }
    }

    #[test]
    fn checkpoints_and_schedule_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(&dir.path().join("s.db"), 1, QueueConfig::default()).unwrap();
        s.put_checkpoint("j", 1, "{\"b\":1}", 3).unwrap();
        s.put_checkpoint("j", 0, "{\"a\":1}", 2).unwrap();
        s.put_checkpoint("j", 1, "{\"b\":2}", 4).unwrap();
        let cps = s.checkpoints("j").unwrap();
        assert_eq!(cps.iter().map(|c| c.step_index).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(cps[1].outcome, "{\"b\":2}");
        assert!(s.checkpoints("other").unwrap().is_empty());
        assert_eq!(s.schedule_last_run("x").unwrap(), None);
        s.set_schedule_run("x", 100).unwrap();
        s.set_schedule_run("x", 200).unwrap();
        assert_eq!(s.schedule_last_run("x").unwrap(), Some(200));
    }

    #[test]
    fn prune_terminal_removes_pruned_jobs_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = QueueConfig {
            retention_ms: 1_000,
            ..QueueConfig::default()
        };
        let mut s = StateDb::open(&dir.path().join("s.db"), 1, cfg).unwrap();
        let EnqueueOutcome::Enqueued(old) = s.enqueue(new_job(), 1).unwrap() else {
            panic!("enqueue")
        };
        let EnqueueOutcome::Enqueued(live) = s.enqueue(new_job(), 2).unwrap() else {
            panic!("enqueue")
        };
        let j = s.claim(10).unwrap().unwrap();
        assert_eq!(j.id, old);
        s.put_checkpoint(&old, 0, "{}", 10).unwrap();
        s.settle(&old, &JobResult::Done, 20).unwrap();
        s.put_checkpoint(&live, 0, "{}", 21).unwrap();
        assert_eq!(s.prune_terminal(5_000).unwrap(), 1);
        assert!(s.checkpoints(&old).unwrap().is_empty());
        assert_eq!(s.checkpoints(&live).unwrap().len(), 1);
    }
}
