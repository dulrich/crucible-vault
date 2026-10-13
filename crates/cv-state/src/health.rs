//! Per-service health breaker, persisted in `state.db`.
//!
//! Port of `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/serviceHealth.ts:36-88` and the
//! `ServiceHealthRegistry` state machine below it: closed → open after a consecutive
//! failure weight of [`OPEN_THRESHOLD`] (a refused connection weighs [`REFUSED_WEIGHT`]) →
//! half-open once the open window (30 s doubling to a 10 min cap, or a reported
//! `retry_after_ms`) elapses → closed on success / straight back to open on a failed probe.
//! Half-open hands out exactly one probe token; a probe never reported on is reclaimed after
//! [`PROBE_STALE_MS`]. Changes from Crucible: state is persisted per service name instead of
//! in memory, and every transition takes an explicit `now_ms`. Transition listeners are not
//! carried (callers diff snapshots). Nothing Obsidian-induced was involved.

use crate::error::{Result, StateError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Consecutive-failure weight needed to open (`SERVICE_OPEN_THRESHOLD`).
pub const OPEN_THRESHOLD: i64 = 3;
/// Weight of a refused connection (`SERVICE_REFUSED_WEIGHT`).
pub const REFUSED_WEIGHT: i64 = 2;
/// Base open window (`SERVICE_OPEN_WINDOW_MS`).
pub const OPEN_WINDOW_MS: i64 = 30_000;
/// Cap on the computed open window (`SERVICE_MAX_OPEN_WINDOW_MS`).
pub const MAX_OPEN_WINDOW_MS: i64 = 10 * 60_000;
/// Floor applied to a reported `retry_after_ms`.
pub const MIN_RETRY_AFTER_MS: i64 = 1_000;
/// Unreported probe tokens are reclaimed after this (`SERVICE_PROBE_STALE_MS`).
pub const PROBE_STALE_MS: i64 = 5 * 60_000;

/// Breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    /// Healthy; claim freely.
    Closed,
    /// Do not claim.
    Open,
    /// One probe may be claimed.
    HalfOpen,
}

impl BreakerState {
    fn as_str(self) -> &'static str {
        match self {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half_open",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "closed" => Ok(BreakerState::Closed),
            "open" => Ok(BreakerState::Open),
            "half_open" => Ok(BreakerState::HalfOpen),
            other => Err(StateError::Corrupt {
                column: "service_health.state",
                value: other.into(),
            }),
        }
    }
}

/// Class of a service failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceFailureKind {
    /// Connection refused (counts double).
    Refused,
    /// Timed out.
    Timeout,
    /// 5xx.
    ServerError,
    /// 429 / quota.
    RateLimited,
}

impl ServiceFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            ServiceFailureKind::Refused => "refused",
            ServiceFailureKind::Timeout => "timeout",
            ServiceFailureKind::ServerError => "server_error",
            ServiceFailureKind::RateLimited => "rate_limited",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "refused" => ServiceFailureKind::Refused,
            "timeout" => ServiceFailureKind::Timeout,
            "server_error" => ServiceFailureKind::ServerError,
            "rate_limited" => ServiceFailureKind::RateLimited,
            other => {
                return Err(StateError::Corrupt {
                    column: "service_health.last_kind",
                    value: other.into(),
                });
            }
        })
    }
}

/// Persisted breaker entry for one service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceHealth {
    /// Service name.
    pub service: String,
    /// State.
    pub state: BreakerState,
    /// Consecutive failure weight.
    pub failure_score: i64,
    /// Opens since the last success (drives the doubling).
    pub open_count: i64,
    /// Last failure class.
    pub last_kind: Option<ServiceFailureKind>,
    /// Last failure text.
    pub last_reason: Option<String>,
    /// When the breaker last opened.
    pub opened_at_ms: Option<i64>,
    /// When the open window ends.
    pub retry_at_ms: Option<i64>,
    /// When the current probe token was handed out.
    pub probe_acquired_at_ms: Option<i64>,
}

impl ServiceHealth {
    /// A fresh, closed entry.
    #[must_use]
    pub fn fresh(service: &str) -> Self {
        Self {
            service: service.to_owned(),
            state: BreakerState::Closed,
            failure_score: 0,
            open_count: 0,
            last_kind: None,
            last_reason: None,
            opened_at_ms: None,
            retry_at_ms: None,
            probe_acquired_at_ms: None,
        }
    }

    /// Whether a probe token is out.
    #[must_use]
    pub fn probe_in_flight(&self) -> bool {
        self.probe_acquired_at_ms.is_some()
    }

