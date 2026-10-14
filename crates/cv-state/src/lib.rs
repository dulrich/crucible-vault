//! `state.db`: versioned migrations, the durable job queue, the service-health breaker,
//! and the `shadow_writes` log (plan `v0-core-daemon` WP-2).
//!
//! Pure and synchronous: no async runtime. Every time-dependent API takes `now_ms`.

mod cutover;
mod error;
pub mod health;
mod id;
mod image_descriptions;
mod job;
pub mod migrate;
mod query;
mod runner_state;
mod store;

#[cfg(test)]
mod tests;

pub use cutover::{CutoverRecord, UnfinishedJob};
pub use error::{Result, StateError};
pub use health::{BreakerState, ServiceFailureKind, ServiceHealth};
pub use id::new_job_id;
pub use image_descriptions::{ImageDescription, ImportSummary};
pub use job::{
    EnqueueOutcome, FailureKind, FailureReason, Job, JobResult, JobSeed, JobStatus, JobTypeSpec,
    Lane, NewJob, namespaced_key,
};
pub use query::{ListQuery, ShadowWrite, StatusCount};
pub use runner_state::StepCheckpoint;
pub use store::{
    DEFAULT_RESEED_WINDOW_MS, DEFAULT_RETENTION_MS, DEFAULT_STALE_LEASE_MS, NewShadowWrite,
    QueueConfig, RequeueBreakdown, StateDb, StateReader,
};
