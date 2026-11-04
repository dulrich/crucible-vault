//! Claim eligibility (plan tn-review-2026-10-29-remediation WP-R1, finding T1): the one rule
//! both [`crate::StateDb::claim`] and [`crate::StateDb::observe_pool_wait`] apply to a queued
//! candidate — a busy resource key (I3), then the service breaker.

use crate::error::Result;
use crate::health::ServiceHealth;
use crate::telemetry::WaitCause;
use cv_protocol::BreakerState;
use rusqlite::{Transaction, params};
use std::collections::HashMap;

/// Every resource key held by a running job, mapped to that job's action (`job_type`). Fails
/// on an unreadable running row's keys (I3 fail-closed).
pub(crate) fn running_keys(tx: &Transaction<'_>) -> Result<HashMap<String, String>> {
    let mut busy = HashMap::new();
    let mut stmt =
        tx.prepare("SELECT resource_keys, job_type FROM jobs WHERE status = 'running'")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (raw, job_type) = row?;
        let keys: Vec<String> = serde_json::from_str(&raw)?;
        for k in keys {
            busy.entry(k).or_insert_with(|| job_type.clone());
        }
    }
    Ok(busy)
}

/// One undeferred queued row (`job_type` added by plan job-slots-fast-lane WP-2 for the pool
/// filter).
pub(crate) struct Candidate {
    /// The job id.
    pub(crate) id: String,
    /// The raw `resource_keys` JSON.
    pub(crate) raw_keys: String,
    /// The breaker service, if any.
    pub(crate) service: Option<String>,
    /// The job type (`action:<id>`).
    pub(crate) job_type: String,
}

/// Every undeferred queued row at `now_ms`, highest lane then oldest id (the claim order).
pub(crate) fn candidates(tx: &Transaction<'_>, now_ms: i64) -> Result<Vec<Candidate>> {
    let mut stmt = tx.prepare(
        "SELECT id, resource_keys, service, job_type FROM jobs WHERE status = 'queued' AND (defer_until_ms IS NULL OR defer_until_ms <= ?1) ORDER BY lane ASC, id ASC",
    )?;
    Ok(stmt
        .query_map(params![now_ms], |r| {
            Ok(Candidate {
                id: r.get(0)?,
                raw_keys: r.get(1)?,
                service: r.get(2)?,
                job_type: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?)
}

/// How a half-open breaker is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeMode {
    /// The claim: a free half-open probe is taken (the caller stores the health).
    Take,
    /// The permit-wait observation: read-only peek, never takes the probe (audit E2).
    Peek,
}

/// The classifier's verdict on one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Eligibility {
    /// Claimable; `probe_taken` when [`ProbeMode::Take`] acquired the half-open probe (the
    /// caller must store the mutated health).
    Claimable { probe_taken: bool },
    /// Not claimable: waits for `cause`, named by `blocker` (key holder's action or service).
    Wait { cause: WaitCause, blocker: String },
}

/// The one eligibility rule over a candidate's parsed `keys`, its `service`, the busy-key map
/// ([`running_keys`]) and the service's loaded breaker `health` (`None` = no breaker entry).
pub(crate) fn classify(
    keys: &[String],
    service: Option<&str>,
    busy: &HashMap<String, String>,
    health: Option<&mut ServiceHealth>,
    mode: ProbeMode,
    now_ms: i64,
) -> Eligibility {
    if let Some(holder) = keys.iter().find_map(|k| busy.get(k)) {
        return Eligibility::Wait {
            cause: WaitCause::Key,
            blocker: holder.clone(),
        };
    }
    let (Some(svc), Some(h)) = (service, health) else {
        return Eligibility::Claimable { probe_taken: false };
    };
    let breaker = || Eligibility::Wait {
        cause: WaitCause::Breaker,
        blocker: svc.to_owned(),
    };
    match (h.state, mode) {
        (BreakerState::Closed, _) => Eligibility::Claimable { probe_taken: false },
        (BreakerState::Open, _) => breaker(),
        (BreakerState::HalfOpen, ProbeMode::Take) => {
            if h.try_acquire_probe(now_ms) {
                Eligibility::Claimable { probe_taken: true }
            } else {
                breaker()
            }
        }
        (BreakerState::HalfOpen, ProbeMode::Peek) => {
            if h.probe_in_flight() {
                breaker()
            } else {
                Eligibility::Claimable { probe_taken: false }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(state: BreakerState, probe: Option<i64>) -> ServiceHealth {
        let mut h = ServiceHealth::fresh("svc");
        h.state = state;
        h.probe_acquired_at_ms = probe;
        h
    }

    #[test]
    fn classifier_covers_key_breaker_probe_and_claimable() {
        let busy: HashMap<String, String> = [("n".to_owned(), "holder".to_owned())].into();
        let keys = |k: &str| vec![k.to_owned()];
        let wait = |cause, b: &str| Eligibility::Wait {
            cause,
            blocker: b.to_owned(),
        };
        let free = |probe_taken| Eligibility::Claimable { probe_taken };
        for mode in [ProbeMode::Take, ProbeMode::Peek] {
            // Busy key wins over any breaker.
            let mut h = health(BreakerState::Open, None);
            let got = classify(&keys("n"), Some("svc"), &busy, Some(&mut h), mode, 5);
            assert_eq!(got, wait(WaitCause::Key, "holder"));
            // Open breaker.
            let got = classify(&keys("m"), Some("svc"), &busy, Some(&mut h), mode, 5);
            assert_eq!(got, wait(WaitCause::Breaker, "svc"));
            // Half-open, probe already taken.
            let mut h = health(BreakerState::HalfOpen, Some(1));
            let got = classify(&keys("m"), Some("svc"), &busy, Some(&mut h), mode, 5);
            assert_eq!(got, wait(WaitCause::Breaker, "svc"));
            // Claimable: closed breaker, no breaker entry, no service.
            let mut h = health(BreakerState::Closed, None);
            let got = classify(&keys("m"), Some("svc"), &busy, Some(&mut h), mode, 5);
            assert_eq!(got, free(false));
            assert_eq!(
                classify(&[], Some("svc"), &busy, None, mode, 5),
                free(false)
            );
            assert_eq!(classify(&[], None, &busy, None, mode, 5), free(false));
        }
        // Half-open, probe free: the claim takes it, the observation only peeks.
        let mut h = health(BreakerState::HalfOpen, None);
        let got = classify(&[], Some("svc"), &busy, Some(&mut h), ProbeMode::Peek, 5);
        assert_eq!((got, h.probe_acquired_at_ms), (free(false), None));
        let got = classify(&[], Some("svc"), &busy, Some(&mut h), ProbeMode::Take, 5);
        assert_eq!((got, h.probe_acquired_at_ms), (free(true), Some(5)));
    }
}
