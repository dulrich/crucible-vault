//! Step dispatch: the one table mapping each [`StepKind`] to its owning module, plus the
//! [`StepModule`] contract every module implements (plan `v0-core-daemon` WP-9; bodies in
//! WP-10..13; typed params in plan `tn-review-baseline-remediation` WP-R8).
//!
//! | Kind | Module | Owner |
//! |---|---|---|
//! | `Materialize` | [`materialize`] | WP-10 |
//! | `Capture` | [`capture`] | WP-10 |
//! | `Guard`, `Move`, `Copy`, `SetProps`, `RemoveProps`, `UpsertTags`, `MarkForwarded` | [`file`] | WP-10 |
//! | `Lint` | [`lint`] | WP-10 |
//! | `Localize` | [`localize`] | WP-11 |
//! | `Agent` | [`agent`] | WP-12 |
//! | `SearchUpsert`, `SearchDelete` | [`search`] | WP-13 |
//! | `RunAction` | here ([`RunActionStep`]) → the runner's child enqueue | WP-9 |
//! | `LinkScan` | [`link_scan`] | A2 WP-3 |
//! | `YtMetadata`, `YtChannelEnrich` | [`youtube`] | A2 WP-4 |
//! | `XDiscover`, `XMetadata`, `XBackfill` | [`x`] | A2 WP-5 |
//! | `ImageDescribe` | [`image_describe`] | A2 WP-6 |
//! | `TrackYoutube` | [`tracker::youtube`] | A2 WP-7 |
//! | `TrackBlogs` | [`tracker::blogs`] | A2 WP-8 |
//! | `IgnoreVideo` | [`ignore`] | A2 WP-9 |
//! | `DailyBrief` | [`daily_brief`] | A2 WP-9 |
//! | `CaptureRepair` | [`capture_repair`] | usability-sprint WP-2 |
//!
//! A module's `run` runs on a blocking thread inside a claimed job (I3 holds for the job's
//! resource keys). It reads with [`StepCtx::read`] (content + hash) and mutates **only** through
//! [`StepCtx::write`]/[`StepCtx::create`] with the hash it read (I2): those calls record the path for
//! the cycle guard (I5).

pub mod agent;
pub mod capture;
pub mod capture_repair;
#[cfg(test)]
mod capture_repair_tests;
pub mod daily_brief;
pub mod file;
pub mod ignore;
pub mod image_describe;
pub mod link_scan;
pub mod lint;
pub mod localize;
pub mod materialize;
pub mod params;
pub mod search;
pub mod tracker;
pub mod web;
#[cfg(test)]
mod wp10_tests;
#[cfg(test)]
mod wp11_tests;
#[cfg(test)]
mod wp12_tests;
#[cfg(test)]
mod wp13_tests;
pub mod x;
pub mod youtube;

use std::path::Path;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, RunAction, Step, StepKind};
use cv_protocol::ActionContext;
use cv_state::{FailureReason, ServiceFailureKind, StateDb};
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

impl StepOutcome {
    /// Merges a step's typed outcome vars (B9): each serialized field becomes a var named by
    /// the field (later keys win). A non-object serialization is ignored.
    pub(crate) fn set_vars(&mut self, v: &impl Serialize) {
        if let Ok(Value::Object(m)) = serde_json::to_value(v) {
            self.vars.extend(m);
        }
    }

    /// Appends `path` to `output_paths` unless already listed (the one dedupe).
    pub(crate) fn push_output(&mut self, path: String) {
        if !self.output_paths.contains(&path) {
            self.output_paths.push(path);
        }
    }
}

/// The var a `move` step sets to the note it moved **from** (alongside `moved_path`).
pub const MOVED_FROM: &str = "moved_from";

/// Once a step has moved the run's active note, later steps that default to the active note
/// operate on the moved path (plan `editor-feedback-round-2` WP-8): when `o` reports a move whose
/// source is `context.active_path`, the active path becomes the destination. Explicit `path`
/// params are unaffected. The runner applies this to fresh and checkpoint-replayed outcomes alike.
pub fn follow_move(context: &mut ActionContext, o: &StepOutcome) {
    let (Some(Value::String(from)), Some(Value::String(to))) =
        (o.vars.get(MOVED_FROM), o.vars.get("moved_path"))
    else {
        return;
    };
    if context.active_path.as_deref() == Some(from.as_str()) {
        context.active_path = Some(to.clone());
    }
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
        /// Why: a plain wait, or a service failure that feeds the breaker (plan WP-2).
        cause: DeferCause,
    },
}

