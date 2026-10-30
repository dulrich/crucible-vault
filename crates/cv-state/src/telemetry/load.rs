//! The one telemetry loader behind [`crate::StateDb::job_telemetry`],
//! [`crate::StateReader::job_telemetry`] and [`crate::StateReader::telemetry_since`]: one query
//! for the selected jobs and one per child table (each filtered by the same `job_id IN (...)`
//! subquery), grouped in Rust. No per-job queries.

use super::split::{AttemptTime, JobTelemetry, StepTime, WaitCause, WaitEvent};
use crate::error::{Result, StateError};
use crate::job::JobStatus;
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use std::collections::HashMap;

/// Which jobs to load.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Select<'a> {
    /// Any existing job by id (queued and running included).
    Id(&'a str),
    /// Terminal jobs settled at or after `since_ms` (all when `None`), optionally one type.
    Settled {
        since_ms: Option<i64>,
        job_type: Option<&'a str>,
    },
}

impl Select<'_> {
    /// The `jobs` WHERE clause and its bound values.
    fn filter(self) -> (&'static str, Vec<Value>) {
        match self {
            Self::Id(id) => ("id = ?1", vec![Value::Text(id.to_owned())]),
            Self::Settled { since_ms, job_type } => (
                "status IN ('done','failed','cancelled') AND settled_at_ms IS NOT NULL \
                 AND settled_at_ms >= COALESCE(?1, 0) AND (?2 IS NULL OR job_type = ?2)",
                vec![
                    since_ms.map_or(Value::Null, Value::Integer),
                    job_type.map_or(Value::Null, |t| Value::Text(t.to_owned())),
                ],
            ),
        }
    }
}

/// Every telemetry row of the jobs `sel` selects, ordered by settle time then id.
pub(crate) fn load(conn: &Connection, sel: Select<'_>) -> Result<Vec<JobTelemetry>> {
    let (filter, args) = sel.filter();
    let rows = conn
        .prepare(&format!(
            "SELECT id, job_type, status, created_ms, settled_at_ms, tel_partial FROM jobs \
             WHERE {filter} ORDER BY settled_at_ms, id"
        ))?
        .query_map(params_from_iter(&args), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut jobs: Vec<JobTelemetry> = Vec::with_capacity(rows.len());
    for (id, job_type, status, created_ms, settled_ms, partial) in rows {
        jobs.push(JobTelemetry {
            id,
            job_type,
            status: JobStatus::parse(&status)?,
            created_ms,
            settled_ms,
            partial,
            attempts: Vec::new(),
            waits: Vec::new(),
            steps: Vec::new(),
        });
    }
    let index: HashMap<String, usize> = jobs
        .iter()
        .enumerate()
        .map(|(i, j)| (j.id.clone(), i))
        .collect();
    let sub = format!("(SELECT id FROM jobs WHERE {filter})");
    let attempts = conn
        .prepare(&format!(
            "SELECT job_id, attempt, claimed_ms, settled_ms FROM job_attempt_times \
             WHERE job_id IN {sub} ORDER BY job_id, attempt"
        ))?
        .query_map(params_from_iter(&args), |r| {
            Ok((
                r.get::<_, String>(0)?,
                AttemptTime {
                    attempt: r.get(1)?,
                    claimed_ms: r.get(2)?,
                    settled_ms: r.get(3)?,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, a) in attempts {
        if let Some(&i) = index.get(&id) {
            jobs[i].attempts.push(a);
        }
    }
    let waits = conn
        .prepare(&format!(
            "SELECT job_id, at_ms, cause, blocker, until_ms FROM job_wait_events \
             WHERE job_id IN {sub} ORDER BY job_id, seq"
        ))?
        .query_map(params_from_iter(&args), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, at_ms, cause, blocker, until_ms) in waits {
        let Some(&i) = index.get(&id) else { continue };
        let Some(parsed) = WaitCause::parse(&cause) else {
            return Err(StateError::Corrupt {
                column: "cause",
                value: cause,
            });
        };
        jobs[i].waits.push(WaitEvent {
            at_ms,
            cause: parsed,
            blocker,
            until_ms,
        });
    }
    let steps = conn
        .prepare(&format!(
            "SELECT job_id, attempt, step_index, step_kind, start_ms, end_ms FROM job_step_times \
             WHERE job_id IN {sub} ORDER BY job_id, attempt, step_index"
        ))?
        .query_map(params_from_iter(&args), |r| {
            Ok((
                r.get::<_, String>(0)?,
                StepTime {
                    attempt: r.get(1)?,
                    step_index: r.get(2)?,
                    step_kind: r.get(3)?,
                    start_ms: r.get(4)?,
                    end_ms: r.get(5)?,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, s) in steps {
        if let Some(&i) = index.get(&id) {
            jobs[i].steps.push(s);
        }
    }
    Ok(jobs)
}
