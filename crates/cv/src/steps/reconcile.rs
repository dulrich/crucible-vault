//! `intake_reconcile` (plan `auto-intake-sprint` WP-6): the one-shot fix-up of existing `note`
//! tracker rows, run by `cv intake reconcile [--dry-run]` through the `intake-reconcile` action.
//!
//! 1. **Link** every `note` row whose `note_path` is NULL: the row URL resolves against the
//!    supersede index ([`SupersedeIndex::identity_matches`], canonical `source` / `post-id`).
//!    Exactly one note links (`set_tracker_note_path`); none is `unresolved`; several is
//!    `ambiguous` and the row stays NULL.
//! 2. **Probe** the open blog rows that were never probed (`access IS NULL`, a `/p/<slug>` URL)
//!    through the WP-2 Substack probe (`substack-api`, one bounded batch, the service's own
//!    breaker/quota). A quota stop or rate limit counts `probe_skipped`, any other failure
//!    `probe_failed`; either leaves the row open, untouched.
//! 3. **Auto-done** each open row with a resolved note: the completeness predicate
//!    ([`cv_ingest::access::completeness`]) is applied to the *resolved body* (its own word
//!    count, the row's probed `access`/`full_word_count`, the `[[blog]]` row's paywall markers),
//!    the evidence is written first (`set_tracker_completeness`), and the row is ticked only on
//!    `Some(true)`. A link or a match alone never ticks.
//!
//! Plan `intake-followups-mail-youtube` WP-5 adds three backfill phases (detail and vars in
//! [`super::reconcile_backfill`]), run after phase 3:
//!
//! 4. **YouTube finalize** (A): open `youtube` rows get the WP-4 finalize core (transcript
//!    clippings of that video tick; bare embeds never do) — replaces their `skipped_kind`.
//! 5. **Mail re-match** (B): `unknown-sender` mail rows re-resolve against the newsletter config
//!    and are re-keyed (collision-safe), retagged and ticked by predicate; never a new note.
//! 6. **Re-clean** (C): `via: email` note bodies go through `clean_body`, CAS-written only on a
//!    change.
//!
//! Plan `intake-round-2-clean-dedupe-reading` WP-6 adds the repair phases (detail and vars in
//! [`super::reconcile_repair`]), after the re-clean (which now drops small localized images by
//! decoded dimensions, `mail_recleaned_r2`):
//!
//! 7. **Re-key** (D): `via: email` notes recompute `source`/`post-id` from the processed `.eml`
//!    (`mail_rekeyed`, `mail_token_stripped`, `rekey_skipped`, `rekey_conflict`).
//! 8. **Merge** (E): a mail note whose canonical identity matches a `via: rss` note (only RSS-origin
//!    twins) is resolved by the supersede engine under the `auto_file` containment rule; rows are
//!    re-pointed to the survivor before the redundant note moves to `.trash/` (`dup_merged`,
//!    `dup_marked`, `dup_other`, `dup_distinct`, `dup_conflict`).
//! 9. **Publication** (F): notes linked from tracker rows get their source row's `publication`
//!    where absent (`publication_backfilled`, `publication_no_row`, `publication_conflict`).
//!
//! Plan `round-9-feedback-todos` WP-2 adds three identity phases (detail and vars in
//! [`super::reconcile_identity`]), and widens E (a note with a `blog` tracker row and no
//! `via: email` is RSS-origin) and C (description repair, see [`super::reconcile_backfill`]):
//!
//! 10. **Forum re-key** (G): a stored page-level `post-id` of a forum post `source` becomes the
//!     `/posts/<P>` key (`forum_rekeyed`).
//! 11. **Spurious duplicate-of** (H): removed only for a forum pair that shared the obsolete page
//!     key and has neither identity nor content evidence left (`dup_of_removed`, `dup_of_kept`).
//! 12. **Fanfic publication** (I): the matched Work's `title`, never overwriting
//!     (`fanfic_publication_backfilled`).
//!
//! The step now writes notes (tags, `complete`, cleaned bodies), so `IntakeReconcile` is
//! note-mutating (`mutates_note`, audit M2).
//!
//! `dry_run` (step param, or a job param `dry_run = true`) computes every count of every phase and
//! writes nothing to `state.db` or the vault (the probe is a read-only fetch). The step holds
//! `intake::identity` (step-intrinsic). Outcome vars are aggregate counts only: `null_paths`,
//! `linked`, `unresolved`, `ambiguous`, `probed`, `probe_failed`, `probe_skipped`, `ticked`,
//! `left_open`, `skipped_kind` (open rows of kinds other than blog and youtube: linked in phase 1
//! but never probed or ticked; mail unknown-sender rows count here before phase 5 re-matches
//! them), the backfill vars (`yt_*`, `x_ticked`, `mail_*`), the repair vars (`mail_rekeyed`, `mail_token_stripped`,
//! `rekey_*`, `dup_*`, `publication_*`), `dry_run`.

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access::{self, Access};
use cv_note::{FrontmatterDoc, JsValue, calculate_word_count};
use cv_providers::FailureKind;
use cv_providers::web::FetchError;
use cv_state::TrackerItem;
use serde::Serialize;
use serde_json::Value;

