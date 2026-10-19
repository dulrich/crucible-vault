//! `state.db`: versioned migrations, the durable job queue and the service-health breaker
//! (plan `v0-core-daemon` WP-2). The `shadow_writes` table stays in the schema with no API
//! (shadow mode retired in plan tn-review-baseline-remediation WP-R6).
//!
//! Pure and synchronous: no async runtime. Every time-dependent API takes `now_ms`.

mod cutover;
mod error;
pub mod health;
mod id;
mod image_descriptions;
mod job;
pub mod link_records;
pub mod migrate;
mod query;
mod runner_state;
mod store;
mod tracker;

#[cfg(test)]
mod tests;

pub use cutover::CutoverRecord;
pub use error::{Result, StateError};
pub use health::{BreakerState, ServiceFailureKind, ServiceHealth};
pub use id::new_job_id;
pub use image_descriptions::{ImageDescription, ImportSummary};
pub use job::{
    EnqueueOutcome, FailureKind, FailureReason, Job, JobResult, JobStatus, Lane, NewJob,
    namespaced_key,
};
pub use link_records::{
    LinkRecord, LinkRecordInput, MergeFlags, UpsertOutcome, read_crucible_link_records,
};
pub use query::{ListQuery, StatusCount};
pub use runner_state::StepCheckpoint;
pub use store::{
    BreakerFailure, ChangeHook, DEFAULT_RETENTION_MS, DEFAULT_STALE_LEASE_MS, QueueConfig,
    RequeueBreakdown, RetryOutcome, StateDb, StateReader,
};
pub use tracker::{IntakeQuery, TrackerItem, TrackerOutcome};
