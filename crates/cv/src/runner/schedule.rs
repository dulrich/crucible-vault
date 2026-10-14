//! Schedule starter (pin 7): daily or interval, last run persisted in `state.db`
//! (`schedule_runs`); a missed run catches up **once**, however many periods were missed.
//!
//! The first pass after start is the only start-time trigger (I4). An action seen for the first
//! time records "now" without firing, so installing a schedule never fires a back-dated run.

use std::time::Duration;

use cv_actions::Starter;
use cv_protocol::ActionContext;
use cv_state::Lane;
use serde_json::Map;
use tokio::sync::watch;

use super::{RunRequest, Runner, Trigger, lock};

/// Longest scheduler sleep (config reloads take effect within this).
const MAX_SLEEP: Duration = Duration::from_secs(30);

pub(super) async fn schedule_loop(runner: Runner, mut stop: watch::Receiver<bool>) {
    loop {
        let r = runner.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || tick(&r)).await {
            tracing::error!(error = %e, "scheduler tick panicked");
        }
        let sleep = next_sleep(&runner);
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(sleep) => {}
        }
    }
}

fn next_sleep(runner: &Runner) -> Duration {
    let clock = runner.clock();
    let now = clock.now_ms();
    let off = clock.local_offset_ms(now);
    runner
        .config()
        .armed_actions()
        .flat_map(|a| a.starters.iter())
        .filter_map(|s| match s {
            Starter::Schedule(s) => Some(s.next_due(now, off)),
            _ => None,
        })
        .min()
        .map_or(MAX_SLEEP, |t| {
            Duration::from_millis(u64::try_from(t - now).unwrap_or(0)).min(MAX_SLEEP)
        })
}

/// One pass: every armed scheduled action whose most recent due time is after its last run
/// enqueues one run; `last_run_ms` is set at enqueue.
pub(super) fn tick(runner: &Runner) -> Vec<String> {
    let config = runner.config();
    let clock = runner.clock();
    let now = clock.now_ms();
    let off = clock.local_offset_ms(now);
    let state = &runner.inner().state;
    let mut out = Vec::new();
    for action in config.armed_actions() {
        let Some(due) = action
            .starters
            .iter()
            .filter_map(|s| match s {
                Starter::Schedule(s) => Some(s.most_recent_due(now, off)),
                _ => None,
            })
            .max()
        else {
            continue;
        };
        let last = match lock(state).schedule_last_run(&action.id) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(action = %action.id, error = %e, "reading schedule_runs");
                continue;
            }
        };
        let Some(last) = last else {
            if let Err(e) = lock(state).set_schedule_run(&action.id, now) {
                tracing::error!(action = %action.id, error = %e, "seeding schedule_runs");
            }
            continue;
        };
        if due <= last {
            continue;
        }
        let mut params = Map::new();
        super::apply_defaults(action, &mut params);
        let req = RunRequest {
            params,
            context: ActionContext::default(),
            trigger: Trigger::Schedule { due_ms: due },
            chain: Vec::new(),
            default_lane: Lane::Background,
            dedupe_key: Some(format!("{}::schedule", action.id)),
        };
        match runner.enqueue(action, req) {
            Ok(id) => {
                out.push(id);
                if let Err(e) = lock(state).set_schedule_run(&action.id, now) {
                    tracing::error!(action = %action.id, error = %e, "recording schedule run");
                }
            }
            Err(e) => {
                tracing::warn!(action = %action.id, error = %e, "scheduled run refused");
                // Record anyway: a run that cannot derive keys would otherwise retry every pass.
                let _ = lock(state).set_schedule_run(&action.id, now);
            }
        }
    }
    out
}
