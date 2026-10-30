//! Store → protocol mappings shared by the daemon's `jobs_list` / `job_timeline` /
//! `jobs_stats` / `intake_list` / `status` handlers and the read-only `cv status` fallback.

use cv_protocol::{
    BlockerMs, IntakeItem, IntakeListParams, JobState, JobSummary, JobTimelineResult, JobTypeStats,
    JobsListParams, JobsStatsResult, TimelineAttempt, TimelineSplit, TimelineStep,
    TimelineUnavailable, TimelineWait, TrackerOutcome,
};
use cv_state::{IntakeQuery, Job, ListQuery, TrackerItem};

use super::page_limit;

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

pub(super) fn enum_str<T: serde::Serialize>(v: T) -> String {
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
                total: s.total,
                deferral: s.deferral,
                permit: s.permit,
                key: s.key,
                breaker: s.breaker,
                run: s.run,
                residual: s.residual,
                blockers: s
                    .top_blockers
                    .iter()
                    .cloned()
                    .map(BlockerMs::from)
                    .collect(),
            })
            .collect(),
        partial_total: stats.iter().map(|s| s.partial).sum(),
    }
}

/// One job's stored telemetry as a `job_timeline` result. A partial or unsettled job keeps its
/// stored events but carries an `unavailable` reason and no split or wait durations.
#[must_use]
pub fn job_timeline(t: &cv_state::JobTelemetry) -> JobTimelineResult {
    use cv_state::JobStatus as S;
    let split = cv_state::split_buckets(t);
    let durations = cv_state::wait_durations(t);
    let unavailable = if split.is_some() {
        None
    } else if t.settled_ms.is_none() {
        Some(TimelineUnavailable::NotSettled)
    } else {
        Some(TimelineUnavailable::PartialCoverage)
    };
    // Telemetry carries no `defer_until_ms`, so a queued job is plain `pending` here.
    let status = match t.status {
        S::Queued => JobState::Pending,
        S::Running => JobState::Running,
        S::Done => JobState::Done,
        S::Failed => JobState::Failed,
        S::Cancelled => JobState::Cancelled,
    };
    JobTimelineResult {
        job_id: t.id.clone(),
        job_type: t.job_type.clone(),
        status,
        created_ms: t.created_ms,
        settled_ms: t.settled_ms,
        unavailable,
        waits: t
            .waits
            .iter()
            .enumerate()
            .map(|(i, w)| TimelineWait {
                at_ms: w.at_ms,
                cause: w.cause,
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
            blockers: b.blockers.into_iter().map(BlockerMs::from).collect(),
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
