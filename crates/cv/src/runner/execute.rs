//! The step executor (moved from `runner/mod.rs`, plan tn-review-2026-11-04-remediation WP-R4, finding P3): sequential steps with per-step checkpoints, and the `run_action` child spawner.

use super::*;

struct Spawner<'a> {
    inner: &'a Inner,
    parent_job: &'a str,
    parent: &'a JobParams,
    /// The run's current context: `active_path` already follows the note by id (`refresh`), so a
    /// child spawned after a move runs on the moved path (plan round-16 WP-5).
    context: &'a ActionContext,
    lane: Lane,
    config: &'a ConfigSet,
}

impl ChildSpawner for Spawner<'_> {
    /// `run_action` step (pin 10): enqueue the target as a child job in the same lane.
    fn spawn_child(&self, action_id: &str, params: Map<String, Value>) -> Result<String, String> {
        self.spawn_with(action_id, params, self.context.clone())
    }

    /// As [`Self::spawn_child`], with the child's `active_path` set to `active_path`.
    fn spawn_child_at(
        &self,
        action_id: &str,
        params: Map<String, Value>,
        active_path: &str,
    ) -> Result<String, String> {
        let context = ActionContext {
            active_path: Some(active_path.to_owned()),
            // A different note: bound at the child's enqueue, never at the parent's id.
            active_id: None,
            ..self.context.clone()
        };
        self.spawn_with(action_id, params, context)
    }
}

impl Spawner<'_> {
    fn spawn_with(
        &self,
        action_id: &str,
        params: Map<String, Value>,
        context: ActionContext,
    ) -> Result<String, String> {
        let mut chain = self.parent.chain.clone();
        chain.push(self.parent.action_id.clone());
        if chain.iter().any(|a| a == action_id) {
            return Err(format!(
                "run_action cycle: {} -> {action_id}",
                chain.join(" -> ")
            ));
        }
        if chain.len() >= MAX_CHAIN_DEPTH {
            return Err(format!("run_action depth limit {MAX_CHAIN_DEPTH} reached"));
        }
        let target = self.config.armed(action_id).ok_or_else(|| {
            format!(
                "run_action target {action_id:?} is not armed: {}",
                self.config.disarm_reason(action_id).unwrap_or("unknown")
            )
        })?;
        let mut merged = params;
        apply_defaults(target, &mut merged);
        let target = Action {
            lane: match self.lane {
                Lane::User => LaneChoice::User,
                Lane::Background => LaneChoice::Background,
            },
            ..target.clone()
        };
        self.inner
            .enqueue(
                &target,
                RunRequest::child(merged, context, self.parent_job, chain, self.lane),
            )
            .map_err(|e| e.to_string())
    }
}

/// Fills absent params from their `ParamSpec` defaults.
pub fn apply_defaults(action: &Action, params: &mut Map<String, Value>) {
    for p in &action.params {
        if !params.contains_key(&p.name)
            && let Some(d) = &p.default
        {
            params.insert(p.name.clone(), Value::String(d.clone()));
        }
    }
}

