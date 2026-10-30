//! Queue and intake exposure (plan `v0-automation-pipelines` WP-11): the store → protocol
//! mappings shared by the daemon's `jobs_list` / `intake_list` / `status` handlers and the
//! read-only `cv status` fallback, plus the `cv jobs …`, `cv intake …` and `cv run …` commands.
//!
//! The commands talk to the daemon only: unreachable is exit 1 with a clear message (nothing is
//! spooled), an RPC error is exit 1, a usage error exit 2.

use anyhow::{Result, anyhow};
use cv_protocol::{
    ActionContext, BlockerMs, BucketPcts, IntakeItem, IntakeListParams, IntakeListResult,
    JobIdParams, JobState, JobSummary, JobTimelineResult, JobTypeStats, JobsListParams,
    JobsListResult, JobsStatsParams, JobsStatsResult, RunActionParams, TimelineAttempt,
    TimelineSplit, TimelineStep, TimelineWait, TrackerOutcome, methods,
};
use cv_state::{IntakeQuery, Job, ListQuery, TrackerItem};

use crate::client::{Client, ClientError};
use crate::config::Config;

/// Default page size of `jobs_list` / `intake_list`.
pub const DEFAULT_LIMIT: u32 = 50;
/// Largest accepted page size (larger requests are clamped).
pub const MAX_LIMIT: u32 = 500;

/// Usage lines (also appended to the top-level `cv --help`).
pub const USAGE: &str = "  cv jobs list [--status S] [--type T] [--limit N] [--offset N]\n                   S = queued|running|done|failed|cancelled|deferred\n  cv jobs retry <job-id> | cv jobs cancel <job-id>\n  cv jobs stats [--type T] [--since <N>m|<N>h|<N>d|YYYY-MM-DD]   per-type wait/run bucket p50/p95/max (a date is UTC midnight; default: all retained rows)\n  cv jobs show <job-id>   one job's timeline: waits, attempts, steps, bucket split\n  cv intake list [--source-kind K] [--source KEY] [--day YYYY-MM-DD] [--outcome line|note|ignored|skipped] [--limit N] [--offset N]\n  cv intake reconcile [--dry-run]   link NULL-path rows, probe + auto-tick the complete ones, finalize youtube rows, re-match + re-clean mail; prints counts as key=value\n  cv run <action-id> [--note <vault-path>]   run an action through the daemon\n";

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

/// The one displayed-status rule (`job`, `cancel`, `job_retry`, `jobs_list`): a queued job is
/// `deferred` only while its `defer_until_ms` is still in the future — the same split the
/// `jobs_list` `queued`/`deferred` filters ([`list_query`]) apply.
#[must_use]
pub fn job_state(j: &Job, now_ms: i64) -> JobState {
    use cv_state::JobStatus as S;
    match j.status {
        S::Queued if j.defer_until_ms.is_some_and(|d| d > now_ms) => JobState::Deferred,
        S::Queued => JobState::Pending,
        S::Running => JobState::Running,
        S::Done => JobState::Done,
        S::Failed => JobState::Failed,
        S::Cancelled => JobState::Cancelled,
    }
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
    JobSummary {
        job_id: j.id.clone(),
        job_type: j.job_type.clone(),
        lane: j.lane,
        status: job_state(j, now_ms),
        service: j.service.clone(),
        attempts: j.attempts,
        created_ms: j.created_ms,
        settled_at_ms: j.settled_at_ms,
        defer_until_ms: j.defer_until_ms,
        error: j.error.clone(),
        failure_kind: j.failure_kind,
    }
}

fn wire_pcts(p: cv_state::Pcts) -> BucketPcts {
    BucketPcts {
        p50: p.p50,
        p95: p.p95,
        max: p.max,
    }
}

fn wire_blockers(b: &[(cv_state::WaitCause, String, i64)]) -> Vec<BlockerMs> {
    b.iter()
        .map(|(c, blocker, ms)| BlockerMs {
            cause: c.as_str().to_owned(),
            blocker: blocker.clone(),
            ms: *ms,
        })
        .collect()
}

