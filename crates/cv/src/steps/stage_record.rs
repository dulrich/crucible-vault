//! The stage outcome recorder (plan `intake-stage-jobs` WP-3): a step that runs as a stage body
//! writes, on every exit that leaves no evidence in the note, the note's `stage_outcomes` row, and
//! clears it on success. The rules (the [`cv_ingest::stage`] module doc's precedence is what the
//! rows feed):
//!
//! - a precondition no-op (the step's `skipped` outcome var, or `auto_file` deliberately leaving a
//!   clip in place) → `skipped` with its reason and the current fingerprint;
//! - a non-transient error (`StepError::Failed`, not a service outage) → `failed`, attempts + 1;
//! - an Ok that left work (the model, reading the note's facts after the step, still reads the
//!   stage open) → `unfinished`, attempts + 1;
//! - a success (the model reads the stage done, or fact-skipped, from the facts) → the row goes.
//!
//! Attempts continue from the existing row only when its fingerprint equals the new one; a changed
//! fingerprint restarts at 1. A deferral (any service outage, a CAS conflict) and a vanished note
//! record nothing: the stage stays pending and the breaker owns the retry.
//!
//! **When:** only when the step runs as a stage body: the running action's `stage` marker names
//! the stage the step kind belongs to ([`body_stage`]) and the step is note-scoped. A multi-step
//! stage (`yt_metadata` + `yt_channel_enrich`) records its Ok after its last body step only; a
//! failure records at whichever step fails. The same step under any other action records nothing.
//! The one call site is [`super::execute`] (one wrapper around step dispatch, not one per step);
//! the runner's settle site records a seam job's failure its steps did not see
//! ([`record_settle_failure`]).
//!
//! The fingerprint is [`fingerprint`] of the note's facts read after the step's work through
//! [`crate::note_facts::note_view`] (the one facts builder).

use std::path::Path;
use std::sync::{Arc, Mutex};

use cv_actions::{Action, ConfigSet, Step, StepKind};
use cv_ingest::stage::{OutcomeKind, Stage, State, fingerprint, stage_states};
use cv_protocol::{ActionContext, NoteLocation};
use cv_search::IndexedHashes;
use cv_state::{FailureReason, IntakeRead, StageOutcomeRow, StateDb};
use serde_json::Value;

use super::{StepCtx, StepError, StepOutcome};
use crate::note_facts::{FactEnv, note_view};

/// The reason `auto_file` records when it deliberately leaves a clip where it is.
pub(crate) const LEFT_IN_PLACE: &str = "left in place";

/// The stage a step kind is a body of (the plan's stage-to-job table).
#[must_use]
pub(crate) fn kind_stage(kind: StepKind) -> Option<Stage> {
    Some(match kind {
        StepKind::AutoFile => Stage::AutoFile,
        StepKind::Localize => Stage::Localize,
        StepKind::ImageDescribe => Stage::ImageDescribe,
        StepKind::YtMetadata | StepKind::YtChannelEnrich => Stage::YtMetadata,
        StepKind::YtClipTidy => Stage::YtClipTidy,
        StepKind::TranscriptRefine => Stage::TranscriptRefine,
        StepKind::IntakeEnrich => Stage::Enrich,
        StepKind::IntakeFinalize => Stage::Finalize,
        StepKind::SearchUpsert => Stage::Index,
        _ => return None,
    })
}

/// The stage `action` marks, when it is one.
#[must_use]
pub(crate) fn action_stage(action: &Action) -> Option<Stage> {
    action.stage.as_deref().and_then(Stage::parse)
}

/// A note-scope step (no `folder`, `scope` absent or `note`).
fn note_scoped(step: &Step) -> bool {
    !step.params.contains_key("folder")
        && step
            .params
            .get("scope")
            .and_then(toml::Value::as_str)
            .is_none_or(|s| s == "note")
}

/// The stage `step` runs as a body of within `action`: `action`'s stage marker names the stage
/// the step's kind belongs to and the step is note-scoped. `None`: not a stage body (the step
/// records nothing).
#[must_use]
pub(crate) fn body_stage(action: &Action, step: &Step) -> Option<Stage> {
    let stage = action_stage(action)?;
    (kind_stage(step.kind) == Some(stage) && note_scoped(step)).then_some(stage)
}

/// Whether the step context runs as the body of `stage` (the action marks `stage`).
#[must_use]
pub(crate) fn runs_as(ctx: &StepCtx<'_>, stage: Stage) -> bool {
    action_stage(ctx.action) == Some(stage)
}

/// What a stage body's run left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Happened {
    /// A non-transient error.
    Failed(String),
    /// An Ok; `skip` is the step's precondition no-op reason, when it made one.
    Ok {
        /// The precondition no-op reason.
        skip: Option<String>,
    },
}

/// Where the recorder reads and writes.
pub(crate) struct Env<'a> {
    pub state: &'a Arc<Mutex<StateDb>>,
    pub vault: &'a Path,
    pub index_db: &'a Path,
    pub search_db: Option<&'a Path>,
    pub config: &'a ConfigSet,
}

