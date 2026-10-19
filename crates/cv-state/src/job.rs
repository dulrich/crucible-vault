//! Job model: lanes, statuses, results, failure kinds.

use crate::error::{Result, StateError};
use serde::{Deserialize, Serialize};

/// Queue lane. `User` outranks `Background`.
///
/// Port of `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/lanes.ts:3-6` (`LANE_RANK`). Crucible's
/// separate `priority` axis is not carried: lanes are the only ordering beyond the id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Interactive work the user is waiting on.
    User,
    /// Everything else.
    Background,
}

impl Lane {
    /// Claim rank; lower is claimed first.
    #[must_use]
    pub fn rank(self) -> i64 {
        match self {
            Lane::User => 0,
            Lane::Background => 1,
        }
    }

    pub(crate) fn from_rank(rank: i64) -> Result<Self> {
        match rank {
            0 => Ok(Lane::User),
            1 => Ok(Lane::Background),
            other => Err(StateError::Corrupt {
                column: "lane",
                value: other.to_string(),
            }),
        }
    }
}

/// Job lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting to be claimed (possibly deferred until `defer_until_ms`).
    Queued,
    /// Claimed by a runner holding a lease.
    Running,
    /// Terminal: succeeded.
    Done,
    /// Terminal: failed.
    Failed,
    /// Terminal: cancelled.
    Cancelled,
}

impl JobStatus {
    /// Column text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    /// Whether the status is terminal (settled).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
        )
    }

    pub(crate) fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "done" => JobStatus::Done,
            "failed" => JobStatus::Failed,
            "cancelled" => JobStatus::Cancelled,
            other => {
                return Err(StateError::Corrupt {
                    column: "status",
                    value: other.into(),
                });
            }
        })
    }
}

/// Typed failure cause a step can name, so callers never string-match `error`.
///
/// Port of `WorkflowFailureReason` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/types.ts:82-86`),
/// extended with `ServiceOutage`: Crucible derived the service/job split from a regex table
/// over the error text (`failedJobRepair.ts:21-34`) because its errors were strings; here the
/// step declares it, and the table is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// A credential is missing.
    NoApiKey,
    /// A dependency was down / unreachable; retrying once it recovers may succeed.
    ServiceOutage,
}

impl FailureReason {
    /// Column text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FailureReason::NoApiKey => "no_api_key",
            FailureReason::ServiceOutage => "service_outage",
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self> {
        match s {
            "no_api_key" => Ok(FailureReason::NoApiKey),
            "service_outage" => Ok(FailureReason::ServiceOutage),
            other => Err(StateError::Corrupt {
                column: "failure_reason",
                value: other.into(),
            }),
        }
    }
}

/// Failure kind stamped on the row at settle time (`failedJobRepair.ts:106-134`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// A dependency outage — eligible for [`crate::StateDb::requeue_service_failures`].
    Service,
    /// The job failed on its own merits.
    Job,
}

impl FailureKind {
    /// Column text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::Service => "service",
            FailureKind::Job => "job",
        }
    }

    /// The single classification point: a failure is `Service` iff its typed reason is
    /// [`FailureReason::ServiceOutage`].
    #[must_use]
    pub fn classify(reason: Option<FailureReason>) -> Self {
        if reason == Some(FailureReason::ServiceOutage) {
            FailureKind::Service
        } else {
            FailureKind::Job
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self> {
        match s {
            "service" => Ok(FailureKind::Service),
            "job" => Ok(FailureKind::Job),
            other => Err(StateError::Corrupt {
                column: "failure_kind",
                value: other.into(),
            }),
        }
    }
}

/// Outcome of a run — a discriminated union; construct the variant you mean.
///
/// Port of `WorkflowResult` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/types.ts:75-120`).
/// `outputPaths`/`notes` base fields are not carried here (WP-9's runner records notes
/// through [`crate::StateDb::append_notes`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobResult {
    /// Work completed.
    Done,
    /// Failed on its own merits (or, with `ServiceOutage`, because a dependency was down).
    Failed {
        /// Required human-readable error.
        error: String,
        /// Typed cause, when the step can name one.
        reason: Option<FailureReason>,
    },
    /// Not attempted / not finishable now; requeue for later.
    Deferred {
        /// Delay before the job is claimable again.
        retry_after_ms: i64,
        /// The service whose ill health caused the deferral, if any.
        service_unhealthy: Option<String>,
    },
    /// Cancelled.
    Cancelled,
}

/// Where an enqueue came from. Auto sources go through [`crate::StateDb::enqueue_seed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    /// Job type name.
    pub job_type: String,
    /// Lane.
    pub lane: Lane,
    /// Un-namespaced dedupe key; stored as `"<type>::<key>"`. `None`/empty never dedupes.
    pub dedupe_key: Option<String>,
    /// Step parameters.
    pub params: serde_json::Value,
    /// I3 resource keys (note paths or `kind::id`).
    pub resource_keys: Vec<String>,
    /// Service whose breaker gates claiming this job.
    pub service: Option<String>,
}

/// Result of an enqueue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// A new row was inserted.
    Enqueued(String),
    /// A pending/running job with the same dedupe key exists; its id.
    Duplicate(String),
}

/// A stored job row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// Id (sorts in creation order).
    pub id: String,
    /// Type name.
    pub job_type: String,
    /// Lane.
    pub lane: Lane,
    /// Status.
    pub status: JobStatus,
    /// Namespaced dedupe key `"<type>::<key>"`.
    pub dedupe_key: Option<String>,
    /// Params (a corrupt column reads as `null`).
    pub params: serde_json::Value,
    /// I3 resource keys.
    pub resource_keys: Vec<String>,
    /// Gating service.
    pub service: Option<String>,
    /// Creation time.
    pub created_ms: i64,
    /// Lease holder token while running.
    pub claim_token: Option<String>,
    /// Lease start.
    pub claimed_at_ms: Option<i64>,
    /// Not claimable before this time.
    pub defer_until_ms: Option<i64>,
    /// Terminal settle time.
    pub settled_at_ms: Option<i64>,
    /// Number of claims so far.
    pub attempts: i64,
    /// Last error.
    pub error: Option<String>,
    /// Typed failure reason.
    pub failure_reason: Option<FailureReason>,
    /// Failure kind stamped at settle.
    pub failure_kind: Option<FailureKind>,
    /// Free-text run notes.
    pub notes: Option<String>,
}

/// Namespace a dedupe key: `"<type>::<key>"`; empty keys are `None`.
#[must_use]
pub fn namespaced_key(job_type: &str, key: Option<&str>) -> Option<String> {
    match key {
        Some(k) if !k.is_empty() => Some(format!("{job_type}::{k}")),
        _ => None,
    }
}
