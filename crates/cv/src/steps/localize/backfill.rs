//! The `localize` step's folder-scope backfill (plan `usability-sprint-captures-links` WP-2).

use cv_text::attachments::has_remote_image_refs;
use serde::Serialize;

use super::super::{StepCtx, StepError, StepOutcome};
use super::execute_note;
use super::params::Params;
use super::relative;

/// The `ignored_ids` kind recording notes a folder-scope backfill attempted and could not fully
/// localize (audit M2 progress invariant: never re-selected, so repeated runs reach new notes).
pub const BACKFILL_KIND: &str = "localize_backfill";

/// The `ignored_ids` kind recording that a [`BACKFILL_KIND`]-excluded note has had its one
/// escaped-paren retry (plan round-9-feedback-todos WP-2, audit M1). Every exclusion made from
/// this round on records it too, so only exclusions older than the escaped-paren matcher retry.
pub const RETRY_KIND: &str = "localize_backfill_retry";

/// Whether `text` has a remote markdown image whose URL carries a backslash-escaped parenthesis
/// (`![](https://h/a\(1\).png)`): the URL shape the reference regexes truncated before WP-1.
pub(crate) fn has_escaped_paren_image(text: &str) -> bool {
    let mut rest = text;
    while let Some(i) = rest.find("](http") {
        let line_start = rest[..i].rfind('\n').map_or(0, |n| n + 1);
        let is_image = rest[line_start..i].contains("![");
        let url = &rest[i + 2..];
        let mut it = url.chars();
        while let Some(c) = it.next() {
            match c {
                '\\' => {
                    if matches!(it.next(), Some('(' | ')')) && is_image {
                        return true;
                    }
                }
                ')' | '\n' | ' ' | '\t' => break,
                _ => {}
            }
        }
        rest = &rest[i + 2..];
    }
    false
}

/// The `ignored_ids` kind recording notes the source-relative backfill (`relative_sources = true`)
/// has attempted (plan `round-15-relative-images-pdf-viewer` WP-1): each note is attempted once,
/// so a re-run changes nothing and the batches reach new notes.
pub const RELATIVE_KIND: &str = "localize_relative_backfill";

