//! `intake_finalize` (plan `intake-followups-mail-youtube` WP-4): links and ticks the tracker
//! rows a just-filed note satisfies. It runs in the filing action before `lint`, so a fresh
//! YouTube clip still has its `## Transcript` shape.
//!
//! | Param | Type | Default |
//! |---|---|---|
//! | `path` | string (placeholders) | `vars.moved_path`, else the active/triggering note |
//!
//! 1. **Rows**: `tracker_items_for_note(path)` first, plus the still-unlinked rows whose match
//!    keys intersect the note's (`source` / `post-id`, [`cv_ingest::identity`], both sides keyed;
//!    plan intake-foundations WP-2) — or every key-matched row when none is linked; else, for a
//!    note with a YouTube video id (`yt-video-id`, else from `source`), the `youtube` rows with
//!    that `item_id`. Every resolved row with a NULL `note_path` is linked.
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
//! `intake_reconcile`, with a dry-run flag and a one-kind filter): the read-only [`decide`] and its
//! [`apply`] (plan intake-stage-jobs WP-2; the stage model's Finalize fact is "the decision is
//! empty"). Outcome vars are aggregates only: `rows`, `linked`,
//! `ticked`, `no_evidence`. Nothing here is adopted from Crucible (its tracker ticks were manual).

use std::collections::BTreeSet;

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access::Access;
use cv_ingest::identity::{NoteFacts, note_keys};
use cv_note::{FrontmatterDoc, JsValue, transcript_evidence};
use cv_state::{IntakeRead, TrackerEvidence, TrackerItem};
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