/// Why a step deferred. Only `Transient`/`Quota` feed the service breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferCause {
    /// Not a service failure: no breaker report, no attempt cap.
    Wait,
    /// A transient service failure: reported to the breaker, bounded retry with backoff.
    Transient(ServiceFailureKind),
    /// Quota exhausted: reported as `RateLimited`, waits for `retry_after`, never exhausts.
    Quota,
}

/// Base delay of a transient retry (doubles per attempt up to [`TRANSIENT_CAP_MS`]).
pub(crate) const TRANSIENT_BASE_MS: i64 = 30_000;
/// Cap of the transient backoff.
pub(crate) const TRANSIENT_CAP_MS: i64 = 30 * 60_000;
/// Transient deferrals past this many claims settle `Failed{ServiceOutage}`.
pub(crate) const TRANSIENT_MAX_ATTEMPTS: i64 = 5;
/// Default quota wait when the service names none.
pub(crate) const QUOTA_DEFAULT_MS: i64 = 60 * 60_000;

/// Maps an HTTP fetch failure to a step disposition (plan WP-2; used by WP-4..8).
///
/// `Transient` → `Deferred{Transient(kind)}` where kind is read from the detail: `"connect"`
/// → `Refused`; `"HTTP 429"` → `RateLimited`; `"HTTP 5xx"` → `ServerError`; anything else
/// (`"timeout"`, `"HTTP 408"`, `"network"`, client errors) → `Timeout`. Delay = server
/// `retry_after` or [`TRANSIENT_BASE_MS`]. `Quota` → `Deferred{Quota}` with `retry_after` or
/// 1 h. `NotFound`/`Permanent`/`Malformed` → `Failed` (never retried).
pub(crate) fn fetch_disposition(e: &cv_providers::web::FetchError) -> StepError {
    use cv_providers::web::FetchError;
    let ms = |d: &Option<std::time::Duration>, default: i64| {
        d.map_or(default, |d| {
            i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
        })
    };
    match e {
        FetchError::Transient {
            service,
            retry_after,
            detail,
        } => {
            let kind = if detail == "connect" {
                ServiceFailureKind::Refused
            } else if detail == "HTTP 429" {
                ServiceFailureKind::RateLimited
            } else if detail.starts_with("HTTP 5") {
                ServiceFailureKind::ServerError
            } else {
                ServiceFailureKind::Timeout
            };
            StepError::Deferred {
                retry_after_ms: ms(retry_after, TRANSIENT_BASE_MS),
                service: Some(service.clone()),
                cause: DeferCause::Transient(kind),
            }
        }
        FetchError::Quota {
            service,
            retry_after,
        } => StepError::Deferred {
            retry_after_ms: ms(retry_after, QUOTA_DEFAULT_MS),
            service: Some(service.clone()),
            cause: DeferCause::Quota,
        },
        other => StepError::failed(other.to_string()),
    }
}

