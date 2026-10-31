//! Step dispatch: the one table mapping each [`StepKind`] to its owning module, plus the
//! [`StepModule`] contract every module implements (plan `v0-core-daemon` WP-9; bodies in
//! WP-10..13).
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
//! | `TrackMail` | [`mail`] | auto-intake-sprint WP-4 |
//! | `AutoFile` | [`auto_file`] | auto-intake-sprint WP-5 |
//! | `IntakeReconcile` | [`reconcile`] | auto-intake-sprint WP-6 |
//! | `IntakeFinalize` | [`intake_finalize`] | intake-followups-mail-youtube WP-4 |
//! | `IgnoreVideo` | [`ignore`] | A2 WP-9 |
//! | `DailyBrief` | [`daily_brief`] | A2 WP-9 |
//! | `DupesScan` | [`dupes`] | round-12-feedback-todos WP-6 |
//! | `DupesResolve` | [`dupes_resolve`] | round-12-feedback-todos WP-7 |
//! | `CancelSourceIntake` | [`cancel_source`] | cancel-source-intake WP-1 |
//! | `IntakeEnrich` | [`intake_enrich`] | intake-auto-enrich WP-3 |
//! | `TranscriptRefine` | [`transcript_refine`] | youtube-transcript-auto-refine WP-1 |
//! | `YtClipTidy` | [`yt_clip_tidy`] | round-19-yt-cleanup-intake WP-2 |
//!
//! A module's `run` runs on a blocking thread inside a claimed job (I3 holds for the job's
//! resource keys). It reads with [`StepCtx::read`] (content + hash) and mutates **only** through
//! [`StepCtx::write`]/[`StepCtx::create`] with the hash it read, or [`StepCtx::modify`] (I2):
//! those calls record the path for the cycle guard (I5).

