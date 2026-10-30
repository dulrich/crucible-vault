//! Job and job-telemetry wire types (`job`, `cancel`, `jobs_list`, `jobs_stats`, `job_timeline`,
//! `job_updated`); re-exported at the crate root. The telemetry vocabulary ([`WaitCause`],
//! [`BucketPcts`]) is owned here and re-used by `cv-state`'s read model.

use crate::enums::{self, FailureKind, Lane};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobIdParams {
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Running,
    Done,
    Failed,
    Deferred,
    Cancelled,
}

/// A job's note hold on the wire: the held vault path and the holding action's display label (the
/// editor's overlay text). Flattened into [`JobStatus`] as `held_note` / `action_label`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteHold {
    #[serde(rename = "held_note")]
    pub path: String,
    /// Absent from older daemons (the editor falls back to the job id).
    #[serde(rename = "action_label", default)]
    pub label: String,
}

/// Result of `job` / `cancel`, and payload of `job_updated`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    pub job_id: String,
    pub status: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Notification-only: the `job_updated` payload carries the run's written paths; the
    /// `job` / `cancel` results always send `[]` (settled outputs are not stored).
    #[serde(default)]
    pub output_paths: Vec<String>,
    /// Terminal statuses only: the note the run asks the client to open (e.g. a `move`
    /// destination). Additive (absent from older daemons; omitted when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_path: Option<String>,
    /// The note this job holds while it runs (its action declares `resources.note = true` and the
    /// run has an active note). Set on `running` (re-sent when a move retargets it) and,
    /// informationally, on the terminal status that releases it. Additive (plan round-17 WP-2).
    /// Flattened: on the wire it is the top-level `held_note` + `action_label` pair, both absent
    /// when there is no hold.
    #[serde(flatten)]
    pub hold: Option<NoteHold>,
}

/// `jobs_list` params. `status`: `queued|running|done|failed|cancelled|deferred`, where
/// `deferred` is queued with a future `defer_until_ms` and `queued` excludes those rows.
/// `limit` defaults to 50 (max 500); `recent_first` defaults to `true`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_first: Option<bool>,
}

/// One queue row in `jobs_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSummary {
    pub job_id: String,
    pub job_type: String,
    pub lane: Lane,
    /// `queued|running|done|failed|cancelled|deferred` ([`JobState::Pending`] is `queued`).
    #[serde(with = "enums::queue_status")]
    pub status: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub attempts: i64,
    pub created_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_until_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<FailureKind>,
}

impl JobSummary {
    /// The wire word for [`JobSummary::status`] (`queued` for [`JobState::Pending`]).
    #[must_use]
    pub fn status_str(&self) -> &'static str {
        enums::queue_status::as_str(self.status)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsListResult {
    pub jobs: Vec<JobSummary>,
}

/// `jobs_stats` params (plan job-telemetry WP-2): settled jobs with `settled_at_ms >= since_ms`
/// (all retained rows when absent), optionally one `job_type`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsStatsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
}

/// Why a queued job was not running, as recorded by the claim pass / claim loop. The serde and
/// stored strings are identical (`deferred|permit|key|breaker`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitCause {
    /// Waiting out a `defer_until_ms` (deferred settle); the blocker is the deferring service.
    Deferred,
    /// Claimable, but every runner permit was taken; the blocker names the running actions.
    Permit,
    /// A resource key was held by a running job; the blocker is that job's action.
    Key,
    /// The job's service breaker was open (or half-open with its probe taken).
    Breaker,
}

impl WaitCause {
    /// Stored (and wire) form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Permit => "permit",
            Self::Key => "key",
            Self::Breaker => "breaker",
        }
    }

    /// Parse the stored form.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "deferred" => Some(Self::Deferred),
            "permit" => Some(Self::Permit),
            "key" => Some(Self::Key),
            "breaker" => Some(Self::Breaker),
            _ => None,
        }
    }
}

/// p50 / p95 / max of one wait or run bucket, in ms (nearest-rank over the attributed jobs).
/// Also `cv-state`'s aggregate type (re-exported there as `Pcts`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketPcts {
    pub p50: i64,
    pub p95: i64,
    pub max: i64,
}

/// Measured wait attributed to one `(cause, blocker)` (`blocker` is `""` when none was named).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockerMs {
    pub cause: WaitCause,
    pub blocker: String,
    pub ms: i64,
}

/// From the read model's `(cause, blocker, ms)` triple.
impl From<(WaitCause, String, i64)> for BlockerMs {
    fn from((cause, blocker, ms): (WaitCause, String, i64)) -> Self {
        Self { cause, blocker, ms }
    }
}

/// One `job_type` row of `jobs_stats`. Aggregates only: no params, paths or note content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobTypeStats {
    pub job_type: String,
    /// Attributed jobs.
    pub n: u64,
    /// Settled jobs excluded for partial coverage.
    pub partial: u64,
    pub total: BucketPcts,
    pub deferral: BucketPcts,
    pub permit: BucketPcts,
    pub key: BucketPcts,
    pub breaker: BucketPcts,
    pub run: BucketPcts,
    pub residual: BucketPcts,
    /// Top blockers by summed ms, largest first.
    pub blockers: Vec<BlockerMs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsStatsResult {
    /// Sorted by summed total ms, descending.
    pub types: Vec<JobTypeStats>,
    /// Sum of the per-type `partial` counts.
    pub partial_total: u64,
}

/// One stored wait-cause change of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineWait {
    pub at_ms: i64,
    pub cause: WaitCause,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
    /// Measured duration; absent when the job's intervals are unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// One executed step of one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineStep {
    pub index: i64,
    pub kind: String,
    pub start_ms: i64,
    /// Absent when the step never stamped an end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
}

/// One claim of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineAttempt {
    pub attempt: i64,
    pub claimed_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_ms: Option<i64>,
    pub steps: Vec<TimelineStep>,
}

/// The bucket split of a settled, fully instrumented job: every ms lands in exactly one bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSplit {
    pub total_ms: i64,
    pub deferral_ms: i64,
    pub permit_ms: i64,
    pub key_ms: i64,
    pub breaker_ms: i64,
    pub run_ms: i64,
    pub residual_ms: i64,
    pub blockers: Vec<BlockerMs>,
}

/// Why a [`JobTimelineResult`] carries no split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimelineUnavailable {
    /// The job has not settled yet.
    #[serde(rename = "not settled")]
    NotSettled,
    /// Some telemetry row of the job is partial (e.g. a recovered lease).
    #[serde(rename = "partial coverage")]
    PartialCoverage,
}

impl TimelineUnavailable {
    /// Wire (and rendered) text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSettled => "not settled",
            Self::PartialCoverage => "partial coverage",
        }
    }
}

impl std::fmt::Display for TimelineUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `job_timeline` result: the stored telemetry of one job. Exactly one of `unavailable` and
/// `split` is present: a partial-coverage or unsettled job never carries a fabricated zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobTimelineResult {
    pub job_id: String,
    pub job_type: String,
    /// Stored status (a queued job is [`JobState::Pending`]).
    pub status: JobState,
    pub created_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_ms: Option<i64>,
    /// Why `split` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<TimelineUnavailable>,
    pub waits: Vec<TimelineWait>,
    pub attempts: Vec<TimelineAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<TimelineSplit>,
}