/// The tracker rows the note satisfies (see the module doc, step 1), deduplicated. Read-only.
fn resolve_rows<D: IntakeRead + ?Sized>(
    config: &ConfigSet,
    db: &D,
    path: &str,
    sources: &[String],
    post_ids: &[String],
    video: Option<&str>,
) -> Result<Vec<TrackerItem>, StepError> {
    let pubs =
        crate::supersede::publication_hosts(config.sources(), &crate::learned_hosts::rows_from(db));
    let keys = note_keys(
        &NoteFacts {
            sources,
            post_ids,
            ..NoteFacts::default()
        },
        &pubs,
        None,
    );
    let values: Vec<String> = sources.iter().chain(post_ids).cloned().collect();
    let mut rows = db.tracker_items_for_note(path)?;
    let by_key = crate::supersede::tracker_rows_by_keys(db, &values, &keys, &pubs)?;
    if rows.is_empty() {
        rows = by_key;
    } else {
        // Linked rows first; an alias row of the same item that is still unlinked joins them
        // (a row linked to another note is never taken over).
        rows.extend(by_key.into_iter().filter(|r| r.note_path.is_none()));
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

/// What finalize would do for one resolved row (read-only; [`decide`]).
#[derive(Debug, Clone)]
pub(crate) struct RowDecision {
    /// The row as read.
    pub(crate) row: TrackerItem,
    /// The row has no `note_path` yet: it gets linked to the note.
    pub(crate) link: bool,
    /// The row's status is still open (a set status is never changed; nothing else applies).
    pub(crate) open: bool,
    /// The evidence write (`access`, `complete`) for an open `blog`/`mail` or ticked `youtube`
    /// row.
    pub(crate) evidence: Option<(Option<String>, Option<bool>)>,
    /// The evidence differs from what the row holds.
    pub(crate) evidence_changed: bool,
    /// The row gets ticked.
    pub(crate) tick: bool,
}

impl RowDecision {
    /// Whether this row's decision changes anything (a link, new evidence, or a tick).
    pub(crate) fn is_delta(&self) -> bool {
        self.link || self.evidence_changed || self.tick
    }
}

/// The finalize decision for one note: every resolved row with what applying it would change
/// (plan intake-stage-jobs WP-2: the stage model's Finalize fact is [`Decision::is_empty`]).
#[derive(Debug, Clone, Default)]
pub(crate) struct Decision {
    /// One entry per resolved row, in resolution order.
    pub(crate) rows: Vec<RowDecision>,
}

impl Decision {
    /// Nothing left to do: no link to add, no evidence that differs, no tick. Re-writing
    /// unchanged evidence on an open row that stays open is not a decision.
    pub(crate) fn is_empty(&self) -> bool {
        !self.rows.iter().any(RowDecision::is_delta)
    }
}

/// The read-only half of [`finalize_note`]: resolves the rows `text` (the note at `path`)
/// satisfies and decides, per row, the link, the evidence and the tick of the module doc.
/// `only_kind` limits the rows to one `source_kind`. Writes nothing.
pub(crate) fn decide<D: IntakeRead + ?Sized>(
    config: &ConfigSet,
    db: &D,
    path: &str,
    text: &str,
    only_kind: Option<&str>,
) -> Result<Decision, StepError> {
    let doc = FrontmatterDoc::new(text);
    let sources = strings(&doc, "source");
    let video = own_video_id(&sources, &doc);
    let mut rows = resolve_rows(
        config,
        db,
        path,
        &sources,
        &strings(&doc, "post-id"),
        video.as_deref(),
    )?;
    if let Some(k) = only_kind {
        rows.retain(|r| r.source_kind == k);
    }
    let evidence = transcript_evidence(text, cleanup_kind(config, &strings(&doc, "tags")));
    let note_access = doc.get_str("access");
    let note_complete = doc.get_bool("complete");
    let complete_percent = config.pipelines().blogs.complete_percent;
    let mut out = Decision::default();
    for row in rows {
        let status = db.tracker_item_status(&row.source_kind, &row.source_key, &row.item_id)?;
        let mut d = RowDecision {
            link: row.note_path.is_none(),
            open: matches!(status, Some(None)),
            evidence: None,
            evidence_changed: false,
            tick: false,
            row,
        };
        if d.open {
            let row = &d.row;
            match row.source_kind.as_str() {
                "youtube" => {
                    if video.as_deref() == Some(row.item_id.as_str()) && evidence.is_some() {
                        d.evidence = Some((row.access.clone(), Some(true)));
                        d.evidence_changed = row.complete != Some(true);
                        d.tick = true;
                    }
                }
                "blog" | "mail" => {
                    let access = note_access.clone().or_else(|| row.access.clone());
                    let complete = note_complete.or_else(|| {
                        let markers: Vec<String> = config
                            .sources()
                            .by_feed(&row.source_key)
                            .map(|b| b.paywall_markers.clone())
                            .unwrap_or_default();
                        body_verdict(
                            access.as_deref().and_then(Access::parse),
                            doc.body(),
                            row.full_word_count.and_then(|n| u32::try_from(n).ok()),
                            &markers,
                            complete_percent,
                        )
                    });
                    let stored = complete.or(row.complete);
                    d.evidence_changed = access != row.access || stored != row.complete;
                    d.evidence = Some((access, stored));
                    d.tick = complete == Some(true);
                }
                _ => {}
            }
        }
        out.rows.push(d);
    }
    Ok(out)
}

/// The writing half of [`finalize_note`]: links and ticks per `decision` (module doc). An open
/// `blog`/`mail` row gets its evidence written as before, changed or not.
///
/// - `dry` computes every count and writes nothing.
/// - `pre_linked` lists rows an earlier reconcile phase already counted as linked (so a dry run,
///   whose database still shows a NULL `note_path`, counts the same as a real run).
pub(crate) fn apply(
    ctx: &StepCtx<'_>,
    path: &str,
    decision: Decision,
    dry: bool,
    pre_linked: &BTreeSet<RowKey>,
) -> Result<Vars, StepError> {
    let mut v = Vars {
        rows: u32::try_from(decision.rows.len()).unwrap_or(u32::MAX),
        ..Vars::default()
    };
    for d in decision.rows {
        let (kind, key, id) = (&d.row.source_kind, &d.row.source_key, &d.row.item_id);
        if d.link {
            let counted = pre_linked.contains(&(kind.clone(), key.clone(), id.clone()));
            if dry {
                if !counted {
                    v.linked += 1;
                }
            } else if ctx.state().set_tracker_note_path(kind, key, id, path)? {
                v.linked += 1;
            }
        }
        if !d.open {
            continue;
        }
        match kind.as_str() {
            "youtube" | "blog" | "mail" => {
                if !dry && let Some((access, complete)) = &d.evidence {
                    // Evidence and tick in one transaction.
                    let ev = TrackerEvidence {
                        access: access.as_deref(),
                        complete: *complete,
                        full_word_count: d.row.full_word_count,
                    };
                    ctx.state()
                        .finalize_tracker_item(kind, key, id, Some(ev), d.tick)?;
                }
                if d.tick {
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

/// The finalize core, shared with `intake_reconcile` (plan intake-followups-mail-youtube WP-5):
/// [`decide`] then [`apply`] (see each).
pub(crate) fn finalize_note(
    ctx: &StepCtx<'_>,
    path: &str,
    text: &str,
    only_kind: Option<&str>,
    dry: bool,
    pre_linked: &BTreeSet<RowKey>,
) -> Result<Vars, StepError> {
    let decision = decide(ctx.config, &*ctx.state(), path, text, only_kind)?;
    apply(ctx, path, decision, dry, pre_linked)
}
