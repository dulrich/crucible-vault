//! `intake_reconcile` (plan `auto-intake-sprint` WP-6): the fix-up of existing `note` tracker
//! rows and intake notes, run by `cv intake reconcile [--dry-run]` through the `intake-reconcile`
//! action. It is note-mutating (`mutates_note`, audit M2).
//!
//! Phases run in this order. Phases 1–3 work on the tracker rows in [`run`]; the rest form the
//! [`PHASES`] table and share one [`Pass`] (the dry flag, the rows phase 1 linked and the lazily
//! built supersede index). Vars per phase:
//!
//! 1. **Link** NULL-path rows by row URL against the supersede index — exactly one note links,
//!    none is unresolved, several stay NULL (`null_paths`, `linked`, `unresolved`, `ambiguous`).
//! 2. **Probe** open never-probed Substack blog rows, one bounded batch (`probed`,
//!    `probe_failed`, `probe_skipped`; a failed or skipped row stays open, untouched).
//! 3. **Tick** open blog rows with a resolved note by the completeness predicate on the resolved
//!    body, evidence and tick in one transaction; a match alone never ticks (`ticked`,
//!    `left_open`, `skipped_kind` for open rows of kinds other than blog and youtube).
//! 4. **YouTube finalize** (A, [`super::reconcile_backfill`]): `yt_ticked`, `yt_linked`,
//!    `yt_no_note`, `yt_ambiguous`, `yt_no_evidence`, `x_ticked`.
//! 5. **Mail re-match** (B, [`super::reconcile_backfill`]): `mail_rematched`,
//!    `mail_collisions`, `mail_missing`, `mail_unmatched`, `mail_ticked`, `mail_retagged`,
//!    `mail_tag_conflict`, `mail_folder_mismatch`, `mail_marker_incomplete`.
//! 6. **Re-clean** (C, [`super::reconcile_backfill`]): `mail_recleaned_r2`,
//!    `mail_clean_unchanged`, `mail_clean_conflict`, `mail_description_repaired`.
//! 7. **Boilerplate** ([`super::reconcile_boilerplate`]): `bp_learned`, `bp_stripped`,
//!    `bp_guarded`, `bp_conflict`.
//! 8. **Mail↔RSS merge** (E, [`super::reconcile_repair`]): `dup_merged`, `dup_marked`,
//!    `dup_other`, `dup_distinct`, `dup_conflict`.
//! 9. **Publication** (F, [`super::reconcile_repair`]): `publication_backfilled`,
//!    `publication_no_row`, `publication_conflict`.
//!
//! `dry_run` (step param; a job param `dry_run`, bool or `"true"`/`"false"`, wins over it) computes every count of every phase and
//! writes nothing to `state.db` or the vault (the probe is a read-only fetch); it is echoed as the
//! `dry_run` var. Outcome vars are aggregate counts only. The step holds `intake::identity`
//! (step-intrinsic).

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access::{self, Access};
use cv_note::{FrontmatterDoc, calculate_word_count};
use cv_providers::FailureKind;
use cv_providers::web::FetchError;
use cv_state::TrackerItem;
use serde::Serialize;

use super::params::{check_keys, get_bool};
use super::reconcile_backfill as backfill;
use super::reconcile_boilerplate as boilerplate;
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
        run(super::params::job_dry_run(ctx, dry_run), ctx)
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
    boilerplate: boilerplate::Counts,
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

/// The context phases 4–9 share.
struct Pass {
    /// The dry flag and the rows phase 1 linked.
    sh: backfill::Shared,
    /// The supersede index, built on first use (phase 1 may already have built it).
    sidx: Option<SupersedeIndex>,
}

/// One table phase: reads and writes through the step context, counts into the aggregate vars.
type Phase = fn(&mut StepCtx<'_>, &mut Pass, &mut Vars) -> Result<(), StepError>;

/// Phases 4–9, in run order (see the module doc for their vars).
const PHASES: &[(&str, Phase)] = &[
    ("youtube", |ctx, p, v| {
        backfill::youtube(ctx, &p.sh, &mut p.sidx, &mut v.backfill)
    }),
    ("mail", |ctx, p, v| {
        backfill::mail(ctx, &p.sh, &mut v.backfill)
    }),
    ("reclean", |ctx, p, v| {
        backfill::reclean(ctx, &p.sh, &mut v.backfill)
    }),
    // After the re-clean so it strips cleaned bodies.
    ("boilerplate", |ctx, p, v| {
        boilerplate::run(ctx, &p.sh, &mut v.boilerplate)
    }),
    ("merge", |ctx, p, v| {
        repair::merge_mail_twins(ctx, &p.sh, &mut p.sidx, &mut v.repair)
    }),
    ("publication", |ctx, p, v| {
        repair::publication(ctx, &p.sh, &mut v.repair)
    }),
];

fn run(dry: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut v = Vars {
        dry_run: dry,
        ..Vars::default()
    };
    let rows = ctx.state().tracker_reconcile_rows()?;
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
                        ctx.state().set_tracker_note_path(
                            &w.item.source_kind,
                            &w.item.source_key,
                            &w.item.item_id,
                            p,
                        )?;
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
        let note_access = doc.get_str("access");
        let access_s = w.access.clone().or(note_access);
        let markers: Vec<String> = ctx
            .config
            .sources()
            .by_feed(&w.item.source_key)
            .map(|b| b.paywall_markers.clone())
            .unwrap_or_default();
        let verdict = body_verdict(
            access_s.as_deref().and_then(Access::parse),
            doc.body(),
            w.full_wc.and_then(|n| u32::try_from(n).ok()),
            &markers,
            cfg.complete_percent,
        );
        let complete = verdict.or(w.item.complete);
        if !dry {
            // Evidence and tick in one transaction (never tick on a match alone).
            let ev = cv_state::TrackerEvidence {
                access: access_s.as_deref(),
                complete,
                full_word_count: w.full_wc,
            };
            ctx.state().finalize_tracker_item(
                &w.item.source_kind,
                &w.item.source_key,
                &w.item.item_id,
                Some(ev),
                verdict == Some(true),
            )?;
        }
        if verdict == Some(true) {
            v.ticked += 1;
        } else {
            v.left_open += 1;
        }
    }

    // 4-9. The phase table, sharing one pass context.
    let mut pass = Pass {
        sh: backfill::Shared {
            dry,
            pre_linked: std::collections::BTreeSet::new(),
            paths: std::collections::HashMap::new(),
        },
        sidx,
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
            pass.sh.pre_linked.insert(k.clone());
            pass.sh.paths.insert(k, p.clone());
        }
    }
    for (name, phase) in PHASES {
        tracing::debug!(phase = name, "intake_reconcile: phase");
        phase(ctx, &mut pass, &mut v)?;
    }

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
                    let ev = cv_state::TrackerEvidence {
                        access: w.access.as_deref(),
                        complete: w.item.complete,
                        full_word_count: w.full_wc,
                    };
                    ctx.state().finalize_tracker_item(
                        &w.item.source_kind,
                        &w.item.source_key,
                        &w.item.item_id,
                        Some(ev),
                        false,
                    )?;
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
