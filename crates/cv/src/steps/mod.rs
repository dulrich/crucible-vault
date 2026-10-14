//! Step dispatch: the one table mapping each [`Step`] variant to its owning module, plus the
//! contract every module implements (plan `v0-core-daemon` WP-9; bodies in WP-10..13).
//!
//! | Variant | Module | Owner |
//! |---|---|---|
//! | `Materialize` | [`materialize`] | WP-10 |
//! | `Capture` | [`capture`] | WP-10 |
//! | `Guard`, `Move`, `Copy`, `SetProps`, `RemoveProps`, `UpsertTags`, `MarkForwarded` | [`file`] | WP-10 |
//! | `Lint` | [`lint`] | WP-10 |
//! | `Localize` | [`localize`] | WP-11 |
//! | `Agent` | [`agent`] | WP-12 |
//! | `SearchUpsert`, `SearchDelete` | [`search`] | WP-13 |
//! | `RunAction` | here → the runner's child enqueue | WP-9 |
//!
//! A module's `execute` runs on a blocking thread inside a claimed job (I3 holds for the job's
//! resource keys). It reads with [`StepCtx::read`] (content + hash) and mutates **only** through
//! [`StepCtx::write`]/[`StepCtx::create`] with the hash it read (I2): those calls set the shadow
//! log's job id for exactly that write and record the path for the cycle guard (I5).

pub mod agent;
pub mod capture;
pub mod file;
pub mod lint;
pub mod localize;
pub mod materialize;
pub mod search;

use std::path::Path;

use cv_actions::{Action, ConfigSet, Step, StepKind};
use cv_protocol::ActionContext;
use cv_state::FailureReason;
use cv_vault::{ContentHash, VaultError, WriterHandle};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// What a successful step produced. Serialized as the step's checkpoint (audit E2), so a resumed
/// run replays these values instead of re-running the step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StepOutcome {
    /// A note the client should open (the last non-`None` across steps wins → `run_action`'s
    /// `open_path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_path: Option<String>,
    /// Notes created/changed, reported in `job_updated{output_paths}` (accumulated, deduped).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_paths: Vec<String>,
    /// Values later steps read via [`StepCtx::vars`] (merged; later keys win).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub vars: Map<String, Value>,
    /// Stop the action here, successfully (e.g. a guard that does not hold).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stop: bool,
}

/// A step failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    /// The job fails (`reason` typed when the step can name it; `ServiceOutage` → requeueable).
    Failed {
        /// Human-readable message (no note content).
        message: String,
        /// Typed cause.
        reason: Option<FailureReason>,
    },
    /// Not finishable now; the job is requeued after `retry_after_ms` and resumes at this step.
    Deferred {
        /// Delay.
        retry_after_ms: i64,
        /// The unhealthy service, if any.
        service: Option<String>,
    },
}

impl StepError {
    /// A plain failure.
    pub fn failed(message: impl Into<String>) -> Self {
        StepError::Failed {
            message: message.into(),
            reason: None,
        }
    }

    /// The stub error of an unimplemented module.
    pub fn not_implemented(kind: StepKind, wp: &str) -> Self {
        Self::failed(format!("step {} not implemented ({wp})", kind.as_str()))
    }
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::Failed { message, .. } => f.write_str(message),
            StepError::Deferred { retry_after_ms, .. } => {
                write!(f, "deferred for {retry_after_ms} ms")
            }
        }
    }
}

impl From<VaultError> for StepError {
    fn from(e: VaultError) -> Self {
        StepError::failed(e.to_string())
    }
}

/// Enqueues a child run for [`Step::RunAction`] (implemented by the runner: same lane, chain
/// depth limit, cycle check).
pub trait ChildSpawner {
    /// Enqueues `action_id` with `params`; returns the child job id.
    fn spawn_child(&self, action_id: &str, params: Map<String, Value>) -> Result<String, String>;
}

/// Everything a step sees. Built per step by the runner.
pub struct StepCtx<'a> {
    /// The job id (shadow writes are attributed to it).
    pub job_id: &'a str,
    /// The running action.
    pub action: &'a Action,
    /// 0-based index of this step.
    pub step_index: usize,
    /// Run params (`run_action{params}` with `ParamSpec` defaults applied).
    pub params: &'a Map<String, Value>,
    /// Client/trigger context (`active_path` is the triggering note for event runs).
    pub context: &'a ActionContext,
    /// Vars accumulated from earlier steps' outcomes (including checkpointed ones).
    pub vars: &'a Map<String, Value>,
    /// The config snapshot the job runs against; read a module's file via
    /// `config.table("kinds.toml")` etc.
    pub config: &'a ConfigSet,
    /// The vault root (live root, or the overlay root in shadow mode — read through the writer).
    pub vault_root: &'a Path,
    /// The `CV_INDEX_DB` base (open a `cv_index::Reader` for queries).
    pub index_db: &'a Path,
    pub(crate) writer: Option<WriterHandle>,
    pub(crate) children: &'a dyn ChildSpawner,
    pub(crate) written: Vec<String>,
}

