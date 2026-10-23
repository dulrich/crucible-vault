//! Step module `localize` (plan `v0-core-daemon` WP-11): localize a note's attachments and repair
//! its broken attachment refs.
//!
//! Ported from Crucible `src/localizeAttachments.ts` (`AttachmentLocalizer.localizeNote`,
//! `repairNote`, `processRemote`, `processLocal`, `writeAttachment`, `downloadRemote`,
//! `convertImage`) and `src/attachmentRepair.ts` (the pure ref/repair domain, ported function by
//! function below with the same names in snake case). Behaviours kept, with their Crucible
//! source:
//! - attachments are named by a deterministic MD5 of the *final, post-conversion* bytes through
//!   the `{{md5}}_MD5.{{ext}}` name template (`src/AGENTS.md:23,49`); an existing file with that
//!   name is reused, never rewritten (Crucible rewrote it with identical bytes via
//!   `modifyBinary` — a no-op write, dropped);
//! - the web clipper's `data:image` lazy placeholders are stripped (`src/AGENTS.md:24,51`);
//! - rewritten markdown embeds carry an empty alt (`src/AGENTS.md:25,53`);
//! - one `_MD5`-gated ref-rewrite chokepoint
//!   ([`cv_text::attachments::rewrite_localized_attachment_refs`]); repair
//!   counts only replacements that landed, and only when the note write itself landed (CAS
//!   success) (`src/AGENTS.md:26`).
//!
//! Dropped (Obsidian-induced): the re-read-before-write and the spanning `withMaterializing`
//! flag (`src/AGENTS.md:22,47`). Crucible needed both because `vault.modify` has no
//! compare-and-swap and its own debounced auto-lint/auto-localize handlers had to be suppressed
//! for the whole download window. Here the note is read once with its hash, every download and
//! conversion happens, and the single note write is a CAS against that hash (I2): a concurrent
//! edit yields a conflict and the step fails, never a clobber; the runner's cycle guard (I5)
//! replaces the materializing flag. Also dropped: the metadata cache as the ref source
//! (`parseAttachmentRefsFromCache`) — refs are parsed from the note text
//! ([`cv_text::attachments::parse_attachment_refs`],
//! fenced code skipped as the cache does) and resolved through the index's file list with
//! `cv_index::Resolver` (`getFirstLinkpathDest`'s replacement); notices/spinners/debug-log.
//!
//! Layout: the pure ref/repair/template domain is
//! `cv_text::attachments`; here are the step ([`params`], the I/O processors in `note`, the
//! folder-scope `backfill`) and the one note pass ([`execute_note`]).

mod backfill;
mod note;
mod params;
#[cfg(test)]
mod tests;

use std::collections::HashSet;

use cv_actions::{ConfigSet, StepKind};
use cv_providers::web::FetchError;
use cv_text::attachments::{
    AttachmentMatch, AttachmentPathIndex, Media, Replacement, count_managed_images, format_ref,
    parse_attachment_refs, resolve_attachment_folder, resolve_local_attachment_repair,
    rewrite_localized_attachment_refs, strip_data_uri_image_placeholders,
};
use cv_vault::VaultError;
use serde::Serialize;

use super::{StepCtx, StepError, StepModule, StepOutcome};
#[cfg(test)]
pub(crate) use backfill::has_escaped_paren_image;
pub use backfill::{BACKFILL_KIND, RETRY_KIND};
#[cfg(test)]
pub(crate) use note::SERVICE;
pub use note::convert_image;
use note::{Counts, Run, VaultView};
pub use params::ImageFormat;
pub(crate) use params::Params;
use params::parse_params;

/// The `localize` step (params all optional; an empty table is valid).
pub(crate) struct Localize;

impl StepModule for Localize {
    type Params = Params;

    fn parse(_: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        parse_params(t)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(&p, ctx)
    }
}