/// The source-relative backfill (`relative_sources = true`): candidates are the indexed `.md`
/// notes under `folder` (empty = the whole vault, `.trash/` excluded) that carry a frontmatter
/// `source` and a page-relative image ref ([`relative::has_relative_candidates`]) and are not
/// recorded under [`RELATIVE_KIND`]. Selection does not depend on remote refs. A throttled or
/// all-unreachable note is not recorded and ends the batch (a later run retries it); a CAS
/// conflict is not recorded; every other attempt is, success or not.
fn execute_relative(
    p: &Params,
    folder: &str,
    ctx: &mut StepCtx<'_>,
) -> Result<StepOutcome, StepError> {
    let state_err = |e: cv_state::StateError| StepError::failed(format!("localize: state: {e}"));
    let prefix = if folder.is_empty() {
        String::new()
    } else {
        format!("{folder}/")
    };
    let paths = {
        let reader = ctx.index()?;
        reader
            .query()
            .md_paths_with_prop_key("source")
            .map_err(|e| StepError::failed(format!("localize: index: {e}")))?
    };
    let mut scanned = 0_u64;
    let mut candidates = Vec::new();
    for f in paths {
        if f.starts_with(".trash/") || !f.starts_with(&prefix) || !f.ends_with(".md") {
            continue;
        }
        scanned += 1;
        if ctx
            .state()
            .is_ignored(RELATIVE_KIND, &f)
            .map_err(state_err)?
        {
            continue;
        }
        let Some((bytes, _)) = ctx.read(&f)? else {
            continue;
        };
        if std::str::from_utf8(&bytes).is_ok_and(relative::has_relative_candidates) {
            candidates.push(f);
        }
    }
    let (mut localized, mut failed, mut attempted, mut throttled) = (0_u64, 0_u64, 0_u64, 0_u64);
    let mut out = StepOutcome::default();
    for f in &candidates {
        if attempted >= p.limit {
            break;
        }
        attempted += 1;
        let reason = match execute_note(p, f, ctx) {
            Ok((v, paths)) => {
                let c = v.counts.unwrap_or_default();
                for op in paths {
                    out.push_output(op);
                }
                if c.throttled > 0 {
                    attempted -= 1;
                    throttled += 1;
                    break;
                }
                if v.all_unreachable {
                    attempted -= 1;
                    break;
                }
                (c.failed > 0).then(|| format!("{} download(s) failed", c.failed))
            }
            Err(StepError::Deferred {
                cause: super::super::DeferCause::Wait,
                ..
            }) => {
                attempted -= 1;
                continue;
            }
            Err(e @ StepError::Deferred { .. }) => return Err(e),
            Err(e) => Some(e.to_string()),
        };
        match &reason {
            None => localized += 1,
            Some(_) => failed += 1,
        }
        let r: String = reason
            .unwrap_or_else(|| "attempted".to_owned())
            .chars()
            .take(200)
            .collect();
        ctx.state()
            .insert_ignored_id(RELATIVE_KIND, f, &r, crate::now_ms())
            .map_err(state_err)?;
    }
    let unattempted_remaining = candidates.len() as u64 - attempted;
    tracing::info!(
        folder,
        scanned,
        localized,
        failed,
        throttled,
        unattempted_remaining,
        "localize relative-source backfill"
    );
    out.set_vars(&FolderVars {
        scanned,
        localized,
        failed,
        throttled,
        ineligible: 0,
        unattempted_remaining,
    });
    Ok(out)
}

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
    if p.relative_sources {
        return execute_relative(p, folder, ctx);
    }
    let files = super::super::util::md_files_under(ctx, folder);
    let scanned = files.len() as u64;
    let mut candidates = Vec::new();
    let state_err = |e: cv_state::StateError| StepError::failed(format!("localize: state: {e}"));
    for f in files {
        let excluded = ctx
            .state()
            .is_ignored(BACKFILL_KIND, &f)
            .map_err(state_err)?;
        if excluded && ctx.state().is_ignored(RETRY_KIND, &f).map_err(state_err)? {
            continue;
        }
        let Some((bytes, _)) = ctx.read(&f)? else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !has_remote_image_refs(text) {
            continue;
        }
        // Plan round-9-feedback-todos WP-2 (audit M1): a note excluded before escaped-paren image
        // URLs were matched gets one retry; when it is excluded again, [`RETRY_KIND`] records the
        // spent retry so it never loops. Every other excluded note stays skipped.
        if excluded && !has_escaped_paren_image(text) {
            continue;
        }
        candidates.push(f);
    }
    // Records a settled or failed note: excluded, and its escaped-paren retry spent (an exclusion
    // made with the WP-1 matcher has already seen the unescaped URL).
    let exclude = |ctx: &StepCtx<'_>, f: &str, r: &str| -> Result<(), StepError> {
        let mut db = ctx.state();
        db.insert_ignored_id(BACKFILL_KIND, f, r, crate::now_ms())
            .map_err(state_err)?;
        db.insert_ignored_id(RETRY_KIND, f, "retry spent", crate::now_ms())
            .map_err(state_err)?;
        Ok(())
    };
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
                if left && v.all_unreachable {
                    // The network is down: not recorded, so a later run retries the note;
                    // ends the batch like the former network deferral.
                    attempted -= 1;
                    break;
                }
                if left && hit_limit {
                    attempted -= 1;
                    throttled += 1;
                    break;
                }
                if left && failed_refs == 0 {
                    ineligible += 1;
                    exclude(
                        ctx,
                        f,
                        "settled: remaining refs are not eligible attachments",
                    )?;
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
                exclude(ctx, f, &r)?;
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
