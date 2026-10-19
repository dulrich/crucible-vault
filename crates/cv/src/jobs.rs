//! Queue and intake exposure (plan `v0-automation-pipelines` WP-11): the store → protocol
//! mappings shared by the daemon's `jobs_list` / `intake_list` / `status` handlers and the
//! read-only `cv status` fallback, plus the `cv jobs …`, `cv intake …` and `cv run …` commands.
//!
//! The commands talk to the daemon only: unreachable is exit 1 with a clear message (nothing is
//! spooled), an RPC error is exit 1, a usage error exit 2.

use anyhow::{Result, anyhow};
use cv_protocol::{
    ActionContext, IntakeItem, IntakeListParams, IntakeListResult, JobIdParams, JobSummary,
    JobsListParams, JobsListResult, RunActionParams, methods,
};
use cv_state::{IntakeQuery, Job, ListQuery, TrackerItem, TrackerOutcome};

use crate::client::{Client, ClientError};
use crate::config::Config;

/// Default page size of `jobs_list` / `intake_list`.
pub const DEFAULT_LIMIT: u32 = 50;
/// Largest accepted page size (larger requests are clamped).
pub const MAX_LIMIT: u32 = 500;

/// Usage lines (also appended to the top-level `cv --help`).
pub const USAGE: &str = "  cv jobs list [--status S] [--type T] [--limit N] [--offset N]\n                   S = queued|running|done|failed|cancelled|deferred\n  cv jobs retry <job-id> | cv jobs cancel <job-id>\n  cv intake list [--source-kind K] [--source KEY] [--day YYYY-MM-DD] [--outcome line|note|ignored|skipped] [--limit N] [--offset N]\n  cv run <action-id> [--note <vault-path>]   run an action through the daemon\n";

