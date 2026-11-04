//! `intake_finalize` (plan `intake-followups-mail-youtube` WP-4): links and ticks the tracker
//! rows a just-filed note satisfies. It runs in the filing action before `lint`, so a fresh
//! YouTube clip still has its `## Transcript` shape.
//!
//! | Param | Type | Default |
//! |---|---|---|
//! | `path` | string (placeholders) | `vars.moved_path`, else the active/triggering note |
//!
//! 1. **Rows**: `tracker_items_for_note(path)`; else the rows whose `url` is one of the note's
//!    `source` / `post-id` values (as written and canonicalized, `cv_text::canonicalize_url`);
//!    else, for a note with a YouTube video id (`yt-video-id`, else from `source`), the
//!    `youtube` rows with that `item_id`. Every resolved row with a NULL `note_path` is linked.
//! 2. **Ticks** (rows whose status is already set — `x` or `-` — are never changed; a tick is
//!    never undone):
//!    - `youtube`: only when the note's **own** `source` (else `yt-video-id`) is that video and
//!      [`cv_note::transcript_evidence`] holds — a clipping of the video, not a note that embeds
//!      it (user decision, 2026-10-21). Evidence (`complete = true`) and the tick in one transaction.
//!    - `blog` / `mail`: the same evidence + tick write as the supersede engine (`finalize_tracker_item`,
//!      one transaction, audit M2 of plan auto-intake-sprint) with the note's `complete`
//!      frontmatter, else the completeness predicate of the note body
//!      ([`super::reconcile::body_verdict`], `cv_ingest::access::completeness`).
//!    - Other kinds are linked only. The live tracker has no X rows, so no `x` arm exists.
//!
//! Writes `state.db` only, never a note; holds `intake::identity` (step-intrinsic). A note with
//! no resolvable row is a no-op success. The per-note core is [`finalize_note`] (also run by
//! `intake_reconcile`, with a dry-run flag and a one-kind filter). Outcome vars are aggregates only: `rows`, `linked`,
//! `ticked`, `no_evidence`. Nothing here is adopted from Crucible (its tracker ticks were manual).

use std::collections::BTreeSet;

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access::Access;
use cv_note::{FrontmatterDoc, JsValue, transcript_evidence};
use cv_state::{TrackerEvidence, TrackerItem};
use serde::Serialize;
use serde_json::Value;

use super::file::{cleanup_kind, target_path};
use super::params::{check_keys, check_placeholders, get_str};
use super::reconcile::body_verdict;
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// The `intake_finalize` step.
pub(crate) struct IntakeFinalize;

impl StepModule for IntakeFinalize {
    type Params = Option<String>;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Self::Params, String> {
        check_keys(t, kind, "intake_finalize")?;
        let path = get_str(t, "path")?;
        if let Some(p) = &path {
            check_placeholders(p)?;
        }
        Ok(path)
    }

    fn run(path: Self::Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(path.as_deref(), ctx)
    }
}

/// Outcome vars (aggregates only).
#[derive(Default, Serialize)]
pub(crate) struct Vars {
    pub(crate) rows: u32,
    pub(crate) linked: u32,
    pub(crate) ticked: u32,
    pub(crate) no_evidence: u32,
}

