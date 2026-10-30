//! Text output of the `cv jobs|intake` commands.

use cv_protocol::{
    BlockerMs, IntakeListResult, JobTimelineResult, JobsListResult, JobsStatsResult,
};

use super::wire::enum_str;

/// `key=value` lines (aggregate counts only; sorted by key).
pub(super) fn render_counts(counts: &serde_json::Map<String, serde_json::Value>) -> String {
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

pub(super) fn render_jobs(r: &JobsListResult, now: i64) -> String {
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
    format!("  blocker   {} {}  {}\n", b.cause.as_str(), who, dur(b.ms))
}

pub(super) fn render_stats(r: &JobsStatsResult) -> String {
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

pub(super) fn render_timeline(t: &JobTimelineResult) -> String {
    let rel = |at: i64| format!("+{}", dur(at - t.created_ms));
    let mut out = format!("{}  {}  {}\n", t.job_id, t.job_type, enum_str(t.status));
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
            w.cause.as_str(),
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
        Some(s) => out.push_str(&format!("  {:<9} settled {}\n", rel(s), enum_str(t.status))),
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
        // `split` is absent exactly when `unavailable` names the reason.
        None => {
            if let Some(u) = t.unavailable {
                out.push_str(&format!("  bucket split: unavailable ({u})\n"));
            }
        }
    }
    out
}

pub(super) fn render_intake(r: &IntakeListResult) -> String {
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