/// `jobs_stats` result from the aggregated per-type stats (aggregates only: no params or paths).
#[must_use]
pub fn jobs_stats_result(stats: &[cv_state::TypeStats]) -> JobsStatsResult {
    JobsStatsResult {
        types: stats
            .iter()
            .map(|s| JobTypeStats {
                job_type: s.job_type.clone(),
                n: s.n,
                partial: s.partial,
                total: wire_pcts(s.total),
                deferral: wire_pcts(s.deferral),
                permit: wire_pcts(s.permit),
                key: wire_pcts(s.key),
                breaker: wire_pcts(s.breaker),
                run: wire_pcts(s.run),
                residual: wire_pcts(s.residual),
                blockers: wire_blockers(&s.top_blockers),
            })
            .collect(),
        partial_total: stats.iter().map(|s| s.partial).sum(),
    }
}

/// One job's stored telemetry as a `job_timeline` result. A partial or unsettled job keeps its
/// stored events but carries `available = false` and no split or wait durations.
#[must_use]
pub fn job_timeline(t: &cv_state::JobTelemetry) -> JobTimelineResult {
    let split = cv_state::split_buckets(t);
    let durations = cv_state::wait_durations(t);
    let unavailable = if split.is_some() {
        None
    } else if t.settled_ms.is_none() {
        Some("not settled".to_owned())
    } else {
        Some("partial coverage".to_owned())
    };
    JobTimelineResult {
        job_id: t.id.clone(),
        job_type: t.job_type.clone(),
        status: t.status.as_str().to_owned(),
        created_ms: t.created_ms,
        settled_ms: t.settled_ms,
        available: split.is_some(),
        unavailable,
        waits: t
            .waits
            .iter()
            .enumerate()
            .map(|(i, w)| TimelineWait {
                at_ms: w.at_ms,
                cause: w.cause.as_str().to_owned(),
                blocker: w.blocker.clone(),
                until_ms: w.until_ms,
                duration_ms: durations.as_ref().and_then(|d| d.get(i).copied()),
            })
            .collect(),
        attempts: t
            .attempts
            .iter()
            .map(|a| TimelineAttempt {
                attempt: a.attempt,
                claimed_ms: a.claimed_ms,
                settled_ms: a.settled_ms,
                steps: t
                    .steps
                    .iter()
                    .filter(|s| s.attempt == a.attempt)
                    .map(|s| TimelineStep {
                        index: s.step_index,
                        kind: s.step_kind.clone(),
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                    })
                    .collect(),
            })
            .collect(),
        split: split.map(|b| TimelineSplit {
            total_ms: b.total_ms,
            deferral_ms: b.deferral_ms,
            permit_ms: b.permit_ms,
            key_ms: b.key_ms,
            breaker_ms: b.breaker_ms,
            run_ms: b.run_ms,
            residual_ms: b.residual_ms,
            blockers: wire_blockers(&b.blockers),
        }),
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
        outcome: t.outcome,
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
        state: h.state,
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
    JobsStats {
        job_type: Option<String>,
        since: Option<Since>,
    },
    JobShow(String),
    IntakeList(IntakeListParams),
    IntakeReconcile {
        dry_run: bool,
    },
    Run {
        action_id: String,
        note: Option<String>,
    },
}

/// A `--since` bound: a relative age or an absolute UTC instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// `<N>m|h|d`: this many ms before now.
    Ago(i64),
    /// `YYYY-MM-DD`: UTC midnight, in epoch ms.
    At(i64),
}

