//! The three backfill phases of `intake_reconcile` (plan `intake-followups-mail-youtube` WP-5);
//! phases 1–3 live in [`super::reconcile`]. Every phase is dry-aware: with `dry` it computes the
//! same counts and writes nothing to `state.db` or the vault.
//!
//! - **A. YouTube finalize** ([`youtube`]): each open `youtube` row (any outcome) finds its filed
//!   note — the row's `note_path`, else the link phase 1 resolved, else the supersede index by the
//!   row URL / canonical video URL — and the WP-4 finalize core
//!   ([`super::intake_finalize::finalize_note`]) links and ticks it on the locked rule (the note's
//!   own `source` is the video **and** transcript evidence). Counts `yt_ticked`, `yt_linked`,
//!   `yt_no_note` (no / unreadable note), `yt_ambiguous` (several notes), `yt_no_evidence`. No
//!   code inserts `x` rows, so `x_ticked` is always 0.
//! - **B. Mail re-match** ([`mail`]): every `mail` row under `unknown-sender` (any outcome) finds
//!   its message by `Message-ID` in `<maildir>/<processed>/` and `<maildir>/processed/` (counted
//!   `mail_missing` when absent or unparseable). When the resolver
//!   ([`super::mail::resolve_newsletter`], explicit `[[newsletter]]` row, else the Substack host's
//!   `[[blog]]` row) now matches, the row is re-keyed to the newsletter's name
//!   ([`cv_state::StateDb::rekey_tracker_item`]: in place, or merged into a row already at the new
//!   key — `mail_collisions`) and counted `mail_rematched`; otherwise `mail_unmatched`. For a
//!   `note` row with a note, the newsletter's kind and extra tags are appended (`mail_retagged`;
//!   never moves the note — a different kind folder is counted `mail_folder_mismatch` and left),
//!   a paywall marker of the newsletter in the original message body turns the note's `complete`
//!   to false (`mail_marker_incomplete`: a fresh run would have judged it partial), and the finalize
//!   core ticks by predicate (`mail_ticked`; never on a match alone). No note is ever created.
//!   A CAS conflict on the tag write is `mail_tag_conflict` (skipped, not retried).
//! - **C. Re-clean r2** ([`reclean`], plan intake-round-2-clean-dedupe-reading WP-6): every note
//!   with frontmatter `via: email` has its body (the frontmatter stays byte-for-byte, except
//!   `description`, below) put through
//!   [`cv_ingest::mail::clean_body_with`], whose dimension hook decodes the header of a
//!   localized `_attachments/…` image from the vault so small icons and avatars drop; written
//!   (CAS) only when it changes — `mail_recleaned_r2`, `mail_clean_unchanged`,
//!   `mail_clean_conflict` (skipped, not retried). Idempotent: a second run writes nothing.
//!   `word-count` is left as captured. (Round 1 counted this phase as `mail_recleaned`; the var
//!   was renamed, the phase is the same one.) Plan round-9-feedback-todos WP-2 (audit M4): when
//!   the body changes, `description` is recomputed from the cleaned body as capture does; and,
//!   independently, a `description` that carries CSS ([`has_css`]) is recomputed from the
//!   current body even when the body is already clean (`mail_description_repaired`; a
//!   description-only write counts `mail_clean_unchanged` for the body). Any other description
//!   is never touched; a second run writes nothing.
//!
//! The round-2 repair phases (re-key, mail↔RSS merge, publication) live in
//! [`super::reconcile_repair`].
//!
//! A re-run after an interrupted run is the recovery path: a re-keyed row is no longer
//! `unknown-sender`, so only the rows still under it are finished.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use cv_actions::pipelines::NewsletterSource;
use cv_ingest::access;
use cv_ingest::mail;
use cv_note::{FrontmatterDoc, JsValue};
use cv_protocol::TrackerOutcome;
use cv_state::{Rekey, TrackerItem};
use serde::Serialize;

use super::intake_finalize::{RowKey, finalize_note};
use super::mail::{KIND, UNKNOWN, note_target, raw_markdown, resolve_newsletter};
use super::{DeferCause, StepCtx, StepError};
use crate::supersede::SupersedeIndex;

