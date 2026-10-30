//! Queue and intake exposure (plan `v0-automation-pipelines` WP-11): the store → protocol
//! mappings shared by the daemon's `jobs_list` / `intake_list` / `status` handlers and the
//! read-only `cv status` fallback, plus the `cv jobs …`, `cv intake …` and `cv run …` commands.
//!
//! The commands talk to the daemon only: unreachable is exit 1 with a clear message (nothing is
//! spooled), an RPC error is exit 1, a usage error exit 2.

//!
//! Parts: [`wire`] (store → protocol mappings), [`cli`] (argument parsing), `render` (text
//! output).

use anyhow::{Result, anyhow};
use cv_protocol::{
    ActionContext, JobIdParams, JobState, JobsStatsParams, RunActionParams, methods,
};

use crate::client::{Client, ClientError};
use crate::config::Config;

pub mod cli;
mod render;
pub mod wire;

pub use cli::{Command, Since, parse, parse_since};
use render::{render_counts, render_intake, render_jobs, render_stats, render_timeline};
use wire::enum_str;
pub use wire::{
    intake_item, intake_query, job_state, job_summary, job_timeline, jobs_stats_result, list_query,
    service_health, store_counts,
};

/// Default page size of `jobs_list` / `intake_list`.
pub const DEFAULT_LIMIT: u32 = 50;
/// Largest accepted page size (larger requests are clamped).
pub const MAX_LIMIT: u32 = 500;

/// Usage lines (also appended to the top-level `cv --help`).
pub const USAGE: &str = "  cv jobs list [--status S] [--type T] [--limit N] [--offset N]\n                   S = queued|running|done|failed|cancelled|deferred\n  cv jobs retry <job-id> | cv jobs cancel <job-id>\n  cv jobs stats [--type T] [--since <N>m|<N>h|<N>d|YYYY-MM-DD]   per-type wait/run bucket p50/p95/max (a date is UTC midnight; default: all retained rows)\n  cv jobs show <job-id>   one job's timeline: waits, attempts, steps, bucket split\n  cv intake list [--source-kind K] [--source KEY] [--day YYYY-MM-DD] [--outcome line|note|ignored|skipped] [--limit N] [--offset N]\n  cv intake reconcile [--dry-run]   link NULL-path rows, probe + auto-tick the complete ones, finalize youtube rows, re-match + re-clean mail; prints counts as key=value\n  cv run <action-id> [--note <vault-path>]   run an action through the daemon\n";

fn page_limit(l: Option<u32>) -> u32 {
    l.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// `cv jobs|intake|run …` from the real environment; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let Some(cmd) = parse(args) else {
        eprint!("usage:\n{USAGE}");
        return 2;
    };
    match Config::from_env() {
        Ok(c) => execute(&c, &cmd),
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

/// Runs a parsed command against the daemon on `config.socket`; returns the exit code.
pub fn execute(config: &Config, cmd: &Command) -> i32 {
    match call(config, cmd) {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

fn call(config: &Config, cmd: &Command) -> Result<String> {
    let client = match Client::connect(&config.socket, "cv-jobs", cv_index::SCHEMA_VERSION) {
        Ok(c) => c,
        Err(ClientError::Io(e)) => {
            return Err(anyhow!(
                "daemon not running ({}: {e}); nothing was queued",
                config.socket.display()
            ));
        }
        Err(e) => return Err(anyhow!("{e}")),
    };
    let e = |e: ClientError| anyhow!("{e}");
    let now = crate::now_ms();
    Ok(match cmd {
        Command::JobsList(p) => render_jobs(
            &client.call::<methods::JobsList>(p.clone()).map_err(e)?,
            now,
        ),
        Command::JobsStats { job_type, since } => render_stats(
            &client
                .call::<methods::JobsStats>(JobsStatsParams {
                    job_type: job_type.clone(),
                    since_ms: since.map(|s| s.resolve(now)),
                })
                .map_err(e)?,
        ),
        Command::JobShow(id) => render_timeline(
            &client
                .call::<methods::JobTimeline>(JobIdParams { job_id: id.clone() })
                .map_err(e)?,
        ),
        Command::JobRetry(id) | Command::JobCancel(id) => {
            let p = JobIdParams { job_id: id.clone() };
            let s = if matches!(cmd, Command::JobRetry(_)) {
                client.call::<methods::JobRetry>(p)
            } else {
                client.call::<methods::Cancel>(p)
            }
            .map_err(e)?;
            let mut out = format!("{} {}", s.job_id, enum_str(s.status));
            if let Some(m) = s.message {
                out.push_str(&format!("  {m}"));
            }
            out.push('\n');
            out
        }
        Command::IntakeList(p) => {
            render_intake(&client.call::<methods::IntakeList>(p.clone()).map_err(e)?)
        }
        Command::IntakeReconcile { dry_run } => reconcile(&client, *dry_run)?,
        Command::Run { action_id, note } => {
            let r = client
                .call::<methods::RunAction>(RunActionParams {
                    action_id: action_id.clone(),
                    params: serde_json::Map::new(),
                    context: ActionContext {
                        active_path: note.clone(),
                        ..ActionContext::default()
                    },
                })
                .map_err(e)?;
            let mut out = format!("job {}\n", r.job_id);
            if let Some(p) = r.open_path {
                out.push_str(&format!("open {p}\n"));
            }
            out
        }
    })
}

/// How long `cv intake reconcile` waits for the daemon (its own bounded wait is 10 min).
const RECONCILE_CALL_WAIT: std::time::Duration = std::time::Duration::from_secs(660);

/// Runs the reconcile job through the daemon and renders its counts as `key=value` lines; a job
/// that did not complete is an error (exit 1).
fn reconcile(client: &Client, dry_run: bool) -> Result<String> {
    let r = client
        .call_within::<methods::ReconcileIntake>(
            cv_protocol::ReconcileIntakeParams { dry_run },
            RECONCILE_CALL_WAIT,
        )
        .map_err(|e| anyhow!("{e}"))?;
    if r.status != JobState::Done {
        return Err(anyhow!(
            "reconcile job {} {}{}",
            r.job_id,
            enum_str(r.status),
            r.message.map(|m| format!(": {m}")).unwrap_or_default()
        ));
    }
    Ok(render_counts(&r.counts))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod telemetry_tests;