fn page_limit(l: Option<u32>) -> u32 {
    l.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// `jobs_list` params → store query. `queued` excludes rows deferred past `now_ms`;
/// `deferred` is exactly those rows. Unknown status → `Err(message)`.
pub fn list_query(p: &JobsListParams, now_ms: i64) -> Result<ListQuery, String> {
    use cv_state::JobStatus as S;
    let (status, deferred) = match p.status.as_deref() {
        None => (None, None),
        Some("queued") => (Some(S::Queued), Some(false)),
        Some("deferred") => (Some(S::Queued), Some(true)),
        Some("running") => (Some(S::Running), None),
        Some("done") => (Some(S::Done), None),
        Some("failed") => (Some(S::Failed), None),
        Some("cancelled") => (Some(S::Cancelled), None),
        Some(other) => {
            return Err(format!(
                "unknown status {other:?} (queued|running|done|failed|cancelled|deferred)"
            ));
        }
    };
    Ok(ListQuery {
        status,
        job_type: p.job_type.clone(),
        limit: page_limit(p.limit),
        offset: p.offset.unwrap_or(0),
        recent_first: p.recent_first.unwrap_or(true),
        deferred,
        now_ms,
    })
}

fn enum_str<T: serde::Serialize>(v: T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// One store job as a `jobs_list` row.
#[must_use]
pub fn job_summary(j: &Job, now_ms: i64) -> JobSummary {
    let status = if j.status == cv_state::JobStatus::Queued
        && j.defer_until_ms.is_some_and(|d| d > now_ms)
    {
        "deferred".to_owned()
    } else {
        j.status.as_str().to_owned()
    };
    JobSummary {
        job_id: j.id.clone(),
        job_type: j.job_type.clone(),
        lane: enum_str(j.lane),
        status,
        service: j.service.clone(),
        attempts: j.attempts,
        created_ms: j.created_ms,
        settled_at_ms: j.settled_at_ms,
        defer_until_ms: j.defer_until_ms,
        error: j.error.clone(),
        failure_kind: j.failure_kind.map(|k| k.as_str().to_owned()),
    }
}

/// `intake_list` params → store query. Bad `day` / `outcome` → `Err(message)`.
pub fn intake_query(p: &IntakeListParams) -> Result<IntakeQuery, String> {
    if let Some(d) = &p.day {
        let b = d.as_bytes();
        let ok = b.len() == 10
            && b[4] == b'-'
            && b[7] == b'-'
            && b.iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
        if !ok {
            return Err(format!("day must be YYYY-MM-DD, got {d:?}"));
        }
    }
    let outcome = match p.outcome.as_deref() {
        None => None,
        Some(o) => Some(
            TrackerOutcome::parse(o)
                .ok_or_else(|| format!("unknown outcome {o:?} (line|note|ignored|skipped)"))?,
        ),
    };
    Ok(IntakeQuery {
        source_kind: p.source_kind.clone(),
        source_key: p.source_key.clone(),
        day: p.day.clone(),
        outcome,
        limit: page_limit(p.limit),
        offset: p.offset.unwrap_or(0),
    })
}

/// One tracker row as an `intake_list` item.
#[must_use]
pub fn intake_item(t: TrackerItem) -> IntakeItem {
    IntakeItem {
        source_kind: t.source_kind,
        source_key: t.source_key,
        item_id: t.item_id,
        title: t.title,
        url: t.url,
        first_seen_ms: t.first_seen_ms,
        day: t.day,
        outcome: t.outcome.as_str().to_owned(),
        score: t.score,
        full_body: t.full_body,
    }
}

/// A breaker entry for `status` (state + detail).
#[must_use]
pub fn service_health(h: cv_state::ServiceHealth) -> cv_protocol::ServiceHealth {
    let last_reason = match (h.last_kind.map(enum_str), h.last_reason) {
        (Some(k), Some(r)) => Some(format!("{k} {r}")),
        (Some(k), None) => Some(k),
        (None, r) => r,
    };
    #[allow(clippy::cast_precision_loss)]
    let failure_score = Some(h.failure_score as f64);
    cv_protocol::ServiceHealth {
        service: h.service,
        state: enum_str(h.state),
        failure_score,
        retry_at_ms: h.retry_at_ms,
        last_reason,
    }
}

/// `tracker_items` / `link_records` counts from a read-only handle (`None` when unreadable).
#[must_use]
pub fn store_counts(r: &cv_state::StateReader) -> (Option<u64>, Option<u64>) {
    let c = |v: cv_state::Result<i64>| v.ok().and_then(|n| u64::try_from(n).ok());
    (c(r.tracker_item_count()), c(r.link_record_count()))
}

// ---------------------------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------------------------

/// A parsed `cv jobs|intake|run` command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    JobsList(JobsListParams),
    JobRetry(String),
    JobCancel(String),
    IntakeList(IntakeListParams),
    Run {
        action_id: String,
        note: Option<String>,
    },
}

fn num(v: &str) -> Option<u32> {
    v.parse().ok()
}

/// Parses `args` (from the top-level command word on: `["jobs", "list", …]`); `None` is a
/// usage error.
#[must_use]
pub fn parse(args: &[String]) -> Option<Command> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["jobs", "list", rest @ ..] => {
            let mut p = JobsListParams::default();
            for pair in pairs(rest)? {
                match pair {
                    ("--status", v) => p.status = Some(v.to_owned()),
                    ("--type", v) => p.job_type = Some(v.to_owned()),
                    ("--limit", v) => p.limit = Some(num(v)?),
                    ("--offset", v) => p.offset = Some(num(v)?),
                    _ => return None,
                }
            }
            Some(Command::JobsList(p))
        }
        ["jobs", "retry", id] if !id.starts_with('-') => Some(Command::JobRetry((*id).to_owned())),
        ["jobs", "cancel", id] if !id.starts_with('-') => {
            Some(Command::JobCancel((*id).to_owned()))
        }
        ["intake", "list", rest @ ..] => {
            let mut p = IntakeListParams::default();
            for pair in pairs(rest)? {
                match pair {
                    ("--source-kind", v) => p.source_kind = Some(v.to_owned()),
                    ("--source", v) => p.source_key = Some(v.to_owned()),
                    ("--day", v) => p.day = Some(v.to_owned()),
                    ("--outcome", v) => p.outcome = Some(v.to_owned()),
                    ("--limit", v) => p.limit = Some(num(v)?),
                    ("--offset", v) => p.offset = Some(num(v)?),
                    _ => return None,
                }
            }
            Some(Command::IntakeList(p))
        }
        ["run", action, rest @ ..] if !action.starts_with('-') => {
            let note = match rest {
                [] => None,
                ["--note", n] => Some((*n).to_owned()),
                _ => return None,
            };
            Some(Command::Run {
                action_id: (*action).to_owned(),
                note,
            })
        }
        _ => None,
    }
}

