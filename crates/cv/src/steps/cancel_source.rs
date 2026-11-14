//! `cancel_source_intake` (plan `cancel-source-intake` WP-1): cancels every open intake row of
//! one source, and the automatic variant the trackers run for sources removed from `sources.toml`.
//!
//! A cancel works like the Ignore Shorts cancel (`cancel_listed_shorts`): each open row
//! (`outcome = 'line'`, NULL status) of the exact `(source_kind, source_key)` becomes
//! `ignored` / `-` plus an `ignored_ids` row, so the item is never offered again even if the
//! source is re-added. `state.db` only: no vault file is written, no daily-note line is touched,
//! and nothing goes over the network.
//!
//! - **Manual** (step `cancel_source_intake`, one required text param `source`, placeholders
//!   expanded): matched case-insensitively and exactly (never a substring) against the
//!   `source_key` or the recorded `source_name` of the open rows of kinds `youtube`/`blog`/`mail`.
//!   No match, an empty name, or a name spanning more than one `(kind, key)` (the message lists
//!   the candidates as `kind:key (name)`) fails with no change. Otherwise every open row of that
//!   source is cancelled (reason `source-cancelled`). Outcome vars: `kind`, `source_key`,
//!   `cancelled`.
//! - **Orphans** ([`orphans_pass`] / [`cancel_orphaned`], run first thing by every tracker
//!   invocation, the disabled / no-key / deferred paths included): the open rows of a source key
//!   absent from the current `sources.toml` list of its kind are cancelled (reason
//!   `source-removed`), at most [`ORPHAN_CAP`] rows per source per run. It does nothing when the
//!   kind's list is empty (this covers a missing file), when `sources.toml` has any current
//!   error (the config keeps the last valid copy but we refuse anyway), or when more than half of
//!   the kind's sources with open rows would be orphaned (an accidental truncation; one `warn!`
//!   with both counts). Mail's `unknown-sender` is never orphaned; `skip`/`ignore` rows count as
//!   present.

use std::collections::BTreeSet;

use cv_actions::{ConfigSet, StepKind};
use serde::Serialize;

use super::mail;
use super::params::{check_keys, check_placeholders, expand, get_str};
use super::{StepCtx, StepError, StepModule, StepOutcome};
use crate::now_ms;

/// The kinds the manual step searches.
const KINDS: [&str; 3] = ["youtube", "blog", "mail"];

/// `ignored_ids.reason` of a manual cancel.
const REASON_MANUAL: &str = "source-cancelled";

/// `ignored_ids.reason` of the automatic cancel.
const REASON_REMOVED: &str = "source-removed";

/// Open rows the automatic pass cancels per source per run (the rest follow on later runs).
pub(crate) const ORPHAN_CAP: usize = 500;

/// The `cancel_source_intake` step.
pub(crate) struct CancelSourceIntake;

impl StepModule for CancelSourceIntake {
    type Params = String;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<String, String> {
        check_keys(t, kind, "cancel_source_intake")?;
        let source = get_str(t, "source")?
            .ok_or_else(|| "cancel_source_intake step: `source` is required".to_owned())?;
        check_placeholders(&source)?;
        Ok(source)
    }

    fn run(source: String, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(&source, ctx)
    }
}

/// Outcome vars.
#[derive(Serialize)]
struct Vars {
    kind: String,
    source_key: String,
    cancelled: usize,
}

fn state_err(e: cv_state::StateError) -> StepError {
    StepError::failed(format!("state: {e}"))
}