pub(super) fn execute_steps(
    inner: &Inner,
    job_id: &str,
    lane: Lane,
    retry: bool,
    action: &Action,
    jp: &JobParams,
    config: &ConfigSet,
) -> (JobResult, RunSummary) {
    let mut summary = RunSummary::default();
    let mut vars = Map::new();
    let checkpoints = match lock(&inner.state).checkpoints(job_id) {
        Ok(c) => c,
        Err(e) => {
            return (failed(format!("reading checkpoints: {e}")), summary);
        }
    };
    let mut done: BTreeMap<usize, StepOutcome> = BTreeMap::new();
    for cp in checkpoints {
        // Fail closed: a checkpoint that does not parse means "applied, outcome unknown" — the
        // step is never re-run, so the job fails rather than guessing.
        let parsed = usize::try_from(cp.step_index)
            .ok()
            .zip(serde_json::from_str::<StepOutcome>(&cp.outcome).ok());
        let Some((i, o)) = parsed else {
            return (
                failed(format!("corrupt checkpoint for step {}", cp.step_index)),
                summary,
            );
        };
        done.insert(i, o);
    }
    let mut seen_outputs = BTreeSet::new();
    // The run's own context: `active_path` follows the note by id (`refresh`, review D3).
    let mut context = jp.context.clone();
    let mut absorb = |o: &StepOutcome, summary: &mut RunSummary, vars: &mut Map<String, Value>| {
        if o.open_path.is_some() {
            summary.open_path.clone_from(&o.open_path);
        }
        for p in &o.output_paths {
            if seen_outputs.insert(p.clone()) {
                summary.output_paths.push(p.clone());
            }
        }
        for (k, v) in &o.vars {
            vars.insert(k.clone(), v.clone());
        }
    };
    for (i, step) in action.steps.iter().enumerate() {
        if let Some(o) = done.get(&i) {
            absorb(o, &mut summary, &mut vars);
            if o.stop {
                break;
            }
            continue;
        }
        if lock(&inner.state).cancel_requested(job_id).unwrap_or(false) {
            return (JobResult::Cancelled, summary);
        }
        let idx = i64::try_from(i).unwrap_or(i64::MAX);
        let started = lock(&inner.state).step_started(
            job_id,
            idx,
            step.kind().as_str(),
            inner.clock.now_ms(),
        );
        if let Err(e) = started {
            tracing::warn!(job_id, error = %e, "telemetry: step start not recorded");
        }
        // D5: the cycle guard is armed before each write is issued (the step write seam).
        let guard = |p: &str| inner.record_writes(&action.id, &[p.to_owned()]);
        let mut follows = 0;
        let res = loop {
            // Plan note-identity D5: the job's note located by id before the step acts on it.
            if let Err(e) = refresh(inner, job_id, &mut context) {
                break Err(e);
            }
            let spawner = Spawner {
                inner,
                parent_job: job_id,
                parent: jp,
                context: &context,
                lane,
                config,
            };
            let mut ctx = StepCtx {
                action,
                step_index: i,
                params: &jp.params,
                context: &context,
                vars: &vars,
                config,
                vault_root: &inner.vault_root,
                index_db: &inner.index_db,
                search: Some(&inner.search),
                system_trash: inner.system_trash.as_deref(),
                state: &inner.state,
                writer: inner.writer.writer(),
                children: &spawner,
                written: Vec::new(),
                guard: Some(&guard),
            };
            let res = inner.executor.execute(step, &mut ctx);
            let written = std::mem::take(&mut ctx.written);
            inner.record_writes(&action.id, &written);
            // A step whose note went missing re-runs at the note's new path (D4).
            if matches!(res, Err(StepError::NoteGone { .. })) && follows < MAX_FOLLOWS {
                let before = context.active_path.clone();
                if refresh(inner, job_id, &mut context).is_err() || context.active_path == before {
                    break res;
                }
                follows += 1;
                tracing::info!(
                    job = job_id,
                    step = i + 1,
                    "note moved; step re-runs at its new path"
                );
                continue;
            }
            break res;
        };
        if let Err(e) = lock(&inner.state).step_ended(job_id, idx, inner.clock.now_ms()) {
            tracing::warn!(job_id, error = %e, "telemetry: step end not recorded");
        }
        match res {
            Ok(o) => {
                let text = match serde_json::to_string(&o) {
                    Ok(t) => t,
                    Err(e) => {
                        return (
                            failed(format!("step {} outcome not serializable: {e}", i + 1)),
                            summary,
                        );
                    }
                };
                let put =
                    lock(&inner.state).put_checkpoint(job_id, idx, &text, inner.clock.now_ms());
                if let Err(e) = put {
                    return (
                        failed(format!("committing checkpoint of step {}: {e}", i + 1)),
                        summary,
                    );
                }
                absorb(&o, &mut summary, &mut vars);
                if o.stop {
                    break;
                }
            }
            Err(StepError::Failed { message, reason }) => {
                return (
                    JobResult::Failed {
                        error: format!("step {} ({}): {message}", i + 1, step.kind().as_str()),
                        reason,
                    },
                    summary,
                );
            }
            Err(StepError::NoteGone { path }) => {
                // A retried job whose note was deleted since the first attempt settles quietly
                // (plan round-11 Decision 3; a moved note was followed above, plan
                // note-path-continuity D4); a first attempt still fails.
                if retry {
                    tracing::info!(job = job_id, step = i + 1, "note deleted; job skipped");
                    return (JobResult::Done, summary);
                }
                return (
                    JobResult::Failed {
                        error: format!(
                            "step {} ({}): {path} not found",
                            i + 1,
                            step.kind().as_str()
                        ),
                        reason: None,
                    },
                    summary,
                );
            }
            Err(StepError::Deferred {
                retry_after_ms,
                service,
                cause,
            }) => {
                summary.defer_cause = Some(cause);
                return (
                    JobResult::Deferred {
                        retry_after_ms,
                        service_unhealthy: service,
                    },
                    summary,
                );
            }
        }
    }
    (JobResult::Done, summary)
}

/// How often one step re-runs after its note moved under it (D4).
const MAX_FOLLOWS: usize = 3;

/// The one way a job follows its note (plan note-identity D5; review 2026-11-10 D3): run before
/// each step and on `NoteGone`. A job with an `active_id` is located on the writer thread; one
/// without (its path named no file at enqueue) is bound first. `Live` at another path moves
/// `active_path` and retargets the hold; `Gone` is `NoteGone` (the caller settles it as today: a
/// retry quietly, a first attempt fails); `Unknown`, no path or no writer leave the context as is.
/// An id-store failure fails the step attempt: the job never acts on a possibly stale path.
fn refresh(inner: &Inner, job_id: &str, context: &mut ActionContext) -> Result<(), StepError> {
    let Some(path) = context.active_path.clone() else {
        return Ok(());
    };
    let store = |e: String| StepError::failed(format!("locating the job's note: {e}"));
    let Some(id) = context.active_id else {
        context.active_id = inner.bind_existing(&path).map_err(store)?;
        return Ok(());
    };
    let Some(w) = inner.writer.writer() else {
        return Ok(());
    };
    match w.locate_note(id).map_err(|e| store(e.to_string()))? {
        cv_vault::NoteLocation::Live { path: now } if now != path => {
            tracing::info!("job note moved; following it");
            context.active_path = Some(now);
            inner.retarget_hold(job_id, context);
            Ok(())
        }
        cv_vault::NoteLocation::Gone { .. } => Err(StepError::NoteGone { path }),
        _ => Ok(()),
    }
}