use super::params::{check_keys, get_bool};
use super::reconcile_backfill as backfill;
use super::reconcile_identity as identity;
use super::reconcile_repair as repair;
use super::tracker::blogs::PROBE_SERVICE;
use super::tracker::substack;
use super::util::fetch_bounded;
use super::web::client;
use super::{StepCtx, StepError, StepModule, StepOutcome};
use crate::supersede::SupersedeIndex;

/// Most probes one run makes; the rest count as `probe_skipped` (the next run picks them up).
const PROBE_LIMIT: usize = 200;

/// The `intake_reconcile` step.
pub(crate) struct IntakeReconcile;

impl StepModule for IntakeReconcile {
    type Params = bool;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<bool, String> {
        check_keys(t, kind, "intake_reconcile")?;
        Ok(get_bool(t, "dry_run")?.unwrap_or(false))
    }

    fn run(dry_run: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(dry_run || job_dry_run(ctx), ctx)
    }
}

/// A job param `dry_run` (`true` or `"true"`) turns the dry run on.
fn job_dry_run(ctx: &StepCtx<'_>) -> bool {
    match ctx.params.get("dry_run") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// `intake_reconcile` outcome vars (aggregates only).
#[derive(Default, Serialize)]
struct Vars {
    null_paths: u32,
    linked: u32,
    unresolved: u32,
    ambiguous: u32,
    probed: u32,
    probe_failed: u32,
    probe_skipped: u32,
    ticked: u32,
    left_open: u32,
    skipped_kind: u32,
    dry_run: bool,
    #[serde(flatten)]
    backfill: backfill::Counts,
    #[serde(flatten)]
    repair: repair::Counts,
    #[serde(flatten)]
    identity: identity::Counts,
}

/// How a probe attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeOutcome {
    /// A readable `audience` (and maybe the full word count).
    Probed(Access, Option<u32>),
    /// The probe failed (transport, HTTP error, unreadable body).
    Failed,
    /// The service's quota / rate limit stopped it.
    Skipped,
}

/// Classifies one probe fetch.
pub(super) fn classify_probe(r: &Result<String, FetchError>) -> ProbeOutcome {
    match r {
        Ok(body) => match substack::parse_post_meta(body) {
            Some((aud, wc)) => ProbeOutcome::Probed(Access::from_substack_audience(&aud), wc),
            None => ProbeOutcome::Failed,
        },
        Err(FetchError::Quota { .. }) => ProbeOutcome::Skipped,
        Err(FetchError::Transient {
            kind: FailureKind::RateLimited,
            ..
        }) => ProbeOutcome::Skipped,
        Err(_) => ProbeOutcome::Failed,
    }
}