    fn open(&mut self, now_ms: i64, retry_after_ms: Option<i64>) {
        let shift = u32::try_from(self.open_count.clamp(0, 20)).unwrap_or(20);
        let computed = OPEN_WINDOW_MS
            .saturating_mul(1_i64 << shift)
            .min(MAX_OPEN_WINDOW_MS);
        let window = match retry_after_ms {
            Some(ms) if ms > 0 => ms.max(MIN_RETRY_AFTER_MS),
            _ => computed,
        };
        self.open_count += 1;
        self.state = BreakerState::Open;
        self.opened_at_ms = Some(now_ms);
        self.retry_at_ms = Some(now_ms.saturating_add(window));
        self.probe_acquired_at_ms = None;
    }

    /// Record a failure.
    pub fn report_failure(
        &mut self,
        kind: ServiceFailureKind,
        reason: &str,
        retry_after_ms: Option<i64>,
        now_ms: i64,
    ) {
        self.last_kind = Some(kind);
        self.last_reason = Some(reason.to_owned());
        match self.state {
            // Already open: record, never extend the window.
            BreakerState::Open => {}
            // Failed probe: straight back to open with the next window.
            BreakerState::HalfOpen => self.open(now_ms, retry_after_ms),
            BreakerState::Closed => {
                self.failure_score += if kind == ServiceFailureKind::Refused {
                    REFUSED_WEIGHT
                } else {
                    1
                };
                if self.failure_score >= OPEN_THRESHOLD {
                    self.open(now_ms, retry_after_ms);
                }
            }
        }
    }

    /// Record a success: closes outright and resets the backoff.
    pub fn report_success(&mut self) {
        self.failure_score = 0;
        self.open_count = 0;
        self.opened_at_ms = None;
        self.retry_at_ms = None;
        self.probe_acquired_at_ms = None;
        self.state = BreakerState::Closed;
    }

    /// Promote an elapsed open window to half-open; reclaim a stale probe token.
    pub fn tick(&mut self, now_ms: i64) {
        if let Some(at) = self.probe_acquired_at_ms
            && now_ms - at > PROBE_STALE_MS
        {
            tracing::warn!(service = %self.service, "probe token never reported on; reclaiming");
            self.probe_acquired_at_ms = None;
        }
        if self.state == BreakerState::Open && self.retry_at_ms.unwrap_or(0) <= now_ms {
            self.state = BreakerState::HalfOpen;
            self.probe_acquired_at_ms = None;
        }
    }

    /// Hand out the single half-open probe token.
    pub fn try_acquire_probe(&mut self, now_ms: i64) -> bool {
        if self.state != BreakerState::HalfOpen || self.probe_acquired_at_ms.is_some() {
            return false;
        }
        self.probe_acquired_at_ms = Some(now_ms);
        true
    }

    /// Return the probe token without a verdict.
    pub fn release_probe(&mut self) {
        self.probe_acquired_at_ms = None;
    }
}

pub(crate) fn load(conn: &Connection, service: &str) -> Result<Option<ServiceHealth>> {
    conn.query_row(
        "SELECT service, state, failure_score, open_count, last_kind, last_reason, opened_at_ms, retry_at_ms, probe_acquired_at_ms FROM service_health WHERE service = ?1",
        params![service],
        row_raw,
    )
    .optional()?
    .map(from_raw)
    .transpose()
}

pub(crate) fn load_all(conn: &Connection) -> Result<Vec<ServiceHealth>> {
    let mut stmt = conn.prepare(
        "SELECT service, state, failure_score, open_count, last_kind, last_reason, opened_at_ms, retry_at_ms, probe_acquired_at_ms FROM service_health ORDER BY service",
    )?;
    let raws = stmt
        .query_map([], row_raw)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    raws.into_iter().map(from_raw).collect()
}

pub(crate) fn store(conn: &Connection, h: &ServiceHealth) -> Result<()> {
    conn.execute(
        "INSERT INTO service_health (service, state, failure_score, open_count, last_kind, last_reason, opened_at_ms, retry_at_ms, probe_acquired_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(service) DO UPDATE SET state=excluded.state, failure_score=excluded.failure_score, open_count=excluded.open_count,
           last_kind=excluded.last_kind, last_reason=excluded.last_reason, opened_at_ms=excluded.opened_at_ms,
           retry_at_ms=excluded.retry_at_ms, probe_acquired_at_ms=excluded.probe_acquired_at_ms",
        params![
            h.service,
            h.state.as_str(),
            h.failure_score,
            h.open_count,
            h.last_kind.map(ServiceFailureKind::as_str),
            h.last_reason,
            h.opened_at_ms,
            h.retry_at_ms,
            h.probe_acquired_at_ms
        ],
    )?;
    Ok(())
}

type Raw = (
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn row_raw(r: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
    ))
}

fn from_raw(r: Raw) -> Result<ServiceHealth> {
    Ok(ServiceHealth {
        service: r.0,
        state: BreakerState::parse(&r.1)?,
        failure_score: r.2,
        open_count: r.3,
        last_kind: r.4.as_deref().map(ServiceFailureKind::parse).transpose()?,
        last_reason: r.5,
        opened_at_ms: r.6,
        retry_at_ms: r.7,
        probe_acquired_at_ms: r.8,
    })
}