pub mod agent;
#[cfg(test)]
mod agent_tests;
pub mod auto_file;
#[cfg(test)]
mod auto_file_tests;
pub mod cancel_source;
#[cfg(test)]
mod cancel_source_tests;
pub mod capture;
#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod config_tests;
pub mod daily_brief;
pub mod dupes;
pub mod dupes_resolve;
#[cfg(test)]
mod dupes_resolve_tests;
#[cfg(test)]
mod dupes_tests;
pub mod file;
#[cfg(test)]
mod file_tests;
pub mod ignore;
pub mod image_describe;
pub mod intake_enrich;
#[cfg(test)]
mod intake_enrich_tests;
pub mod intake_finalize;
#[cfg(test)]
mod intake_finalize_tests;
pub mod link_scan;
pub mod lint;
#[cfg(test)]
mod lint_tests;
pub mod localize;
#[cfg(test)]
mod localize_tests;
pub mod mail;
#[cfg(test)]
mod mail_tests;
pub mod materialize;
#[cfg(test)]
mod materialize_tests;
pub(crate) mod metadata;
pub mod params;
pub mod reconcile;
mod reconcile_backfill;
#[cfg(test)]
mod reconcile_backfill_tests;
mod reconcile_boilerplate;
mod reconcile_repair;
#[cfg(test)]
mod reconcile_tests;
pub mod search;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
pub(crate) mod testkit;
pub mod tidy;
pub mod tracker;
pub mod transcript_refine;
#[cfg(test)]
mod transcript_refine_tests;
pub(crate) mod util;
pub mod web;
pub mod x;
pub mod youtube;
pub mod yt_clip_tidy;
#[cfg(test)]
mod yt_clip_tidy_tests;

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
    /// The step's target note no longer exists. A retried job settles this as skipped ("note
    /// moved or deleted", no Error notification); a first attempt fails as before.
    NoteGone {
        /// The vault path that was not found.
        path: String,
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

/// Retry delay of a step whose CAS write kept conflicting with concurrent edits (SR7).
pub(crate) const CONFLICT_RETRY_MS: i64 = 5_000;
/// Read → transform → CAS-write attempts of [`StepCtx::modify`] before it defers (SR7).
pub(crate) const MODIFY_ATTEMPTS: usize = 3;

impl StepError {
    /// A plain failure.
    pub fn failed(message: impl Into<String>) -> Self {
        StepError::Failed {
            message: message.into(),
            reason: None,
        }
    }

    /// The one mapping of a typed HTTP/provider failure to a step disposition (B8):
    ///
    /// | `FailureKind` | disposition |
    /// |---|---|
    /// | `Connect` | `Deferred{Transient(Refused)}` |
    /// | `Timeout` | `Deferred{Transient(Timeout)}` |
    /// | `RateLimited` | `Deferred{Transient(RateLimited)}` |
    /// | `Server` | `Deferred{Transient(ServerError)}` |
    /// | `Quota` | `Deferred{Quota}`, `retry_after` or [`QUOTA_DEFAULT_MS`] |
    /// | `Auth` | `Failed{NoApiKey}` |
    /// | `Permanent` | `Failed` |
    ///
    /// A transient delay is the failure's `retry_after`, else `transient_default_ms` (web
    /// fetches: [`TRANSIENT_BASE_MS`]; providers: the agent's 60 s outage defer). `message` is
    /// the failure text (no note content) used when the step fails.
    pub(crate) fn from_failure(
        service: &str,
        failure: cv_providers::Failure,
        transient_default_ms: i64,
        message: String,
    ) -> Self {
        use cv_providers::FailureKind as F;
        let ms = |default: i64| {
            failure.retry_after.map_or(default, |d| {
                i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
            })
        };
        let transient = |kind| StepError::Deferred {
            retry_after_ms: ms(transient_default_ms),
            service: Some(service.to_owned()),
            cause: DeferCause::Transient(kind),
        };
        match failure.kind {
            F::Connect => transient(ServiceFailureKind::Refused),
            F::Timeout => transient(ServiceFailureKind::Timeout),
            F::RateLimited => transient(ServiceFailureKind::RateLimited),
            F::Server => transient(ServiceFailureKind::ServerError),
            F::Quota => StepError::Deferred {
                retry_after_ms: ms(QUOTA_DEFAULT_MS),
                service: Some(service.to_owned()),
                cause: DeferCause::Quota,
            },
            F::Auth => StepError::Failed {
                message,
                reason: Some(FailureReason::NoApiKey),
            },
            F::Permanent => StepError::failed(message),
        }
    }

    /// [`Self::from_failure`] of a web fetch (breaker key = the fetch's service).
    pub(crate) fn from_fetch(e: &cv_providers::web::FetchError) -> Self {
        Self::from_failure(e.service(), e.failure(), TRANSIENT_BASE_MS, e.to_string())
    }

    /// The deferral of a CAS write that lost to a concurrent edit (SR7): a plain wait, never a
    /// failure and never a breaker report.
    pub(crate) fn conflict() -> Self {
        StepError::Deferred {
            retry_after_ms: CONFLICT_RETRY_MS,
            service: None,
            cause: DeferCause::Wait,
        }
    }

    /// Whether this is the [`Self::conflict`] deferral (a CAS write that lost a race).
    pub(crate) fn is_conflict(&self) -> bool {
        matches!(
            self,
            StepError::Deferred {
                cause: DeferCause::Wait,
                service: None,
                ..
            }
        )
    }
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::Failed { message, .. } => f.write_str(message),
            StepError::NoteGone { path } => write!(f, "{path} not found"),
            StepError::Deferred { retry_after_ms, .. } => {
                write!(f, "deferred for {retry_after_ms} ms")
            }
        }
    }
}

impl From<VaultError> for StepError {
    /// A CAS conflict defers ([`StepError::conflict`], SR7); anything else fails.
    fn from(e: VaultError) -> Self {
        match e {
            VaultError::Conflict { .. } => StepError::conflict(),
            e => StepError::failed(e.to_string()),
        }
    }
}

impl From<cv_note::FmError> for StepError {
    /// A frontmatter edit/parse error fails the step (the one frontmatter-error mapping, M2).
    fn from(e: cv_note::FmError) -> Self {
        StepError::failed(e.to_string())
    }
}

