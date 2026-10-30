//! The telemetry read model (plan job-telemetry WP-1): the stored rows of one job, the pure
//! bucket split over them and the per-type aggregate.

use crate::job::JobStatus;
/// The wait-cause and percentile vocabulary is the wire's (plan tn-review-2026-10-29 WP-R2).
pub use cv_protocol::{BucketPcts as Pcts, WaitCause};
use serde::{Deserialize, Serialize};

/// One claim of a job (telemetry attempt `attempt`, monotonic per job, never reset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptTime {
    pub attempt: i64,
    pub claimed_ms: i64,
    /// `None` when the attempt never settled normally (recovered lease; the row is partial).
    pub settled_ms: Option<i64>,
}

/// A wait-cause change: from `at_ms` the job waited for `cause` until the next event, the next
/// claim, or settle (a deferral also ends at `until_ms`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitEvent {
    pub at_ms: i64,
    pub cause: WaitCause,
    pub blocker: Option<String>,
    pub until_ms: Option<i64>,
}

/// One executed step of one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepTime {
    pub attempt: i64,
    pub step_index: i64,
    pub step_kind: String,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
}

/// Every stored telemetry row of one job (input of [`split_buckets`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobTelemetry {
    pub id: String,
    pub job_type: String,
    pub status: JobStatus,
    pub created_ms: i64,
    pub settled_ms: Option<i64>,
    /// Partial-coverage marker (Decision 6): created/claimed before v16, recovered without a
    /// normal settle, or cancelled while queued.
    pub partial: bool,
    /// In claim order.
    pub attempts: Vec<AttemptTime>,
    /// In record order.
    pub waits: Vec<WaitEvent>,
    /// By attempt, then step index.
    pub steps: Vec<StepTime>,
}

/// `settled − created` of one job, split so every millisecond lands in exactly one bucket:
/// `deferral + permit + key + breaker + run + residual == total`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketSplit {
    pub total_ms: i64,
    pub deferral_ms: i64,
    pub permit_ms: i64,
    pub key_ms: i64,
    pub breaker_ms: i64,
    pub run_ms: i64,
    /// Non-executing / unobserved residual: time terminal between a failure and its retry,
    /// and queued time no claim pass observed with a cause. Never folded into a measured bucket.
    pub residual_ms: i64,
    /// Measured wait per `(cause, blocker)` (blocker `""` when none was named).
    pub blockers: Vec<(WaitCause, String, i64)>,
}

/// Measured duration of each wait event of `t` (same order as `t.waits`), given the settle time:
/// a wait spans from its `at_ms` to the earliest of the next event, the next claim at or after
/// it, and settle (a deferral additionally ends at its `until_ms`).
fn wait_spans(t: &JobTelemetry, settled: i64) -> Vec<i64> {
    let clamp = |v: i64| v.clamp(t.created_ms, settled);
    let mut claims: Vec<i64> = t.attempts.iter().map(|a| a.claimed_ms).collect();
    claims.sort_unstable();
    t.waits
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let next_event = t.waits.get(i + 1).map_or(settled, |n| n.at_ms);
            let next_claim = claims
                .iter()
                .copied()
                .find(|c| *c >= w.at_ms)
                .unwrap_or(settled);
            let mut end = next_event.min(next_claim);
            if w.cause == WaitCause::Deferred
                && let Some(u) = w.until_ms
            {
                end = end.min(u);
            }
            (clamp(end) - clamp(w.at_ms)).max(0)
        })
        .collect()
}

/// Per-wait measured durations of a settled, fully instrumented job (`None` otherwise, like
/// [`split_buckets`]); same order as `t.waits`.
#[must_use]
pub fn wait_durations(t: &JobTelemetry) -> Option<Vec<i64>> {
    let settled = t.settled_ms?;
    (!t.partial).then(|| wait_spans(t, settled))
}