impl Since {
    /// The `since_ms` bound at clock `now_ms`.
    #[must_use]
    pub fn resolve(self, now_ms: i64) -> i64 {
        match self {
            Self::Ago(ms) => now_ms.saturating_sub(ms),
            Self::At(ms) => ms,
        }
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian civil date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parses `--since`: `<N>m|<N>h|<N>d` (N a non-negative integer) or a real `YYYY-MM-DD` date
/// (UTC midnight, 1970 or later). Anything else is `None` (a usage error), never coerced.
#[must_use]
pub fn parse_since(v: &str) -> Option<Since> {
    if !v.is_ascii() || v.is_empty() {
        return None;
    }
    let b = v.as_bytes();
    if b.len() == 10 && b[4] == b'-' && b[7] == b'-' {
        let field = |r: std::ops::Range<usize>| -> Option<i64> {
            let t = &v[r];
            if t.bytes().all(|c| c.is_ascii_digit()) {
                t.parse().ok()
            } else {
                None
            }
        };
        let (y, m, d) = (field(0..4)?, field(5..7)?, field(8..10)?);
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let dim = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        if y < 1970 || d < 1 || d > dim {
            return None;
        }
        return Some(Since::At(days_from_civil(y, m, d) * 86_400_000));
    }
    let (num, unit) = v.split_at(v.len() - 1);
    if num.is_empty() || !num.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = num.parse().ok()?;
    let unit_ms: i64 = match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    n.checked_mul(unit_ms).map(Since::Ago)
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
        ["jobs", "stats", rest @ ..] => {
            let (mut job_type, mut since) = (None, None);
            for pair in pairs(rest)? {
                match pair {
                    ("--type", v) => job_type = Some(v.to_owned()),
                    ("--since", v) => since = Some(parse_since(v)?),
                    _ => return None,
                }
            }
            Some(Command::JobsStats { job_type, since })
        }
        ["jobs", "show", id] if !id.starts_with('-') => Some(Command::JobShow((*id).to_owned())),
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
        ["intake", "reconcile"] => Some(Command::IntakeReconcile { dry_run: false }),
        ["intake", "reconcile", "--dry-run"] => Some(Command::IntakeReconcile { dry_run: true }),
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

/// `key=value` lines (aggregate counts only; sorted by key).
fn render_counts(counts: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut keys: Vec<&String> = counts.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|k| format!("{k}={}\n", counts[k]))
        .collect()
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
                j.job_id,
                j.status_str(),
                j.job_type,
                j.attempts,
                when,
                detail
            )
            .trim_end(),
        );
        out.push('\n');
    }
    out
}

/// A duration in ms, human: `850ms`, `4.2s`, `3m05s`, `2h10m`.
fn dur(ms: i64) -> String {
    let ms = ms.max(0);
    match ms {
        0..1000 => format!("{ms}ms"),
        1000..60_000 => format!("{}.{}s", ms / 1000, (ms % 1000) / 100),
        60_000..3_600_000 => format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
        _ => format!("{}h{:02}m", ms / 3_600_000, (ms % 3_600_000) / 60_000),
    }
}

fn blocker_line(b: &BlockerMs) -> String {
    let who = if b.blocker.is_empty() {
        "-"
    } else {
        &b.blocker
    };
    format!("  blocker   {} {}  {}\n", b.cause, who, dur(b.ms))
}

fn render_stats(r: &JobsStatsResult) -> String {
    if r.types.is_empty() && r.partial_total == 0 {
        return "no settled jobs\n".to_owned();
    }
    let mut out = String::new();
    for t in &r.types {
        out.push_str(&format!(
            "{}  n={}  partial={}\n",
            t.job_type, t.n, t.partial
        ));
        for (name, p) in [
            ("total", &t.total),
            ("deferral", &t.deferral),
            ("permit", &t.permit),
            ("key", &t.key),
            ("breaker", &t.breaker),
            ("run", &t.run),
            ("residual", &t.residual),
        ] {
            out.push_str(&format!(
                "  {name:<9} p50 {}  p95 {}  max {}\n",
                dur(p.p50),
                dur(p.p95),
                dur(p.max)
            ));
        }
        for b in &t.blockers {
            out.push_str(&blocker_line(b));
        }
    }
    if r.partial_total > 0 {
        out.push_str(&format!(
            "partial_total={} (partial coverage; excluded from the buckets)\n",
            r.partial_total
        ));
    }
    out
}