impl StepError {
    /// A plain failure.
    pub fn failed(message: impl Into<String>) -> Self {
        StepError::Failed {
            message: message.into(),
            reason: None,
        }
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
    /// The job id.
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
    /// The vault root (read through the writer).
    pub vault_root: &'a Path,
    /// The `CV_INDEX_DB` base (open a `cv_index::Reader` for queries).
    pub index_db: &'a Path,
    /// The daemon's shared `semsearch` index (`None` outside a runner: search steps fail).
    pub search: Option<&'a cv_search::SearchDb>,
    /// The daemon's `state.db` (e.g. image descriptions for the search feed). Lock briefly and
    /// never across a writer call; steps run on a blocking thread, so a std `Mutex` is fine.
    pub state: &'a Arc<Mutex<StateDb>>,
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

    /// CAS write: `expected` is the hash the step read (`None` = must not
    /// exist). A mismatch is [`VaultError::Conflict`] and never clobbers.
    pub fn write(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        expected: Option<ContentHash>,
    ) -> Result<ContentHash, StepError> {
        let p = path.to_owned();
        let h = self
            .writer()?
            .call(move |vw| vw.write(&p, &bytes, expected))??;
        self.written.push(path.to_owned());
        Ok(h)
    }

    /// Creates `path` (error if it exists).
    pub fn create(&mut self, path: &str, bytes: Vec<u8>) -> Result<ContentHash, StepError> {
        let p = path.to_owned();
        let h = self.writer()?.call(move |vw| vw.create(&p, &bytes))??;
        self.written.push(path.to_owned());
        Ok(h)
    }

    /// Runs `f` on the writer thread (for multi-op steps such as rename/delete); `touched` paths are recorded for the cycle guard.
    pub fn with_writer<T: Send + 'static>(
        &mut self,
        touched: &[&str],
        f: impl FnOnce(&mut cv_vault::VaultWriter) -> T + Send + 'static,
    ) -> Result<T, StepError> {
        let out = self.writer()?.call(f)?;
        self.written.extend(touched.iter().map(|s| (*s).to_owned()));
        Ok(out)
    }

    /// Paths this step wrote so far.
    pub fn written(&self) -> &[String] {
        &self.written
    }

    /// A sub-context for an inline step call (e.g. the tracker's intake capture): the same run,
    /// with `vars` in place of this step's vars and its own (empty) written list.
    pub fn with_vars<'b>(&'b self, vars: &'b Map<String, Value>) -> StepCtx<'b> {
        StepCtx {
            job_id: self.job_id,
            action: self.action,
            step_index: self.step_index,
            params: self.params,
            context: self.context,
            vars,
            config: self.config,
            vault_root: self.vault_root,
            index_db: self.index_db,
            search: self.search,
            state: self.state,
            writer: self.writer.clone(),
            children: self.children,
            written: Vec::new(),
        }
    }
}

/// The contract every step module implements (plan tn-review-baseline-remediation WP-R8, SM5):
/// `parse` turns the step's TOML params into typed `Params` (config load: a failure disarms the
/// action, I5; allowed keys come from the `cv_schema` step record), `run` executes them.
/// [`validate`] is `parse` with the result dropped; [`execute`] parses once and runs.
pub trait StepModule {
    /// The typed params.
    type Params;
    /// Parses and validates the params of a `kind` step against `config`.
    fn parse(kind: StepKind, t: &toml::Table, config: &ConfigSet) -> Result<Self::Params, String>;
    /// Runs the step.
    fn run(p: Self::Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError>;
}

/// Params of a step that takes none (`daily_brief`, `track_blogs`): a stray key disarms the
/// action at config load (I5).
pub(crate) fn no_params(kind: StepKind, t: &toml::Table) -> Result<(), String> {
    params::check_keys(t, kind, kind.as_str())
}

/// `run_action`: enqueue another action as a child job (the runner's [`ChildSpawner`]).
pub struct RunActionStep;

impl StepModule for RunActionStep {
    type Params = RunAction;

    fn parse(_: StepKind, t: &toml::Table, config: &ConfigSet) -> Result<RunAction, String> {
        let r = RunAction::parse(t)?;
        if config.is_known(&r.action_id) {
            Ok(r)
        } else {
            Err(format!("run_action names unknown action {:?}", r.action_id))
        }
    }

    fn run(p: RunAction, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        let child = ctx
            .children
            .spawn_child(&p.action_id, p.params)
            .map_err(StepError::failed)?;
        let mut out = StepOutcome::default();
        out.set_vars(&RunActionVars {
            child_job_id: child,
        });
        Ok(out)
    }
}

/// `run_action` outcome vars.
#[derive(Serialize)]
struct RunActionVars {
    child_job_id: String,
}

/// Generates [`validate`] and [`execute`] from one `kind => module` table.
macro_rules! step_modules {
    ($($kind:ident => $m:ty,)*) => {
        /// Validates one step (config load; a failure disarms the action, I5).
        pub fn validate(step: &Step, config: &ConfigSet) -> Result<(), String> {
            match step.kind {
                $(StepKind::$kind => <$m>::parse(step.kind, &step.params, config).map(drop),)*
            }
        }

        /// Executes one step (the production [`crate::runner::StepExecutor`]).
        pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
            match step.kind {
                $(StepKind::$kind => {
                    let p = <$m>::parse(step.kind, &step.params, ctx.config)
                        .map_err(StepError::failed)?;
                    <$m>::run(p, ctx)
                })*
            }
        }
    };
}

