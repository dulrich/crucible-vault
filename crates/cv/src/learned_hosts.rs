//! Learned publication hosts (plan `intake-foundations` WP-3): the `cv` binary's side of the
//! `state.db` `learned_hosts` table. The ownership rule lives in `cv_actions::sources`
//! ([`SourcesConfig::may_learn`]); this module reads the rows as plain data, records proofs
//! (trackers), seeds from recorded blog rows at daemon start, and prunes on reload. A learning
//! failure is logged and never fails the caller.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use cv_actions::SourcesConfig;
use cv_actions::sources::{LearnedRow, feed_link_host, foreign_link_host};
use cv_state::{LearnedHost, StateDb};

use crate::sync::lock;

/// Evidence of an RSS item whose link host differs from its feed's.
pub const EVIDENCE_RSS: &str = "rss-link";
/// Evidence of a matched sender's mail whose canonical post URL was not rebased.
pub const EVIDENCE_MAIL: &str = "mail-url";
/// Evidence of a recorded blog row at the start-up seed.
pub const EVIDENCE_SEED: &str = "rss-seed";

/// Every learned row as plain data (an unreadable table reads as none, with a warning).
#[must_use]
pub fn rows(state: &StateDb) -> Vec<LearnedRow> {
    rows_from(state)
}

/// [`rows`] over any read-only intake view (the daemon-free report reads a `StateReader`).
#[must_use]
pub fn rows_from<S: cv_state::IntakeRead + ?Sized>(state: &S) -> Vec<LearnedRow> {
    match state.learned_hosts() {
        Ok(v) => v
            .into_iter()
            .map(|h| LearnedRow {
                host: h.host,
                publication: h.publication,
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "learned hosts unreadable; none applied");
            Vec::new()
        }
    }
}

/// Records that record `name` proved `host` when the ownership rule allows it; a reserved host,
/// an unknown record, or a host another publication already owns learns nothing. Returns
/// whether a row was inserted.
pub fn observe(
    state: &mut StateDb,
    sources: &SourcesConfig,
    name: &str,
    host: &str,
    evidence: &str,
    now_ms: i64,
) -> bool {
    let Some(host) = cv_text::url_canon::bare_host_name(host) else {
        return false;
    };
    if !sources.may_learn(&host, name) {
        return false;
    }
    let row = LearnedHost {
        host: host.clone(),
        publication: name.to_owned(),
        first_ms: now_ms,
        evidence: evidence.to_owned(),
    };
    match state.insert_learned_host(&row) {
        Ok(true) => {
            tracing::info!(%host, publication = %name, %evidence, "publication host learned");
            true
        }
        Ok(false) => false,
        Err(e) => {
            tracing::warn!(error = %e, %host, "learned host not recorded");
            false
        }
    }
}

/// A live RSS item's observation: learns its link host for the feed's record per
/// [`foreign_link_host`]. The evidence is the parsed item itself: `link` is the item's own
/// non-empty `<link>` (empty when the item had none and its URL fell back to the GUID, which
/// proves nothing). It holds whether or not the GUID equals the link.
pub fn observe_feed_item(
    state: &mut StateDb,
    sources: &SourcesConfig,
    feed: &str,
    link: &str,
    now_ms: i64,
) -> bool {
    let Some(p) = sources.by_feed(feed) else {
        return false;
    };
    let Some(host) = foreign_link_host(link, feed) else {
        return false;
    };
    let name = p.name.clone();
    observe(state, sources, &name, &host, EVIDENCE_RSS, now_ms)
}

/// Drops learned rows the ownership rule now rejects: a host now reserved by a configured
/// matcher, or a record that no longer exists. Returns how many went.
pub fn prune(state: &mut StateDb, sources: &SourcesConfig) -> usize {
    let mut dropped = 0;
    for r in rows(state) {
        if sources.may_learn(&r.host, &r.publication) {
            continue;
        }
        match state.delete_learned_host(&r.host) {
            Ok(true) => {
                dropped += 1;
                tracing::info!(host = %r.host, publication = %r.publication, "learned host dropped");
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, host = %r.host, "learned host not dropped"),
        }
    }
    dropped
}

/// The start-up seed from recorded blog rows (`source_key` = feed URL). A row seeds its host
/// only when [`feed_link_host`] proves it (the stored-row evidence: http(s), `url` not the GUID —
/// a stored row cannot tell a link from a URL-shaped GUID — and a host other than the feed's),
/// the host is not reserved, the feed's record exists, and within this pass the host is
/// observed under exactly one publication. Idempotent: a second run inserts nothing. Prunes
/// first. Returns the number inserted.
pub fn seed(state: &Mutex<StateDb>, sources: &SourcesConfig, now_ms: i64) -> usize {
    let mut st = lock(state);
    prune(&mut st, sources);
    let tracked = match st.tracker_url_rows(crate::steps::tracker::blogs::KIND) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "learned-host seed skipped: tracker rows unreadable");
            return 0;
        }
    };
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (feed, item_id, url) in &tracked {
        let Some(p) = sources.by_feed(feed) else {
            continue;
        };
        let Some(host) = feed_link_host(url, item_id, feed) else {
            continue;
        };
        if sources.may_learn(&host, &p.name) {
            seen.entry(host).or_default().insert(p.name.clone());
        }
    }
    let mut n = 0;
    for (host, names) in seen {
        let mut it = names.iter();
        if let (Some(name), None) = (it.next(), it.next())
            && observe(&mut st, sources, name, &host, EVIDENCE_SEED, now_ms)
        {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
#[path = "learned_hosts_tests.rs"]
mod tests;