/// Split a settled, fully instrumented job's `settled − created` into wait buckets (deferral,
/// permit, key, breaker, each with its blockers), run time and the
/// labelled non-executing / unobserved residual (`total − measured`). Pure over the stored rows
/// ([`crate::StateDb::job_telemetry`] loads them).
///
/// Returns `None` for a job that has not settled or carries the partial-coverage marker: its
/// intervals are unavailable, never reported as a fabricated zero or cause.
///
/// A wait event spans from its `at_ms` to the earliest of the next event, the next claim at or
/// after it, and settle; a deferral additionally ends at its `until_ms` (the time after it until
/// the claim is unobserved). An attempt runs from its claim to its settle.
#[must_use]
pub fn split_buckets(t: &JobTelemetry) -> Option<BucketSplit> {
    let settled = t.settled_ms?;
    if t.partial {
        return None;
    }
    let clamp = |v: i64| v.clamp(t.created_ms, settled);
    let mut out = BucketSplit {
        total_ms: settled - t.created_ms,
        ..BucketSplit::default()
    };
    for (i, a) in t.attempts.iter().enumerate() {
        let next_claim = t.attempts.get(i + 1).map_or(settled, |n| n.claimed_ms);
        let end = clamp(a.settled_ms.unwrap_or(next_claim));
        out.run_ms += (end - clamp(a.claimed_ms)).max(0);
    }
    let mut blockers: std::collections::BTreeMap<(WaitCause, String), i64> =
        std::collections::BTreeMap::new();
    for (w, ms) in t.waits.iter().zip(wait_spans(t, settled)) {
        match w.cause {
            WaitCause::Deferred => out.deferral_ms += ms,
            WaitCause::Permit => out.permit_ms += ms,
            WaitCause::Key => out.key_ms += ms,
            WaitCause::Breaker => out.breaker_ms += ms,
        }
        *blockers
            .entry((w.cause, w.blocker.clone().unwrap_or_default()))
            .or_insert(0) += ms;
    }
    out.blockers = blockers
        .into_iter()
        .map(|((c, b), ms)| (c, b, ms))
        .collect();
    out.residual_ms =
        out.total_ms - out.deferral_ms - out.permit_ms - out.key_ms - out.breaker_ms - out.run_ms;
    Some(out)
}

/// Per-`job_type` aggregate of [`job_stats`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeStats {
    pub job_type: String,
    /// Attributed jobs (settled, fully instrumented).
    pub n: u64,
    /// Settled jobs excluded because their coverage is partial (counted, never attributed).
    pub partial: u64,
    /// Sum of `total_ms` over the attributed jobs (the sort key).
    pub total_sum_ms: i64,
    pub total: Pcts,
    pub deferral: Pcts,
    pub permit: Pcts,
    pub key: Pcts,
    pub breaker: Pcts,
    pub run: Pcts,
    pub residual: Pcts,
    /// Up to 3 `(cause, blocker, summed ms)`, largest first.
    pub top_blockers: Vec<(WaitCause, String, i64)>,
}

/// Nearest-rank percentile of an ascending-sorted slice: `sorted[ceil(p/100 * n) - 1]`
/// (rank clamped to `1..=n`); `0` for an empty slice.
#[must_use]
pub fn nearest_rank(sorted: &[i64], p: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = (n * p).div_ceil(100).clamp(1, n);
    sorted[rank - 1]
}

fn pcts(mut v: Vec<i64>) -> Pcts {
    v.sort_unstable();
    Pcts {
        p50: nearest_rank(&v, 50),
        p95: nearest_rank(&v, 95),
        max: v.last().copied().unwrap_or(0),
    }
}

/// Aggregate settled jobs per `job_type`, sorted by summed total ms descending (ties by type).
/// A settled job whose [`split_buckets`] is `None` counts in `partial` and in no bucket;
/// unsettled jobs are ignored.
#[must_use]
pub fn job_stats(jobs: &[JobTelemetry]) -> Vec<TypeStats> {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<&str, (u64, Vec<BucketSplit>)> = BTreeMap::new();
    for j in jobs.iter().filter(|j| j.settled_ms.is_some()) {
        let e = by.entry(j.job_type.as_str()).or_default();
        match split_buckets(j) {
            Some(b) => e.1.push(b),
            None => e.0 += 1,
        }
    }
    let mut out: Vec<TypeStats> = by
        .into_iter()
        .map(|(t, (partial, splits))| {
            let col = |f: fn(&BucketSplit) -> i64| pcts(splits.iter().map(f).collect());
            let mut blockers: BTreeMap<(WaitCause, String), i64> = BTreeMap::new();
            for b in &splits {
                for (c, name, ms) in &b.blockers {
                    *blockers.entry((*c, name.clone())).or_insert(0) += ms;
                }
            }
            let mut top: Vec<(WaitCause, String, i64)> = blockers
                .into_iter()
                .map(|((c, b), ms)| (c, b, ms))
                .collect();
            top.sort_by_key(|b| std::cmp::Reverse(b.2));
            top.truncate(3);
            TypeStats {
                job_type: t.to_owned(),
                n: splits.len() as u64,
                partial,
                total_sum_ms: splits.iter().map(|b| b.total_ms).sum(),
                total: col(|b| b.total_ms),
                deferral: col(|b| b.deferral_ms),
                permit: col(|b| b.permit_ms),
                key: col(|b| b.key_ms),
                breaker: col(|b| b.breaker_ms),
                run: col(|b| b.run_ms),
                residual: col(|b| b.residual_ms),
                top_blockers: top,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.total_sum_ms
            .cmp(&a.total_sum_ms)
            .then_with(|| a.job_type.cmp(&b.job_type))
    });
    out
}