step_modules! {
    Materialize => materialize::Materialize,
    Capture => capture::Capture,
    Guard => file::File,
    Move => file::File,
    Copy => file::File,
    SetProps => file::File,
    RemoveProps => file::File,
    UpsertTags => file::File,
    MarkForwarded => file::File,
    Lint => lint::Lint,
    Localize => localize::Localize,
    Agent => agent::Agent,
    SearchUpsert => search::Search,
    SearchDelete => search::Search,
    LinkScan => link_scan::LinkScan,
    YtMetadata => youtube::Youtube,
    YtChannelEnrich => youtube::Youtube,
    XDiscover => x::X,
    XMetadata => x::X,
    XBackfill => x::X,
    ImageDescribe => image_describe::ImageDescribe,
    TrackYoutube => tracker::youtube::TrackYoutube,
    TrackBlogs => tracker::blogs::TrackBlogs,
    IgnoreVideo => ignore::IgnoreVideo,
    DailyBrief => daily_brief::DailyBrief,
    CaptureRepair => capture_repair::CaptureRepair,
    RunAction => RunActionStep,
}

#[cfg(test)]
mod fetch_disposition_tests {
    use std::time::Duration;

    use cv_providers::web::FetchError;
    use cv_state::ServiceFailureKind as K;

    use super::{DeferCause, QUOTA_DEFAULT_MS, StepError, TRANSIENT_BASE_MS, fetch_disposition};

    fn transient(detail: &str, retry: Option<u64>) -> FetchError {
        FetchError::Transient {
            service: "svc".into(),
            retry_after: retry.map(Duration::from_millis),
            detail: detail.into(),
        }
    }

    fn deferred(ms: i64, cause: DeferCause) -> StepError {
        StepError::Deferred {
            retry_after_ms: ms,
            service: Some("svc".into()),
            cause,
        }
    }

    #[test]
    fn maps_every_fetch_error() {
        let t = |k| deferred(TRANSIENT_BASE_MS, DeferCause::Transient(k));
        assert_eq!(
            fetch_disposition(&transient("connect", None)),
            t(K::Refused)
        );
        assert_eq!(
            fetch_disposition(&transient("timeout", None)),
            t(K::Timeout)
        );
        assert_eq!(
            fetch_disposition(&transient("network", None)),
            t(K::Timeout)
        );
        assert_eq!(
            fetch_disposition(&transient("HTTP 408", None)),
            t(K::Timeout)
        );
        assert_eq!(
            fetch_disposition(&transient("HTTP 503", None)),
            t(K::ServerError)
        );
        assert_eq!(
            fetch_disposition(&transient("HTTP 429", Some(5_000))),
            deferred(5_000, DeferCause::Transient(K::RateLimited))
        );
        let quota = |r| FetchError::Quota {
            service: "svc".into(),
            retry_after: r,
        };
        assert_eq!(
            fetch_disposition(&quota(None)),
            deferred(QUOTA_DEFAULT_MS, DeferCause::Quota)
        );
        assert_eq!(
            fetch_disposition(&quota(Some(Duration::from_secs(7)))),
            deferred(7_000, DeferCause::Quota)
        );
        for e in [
            FetchError::NotFound {
                service: "svc".into(),
                status: 404,
            },
            FetchError::Permanent {
                service: "svc".into(),
                status: 403,
            },
            FetchError::Malformed {
                service: "svc".into(),
                detail: "invalid JSON".into(),
            },
        ] {
            assert!(matches!(fetch_disposition(&e), StepError::Failed { .. }));
        }
    }
}