/// The backfill phases' outcome vars (aggregates only; flattened into the reconcile vars).
#[derive(Default, Serialize)]
pub(super) struct Counts {
    yt_ticked: u32,
    yt_linked: u32,
    yt_no_note: u32,
    yt_ambiguous: u32,
    yt_no_evidence: u32,
    /// Always 0: no code inserts `x` tracker rows.
    x_ticked: u32,
    mail_rematched: u32,
    mail_collisions: u32,
    mail_missing: u32,
    mail_unmatched: u32,
    mail_ticked: u32,
    mail_retagged: u32,
    mail_tag_conflict: u32,
    mail_folder_mismatch: u32,
    mail_marker_incomplete: u32,
    mail_recleaned_r2: u32,
    mail_clean_unchanged: u32,
    mail_clean_conflict: u32,
    mail_description_repaired: u32,
}

/// Facts the earlier reconcile phases hand to the backfill phases.
pub(super) struct Shared {
    pub(super) dry: bool,
    /// Rows phase 1 linked (or would link): finalize must not count them linked again.
    pub(super) pre_linked: BTreeSet<RowKey>,
    /// The note phase 1 resolved for those rows (a dry run's database still shows NULL).
    pub(super) paths: HashMap<RowKey, String>,
}

pub(super) fn st(e: cv_state::StateError) -> StepError {
    StepError::failed(format!("state: {e}"))
}

/// A CAS-conflict deferral (`StepCtx::write` maps a lost race to it).
pub(super) fn is_conflict(e: &StepError) -> bool {
    matches!(
        e,
        StepError::Deferred {
            cause: DeferCause::Wait,
            service: None,
            ..
        }
    )
}

fn key_of(r: &TrackerItem) -> RowKey {
    (
        r.source_kind.clone(),
        r.source_key.clone(),
        r.item_id.clone(),
    )
}

/// The supersede index, built on first use (phase 1 and phase A share it).
pub(super) fn index<'a>(
    slot: &'a mut Option<SupersedeIndex>,
    ctx: &StepCtx<'_>,
) -> Result<&'a SupersedeIndex, StepError> {
    if slot.is_none() {
        *slot = Some(SupersedeIndex::build(ctx)?);
    }
    slot.as_ref()
        .ok_or_else(|| StepError::failed("supersede index unavailable"))
}

pub(super) fn read_text(ctx: &StepCtx<'_>, path: &str) -> Result<Option<String>, StepError> {
    Ok(ctx.read(path)?.and_then(|(b, _)| String::from_utf8(b).ok()))
}

/// Phase A.
pub(super) fn youtube(
    ctx: &StepCtx<'_>,
    sh: &Shared,
    sidx: &mut Option<SupersedeIndex>,
    c: &mut Counts,
) -> Result<(), StepError> {
    let rows = ctx.state().tracker_open_rows("youtube").map_err(st)?;
    let mut done: BTreeSet<String> = BTreeSet::new();
    for r in rows {
        let path = if let Some(p) = r
            .note_path
            .clone()
            .or_else(|| sh.paths.get(&key_of(&r)).cloned())
        {
            p
        } else {
            let urls = [
                r.url.clone(),
                format!("https://www.youtube.com/watch?v={}", r.item_id),
            ];
            let m = index(sidx, ctx)?.identity_matches(&urls, None);
            match m.as_slice() {
                [p] => p.clone(),
                [] => {
                    c.yt_no_note += 1;
                    continue;
                }
                _ => {
                    c.yt_ambiguous += 1;
                    continue;
                }
            }
        };
        if !done.insert(path.clone()) {
            continue;
        }
        let Some(text) = read_text(ctx, &path)? else {
            c.yt_no_note += 1;
            continue;
        };
        let v = finalize_note(ctx, &path, &text, Some("youtube"), sh.dry, &sh.pre_linked)?;
        c.yt_linked += v.linked;
        c.yt_ticked += v.ticked;
        c.yt_no_evidence += v.no_evidence;
    }
    Ok(())
}

/// Message-ID → file, over the two processed directories (read-only; first file wins).
pub(super) fn message_map(root: &Path, processed: &str) -> HashMap<String, PathBuf> {
    let mut dirs = vec![root.join(processed.trim()), root.join("processed")];
    dirs.dedup();
    let mut map = HashMap::new();
    for dir in dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        for f in files {
            if let Some(post) = std::fs::read(&f).ok().and_then(|b| mail::parse_eml(&b)) {
                map.entry(post.message_id).or_insert(f);
            }
        }
    }
    map
}