fn ctx_note_path(p: &Params, ctx: &StepCtx<'_>) -> Result<String, StepError> {
    let from_params = ctx
        .params
        .get("path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    p.path
        .clone()
        .or(from_params)
        .or_else(|| ctx.context.active_path.clone())
        .ok_or_else(|| StepError::failed("localize: no target note (path or active_path)"))
}

/// Crucible `repairNote`'s count: `repaired = resolved − (planned − applied)`, and zero unless
/// the note write landed (planned-not-landed replacements fold into `unrepairable`).
fn repair_counts(
    resolved: u64,
    unrepairable: u64,
    planned: u64,
    applied: u64,
    landed: bool,
) -> (u64, u64) {
    if !landed {
        return (0, unrepairable + resolved);
    }
    let missed = planned.saturating_sub(applied);
    (resolved.saturating_sub(missed), unrepairable + missed)
}

/// Executes one `localize` step (mode `localize` or `repair`) on the target note, or — with
/// `scope = "folder"` — on up to `limit` candidate notes under `folder`.
pub(crate) fn run(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    if let Some(folder) = p.folder_scope.clone() {
        return backfill::execute_folder(p, &folder, ctx);
    }
    let path = ctx_note_path(p, ctx)?;
    let (vars, output_paths) = execute_note(p, &path, ctx)?;
    // A host that throttled (429/5xx) is a transient service failure: what
    // could be localized has landed; the step defers on the breaker key so the throttled refs
    // are retried later.
    if let Some(e) = &vars.throttle {
        return Err(StepError::from_fetch(e));
    }
    let mut out = StepOutcome {
        output_paths,
        ..StepOutcome::default()
    };
    out.set_vars(&vars);
    Ok(out)
}

/// Outcome vars of a note-scope `localize` (counts are absent on a non-`.md` target; the
/// repair counts only in `mode = "repair"`).
#[derive(Debug, Default, Serialize)]
pub(super) struct NoteVars {
    pub(super) changed: bool,
    pub(super) images: u64,
    #[serde(flatten)]
    pub(super) counts: Option<NoteCounts>,
    /// The first throttled download (not a var).
    #[serde(skip)]
    pub(super) throttle: Option<FetchError>,
    /// Every remote ref was unreachable (not a var).
    #[serde(skip)]
    pub(super) all_unreachable: bool,
}

/// The download/rewrite counts of a localized `.md` note.
#[derive(Debug, Default, Serialize)]
pub(super) struct NoteCounts {
    pub(super) downloaded: u64,
    pub(super) reused: u64,
    pub(super) stripped: u64,
    pub(super) failed: u64,
    pub(super) throttled: u64,
    pub(super) ineligible: u64,
    pub(super) localized: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) repaired: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) unrepairable: Option<u64>,
}

