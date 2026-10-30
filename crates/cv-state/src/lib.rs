//! `state.db`: versioned migrations, the durable job queue and the service-health breaker
//! (plan `v0-core-daemon` WP-2).
//!
//! Pure and synchronous: no async runtime. Every time-dependent API takes `now_ms`.

mod dup_labels;
mod error;
pub mod health;
mod id;
mod image_descriptions;
mod job;
pub mod link_records;
mod mail_boilerplate;
pub mod migrate;
mod query;
mod runner_state;
mod store;
mod telemetry;
mod tracker;
mod trash;

#[cfg(test)]
mod tests;

pub use dup_labels::DupLabel;
pub use error::{Result, StateError};
pub use health::{ServiceFailureKind, ServiceHealth};
pub use id::new_job_id;
pub use image_descriptions::ImageDescription;
pub use job::{EnqueueOutcome, FailureReason, Job, JobResult, JobStatus, NewJob, namespaced_key};
pub use link_records::{LinkRecord, LinkRecordInput, MergeFlags, UpsertOutcome};
pub use query::{
    AttemptRun, AttemptTime, BucketSplit, JobTelemetry, ListQuery, StatusCount, StepTime,
    WaitCause, WaitEvent, split_buckets,
};
pub use runner_state::StepCheckpoint;
pub use store::{
    BreakerFailure, ChangeHook, DEFAULT_RETENTION_MS, DEFAULT_STALE_LEASE_MS, QueueConfig,
    RequeueBreakdown, RetryOutcome, StateDb, StateReader,
};
pub use tracker::{IntakeQuery, Rekey, TrackerEvidence, TrackerItem};
pub use trash::{NewTrashEntry, TrashEntry};
