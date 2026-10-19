//! `actions_list` and `run_action` (the command starter, pin 8).

use cv_protocol::{
    ActionInfo, JobState, Notification, RpcError, RunActionParams, RunActionResult, error_codes,
};
use cv_state::Lane;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::{EnqueueError, NON_DURABLE_WAIT, RunRequest, Runner, Trigger};
use crate::daemon::{Shared, rpc_err};

/// Armed actions that a client can start (have a command starter), as `ActionInfo`s.
pub fn actions_list(runner: &Runner) -> Vec<ActionInfo> {
    runner
        .config()
        .armed_actions()
        .filter(|a| a.has_command_starter())
        .map(|a| ActionInfo {
            id: a.id.clone(),
            title: a.label.clone(),
            group: a.group.clone(),
            keys: a.keys.clone(),
            params: a.params.clone(),
            needs: a.needs.clone(),
            durable: a.durable,
        })
        .collect()
}

fn invalid(m: impl Into<String>) -> RpcError {
    rpc_err(error_codes::INVALID_PARAMS, m)
}

/// Validates and enqueues a user run (`Lane::User` unless the action pins a lane).
pub async fn run_action(
    shared: &Shared,
    runner: &Runner,
    p: RunActionParams,
) -> Result<RunActionResult, RpcError> {
    let config = runner.config();
    if !config.is_known(&p.action_id) {
        return Err(rpc_err(
            error_codes::NOT_FOUND,
            format!("no action {:?}", p.action_id),
        ));
    }
    let Some(action) = config.armed(&p.action_id) else {
        return Err(invalid(format!(
            "action {:?} is disarmed: {}",
            p.action_id,
            config.disarm_reason(&p.action_id).unwrap_or("invalid")
        )));
    };
    if !action.has_command_starter() {
        return Err(invalid(format!(
            "action {:?} has no command starter",
            action.id
        )));
    }
    let ctx = &p.context;
    for need in &action.needs {
        let present = match need.as_str() {
            "active_path" => ctx.active_path.is_some(),
            "selection" => ctx.selection.is_some(),
            "value" => ctx.value.is_some(),
            "line" => ctx.line.is_some(),
            _ => false,
        };
        if !present {
            return Err(invalid(format!(
                "action {:?} needs context `{need}`",
                action.id
            )));
        }
    }
    let mut params = p.params.clone();
    super::apply_defaults(action, &mut params);
    for spec in &action.params {
        let missing = match params.get(&spec.name) {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(_) => false,
        };
        if spec.required && missing {
            return Err(invalid(format!("missing required param {:?}", spec.name)));
        }
    }
    if let Some(expected) = ctx.buffer_hash.clone() {
        let Some(path) = ctx.active_path.clone() else {
            return Err(invalid("buffer_hash given without active_path"));
        };
        let current = shared
            .writer_job(move |vw| Ok(vw.fs().hash(&path)?))
            .await?;
        let cur = current.as_ref().map(|h| h.as_str().to_owned());
        if cur.as_deref() != Some(expected.as_str()) {
            return Err(RpcError {
                code: error_codes::CONFLICT,
                message: "stale buffer_hash".into(),
                data: Some(json!({ "current_hash": cur })),
            });
        }
    }
    let mut updates: Option<broadcast::Receiver<Notification>> =
        (!action.durable).then(|| shared.subscribe());
    let req = RunRequest {
        params,
        context: p.context.clone(),
        trigger: Trigger::Command,
        chain: Vec::new(),
        default_lane: Lane::User,
        dedupe_key: None,
    };
    let job_id = runner.enqueue(action, req).map_err(|e| match e {
        EnqueueError::NoResourceKeys(m) => invalid(m),
        EnqueueError::State(m) => rpc_err(error_codes::INTERNAL_ERROR, m),
    })?;
    let mut out = RunActionResult {
        job_id: job_id.clone(),
        open_path: None,
    };
    if let Some(rx) = updates.as_mut() {
        let done = tokio::time::timeout(super::NON_DURABLE_WAIT, wait_terminal(rx, &job_id)).await;
        if done.is_ok() {
            out.open_path = runner.finished(&job_id).and_then(|s| s.open_path);
        } else {
            tracing::info!(%job_id, wait = ?NON_DURABLE_WAIT, "non-durable run still going; returning job id");
        }
    }
    Ok(out)
}

async fn wait_terminal(rx: &mut broadcast::Receiver<Notification>, job_id: &str) {
    loop {
        match rx.recv().await {
            Ok(Notification::JobUpdated(s))
                if s.job_id == job_id
                    && matches!(
                        s.status,
                        JobState::Done | JobState::Failed | JobState::Cancelled
                    ) =>
            {
                return;
            }
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}