/// The completeness verdict of a resolved body: `Some(true)` is the only tick condition.
pub(super) fn body_verdict(
    access: Option<Access>,
    body: &str,
    full_words: Option<u32>,
    markers: &[String],
    complete_percent: u32,
) -> Option<bool> {
    let delivered = u32::try_from(calculate_word_count(body)).unwrap_or(u32::MAX);
    access::completeness(
        access,
        delivered,
        full_words,
        !body.trim().is_empty(),
        access::marker_hit(body, markers),
        complete_percent,
    )
}

/// One `note` row while reconcile works on it.
struct Work {
    item: TrackerItem,
    open: bool,
    path: Option<String>,
    access: Option<String>,
    full_wc: Option<i64>,
    /// The probe failed or was skipped: the row is left open, untouched.
    blocked: bool,
}

fn st(e: cv_state::StateError) -> StepError {
    StepError::failed(format!("state: {e}"))
}

fn run(dry: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut v = Vars {
        dry_run: dry,
        ..Vars::default()
    };
    let rows = ctx.state().tracker_reconcile_rows().map_err(st)?;
    let mut work: Vec<Work> = rows
        .into_iter()
        .map(|(item, status)| Work {
            open: !matches!(status, Some('x' | 'X')),
            path: item.note_path.clone(),
            access: item.access.clone(),
            full_wc: item.full_word_count,
            item,
            blocked: false,
        })
        .collect();

    // 1. Link NULL-path rows.
    let mut sidx: Option<SupersedeIndex> = None;
    if work.iter().any(|w| w.path.is_none()) {
        let idx = backfill::index(&mut sidx, ctx)?;
        for w in work.iter_mut().filter(|w| w.path.is_none()) {
            v.null_paths += 1;
            let m = idx.identity_matches(std::slice::from_ref(&w.item.url), None);
            match m.as_slice() {
                [] => v.unresolved += 1,
                [p] => {
                    v.linked += 1;
                    if !dry {
                        ctx.state()
                            .set_tracker_note_path(
                                &w.item.source_kind,
                                &w.item.source_key,
                                &w.item.item_id,
                                p,
                            )
                            .map_err(st)?;
                    }
                    w.path = Some(p.clone());
                }
                _ => v.ambiguous += 1,
            }
        }
    }

    // 2. Probe open, never-probed blog rows.
    probe(ctx, &mut work, dry, &mut v)?;

    // 3. Auto-done from the resolved body.
    let cfg = ctx.config.pipelines().blogs.clone();
    for w in work.iter_mut().filter(|w| w.open) {
        if w.item.source_kind != "blog" {
            // Open youtube rows are finalized by phase 4 instead.
            if w.item.source_kind != "youtube" {
                v.skipped_kind += 1;
            }
            continue;
        }
        let (Some(path), false) = (w.path.clone(), w.blocked) else {
            v.left_open += 1;
            continue;
        };
        let Some((bytes, _)) = ctx.read(&path)? else {
            v.left_open += 1;
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            v.left_open += 1;
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        let note_access = match doc.get("access") {
            Ok(Some(JsValue::Str(s))) => Some(s),
            _ => None,
        };
        let access_s = w.access.clone().or(note_access);
        let markers: Vec<String> = ctx
            .config
            .sources()
            .blog
            .iter()
            .filter(|b| b.link.trim() == w.item.source_key)
            .flat_map(|b| b.paywall_markers.clone())
            .collect();
        let verdict = body_verdict(
            access_s.as_deref().and_then(Access::parse),
            doc.body(),
            w.full_wc.and_then(|n| u32::try_from(n).ok()),
            &markers,
            cfg.complete_percent,
        );
        let complete = verdict.or(w.item.complete);
        if !dry {
            // Evidence first, tick second (never tick on a match alone).
            let mut db = ctx.state();
            db.set_tracker_completeness(
                &w.item.source_kind,
                &w.item.source_key,
                &w.item.item_id,
                access_s.as_deref(),
                complete,
                w.full_wc,
            )
            .map_err(st)?;
            if verdict == Some(true) {
                db.set_tracker_item_status(
                    &w.item.source_kind,
                    &w.item.source_key,
                    &w.item.item_id,
                    Some('x'),
                )
                .map_err(st)?;
            }
        }
        if verdict == Some(true) {
            v.ticked += 1;
        } else {
            v.left_open += 1;
        }
    }

    // 4-6. Backfill phases (plan intake-followups-mail-youtube WP-5).
    let mut sh = backfill::Shared {
        dry,
        pre_linked: std::collections::BTreeSet::new(),
        paths: std::collections::HashMap::new(),
    };
    for w in &work {
        if w.item.note_path.is_none()
            && let Some(p) = &w.path
        {
            let k = (
                w.item.source_kind.clone(),
                w.item.source_key.clone(),
                w.item.item_id.clone(),
            );
            sh.pre_linked.insert(k.clone());
            sh.paths.insert(k, p.clone());
        }
    }
    backfill::youtube(ctx, &sh, &mut sidx, &mut v.backfill)?;
    backfill::mail(ctx, &sh, &mut v.backfill)?;
    backfill::reclean(ctx, &sh, &mut v.backfill)?;

    // 7-9. Round-2 repair (plan intake-round-2-clean-dedupe-reading WP-6): re-key, merge, publication.
    repair::rekey_and_merge(ctx, &sh, &mut sidx, &mut v.repair)?;
    repair::publication(ctx, &sh, &mut v.repair)?;

    // 10-12. Round-9 repair (plan round-9-feedback-todos WP-2): forum post-id re-key, spurious
    // forum `duplicate-of`, fanfic publication.
    identity::forum_rekey(ctx, &sh, &mut v.identity)?;
    identity::forum_duplicates(ctx, &sh, &mut v.identity)?;
    identity::fanfic_publication(ctx, &sh, &mut v.identity)?;

    let mut out = StepOutcome::default();
    out.set_vars(&v);
    Ok(out)
}

/// Phase 2: probes the open blog rows with no `access` and a probe URL; on success records the
/// row's `access` / `full_word_count` (unless dry). Failed / skipped rows are `blocked`.
fn probe(ctx: &StepCtx<'_>, work: &mut [Work], dry: bool, v: &mut Vars) -> Result<(), StepError> {
    let mut targets: Vec<(usize, String)> = Vec::new();
    for (i, w) in work.iter_mut().enumerate() {
        if !w.open || w.item.source_kind != "blog" || w.access.is_some() {
            continue;
        }
        let Some(u) = substack::probe_url(&w.item.url) else {
            continue;
        };
        if targets.len() >= PROBE_LIMIT {
            v.probe_skipped += 1;
            w.blocked = true;
        } else {
            targets.push((i, u));
        }
    }
    if targets.is_empty() {
        return Ok(());
    }
    let c = client()?;
    let bodies = fetch_bounded(
        targets.iter().map(|(_, u)| u.clone()).collect(),
        |u| async move { c.get_text(PROBE_SERVICE, &u).await },
    )?;
    for ((i, u), body) in targets.into_iter().zip(bodies) {
        match classify_probe(&body) {
            ProbeOutcome::Probed(a, wc) => {
                v.probed += 1;
                let w = &mut work[i];
                w.access = Some(a.as_str().to_owned());
                w.full_wc = wc.map(i64::from);
                if !dry {
                    ctx.state()
                        .set_tracker_completeness(
                            &w.item.source_kind,
                            &w.item.source_key,
                            &w.item.item_id,
                            w.access.as_deref(),
                            w.item.complete,
                            w.full_wc,
                        )
                        .map_err(st)?;
                }
            }
            ProbeOutcome::Failed => {
                tracing::warn!(url = %u, "intake_reconcile: post probe failed");
                v.probe_failed += 1;
                work[i].blocked = true;
            }
            ProbeOutcome::Skipped => {
                v.probe_skipped += 1;
                work[i].blocked = true;
            }
        }
    }
    Ok(())
}