/// Splits `--flag value` pairs; an odd count or a non-flag key is `None`.
fn pairs<'a>(rest: &[&'a str]) -> Option<Vec<(&'a str, &'a str)>> {
    if !rest.len().is_multiple_of(2) {
        return None;
    }
    rest.chunks(2)
        .map(|c| c[0].starts_with("--").then_some((c[0], c[1])))
        .collect()
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

fn truncate(s: &str, n: usize) -> String {
    let one_line = s.replace('\n', " ");
    if one_line.chars().count() <= n {
        one_line
    } else {
        let mut t: String = one_line.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

fn ago(ms: i64, now: i64) -> String {
    let s = (now - ms).max(0) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn render_jobs(r: &JobsListResult, now: i64) -> String {
    if r.jobs.is_empty() {
        return "no jobs\n".to_owned();
    }
    let mut out = String::new();
    for j in &r.jobs {
        let when = match (j.settled_at_ms, j.defer_until_ms) {
            (Some(s), _) => format!("settled {} ago", ago(s, now)),
            (None, Some(d)) if d > now => format!("until +{}", ago(now, d)),
            _ => format!("age {}", ago(j.created_ms, now)),
        };
        let detail = match (&j.error, &j.service) {
            (Some(e), _) => truncate(e, 80),
            (None, Some(s)) => format!("service {s}"),
            (None, None) => String::new(),
        };
        out.push_str(
            format!(
                "{}  {:<9}  {}  {}  {}  {}",
                j.job_id, j.status, j.job_type, j.attempts, when, detail
            )
            .trim_end(),
        );
        out.push('\n');
    }
    out
}

fn render_intake(r: &IntakeListResult) -> String {
    if r.items.is_empty() {
        return "no intake items\n".to_owned();
    }
    let mut out = String::new();
    for i in &r.items {
        let score = i
            .score
            .map(|s| format!("  score {s:.2}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{}  {:<7}  {}  {}  {}  {}{}\n",
            i.day,
            i.outcome,
            i.source_kind,
            i.item_id,
            truncate(&i.title, 60),
            i.url,
            score
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parse_accepts_and_rejects() {
        assert_eq!(
            parse(&a(&["jobs", "list", "--status", "failed", "--limit", "5"])),
            Some(Command::JobsList(JobsListParams {
                status: Some("failed".into()),
                limit: Some(5),
                ..JobsListParams::default()
            }))
        );
        assert_eq!(
            parse(&a(&["run", "link-scan", "--note", "a/b.md"])),
            Some(Command::Run {
                action_id: "link-scan".into(),
                note: Some("a/b.md".into())
            })
        );
        assert!(matches!(
            parse(&a(&[
                "intake",
                "list",
                "--day",
                "2026-10-15",
                "--outcome",
                "line"
            ])),
            Some(Command::IntakeList(_))
        ));
        assert_eq!(
            parse(&a(&["jobs", "retry", "j1"])),
            Some(Command::JobRetry("j1".into()))
        );
        for bad in [
            &["jobs"][..],
            &["jobs", "list", "--limit"],
            &["jobs", "list", "--limit", "x"],
            &["jobs", "list", "--bogus", "1"],
            &["jobs", "retry"],
            &["jobs", "cancel", "a", "b"],
            &["run"],
            &["run", "x", "--note"],
            &["intake", "list", "stray", "x"],
        ] {
            assert_eq!(parse(&a(bad)), None, "{bad:?}");
            assert_eq!(run(&a(bad)), 2, "{bad:?}");
        }
    }

    #[test]
    fn unreachable_daemon_exits_1() {
        let dir = std::env::temp_dir().join(format!("cv-wp11-jobs-{}", std::process::id()));
        let home = dir.to_string_lossy().into_owned();
        let sock = dir.join("none.sock").to_string_lossy().into_owned();
        let config = Config::resolve(|k| match k {
            "HOME" => Some(home.clone()),
            "CV_SOCKET" => Some(sock.clone()),
            _ => None,
        })
        .unwrap();
        for cmd in [
            Command::JobsList(JobsListParams::default()),
            Command::JobRetry("j".into()),
            Command::Run {
                action_id: "x".into(),
                note: None,
            },
        ] {
            assert_eq!(execute(&config, &cmd), 1);
        }
        assert!(
            !config.spool_dir().exists(),
            "nothing spooled for an unreachable daemon"
        );
    }

    #[test]
    fn query_validation() {
        let bad = JobsListParams {
            status: Some("pending".into()),
            ..JobsListParams::default()
        };
        assert!(list_query(&bad, 0).is_err());
        let q = list_query(
            &JobsListParams {
                status: Some("deferred".into()),
                limit: Some(10_000),
                ..JobsListParams::default()
            },
            7,
        )
        .unwrap();
        assert_eq!((q.deferred, q.limit, q.now_ms), (Some(true), MAX_LIMIT, 7));
        assert!(q.recent_first);
        assert!(
            intake_query(&IntakeListParams {
                day: Some("2026-1-5".into()),
                ..IntakeListParams::default()
            })
            .is_err()
        );
        assert!(
            intake_query(&IntakeListParams {
                outcome: Some("maybe".into()),
                ..IntakeListParams::default()
            })
            .is_err()
        );
    }
}