/// Classifies a stage body's result; `None` records nothing (deferral, vanished note, outage, or
/// an Ok of a non-last body step).
pub(crate) fn happened(
    action: &Action,
    step_index: usize,
    step: &Step,
    stage: Stage,
    r: &Result<StepOutcome, StepError>,
) -> Option<Happened> {
    match r {
        Err(StepError::Failed {
            reason: Some(FailureReason::ServiceOutage),
            ..
        })
        | Err(StepError::Deferred { .. } | StepError::NoteGone { .. }) => None,
        Err(StepError::Failed { message, .. }) => Some(Happened::Failed(format!(
            "{}: {message}",
            step.kind().as_str()
        ))),
        Ok(o) => {
            let later = action
                .steps
                .iter()
                .skip(step_index + 1)
                .any(|s| body_stage(action, s) == Some(stage));
            if later && !o.stop {
                return None;
            }
            if stage == Stage::AutoFile && o.vars.get("child_job_id").is_some_and(|v| !v.is_null())
            {
                // The filing was handed to a `file-as-kind` job: its own steps carry it.
                return None;
            }
            let skip = o
                .vars
                .get("skipped")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| (stage == Stage::AutoFile).then(|| LEFT_IN_PLACE.to_owned()));
            Some(Happened::Ok { skip })
        }
    }
}

/// The wrapper's hook: after a step ran, records its stage outcome when it is a stage body.
/// Never fails the step (a recorder error is logged).
pub(crate) fn after_step(step: &Step, ctx: &StepCtx<'_>, r: &Result<StepOutcome, StepError>) {
    let Some(stage) = body_stage(ctx.action, step) else {
        return;
    };
    let Some(what) = happened(ctx.action, ctx.step_index, step, stage, r) else {
        return;
    };
    let env = Env {
        state: ctx.state,
        vault: ctx.vault_root,
        index_db: ctx.index_db,
        search_db: ctx.search.map(cv_search::SearchDb::path),
        config: ctx.config,
    };
    if let Err(e) = record(&env, ctx.context, stage, &what) {
        tracing::warn!(error = %e, stage = stage.as_str(), "stage outcome not recorded");
    }
}

/// Settle-site recording (runner): a seam job of `stage` that failed terminally outside its
/// steps' own recording records `failed` against the stage.
pub(crate) fn record_settle_failure(
    env: &Env<'_>,
    context: &ActionContext,
    stage: Stage,
    error: &str,
) {
    if let Err(e) = record(env, context, stage, &Happened::Failed(error.to_owned())) {
        tracing::warn!(error = %e, stage = stage.as_str(), "stage outcome not recorded at settle");
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The note's current path and id: by the context's id when bound, else its active path.
fn locate(db: &StateDb, context: &ActionContext) -> anyhow::Result<Option<(String, i64)>> {
    let path = match context.active_id {
        Some(id) => match db.note_id_locate(id)? {
            NoteLocation::Live { path } => Some(path),
            NoteLocation::Gone { .. } => return Ok(None),
            NoteLocation::Unknown => context.active_path.clone(),
        },
        None => context.active_path.clone(),
    };
    let Some(path) = path.filter(|p| !p.trim().is_empty()) else {
        return Ok(None);
    };
    Ok(db.live_note_id(&path)?.map(|id| (path, id)))
}

/// Writes or clears the note's `stage` row for what happened (module doc).
pub(crate) fn record(
    env: &Env<'_>,
    context: &ActionContext,
    stage: Stage,
    what: &Happened,
) -> anyhow::Result<()> {
    let reader = cv_index::Reader::open_current(env.index_db)?;
    let indexed = match env.search_db {
        Some(p) if stage == Stage::Index => IndexedHashes::open_read_only(p).ok(),
        _ => None,
    };
    let mut db = crate::sync::lock(env.state);
    let Some((path, note_id)) = locate(&db, context)? else {
        return Ok(());
    };
    let view = {
        let fe = FactEnv {
            vault: env.vault,
            config: env.config,
            reader: &reader,
            state: Some(&*db),
            indexed: indexed.as_ref(),
        };
        note_view(&fe, &path)?
    };
    let Some(view) = view else {
        return Ok(());
    };
    // The stage as the note's own facts judge it (no rows): done or fact-skipped is success.
    let judged = stage_states(&view.facts, &[], u32::MAX)
        .into_iter()
        .find(|(s, _)| *s == stage)
        .map(|(_, st)| st);
    let Some(judged) = judged else {
        // Not a stage this note's kind requires: no row.
        db.clear_stage_outcomes(note_id, Some(stage.as_str()))?;
        return Ok(());
    };
    let settled = matches!(judged, State::Done | State::Skipped(_));
    let fp = fingerprint(stage, &view.facts);
    let prev = view.outcomes.iter().find(|o| o.stage == stage);
    let next_attempt = prev
        .filter(|o| o.fingerprint == fp)
        .map_or(1, |o| o.attempts.saturating_add(1));
    let (kind, reason, attempts) = match what {
        // A recorded failed filing outranks the "already moved" evidence (model precedence).
        Happened::Failed(_) if settled && stage != Stage::AutoFile => {
            db.clear_stage_outcomes(note_id, Some(stage.as_str()))?;
            return Ok(());
        }
        Happened::Failed(msg) => (OutcomeKind::Failed, msg.clone(), next_attempt),
        Happened::Ok { .. } if settled => {
            db.clear_stage_outcomes(note_id, Some(stage.as_str()))?;
            return Ok(());
        }
        Happened::Ok { skip: Some(r) } => (OutcomeKind::Skipped, r.clone(), 0),
        Happened::Ok { skip: None } => (OutcomeKind::Unfinished, String::new(), next_attempt),
    };
    let now = now_ms();
    db.upsert_stage_outcome(&StageOutcomeRow {
        note_id,
        stage: stage.as_str().to_owned(),
        outcome: kind.as_str().to_owned(),
        reason,
        attempts,
        fingerprint: fp,
        first_ms: now,
        updated_ms: now,
    })?;
    Ok(())
}