/// A frontmatter value as strings (a scalar string or a list of them).
fn strings(doc: &FrontmatterDoc, key: &str) -> Vec<String> {
    match doc.get(key) {
        Ok(Some(JsValue::Str(s))) => vec![s],
        Ok(Some(JsValue::Arr(a))) => a
            .into_iter()
            .filter_map(|v| match v {
                JsValue::Str(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .map(|s| s.trim().to_owned())
    .filter(|s| !s.is_empty())
    .collect()
}

/// The note's own video id: from its `source`, else `yt-video-id`.
fn own_video_id(sources: &[String], doc: &FrontmatterDoc) -> Option<String> {
    sources
        .iter()
        .find_map(|s| cv_text::extract_video_id_from_url(s))
        .or_else(|| strings(doc, "yt-video-id").into_iter().next())
}

/// The step's target: `path`, else `vars.moved_path`, else the active note.
fn note_path(ctx: &StepCtx<'_>, path: Option<&str>) -> Result<String, StepError> {
    if path.is_none()
        && let Some(Value::String(m)) = ctx.vars.get("moved_path")
        && !m.trim().is_empty()
    {
        return Ok(m.trim().trim_matches('/').to_owned());
    }
    target_path(ctx, path)
}

/// The tracker rows the note satisfies (see the module doc, step 1), deduplicated.
fn resolve_rows(
    ctx: &StepCtx<'_>,
    path: &str,
    sources: &[String],
    post_ids: &[String],
    video: Option<&str>,
) -> Result<Vec<TrackerItem>, StepError> {
    let db = ctx.state();
    let mut rows = db.tracker_items_for_note(path)?;
    if rows.is_empty() {
        let mut urls = BTreeSet::new();
        for u in sources.iter().chain(post_ids) {
            urls.insert(u.clone());
            if let Some(c) = cv_text::canonicalize_url(u) {
                urls.insert(c.canonical);
            }
        }
        for u in &urls {
            rows.extend(db.tracker_items_for_url(u)?);
        }
    }
    if rows.is_empty()
        && let Some(id) = video
    {
        rows = db.tracker_items_for_item("youtube", id)?;
    }
    let mut seen = BTreeSet::new();
    rows.retain(|r| {
        seen.insert((
            r.source_kind.clone(),
            r.source_key.clone(),
            r.item_id.clone(),
        ))
    });
    Ok(rows)
}

fn run(path: Option<&str>, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let path = note_path(ctx, path)?;
    let (bytes, _) = ctx.read_required(&path)?;
    let text = String::from_utf8(bytes).map_err(|_| StepError::failed("note is not UTF-8"))?;
    let v = finalize_note(ctx, &path, &text, None, false, &BTreeSet::new())?;
    let mut out = StepOutcome::default();
    out.set_vars(&v);
    Ok(out)
}

/// A tracker row's key: `(source_kind, source_key, item_id)`.
pub(crate) type RowKey = (String, String, String);

/// The finalize core, shared with `intake_reconcile` (plan intake-followups-mail-youtube WP-5):
/// resolves the rows `text` (the note at `path`) satisfies, links them and ticks them per the
/// module doc.
///
/// - `only_kind` limits the rows to one `source_kind` (reconcile attributes counts per phase);
///   `None` takes every resolved row (the step).
/// - `dry` computes every count and writes nothing.
/// - `pre_linked` lists rows an earlier reconcile phase already counted as linked (so a dry run,
///   whose database still shows a NULL `note_path`, counts the same as a real run).
pub(crate) fn finalize_note(
    ctx: &StepCtx<'_>,
    path: &str,
    text: &str,
    only_kind: Option<&str>,
    dry: bool,
    pre_linked: &BTreeSet<RowKey>,
) -> Result<Vars, StepError> {
    let doc = FrontmatterDoc::new(text);
    let sources = strings(&doc, "source");
    let video = own_video_id(&sources, &doc);
    let mut rows = resolve_rows(
        ctx,
        path,
        &sources,
        &strings(&doc, "post-id"),
        video.as_deref(),
    )?;
    if let Some(k) = only_kind {
        rows.retain(|r| r.source_kind == k);
    }
    let mut v = Vars {
        rows: u32::try_from(rows.len()).unwrap_or(u32::MAX),
        ..Vars::default()
    };
    let evidence = transcript_evidence(text, cleanup_kind(ctx.config, &strings(&doc, "tags")));
    let note_access = doc.get_str("access");
    let note_complete = doc.get_bool("complete");
    let complete_percent = ctx.config.pipelines().blogs.complete_percent;
    for row in rows {
        let (kind, key, id) = (&row.source_kind, &row.source_key, &row.item_id);
        let status = {
            let mut db = ctx.state();
            if row.note_path.is_none() {
                let counted = pre_linked.contains(&(kind.clone(), key.clone(), id.clone()));
                if dry {
                    if !counted {
                        v.linked += 1;
                    }
                } else if db.set_tracker_note_path(kind, key, id, path)? {
                    v.linked += 1;
                }
            }
            db.tracker_item_status(kind, key, id)?
        };
        if matches!(status, Some(Some(_)) | None) {
            continue;
        }
        match kind.as_str() {
            "youtube" => {
                if video.as_deref() == Some(id.as_str()) && evidence.is_some() {
                    if !dry {
                        // Evidence and tick in one transaction.
                        let ev = TrackerEvidence {
                            access: row.access.as_deref(),
                            complete: Some(true),
                            full_word_count: row.full_word_count,
                        };
                        ctx.state()
                            .finalize_tracker_item(kind, key, id, Some(ev), true)?;
                    }
                    v.ticked += 1;
                } else {
                    v.no_evidence += 1;
                }
            }
            "blog" | "mail" => {
                let access = note_access.clone().or_else(|| row.access.clone());
                let complete = note_complete.or_else(|| {
                    let markers: Vec<String> = ctx
                        .config
                        .sources()
                        .blog
                        .iter()
                        .filter(|b| b.link.trim() == key)
                        .flat_map(|b| b.paywall_markers.clone())
                        .collect();
                    body_verdict(
                        access.as_deref().and_then(Access::parse),
                        doc.body(),
                        row.full_word_count.and_then(|n| u32::try_from(n).ok()),
                        &markers,
                        complete_percent,
                    )
                });
                if !dry {
                    let access = access.or_else(|| row.access.clone());
                    let ev = TrackerEvidence {
                        access: access.as_deref(),
                        complete: complete.or(row.complete),
                        full_word_count: row.full_word_count,
                    };
                    ctx.state().finalize_tracker_item(
                        kind,
                        key,
                        id,
                        Some(ev),
                        complete == Some(true),
                    )?;
                }
                if complete == Some(true) {
                    v.ticked += 1;
                } else {
                    v.no_evidence += 1;
                }
            }
            _ => v.no_evidence += 1,
        }
    }
    Ok(v)
}