/// The distinct `(kind, key, name)` of open sources matching `query` (trimmed, non-empty),
/// case-insensitively against the key or the recorded name; one entry per `(kind, key)`.
fn matches(
    ctx: &StepCtx<'_>,
    query: &str,
) -> Result<Vec<(&'static str, String, String)>, StepError> {
    let q = query.to_lowercase();
    let st = ctx.state();
    let mut out: Vec<(&'static str, String, String)> = Vec::new();
    for kind in KINDS {
        for (key, name) in st.tracker_open_sources(kind).map_err(state_err)? {
            let name = name.unwrap_or_default();
            let hit = key.to_lowercase() == q || name.trim().to_lowercase() == q;
            if hit && !out.iter().any(|(k, key2, _)| *k == kind && *key2 == key) {
                out.push((kind, key, name));
            }
        }
    }
    Ok(out)
}

/// Executes the manual step.
fn run(source: &str, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let source = expand(ctx, source)?;
    let query = source.trim();
    if query.is_empty() {
        return Err(StepError::failed("cancel_source_intake: `source` is empty"));
    }
    let hits = matches(ctx, query)?;
    let (kind, key) = match hits.as_slice() {
        [] => {
            return Err(StepError::failed(format!(
                "cancel_source_intake: no source with open intake matches {query:?}"
            )));
        }
        [(kind, key, _)] => (*kind, key.clone()),
        many => {
            let list: Vec<String> = many
                .iter()
                .map(|(k, key, name)| format!("{k}:{key} ({name})"))
                .collect();
            return Err(StepError::failed(format!(
                "cancel_source_intake: {query:?} is ambiguous: {}",
                list.join(", ")
            )));
        }
    };
    let cancelled = ctx
        .state()
        .cancel_source_open_items(kind, &key, REASON_MANUAL, now_ms(), usize::MAX)
        .map_err(state_err)?;
    let mut out = StepOutcome::default();
    out.set_vars(&Vars {
        kind: kind.to_owned(),
        source_key: key,
        cancelled,
    });
    Ok(out)
}

/// Whether `sources.toml` has any current error (parse, type or entry).
fn sources_broken(config: &ConfigSet) -> bool {
    config.errors().iter().any(|e| e.contains("sources.toml"))
}

/// The automatic cancel for one kind: open rows of sources whose key is not in `present` (ALL the
/// kind's `sources.toml` keys, `skip`/`ignore` rows included). Returns rows cancelled; see the
/// module doc for when it does nothing.
pub(crate) fn cancel_orphaned(
    ctx: &StepCtx<'_>,
    kind: &str,
    present: &[String],
) -> Result<usize, StepError> {
    if present.is_empty() {
        return Ok(0);
    }
    if sources_broken(ctx.config) {
        tracing::warn!(
            kind,
            "cancel orphaned intake: sources.toml has errors; skipped"
        );
        return Ok(0);
    }
    let mut st = ctx.state();
    let keys: BTreeSet<String> = st
        .tracker_open_sources(kind)
        .map_err(state_err)?
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| !(kind == mail::KIND && k == mail::UNKNOWN))
        .collect();
    let have: BTreeSet<&str> = present.iter().map(String::as_str).collect();
    let orphans: Vec<&String> = keys.iter().filter(|k| !have.contains(k.as_str())).collect();
    if orphans.is_empty() {
        return Ok(0);
    }
    if orphans.len() * 2 > keys.len() {
        tracing::warn!(
            kind,
            orphaned = orphans.len(),
            with_open_rows = keys.len(),
            "cancel orphaned intake: more than half of the sources would be cancelled; skipped"
        );
        return Ok(0);
    }
    let mut changed = 0;
    for key in orphans {
        changed += st
            .cancel_source_open_items(kind, key, REASON_REMOVED, now_ms(), ORPHAN_CAP)
            .map_err(state_err)?;
    }
    Ok(changed)
}

/// Channel ids of every publication record (the YouTube tracker's `source_key`), `skip`/`ignore`
/// rows included.
pub(crate) fn youtube_keys(config: &ConfigSet) -> Vec<String> {
    config
        .sources()
        .channels()
        .map(|(id, _)| id.to_owned())
        .collect()
}

/// Trimmed feed links of every publication record, `skip`/`ignore` rows included (the blog
/// tracker's `source_key`).
pub(crate) fn blog_keys(config: &ConfigSet) -> Vec<String> {
    config
        .sources()
        .feeds()
        .map(|(l, _)| l.trim().to_owned())
        .collect()
}

/// Every publication record's name (the mail `source_key`), plus the legacy names a fold renamed
/// (`mail_key_renames`) so their rows stay open until the reconcile re-keys them.
pub(crate) fn mail_keys(config: &ConfigSet) -> Vec<String> {
    let s = config.sources();
    let from_mail = |i: usize| s.origins()[i].iter().any(|o| o.table == "newsletter");
    let (mail, rest): (Vec<_>, Vec<_>) = s
        .publications()
        .iter()
        .enumerate()
        .partition(|(i, _)| from_mail(*i));
    mail.into_iter()
        .chain(rest)
        .map(|(_, r)| r.name.clone())
        .chain(s.mail_key_renames().into_iter().map(|(old, _)| old))
        .collect()
}

/// The tracker hook: runs [`cancel_orphaned`] and logs (never propagates) a failure, so it can
/// never change the tracker's own outcome.
pub(crate) fn orphans_pass(ctx: &StepCtx<'_>, kind: &str, present: &[String]) {
    match cancel_orphaned(ctx, kind, present) {
        Ok(0) => {}
        Ok(n) => tracing::info!(kind, cancelled = n, "cancelled intake of removed sources"),
        Err(e) => tracing::warn!(kind, error = %e, "cancel orphaned intake failed"),
    }
}