impl From<cv_state::StateError> for StepError {
    /// A `state.db` error fails the step (the one state-error mapping, L6).
    fn from(e: cv_state::StateError) -> Self {
        StepError::failed(format!("state: {e}"))
    }
}

impl From<cv_index::Error> for StepError {
    /// An `index.db` error fails the step (the one index-error mapping, D2).
    fn from(e: cv_index::Error) -> Self {
        StepError::failed(format!("index: {e}"))
    }
}

/// Enqueues a child run for [`Step::RunAction`] (implemented by the runner: same lane, chain
/// depth limit, cycle check).
pub trait ChildSpawner {
    /// Enqueues `action_id` with `params`; returns the child job id.
    fn spawn_child(&self, action_id: &str, params: Map<String, Value>) -> Result<String, String>;

    /// Enqueues `action_id` with `params` on `active_path` instead of the run's own active note
    /// (`auto_file` hands enrichment to a supersede target; plan round-16 WP-5).
    fn spawn_child_at(
        &self,
        action_id: &str,
        params: Map<String, Value>,
        active_path: &str,
    ) -> Result<String, String> {
        let _ = (action_id, params, active_path);
        Err("spawning on another path is not supported here".into())
    }
}

/// A boolean job param `key`: a bool, or the string `"true"`/`"false"` (any case); anything
/// else (absent, another type, another string) is `None`.
fn flag_of(params: &Map<String, Value>, key: &str) -> Option<bool> {
    match params.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Some(true),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// The job's boolean param `key` (see [`flag_of`]). A present job flag wins over the step param
/// (`job_flag(ctx, key).unwrap_or(step_value)`), the one precedence rule (D3).
pub(crate) fn job_flag(ctx: &StepCtx<'_>, key: &str) -> Option<bool> {
    flag_of(ctx.params, key)
}

/// Everything a step sees. Built per step by the runner.
pub struct StepCtx<'a> {
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
    /// The system-trash root (`<Config::data_home>/Trash`, what moves to the system trash use);
    /// `None` without a data home or outside a runner.
    pub system_trash: Option<&'a Path>,
    /// The daemon's `state.db` (e.g. image descriptions for the search feed). Lock briefly and
    /// never across a writer call; steps run on a blocking thread, so a std `Mutex` is fine.
    pub state: &'a Arc<Mutex<StateDb>>,
    pub(crate) writer: Option<WriterHandle>,
    pub(crate) children: &'a dyn ChildSpawner,
    pub(crate) written: Vec<String>,
}

