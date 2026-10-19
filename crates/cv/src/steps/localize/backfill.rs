//! The `localize` step's folder-scope backfill (plan `usability-sprint-captures-links` WP-2).

use cv_text::attachments::has_remote_image_refs;
use serde::Serialize;

use super::super::{StepCtx, StepError, StepOutcome};
use super::execute_note;
use super::params::Params;

/// The `ignored_ids` kind recording notes a folder-scope backfill attempted and could not fully
/// localize (audit M2 progress invariant: never re-selected, so repeated runs reach new notes).
pub const BACKFILL_KIND: &str = "localize_backfill";

/// Outcome vars of a folder-scope `localize` backfill.
#[derive(Serialize)]
struct FolderVars {
    scanned: u64,
    localized: u64,
    failed: u64,
    throttled: u64,
    ineligible: u64,
    unattempted_remaining: u64,
}

/// Folder-scope backfill: candidates are `.md` under `folder` (sorted) with remote/data image
/// refs that are not recorded under [`BACKFILL_KIND`]; at most `limit` are attempted. A note
/// whose localize errs, or that still has such refs afterwards, is recorded (never re-selected).
/// A note whose downloads were throttled (429/5xx) is neither recorded nor counted as attempted,
/// and ends the batch: the host is rate-limiting, so a later run retries it (`throttled = 1`).
/// A note left only with refs that are not eligible attachments (embed pages excluded up front;
/// e.g. a URL serving HTML) is settled — recorded so it is not re-selected, counted `ineligible`,
/// not `failed`.
pub(super) fn execute_folder(
    p: &Params,
    folder: &str,
    ctx: &mut StepCtx<'_>,
) -> Result<StepOutcome, StepError> {
    let files = super::super::util::md_files_under(ctx, folder);
    let scanned = files.len() as u64;
    let mut candidates = Vec::new();
    for f in files {
        if ctx
            .state()
            .is_ignored(BACKFILL_KIND, &f)
            .map_err(|e| StepError::failed(format!("localize: state: {e}")))?
        {
            continue;
        }
        let Some((bytes, _)) = ctx.read(&f)? else {
            continue;
        };
        if std::str::from_utf8(&bytes).is_ok_and(has_remote_image_refs) {
            candidates.push(f);
        }
    }
    let (mut localized, mut failed, mut attempted, mut throttled) = (0_u64, 0_u64, 0_u64, 0_u64);
    let mut ineligible = 0_u64;
    let mut out = StepOutcome::default();
    for f in &candidates {
        if attempted >= p.limit {
            break;
        }
        attempted += 1;
        let reason = match execute_note(p, f, ctx) {
            Ok((v, paths)) => {
                let c = v.counts.unwrap_or_default();
                let hit_limit = c.throttled > 0;
                let failed_refs = c.failed;
                for op in paths {
                    out.push_output(op);
                }
                let left = ctx
                    .read(f)?
                    .is_some_and(|(b, _)| std::str::from_utf8(&b).is_ok_and(has_remote_image_refs));
                if left && hit_limit {
                    attempted -= 1;
                    throttled += 1;
                    break;
                }
                if left && failed_refs == 0 {
                    ineligible += 1;
                    ctx.state()
                        .insert_ignored_id(
                            BACKFILL_KIND,
                            f,
                            "settled: remaining refs are not eligible attachments",
                            crate::now_ms(),
                        )
                        .map_err(|e| StepError::failed(format!("localize: state: {e}")))?;
                    continue;
                }
                left.then(|| format!("{failed_refs} download(s) failed; refs remain"))
            }
            Err(StepError::Deferred {
                cause: super::super::DeferCause::Wait,
                ..
            }) => {
                // A CAS conflict (SR7): never recorded as ignored — the note stays eligible
                // and a later run retries it.
                attempted -= 1;
                continue;
            }
            Err(e @ StepError::Deferred { .. }) => return Err(e),
            Err(e) => Some(e.to_string()),
        };
        match reason {
            None => localized += 1,
            Some(r) => {
                failed += 1;
                let r: String = r.chars().take(200).collect();
                ctx.state()
                    .insert_ignored_id(BACKFILL_KIND, f, &r, crate::now_ms())
                    .map_err(|e| StepError::failed(format!("localize: state: {e}")))?;
            }
        }
    }
    let unattempted_remaining = candidates.len() as u64 - attempted;
    tracing::info!(
        folder,
        scanned,
        localized,
        failed,
        ineligible,
        throttled,
        unattempted_remaining,
        "localize backfill"
    );
    out.set_vars(&FolderVars {
        scanned,
        localized,
        failed,
        throttled,
        ineligible,
        unattempted_remaining,
    });
    Ok(out)
}