impl StepCtx<'_> {
    /// The single writer (I2); fails when the daemon is shutting down.
    pub fn writer(&self) -> Result<&WriterHandle, StepError> {
        self.writer
            .as_ref()
            .ok_or_else(|| StepError::failed("writer unavailable"))
    }

    /// Reads `path` with its hash (the CAS token for a later [`Self::write`]).
    pub fn read(&self, path: &str) -> Result<Option<(Vec<u8>, ContentHash)>, StepError> {
        let p = path.to_owned();
        let r = self.writer()?.call(move |vw| -> cv_vault::Result<_> {
            let bytes = vw.read(&p)?;
            Ok(bytes.map(|b| {
                let h = ContentHash::of(&b);
                (b, h)
            }))
        })??;
        Ok(r)
    }

    /// CAS write attributed to this job: `expected` is the hash the step read (`None` = must not
    /// exist). A mismatch is [`VaultError::Conflict`] and never clobbers.
    pub fn write(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        expected: Option<ContentHash>,
    ) -> Result<ContentHash, StepError> {
        let (p, job) = (path.to_owned(), self.job_id.to_owned());
        let h = self.writer()?.call(move |vw| {
            vw.fs_mut().set_job_id(Some(job));
            let r = vw.write(&p, &bytes, expected);
            vw.fs_mut().set_job_id(None);
            r
        })??;
        self.written.push(path.to_owned());
        Ok(h)
    }

    /// Creates `path` (error if it exists), attributed to this job.
    pub fn create(&mut self, path: &str, bytes: Vec<u8>) -> Result<ContentHash, StepError> {
        let (p, job) = (path.to_owned(), self.job_id.to_owned());
        let h = self.writer()?.call(move |vw| {
            vw.fs_mut().set_job_id(Some(job));
            let r = vw.create(&p, &bytes);
            vw.fs_mut().set_job_id(None);
            r
        })??;
        self.written.push(path.to_owned());
        Ok(h)
    }

    /// Runs `f` on the writer thread with this job's id set for its writes (for multi-op steps
    /// such as rename/delete); `touched` paths are recorded for the cycle guard.
    pub fn with_writer<T: Send + 'static>(
        &mut self,
        touched: &[&str],
        f: impl FnOnce(&mut cv_vault::VaultWriter) -> T + Send + 'static,
    ) -> Result<T, StepError> {
        let job = self.job_id.to_owned();
        let out = self.writer()?.call(move |vw| {
            vw.fs_mut().set_job_id(Some(job));
            let r = f(vw);
            vw.fs_mut().set_job_id(None);
            r
        })?;
        self.written.extend(touched.iter().map(|s| (*s).to_owned()));
        Ok(out)
    }

    /// Paths this step wrote so far.
    pub fn written(&self) -> &[String] {
        &self.written
    }
}

/// Validates one step (config load; a failure disarms the action, I5).
pub fn validate(step: &Step, config: &ConfigSet) -> Result<(), String> {
    match step {
        Step::Materialize(_) => materialize::validate(step, config),
        Step::Capture(_) => capture::validate(step, config),
        Step::Guard(_)
        | Step::Move(_)
        | Step::Copy(_)
        | Step::SetProps(_)
        | Step::RemoveProps(_)
        | Step::UpsertTags(_)
        | Step::MarkForwarded(_) => file::validate(step, config),
        Step::Lint(_) => lint::validate(step, config),
        Step::Localize(_) => localize::validate(step, config),
        Step::Agent(_) => agent::validate(step, config),
        Step::SearchUpsert(_) | Step::SearchDelete(_) => search::validate(step, config),
        Step::RunAction { action_id, .. } => {
            if config.is_known(action_id) {
                Ok(())
            } else {
                Err(format!("run_action names unknown action {action_id:?}"))
            }
        }
    }
}

/// Executes one step (the production [`crate::runner::StepExecutor`]).
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    match step {
        Step::Materialize(_) => materialize::execute(step, ctx),
        Step::Capture(_) => capture::execute(step, ctx),
        Step::Guard(_)
        | Step::Move(_)
        | Step::Copy(_)
        | Step::SetProps(_)
        | Step::RemoveProps(_)
        | Step::UpsertTags(_)
        | Step::MarkForwarded(_) => file::execute(step, ctx),
        Step::Lint(_) => lint::execute(step, ctx),
        Step::Localize(_) => localize::execute(step, ctx),
        Step::Agent(_) => agent::execute(step, ctx),
        Step::SearchUpsert(_) | Step::SearchDelete(_) => search::execute(step, ctx),
        Step::RunAction { action_id, params } => {
            let child = ctx
                .children
                .spawn_child(action_id, params.clone())
                .map_err(StepError::failed)?;
            let mut vars = Map::new();
            vars.insert("child_job_id".into(), Value::String(child));
            Ok(StepOutcome {
                vars,
                ..StepOutcome::default()
            })
        }
    }
}