impl<'a> StepCtx<'a> {
    /// The locked `state.db` with the one poison policy, recover; hold it briefly.
    pub fn state(&self) -> std::sync::MutexGuard<'a, StateDb> {
        crate::sync::lock(self.state)
    }

    /// The current index generation ([`cv_index::Reader::open_current`] owns the swap retry).
    pub fn index(&self) -> Result<cv_index::Reader, StepError> {
        cv_index::Reader::open_current(self.index_db).map_err(StepError::from)
    }

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

    /// [`Self::read`] for a step whose single target note must exist: a missing note is
    /// [`StepError::NoteGone`] (the runner settles it quietly), not a hard failure.
    pub fn read_required(&self, path: &str) -> Result<(Vec<u8>, ContentHash), StepError> {
        self.read(path)?.ok_or_else(|| StepError::NoteGone {
            path: path.to_owned(),
        })
    }

    /// CAS write: `expected` is the hash the step read (`None` = must not
    /// exist). A mismatch is [`VaultError::Conflict`] — never a clobber — and maps to
    /// [`StepError::conflict`] (a `Deferred{Wait}`).
    pub fn write(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        expected: Option<ContentHash>,
    ) -> Result<ContentHash, StepError> {
        Ok(self.try_write(path, bytes, expected)??)
    }

    /// [`Self::write`] with the vault error unmapped (the outer error is the writer's).
    fn try_write(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        expected: Option<ContentHash>,
    ) -> Result<Result<ContentHash, VaultError>, StepError> {
        #[cfg(test)]
        race::before_write(self, path)?;
        let p = path.to_owned();
        let r = self
            .writer()?
            .call(move |vw| vw.write(&p, &bytes, expected))?;
        if r.is_ok() {
            self.written.push(path.to_owned());
        }
        Ok(r)
    }

    /// Read → pure transform → CAS write, re-applied on a conflict (SR7). `f` gets the note's
    /// current text and returns `Ok(None)` for no change or `Ok(Some(new))`; it may run up to
    /// [`MODIFY_ATTEMPTS`] times (each on freshly read text), so it must be re-appliable. A
    /// conflict on every attempt defers ([`StepError::conflict`]); a missing or non-UTF-8 note
    /// fails. Returns the new hash, or `None` when `f` made no change. Steps that fetch over the
    /// network do so **before** calling this, so no CAS hash is held across a round-trip.
    pub fn modify(
        &mut self,
        path: &str,
        mut f: impl FnMut(&str) -> Result<Option<String>, StepError>,
    ) -> Result<Option<ContentHash>, StepError> {
        for _ in 0..MODIFY_ATTEMPTS {
            let (bytes, hash) = self
                .read(path)?
                .ok_or_else(|| StepError::failed(format!("{path} not found")))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| StepError::failed(format!("{path} is not UTF-8")))?;
            let Some(new) = f(&text)? else {
                return Ok(None);
            };
            match self.try_write(path, new.into_bytes(), Some(hash))? {
                Ok(h) => return Ok(Some(h)),
                Err(VaultError::Conflict { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(StepError::conflict())
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
            action: self.action,
            step_index: self.step_index,
            params: self.params,
            context: self.context,
            vars,
            config: self.config,
            vault_root: self.vault_root,
            index_db: self.index_db,
            search: self.search,
            system_trash: self.system_trash,
            state: self.state,
            writer: self.writer.clone(),
            children: self.children,
            written: Vec::new(),
        }
    }
}

/// The contract every step module implements:
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
    TrackMail => mail::TrackMail,
    AutoFile => auto_file::AutoFile,
    IntakeReconcile => reconcile::IntakeReconcile,
    IntakeFinalize => intake_finalize::IntakeFinalize,
    IgnoreVideo => ignore::IgnoreVideo,
    DailyBrief => daily_brief::DailyBrief,
    DupesScan => dupes::DupesScan,
    DupesResolve => dupes_resolve::DupesResolve,
    CancelSourceIntake => cancel_source::CancelSourceIntake,
    Tidy => tidy::Tidy,
    IntakeEnrich => intake_enrich::IntakeEnrich,
    TranscriptRefine => transcript_refine::TranscriptRefine,
    YtClipTidy => yt_clip_tidy::YtClipTidy,
    RunAction => RunActionStep,
}

/// Test-only concurrent-edit injection (SR7 tests): [`race::inject`] registers `times` edits of
/// one note; each of the next `times` CAS writes of that note through [`StepCtx::write`] /
/// [`StepCtx::modify`] first lands the edit (a write by "someone else" after the step read).
#[cfg(test)]
pub(crate) mod race {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::{StepCtx, StepError};

    type Edit = Box<dyn FnMut(&str) -> String + Send>;

    static HOOKS: Mutex<Vec<(PathBuf, String, usize, Edit)>> = Mutex::new(Vec::new());

    /// Registers `times` concurrent edits of `path` in the vault at `root`.
    pub(crate) fn inject(
        root: &Path,
        path: &str,
        times: usize,
        edit: impl FnMut(&str) -> String + Send + 'static,
    ) {
        crate::sync::lock(&HOOKS).push((
            root.to_path_buf(),
            path.to_owned(),
            times,
            Box::new(edit),
        ));
    }