fn render_timeline(t: &JobTimelineResult) -> String {
    let rel = |at: i64| format!("+{}", dur(at - t.created_ms));
    let mut out = format!("{}  {}  {}\n", t.job_id, t.job_type, t.status);
    out.push_str("  created   +0ms\n");
    for w in &t.waits {
        let who = w
            .blocker
            .as_deref()
            .filter(|b| !b.is_empty())
            .unwrap_or("-");
        let d = w.duration_ms.map_or_else(|| "unavailable".to_owned(), dur);
        out.push_str(&format!(
            "  {:<9} wait {} {}  {}\n",
            rel(w.at_ms),
            w.cause,
            who,
            d
        ));
    }
    for a in &t.attempts {
        let run = a.settled_ms.map_or_else(
            || "unsettled".to_owned(),
            |s| format!("run {}", dur(s - a.claimed_ms)),
        );
        out.push_str(&format!(
            "  {:<9} attempt {} claimed  {}\n",
            rel(a.claimed_ms),
            a.attempt,
            run
        ));
        for s in &a.steps {
            let d = s
                .end_ms
                .map_or_else(|| "unfinished".to_owned(), |e| dur(e - s.start_ms));
            out.push_str(&format!("            step {} {}  {}\n", s.index, s.kind, d));
        }
    }
    match t.settled_ms {
        Some(s) => out.push_str(&format!("  {:<9} settled {}\n", rel(s), t.status)),
        None => out.push_str("  not settled\n"),
    }
    match &t.split {
        Some(b) => {
            out.push_str(&format!(
                "  bucket split: total {}  deferral {}  permit {}  key {}  breaker {}  run {}  residual {}\n",
                dur(b.total_ms),
                dur(b.deferral_ms),
                dur(b.permit_ms),
                dur(b.key_ms),
                dur(b.breaker_ms),
                dur(b.run_ms),
                dur(b.residual_ms)
            ));
            for x in &b.blockers {
                out.push_str(&blocker_line(x));
            }
        }
        None => out.push_str(&format!(
            "  bucket split: unavailable ({})\n",
            t.unavailable.as_deref().unwrap_or("not available")
        )),
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
            parse(&a(&["intake", "reconcile"])),
            Some(Command::IntakeReconcile { dry_run: false })
        );
        assert_eq!(
            parse(&a(&["intake", "reconcile", "--dry-run"])),
            Some(Command::IntakeReconcile { dry_run: true })
        );
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
            &["intake", "reconcile", "stray"],
            &["intake", "reconcile", "--dry-run", "x"],
            &["intake", "reconcile", "--dryrun"],
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

#[cfg(test)]
mod telemetry_tests {
    use super::*;
    use cv_state::{AttemptTime, JobStatus, JobTelemetry, StepTime, WaitCause, WaitEvent};

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parse_stats_and_show() {
        assert_eq!(
            parse(&a(&["jobs", "stats"])),
            Some(Command::JobsStats {
                job_type: None,
                since: None
            })
        );
        assert_eq!(
            parse(&a(&["jobs", "stats", "--type", "t", "--since", "90m"])),
            Some(Command::JobsStats {
                job_type: Some("t".into()),
                since: Some(Since::Ago(90 * 60_000))
            })
        );
        assert_eq!(parse_since("2h"), Some(Since::Ago(7_200_000)));
        assert_eq!(parse_since("3d"), Some(Since::Ago(3 * 86_400_000)));
        assert_eq!(parse_since("1970-01-02"), Some(Since::At(86_400_000)));
        assert_eq!(
            parse_since("2024-02-29"),
            Some(Since::At(1_709_164_800_000))
        );
        assert_eq!(Since::Ago(1_000).resolve(5_000), 4_000);
        assert_eq!(Since::At(7).resolve(5_000), 7);
        assert_eq!(
            parse(&a(&["jobs", "show", "j1"])),
            Some(Command::JobShow("j1".into()))
        );
        for bad in [
            "",
            "m",
            "5",
            "5x",
            "-5m",
            "1.5h",
            "2023-02-29",
            "2023-13-01",
            "1969-12-31",
            "2023-1-01",
            "99999999999999999999d",
        ] {
            assert_eq!(parse_since(bad), None, "{bad:?}");
        }
        for bad in [
            &["jobs", "stats", "--since", "soon"][..],
            &["jobs", "stats", "--since"],
            &["jobs", "stats", "--bogus", "1"],
            &["jobs", "show"],
            &["jobs", "show", "a", "b"],
        ] {
            assert_eq!(parse(&a(bad)), None, "{bad:?}");
            assert_eq!(run(&a(bad)), 2, "{bad:?}");
        }
    }

    fn tel(partial: bool) -> JobTelemetry {
        JobTelemetry {
            id: "job-1".into(),
            job_type: "synth".into(),
            status: JobStatus::Done,
            created_ms: 1_000,
            settled_ms: Some(9_000),
            partial,
            attempts: vec![
                AttemptTime {
                    attempt: 1,
                    claimed_ms: 3_000,
                    settled_ms: Some(4_000),
                },
                AttemptTime {
                    attempt: 2,
                    claimed_ms: 6_000,
                    settled_ms: Some(9_000),
                },
            ],
            waits: vec![WaitEvent {
                at_ms: 1_000,
                cause: WaitCause::Key,
                blocker: Some("holder".into()),
                until_ms: None,
            }],
            steps: vec![
                StepTime {
                    attempt: 1,
                    step_index: 0,
                    step_kind: "guard".into(),
                    start_ms: 3_000,
                    end_ms: Some(3_500),
                },
                StepTime {
                    attempt: 2,
                    step_index: 0,
                    step_kind: "shell".into(),
                    start_ms: 6_000,
                    end_ms: Some(8_400),
                },
            ],
        }
    }

    #[test]
    fn show_renders_a_timeline_and_never_a_fabricated_zero_for_partial_jobs() {
        let full = render_timeline(&job_timeline(&tel(false)));
        println!("{full}");
        assert!(full.contains("wait key holder  2.0s"), "{full}");
        assert!(full.contains("step 0 shell  2.4s"), "{full}");
        assert!(full.contains("bucket split: total 8.0s"), "{full}");
        assert!(!full.contains("unavailable"), "{full}");
        let partial = render_timeline(&job_timeline(&tel(true)));
        println!("{partial}");
        assert!(
            partial.contains("bucket split: unavailable (partial coverage)"),
            "{partial}"
        );
        assert!(
            partial.contains("wait key holder  unavailable"),
            "{partial}"
        );
        let mut open = tel(false);
        open.settled_ms = None;
        let open = render_timeline(&job_timeline(&open));
        assert!(
            open.contains("bucket split: unavailable (not settled)"),
            "{open}"
        );
    }

    #[test]
    fn stats_output_carries_aggregates_only() {
        // Seed text the renderer must never see: a sentinel in a job id-adjacent field cannot
        // reach `JobTelemetry` (it holds no params), so assert the rendered block directly.
        let jobs = [tel(false), tel(true)];
        let r = jobs_stats_result(&cv_state::job_stats(&jobs));
        let text = render_stats(&r);
        println!("{text}");
        assert!(text.starts_with("synth  n=1  partial=1\n"), "{text}");
        assert!(
            text.contains("  total     p50 8.0s  p95 8.0s  max 8.0s"),
            "{text}"
        );
        assert!(text.contains("  blocker   key holder  2.0s"), "{text}");
        assert!(text.contains("partial_total=1"), "{text}");
        assert!(!text.contains("job-1"), "no job ids in the aggregate view");
        assert_eq!(
            render_stats(&JobsStatsResult {
                types: vec![],
                partial_total: 0
            }),
            "no settled jobs\n"
        );
    }
}