/// Appends the newsletter's kind and extra tags to a rematched `note` row's note and settles its
/// `complete` against the newsletter's paywall markers. Returns the note's (possibly new) text.
fn retag(
    ctx: &mut StepCtx<'_>,
    path: &str,
    nl: &NewsletterSource,
    marker: bool,
    dry: bool,
    c: &mut Counts,
) -> Result<Option<String>, StepError> {
    let (folder, tags, _) = note_target(ctx, &nl.kind, &nl.tags)?;
    let Some((bytes, hash)) = ctx.read(path)? else {
        return Ok(None);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    if cv_text::vpath::split(path).0 != folder {
        c.mail_folder_mismatch += 1;
    }
    let mut doc = FrontmatterDoc::new(text.as_str());
    let mut changed = false;
    for t in &tags {
        let t = t.trim().trim_start_matches('#');
        if !t.is_empty() {
            changed |= doc.append_to_list("tags", &JsValue::Str(t.to_owned()))?;
        }
    }
    let was_complete = !matches!(doc.get("complete"), Ok(Some(JsValue::Bool(false))));
    if marker && was_complete {
        doc.set("complete", &JsValue::Bool(false))?;
        c.mail_marker_incomplete += 1;
        changed = true;
    }
    if !changed {
        return Ok(Some(text));
    }
    if dry {
        c.mail_retagged += 1;
        return Ok(Some(doc.into_text()));
    }
    let new = doc.into_text();
    match ctx.write(path, new.clone().into_bytes(), Some(hash)) {
        Ok(_) => {
            c.mail_retagged += 1;
            Ok(Some(new))
        }
        Err(e) if is_conflict(&e) => {
            c.mail_tag_conflict += 1;
            Ok(Some(text))
        }
        Err(e) => Err(e),
    }
}

/// Phase B.
pub(super) fn mail(ctx: &mut StepCtx<'_>, sh: &Shared, c: &mut Counts) -> Result<(), StepError> {
    let rows = ctx
        .state()
        .tracker_rows_of_source(KIND, UNKNOWN)
        .map_err(st)?;
    if rows.is_empty() {
        return Ok(());
    }
    let cfg = ctx.config.pipelines().mail.clone();
    let map = if cfg.maildir.trim().is_empty() {
        HashMap::new()
    } else {
        message_map(Path::new(cfg.maildir.trim()), &cfg.processed)
    };
    let newsletters = ctx.config.sources().newsletter.clone();
    let blogs = ctx.config.sources().blog.clone();
    let mut tagged: BTreeSet<String> = BTreeSet::new();
    let mut finalized: BTreeSet<String> = BTreeSet::new();
    for r in rows {
        let Some(file) = map.get(&r.item_id) else {
            c.mail_missing += 1;
            continue;
        };
        let Some(post) = std::fs::read(file).ok().and_then(|b| mail::parse_eml(&b)) else {
            c.mail_missing += 1;
            continue;
        };
        let Some(nl) = resolve_newsletter(&post, &newsletters, &blogs) else {
            c.mail_unmatched += 1;
            continue;
        };
        let nl: &NewsletterSource = &nl;
        c.mail_rematched += 1;
        if sh.dry {
            let exists = ctx
                .state()
                .tracker_item(KIND, &nl.name, &r.item_id)
                .map_err(st)?
                .is_some();
            c.mail_collisions += u32::from(exists);
        } else {
            let moved = ctx
                .state()
                .rekey_tracker_item(KIND, UNKNOWN, &r.item_id, &nl.name, &nl.name)
                .map_err(st)?;
            c.mail_collisions += u32::from(moved == Rekey::Merged);
        }
        let Some(path) = r
            .note_path
            .clone()
            .or_else(|| sh.paths.get(&key_of(&r)).cloned())
        else {
            continue;
        };
        let mut text = None;
        if r.outcome == TrackerOutcome::Note && tagged.insert(path.clone()) {
            let mut markers = nl.paywall_markers.clone();
            if let Some(b) = nl
                .blog
                .as_deref()
                .and_then(|n| blogs.iter().find(|b| b.name == n))
            {
                markers.extend(b.paywall_markers.iter().cloned());
            }
            let marker = access::marker_hit(&raw_markdown(&post), &markers);
            text = retag(ctx, &path, nl, marker, sh.dry, c)?;
        }
        if finalized.insert(path.clone()) {
            let Some(text) = text.or(read_text(ctx, &path)?) else {
                continue;
            };
            let v = finalize_note(ctx, &path, &text, Some(KIND), sh.dry, &sh.pre_linked)?;
            c.mail_ticked += v.ticked;
        }
    }
    Ok(())
}

/// The decoded `(width, height)` of a localized image `target` (vault-relative), read from the
/// file header only. `None` for remote, absolute, `..` or unreadable targets.
pub(super) fn image_dims(root: &Path, target: &str) -> Option<(u32, u32)> {
    let t = target.trim().trim_matches(['<', '>']);
    let rel = Path::new(t);
    if t.is_empty()
        || t.contains("://")
        || rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    image::ImageReader::open(root.join(rel))
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// `text` with its body (after the frontmatter) cleaned; `None` when nothing changes.
pub(super) fn recleaned(text: &str, dims: &dyn Fn(&str) -> Option<(u32, u32)>) -> Option<String> {
    let doc = FrontmatterDoc::new(text);
    let body = doc.body();
    let prefix = &text[..text.len() - body.len()];
    let newline = |c: char| c == '\n' || c == '\r';
    let core = body.trim_matches(newline);
    if core.is_empty() {
        return None;
    }
    let lead = &body[..body.len() - body.trim_start_matches(newline).len()];
    let trail = &body[body.trim_end_matches(newline).len()..];
    let cleaned = cv_ingest::mail::clean_body_with(core, dims);
    let out = format!("{prefix}{lead}{cleaned}{trail}");
    (out != text).then_some(out)
}

/// Phase C.
pub(super) fn reclean(ctx: &mut StepCtx<'_>, sh: &Shared, c: &mut Counts) -> Result<(), StepError> {
    let root = ctx.vault_root.to_path_buf();
    let dims = |t: &str| image_dims(&root, t);
    let paths: BTreeSet<String> = {
        let reader = ctx.index()?;
        reader
            .query()
            .md_props_with_key("via")
            .map_err(|e| StepError::failed(format!("index: {e}")))?
            .into_iter()
            .filter(|r| {
                matches!(&r.value, serde_json::Value::String(s) if s.trim().eq_ignore_ascii_case("email"))
            })
            .map(|r| r.path)
            .collect()
    };
    for path in paths {
        let Some((bytes, hash)) = ctx.read(&path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let body_new = recleaned(&text, &dims);
        let base = body_new.as_deref().unwrap_or(&text);
        let desc_new = redescribed(base, body_new.is_some());
        let Some(new) = desc_new.clone().or(body_new.clone()) else {
            c.mail_clean_unchanged += 1;
            continue;
        };
        let (body_changed, desc_changed) = (body_new.is_some(), desc_new.is_some());
        if !sh.dry {
            match ctx.write(&path, new.into_bytes(), Some(hash)) {
                Ok(_) => {}
                Err(e) if is_conflict(&e) => {
                    c.mail_clean_conflict += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        if body_changed {
            c.mail_recleaned_r2 += 1;
        } else {
            c.mail_clean_unchanged += 1;
        }
        c.mail_description_repaired += u32::from(desc_changed);
    }
    Ok(())
}

/// Whether a captured `description` carries CSS (plan round-9-feedback-todos WP-2, audit M4): an
/// at-rule (`@media`, `@import`, `@font-face`, `@keyframes`), `!important`, or a declaration
/// block — a `{` … `}` whose inside starts with a lowercase property name (letters and `-`)
/// followed by `:`. Prose with braces (`{name}`, `{ x }`) does not match.
pub(super) fn has_css(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    if [
        "@media",
        "@import",
        "@font-face",
        "@keyframes",
        "!important",
    ]
    .iter()
    .any(|k| l.contains(k))
    {
        return true;
    }
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return false;
        };
        let inner = &after[..close];
        if let Some((prop, _)) = inner.split_once(':') {
            let prop = prop.trim();
            if !prop.is_empty() && prop.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
                return true;
            }
        }
        rest = &after[close + 1..];
    }
    false
}

/// `text` with its `description` recomputed from its body the way mail capture does
/// ([`super::mail::mail_description`]) — when `body_changed` (the re-clean rewrote the body), or
/// when the current description carries CSS ([`has_css`]). `None` when the note has no
/// `description` or it already holds; a description that is neither is never touched.
pub(super) fn redescribed(text: &str, body_changed: bool) -> Option<String> {
    let mut doc = FrontmatterDoc::new(text);
    let current = match doc.get("description") {
        Ok(Some(JsValue::Str(s))) => s,
        _ => return None,
    };
    if !body_changed && !has_css(&current) {
        return None;
    }
    let fresh = super::mail::mail_description(doc.body());
    if fresh == current {
        return None;
    }
    doc.set("description", &JsValue::Str(fresh)).ok()?;
    Some(doc.into_text())
}