    pub(super) fn before_write(ctx: &StepCtx<'_>, path: &str) -> Result<(), StepError> {
        let Some((bytes, hash)) = ctx.read(path)? else {
            return Ok(());
        };
        let next = {
            let mut hooks = crate::sync::lock(&HOOKS);
            let Some(h) = hooks
                .iter_mut()
                .find(|h| h.0 == ctx.vault_root && h.1 == path && h.2 > 0)
            else {
                return Ok(());
            };
            h.2 -= 1;
            (h.3)(&String::from_utf8_lossy(&bytes))
        };
        let p = path.to_owned();
        ctx.writer()?
            .call(move |vw| vw.write(&p, next.as_bytes(), Some(hash)))??;
        Ok(())
    }
}

#[cfg(test)]
mod failure_tests {
    use std::time::Duration;

    use cv_providers::web::FetchError;
    use cv_providers::{Failure, FailureKind as F, ProviderError};
    use cv_state::{FailureReason, ServiceFailureKind as K};

    use super::{DeferCause, QUOTA_DEFAULT_MS, StepError, TRANSIENT_BASE_MS};

    fn transient(kind: F, detail: &str, retry: Option<u64>) -> FetchError {
        FetchError::Transient {
            service: "svc".into(),
            retry_after: retry.map(Duration::from_millis),
            kind,
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
        let d = |e: FetchError| StepError::from_fetch(&e);
        assert_eq!(d(transient(F::Connect, "connect", None)), t(K::Refused));
        assert_eq!(d(transient(F::Timeout, "timeout", None)), t(K::Timeout));
        assert_eq!(d(transient(F::Timeout, "network", None)), t(K::Timeout));
        assert_eq!(d(transient(F::Timeout, "HTTP 408", None)), t(K::Timeout));
        assert_eq!(d(transient(F::Server, "HTTP 503", None)), t(K::ServerError));
        assert_eq!(
            d(transient(F::RateLimited, "rate limited", Some(5_000))),
            deferred(5_000, DeferCause::Transient(K::RateLimited))
        );
        let quota = |r| FetchError::Quota {
            service: "svc".into(),
            retry_after: r,
        };
        assert_eq!(
            d(quota(None)),
            deferred(QUOTA_DEFAULT_MS, DeferCause::Quota)
        );
        assert_eq!(
            d(quota(Some(Duration::from_secs(7)))),
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
            assert!(matches!(d(e), StepError::Failed { reason: None, .. }));
        }
    }

    #[test]
    fn classify_status_carries_the_kind() {
        let k = |s| {
            cv_providers::web::classify_status("svc", s, None)
                .failure()
                .kind
        };
        assert_eq!(
            [k(408), k(429), k(500), k(503), k(404), k(400)],
            [
                F::Timeout,
                F::RateLimited,
                F::Server,
                F::Server,
                F::Permanent,
                F::Permanent
            ]
        );
    }

    #[test]
    fn maps_provider_failures() {
        let m = |e: ProviderError| StepError::from_failure("p", e.failure(), 60_000, e.to_string());
        let t = |k| StepError::Deferred {
            retry_after_ms: 60_000,
            service: Some("p".into()),
            cause: DeferCause::Transient(k),
        };
        let http = |status| ProviderError::Http { api: "x", status };
        assert_eq!(m(http(429)), t(K::RateLimited));
        assert_eq!(m(http(408)), t(K::Timeout));
        assert_eq!(m(http(502)), t(K::ServerError));
        assert_eq!(
            m(ProviderError::CliTimeout {
                command: "c".into(),
                ms: 5
            }),
            t(K::Timeout)
        );
        assert!(matches!(
            m(http(400)),
            StepError::Failed { reason: None, .. }
        ));
        assert!(matches!(
            m(ProviderError::Secret("missing".into())),
            StepError::Failed {
                reason: Some(FailureReason::NoApiKey),
                ..
            }
        ));
        assert_eq!(
            StepError::from_failure(
                "p",
                Failure {
                    kind: F::Server,
                    retry_after: Some(Duration::from_secs(2)),
                },
                60_000,
                String::new()
            ),
            StepError::Deferred {
                retry_after_ms: 2_000,
                service: Some("p".into()),
                cause: DeferCause::Transient(K::ServerError),
            }
        );
    }
}