/// Localizes one note: its outcome vars and output paths.
pub(super) fn execute_note(
    p: &Params,
    path: &str,
    ctx: &mut StepCtx<'_>,
) -> Result<(NoteVars, Vec<String>), StepError> {
    let path = path.to_owned();
    let p = p.clone();
    if !path.ends_with(".md") {
        return Ok((NoteVars::default(), Vec::new()));
    }
    let Some((bytes, _)) = ctx.read(&path)? else {
        return Err(StepError::NoteGone { path });
    };
    let content = String::from_utf8(bytes)
        .map_err(|_| StepError::failed(format!("localize: {path} is not UTF-8")))?;
    let matches = parse_attachment_refs(&content);
    let placeholders = if !p.repair && p.enabled.contains(&Media::Images) {
        strip_data_uri_image_placeholders(&content).1
    } else {
        0
    };
    let mut run = Run {
        p: &p,
        client: None,
        handle: tokio::runtime::Handle::try_current().ok(),
        counts: Counts::default(),
    };
    if matches.iter().any(|m| m.is_remote) {
        if run.handle.is_none() {
            return Err(StepError::failed("no async runtime"));
        }
        run.client = Some(super::web::client()?);
    }
    let needs_view = p.repair || matches.iter().any(|m| !m.is_remote);
    let view = if needs_view {
        match VaultView::open(ctx.index_db, &path) {
            Ok(v) => Some(v),
            Err(e) if p.repair => return Err(StepError::failed(format!("localize: index: {e}"))),
            Err(e) => {
                tracing::debug!(error = %e, "localize: no index; local refs left as-is");
                None
            }
        }
    } else {
        None
    };

    let mut replacements: Vec<Replacement> = Vec::new();
    // (ref text replaced, old local path) for re-homed local attachments.
    let mut retire: Vec<(String, String)> = Vec::new();
    let (mut resolved_count, mut unrepairable) = (0_u64, 0_u64);
    if p.repair {
        let now = chrono::Local::now().naive_local();
        let expected = resolve_attachment_folder(&p.folder_template, &path, "", "", now);
        let pix = view
            .as_ref()
            .map(|v| AttachmentPathIndex::build(&v.paths))
            .unwrap_or_default();
        for m in &matches {
            if !m.is_remote
                && view
                    .as_ref()
                    .and_then(|v| v.resolve(&path, &m.link))
                    .is_some()
            {
                continue;
            }
            let new_ref = if m.is_remote {
                run.process_remote(ctx, &path, m)?
            } else {
                resolve_local_attachment_repair(&m.link, &expected, &pix)
                    .ok()
                    .map(|t| format_ref(m, &t))
            };
            if plan_replacement(&mut replacements, m, new_ref) {
                resolved_count += 1;
            } else {
                unrepairable += 1;
            }
        }
    } else {
        for m in &matches {
            let new_ref = if m.is_remote {
                run.process_remote(ctx, &path, m)?
            } else {
                match run.process_local(ctx, &path, m, view.as_ref())? {
                    Some((r, old)) => {
                        retire.push((m.original.clone(), old));
                        Some(r)
                    }
                    None => None,
                }
            };
            plan_replacement(&mut replacements, m, new_ref);
        }
    }

    // Every remote ref unreachable: the refs stay as they are and the step succeeds so the
    // chain goes on (describe, auto-file); the backfill re-attempts the note later (it is in
    // neither exclusion set) at whatever path it then has (plan round-11 Decision 2).
    let c = &run.counts;
    let all_unreachable = c.remote_attempts > 0 && c.unreachable == c.remote_attempts;
    if all_unreachable {
        tracing::warn!(note = %path, refs = c.remote_attempts, "localize: every remote image unreachable; left for the backfill");
    }

    // The one note write (I2), after every download (SR7): `modify` re-reads the note and
    // re-applies the ref replacements to its current text, so an edit made while downloading
    // is kept, and a lost CAS race re-applies rather than fails.
    let mut applied_from: Vec<String> = Vec::new();
    let mut images = 0_u64;
    let strip = placeholders > 0;
    let written = ctx.modify(&path, |cur| {
        let rewritten = rewrite_localized_attachment_refs(cur, &replacements);
        let mut updated = rewritten.content;
        if strip {
            updated = strip_data_uri_image_placeholders(&updated).0;
        }
        // Managed image references left in the note after the pass (localized + reused +
        // earlier).
        images = count_managed_images(&updated);
        applied_from = rewritten.applied_from;
        Ok((updated != cur).then_some(updated))
    })?;
    let changed = written.is_some();
    let mut output_paths = run.counts.created.clone();
    if changed {
        output_paths.push(path.clone());
        // Retire re-homed local sources no other note references (Crucible processLocal's
        // trash, after the write so a conflict never strands the note's refs).
        let applied: HashSet<&str> = applied_from.iter().map(String::as_str).collect();
        for (from, old) in retire {
            let keep = !applied.contains(from.as_str())
                || view
                    .as_ref()
                    .is_none_or(|v| v.has_other_referrer(&old, &path));
            if keep {
                continue;
            }
            if let Some((_, h)) = ctx.read(&old)? {
                let o = old.clone();
                let r: Result<(), VaultError> =
                    ctx.with_writer(&[old.as_str()], move |vw| vw.delete(&o, h))?;
                if let Err(e) = r {
                    tracing::debug!(error = %e, "localize: could not retire old attachment");
                }
            }
        }
    }
    run.counts.stripped = if changed { placeholders as u64 } else { 0 };
    let c = &run.counts;
    let applied = applied_from.len() as u64;
    let mut counts = NoteCounts {
        downloaded: c.downloaded,
        reused: c.reused,
        stripped: c.stripped,
        failed: c.failed,
        throttled: c.throttled,
        ineligible: c.ineligible,
        localized: if changed { applied } else { 0 },
        ..NoteCounts::default()
    };
    if p.repair {
        let (repaired, unrep) = repair_counts(
            resolved_count,
            unrepairable,
            replacements.len() as u64,
            applied,
            changed || replacements.is_empty(),
        );
        counts.repaired = Some(repaired);
        counts.unrepairable = Some(unrep);
    }
    let vars = NoteVars {
        changed,
        images,
        counts: Some(counts),
        throttle: run.counts.first_throttle.take(),
        all_unreachable,
    };
    Ok((vars, output_paths))
}

/// One ref's planned rewrite (the repair and localize passes share it): records `from → to`
/// when the new ref differs from the original; returns whether the ref was resolved at all.
fn plan_replacement(
    replacements: &mut Vec<Replacement>,
    m: &AttachmentMatch,
    new_ref: Option<String>,
) -> bool {
    let Some(to) = new_ref else {
        return false;
    };
    if to != m.original {
        replacements.push(Replacement {
            from: m.original.clone(),
            to,
        });
    }
    true
}
