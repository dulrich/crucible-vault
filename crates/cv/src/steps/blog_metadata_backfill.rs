//! `blog_metadata_backfill` (plan `round-12-feedback-todos` WP-8, Decisions 5, 6): the one-shot
//! fold of the Crucible-era `_blog_metadata/` notes into cv's intake, behind the
//! `blog-metadata-backfill` action and `cv intake backfill-blog-metadata [--apply]`. It is
//! note-mutating and holds `intake::identity` (step-intrinsic): it re-keys post-ids and tracker
//! rows.
//!
//! **Dry run by default.** It writes only when `dry_run = false` is given explicitly (a job param,
//! which wins, or the step param); otherwise every count is computed and nothing is written.
//!
//! **Association (replay-safe).** Every key comes from the metadata note's `source`, which this
//! step never rewrites: the post id under the configured and the default `PostIdOptions`, the
//! raw and canonical source URL, plus the stored `post-id` while present. Blog tracker rows
//! match by `item_id` or `url`; twins (captures outside `_blog_metadata/`, Decision 1 folders
//! and `_raw_transcript/` excluded) match by recomputed identity ([`dupes::identity`]) equal to
//! the configured id.
//!
//! **Phases.**
//! 1. Re-key a stale `post-id` (stored ≠ configured recomputed) on metadata notes and their twins
//!    (`rekeyed_notes`).
//! 2. Score each twin with [`dupes::score`] (a = metadata, b = capture); the best is the highest
//!    directional containment of the metadata body in the capture, ties → first path.
//! 3. **Certain** (Decision 5: containment >= 0.9 and no user inbound link to the metadata note,
//!    [`user_inbound_sources`]): matched rows → the capture, then the metadata note goes to
//!    `.trash/` (`certain`).
//! 4. **Uncertain** (a twin, not certain): `possible-duplicate` on both notes of every twin pair
//!    unless `not-duplicate-of` already holds it; rows → the metadata note (`uncertain`,
//!    `inbound_guarded` for those the link guard alone kept, `labels_added`).
//! 5. **No twin**: kind `blog` tags (and the unticked `trim-label`), rows → the destination (one
//!    row created when none matches, `rows_created`), then the move to the day folder of its
//!    `published` date with the writer's link maintenance (`no_twin`; `skipped_no_date` when
//!    `published` does not parse, `skipped_conflict` when the destination exists). Tags and rows
//!    precede the move, so an interrupted run redoes them on the rerun.
//! 6. Every matched row gets `day = published`; a `skipped` row becomes `note` so it is a visible
//!    intake item (`rows_updated`, `rows_visible`).
//!
//! Outside `_blog_metadata/` it only re-keys twins' `post-id`, writes `possible-duplicate`, writes
//! tracker rows and lets the move rewrite links. Vars are aggregates only.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use cv_actions::{ConfigSet, StepKind};
use cv_note::{FrontmatterDoc, JsValue};
use cv_protocol::TrackerOutcome;
use cv_state::TrackerItem;
use cv_text::url_canon::{PostIdOptions, bare_host, canonicalize_url, post_id_from_url};
use serde::Serialize;
use serde_json::Value;

use super::dupes::{
    EXCLUDED_TOPS, NOT_DUP_KEY, POSSIBLE_KEY, identity, link_target, post_id_options, score, top_of,
};
use super::dupes_resolve::user_inbound_sources;
use super::file::{apply_trim_label, kind_def};
use super::params::{check_keys, get_bool, get_str_list};
use super::{StepCtx, StepError, StepModule, StepOutcome};
use crate::now_ms;

/// The folder this step folds into intake.
pub(crate) const META_TOP: &str = "_blog_metadata";
/// Folders whose notes are never a blog capture (user decision 2026-10-23: a transcript is not a
/// twin, so its metadata note is never trashed in favour of it).
const NOT_TWIN_TOPS: [&str; 1] = ["_raw_transcript"];
/// Decision 5: the metadata body must be contained in the capture at least this much.
pub(crate) const CERTAIN_CONTAINMENT: f64 = 0.9;
const KIND: &str = "blog";

/// The `blog_metadata_backfill` step.
pub(crate) struct BlogMetadataBackfill;

impl StepModule for BlogMetadataBackfill {
    type Params = Option<bool>;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Option<bool>, String> {
        check_keys(t, kind, "blog_metadata_backfill")?;
        get_bool(t, "dry_run")
    }

    fn run(step_dry: Option<bool>, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        let dry = job_dry_run(ctx).or(step_dry).unwrap_or(true);
        run(dry, Stop::Full, ctx)
    }
}

/// The job param `dry_run` (`true`/`false`, bool or string); absent or other → `None`.
fn job_dry_run(ctx: &StepCtx<'_>) -> Option<bool> {
    match ctx.params.get("dry_run") {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Some(true),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// Where a run stops (tests simulate an interrupted apply with [`Stop::AfterRekey`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    AfterRekey,
    Full,
}

/// Outcome vars (aggregates only).
#[derive(Default, Serialize)]
struct Vars {
    notes: u32,
    skipped_no_source: u32,
    rekeyed_notes: u32,
    certain: u32,
    uncertain: u32,
    inbound_guarded: u32,
    labels_added: u32,
    no_twin: u32,
    skipped_no_date: u32,
    skipped_conflict: u32,
    rows_matched: u32,
    rows_updated: u32,
    rows_visible: u32,
    rows_created: u32,
    dry_run: bool,
}

/// `published` (`YYYY-MM-DD` or an ISO datetime): its date part, no timezone shift.
pub(crate) fn published_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    let head = s.get(..10)?;
    if s.len() > 10 && !s[10..].starts_with(['T', 't', ' ']) {
        return None;
    }
    NaiveDate::parse_from_str(head, "%Y-%m-%d").ok()
}

fn read_text(
    ctx: &StepCtx<'_>,
    path: &str,
) -> Result<Option<(String, cv_vault::ContentHash)>, StepError> {
    Ok(ctx
        .read(path)?
        .and_then(|(b, h)| String::from_utf8(b).ok().map(|s| (s, h))))
}

fn bare(p: &str) -> &str {
    p.strip_suffix(".md").unwrap_or(p)
}

fn wikilink(path: &str) -> JsValue {
    JsValue::Str(format!("[[{}]]", bare(path)))
}

/// Whether `doc`'s `key` holds an entry whose link text is `target` (without `.md`).
fn lists(doc: &FrontmatterDoc, key: &str, target: &str) -> bool {
    doc.get_strings(key)
        .iter()
        .any(|e| bare(link_target(e)) == bare(target))
}

/// One metadata note's association keys (all derived from `source`, plus the stored id).
struct Keys {
    configured: String,
    ids: BTreeSet<String>,
    urls: BTreeSet<String>,
}

fn keys_of(source: &str, stored: Option<String>, opts: &PostIdOptions) -> Keys {
    let configured = post_id_from_url(source, opts);
    let mut ids = BTreeSet::from([
        configured.clone(),
        post_id_from_url(source, &PostIdOptions::default()),
    ]);
    ids.extend(stored);
    ids.retain(|s| !s.is_empty());
    let mut urls = BTreeSet::from([source.to_owned()]);
    if let Some(c) = canonicalize_url(source) {
        urls.insert(c.canonical);
    }
    Keys {
        configured,
        ids,
        urls,
    }
}

/// Blog rows matching `k` by item id or url, distinct, in key order.
fn matched_rows(ctx: &StepCtx<'_>, k: &Keys) -> Result<Vec<TrackerItem>, StepError> {
    let st = ctx.state();
    let mut out: BTreeMap<(String, String), TrackerItem> = BTreeMap::new();
    for id in &k.ids {
        for r in st.tracker_items_for_item(KIND, id)? {
            out.insert((r.source_key.clone(), r.item_id.clone()), r);
        }
    }
    for u in &k.urls {
        for r in st.tracker_items_for_url(u)? {
            if r.source_kind == KIND {
                out.insert((r.source_key.clone(), r.item_id.clone()), r);
            }
        }
    }
    Ok(out.into_values().collect())
}

/// Sets `post-id` to `id` when the note stores a different one. Returns whether it is (or, dry,
/// would be) changed.
fn rekey(ctx: &mut StepCtx<'_>, path: &str, id: &str, dry: bool) -> Result<bool, StepError> {
    let Some((text, _)) = read_text(ctx, path)? else {
        return Ok(false);
    };
    let stale = FrontmatterDoc::new(text.as_str())
        .get_str_trimmed("post-id")
        .is_some_and(|s| s != id);
    if !stale || dry {
        return Ok(stale);
    }
    ctx.modify(path, |old| {
        let mut d = FrontmatterDoc::new(old);
        Ok(d.set("post-id", &JsValue::Str(id.to_owned()))?
            .then(|| d.into_text()))
    })?;
    Ok(true)
}

/// Adds `other` to `path`'s `possible-duplicate` (set semantics). Returns whether it changed.
fn label(ctx: &mut StepCtx<'_>, path: &str, other: &str, dry: bool) -> Result<bool, StepError> {
    let Some((text, _)) = read_text(ctx, path)? else {
        return Ok(false);
    };
    if lists(&FrontmatterDoc::new(text.as_str()), POSSIBLE_KEY, other) {
        return Ok(false);
    }
    if !dry {
        ctx.modify(path, |old| {
            let mut d = FrontmatterDoc::new(old);
            Ok(d.append_to_list(POSSIBLE_KEY, &wikilink(other))?
                .then(|| d.into_text()))
        })?;
    }
    Ok(true)
}

/// Phase 6 on the matched rows: `day` (when known), `note` for a `skipped` row, `note_path`.
fn place_rows(
    ctx: &StepCtx<'_>,
    rows: &[TrackerItem],
    day: Option<&str>,
    note: &str,
    dry: bool,
    v: &mut Vars,
) -> Result<(), StepError> {
    let mut st = ctx.state();
    for r in rows {
        let d = day.unwrap_or(&r.day);
        let skipped = r.outcome == TrackerOutcome::Skipped;
        let outcome = if skipped {
            TrackerOutcome::Note
        } else {
            r.outcome
        };
        let changed = if dry {
            r.day != d || skipped || r.note_path.as_deref() != Some(note)
        } else {
            st.set_tracker_intake(
                (&r.source_kind, &r.source_key, &r.item_id),
                d,
                outcome,
                note,
            )?
        };
        if changed {
            v.rows_updated += 1;
            if skipped {
                v.rows_visible += 1;
            }
        }
    }
    Ok(())
}

/// One metadata note, after phase 1.
struct Meta {
    path: String,
    source: String,
    keys: Keys,
}

/// The run.
pub(crate) fn run(dry: bool, stop: Stop, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let opts = post_id_options(ctx.config);
    let mut v = Vars {
        dry_run: dry,
        ..Vars::default()
    };
    let files = ctx
        .index()?
        .query()
        .file_paths()
        .map_err(|e| StepError::failed(format!("index: {e}")))?;
    let mut metas: Vec<String> = Vec::new();
    let mut twins: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, p) in files {
        if !p.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        let top = top_of(&p);
        if top == META_TOP {
            metas.push(p);
        } else if !EXCLUDED_TOPS.contains(&top)
            && !NOT_TWIN_TOPS.contains(&top)
            && let Some((text, _)) = read_text(ctx, &p)?
            && let Some(id) = identity(&FrontmatterDoc::new(text.as_str()), &opts)
        {
            twins.entry(id).or_default().push(p);
        }
    }
    metas.sort();
    for g in twins.values_mut() {
        g.sort();
    }

    // Phase 1: keys (from `source`) and stale post-id re-keys.
    let mut plan: Vec<Meta> = Vec::new();
    for path in metas {
        let Some((text, _)) = read_text(ctx, &path)? else {
            continue;
        };
        v.notes += 1;
        let doc = FrontmatterDoc::new(text.as_str());
        let Some(source) = doc.get_str_trimmed("source") else {
            v.skipped_no_source += 1;
            continue;
        };
        let keys = keys_of(&source, doc.get_str_trimmed("post-id"), &opts);
        if keys.configured.is_empty() {
            v.skipped_no_source += 1;
            continue;
        }
        if rekey(ctx, &path, &keys.configured, dry)? {
            v.rekeyed_notes += 1;
        }
        for t in twins.get(&keys.configured).into_iter().flatten() {
            let Some((tt, _)) = read_text(ctx, t)? else {
                continue;
            };
            let tid = identity(&FrontmatterDoc::new(tt.as_str()), &opts).unwrap_or_default();
            if rekey(ctx, t, &tid, dry)? {
                v.rekeyed_notes += 1;
            }
        }
        plan.push(Meta { path, source, keys });
    }
    if stop == Stop::AfterRekey {
        let mut out = StepOutcome::default();
        out.set_vars(&v);
        return Ok(out);
    }

    for m in plan {
        let Some((text, _)) = read_text(ctx, &m.path)? else {
            continue;
        };
        let published = FrontmatterDoc::new(text.as_str())
            .get_str("published")
            .as_deref()
            .and_then(published_date);
        let day = published.map(|d| d.format("%Y-%m-%d").to_string());
        let rows = matched_rows(ctx, &m.keys)?;
        #[allow(clippy::cast_possible_truncation)]
        {
            v.rows_matched += rows.len() as u32;
        }
        let group: Vec<String> = twins.get(&m.keys.configured).cloned().unwrap_or_default();

        // Phase 2: the best twin by directional containment (metadata in capture).
        let mut best: Option<(String, f64)> = None;
        for t in &group {
            let Some((tt, _)) = read_text(ctx, t)? else {
                continue;
            };
            let s = score(&text, &tt, &opts);
            if !s.identity_match {
                continue;
            }
            if best.as_ref().is_none_or(|(_, c)| s.containment_a_in_b > *c) {
                best = Some((t.clone(), s.containment_a_in_b));
            }
        }

        if let Some((capture, c)) = best {
            let contained = c >= CERTAIN_CONTAINMENT;
            let guarded = contained && !user_inbound_sources(ctx, &m.path)?.is_empty();
            if contained && !guarded {
                // Phase 3: rows to the capture first, then the trash.
                place_rows(ctx, &rows, day.as_deref(), &capture, dry, &mut v)?;
                if !dry {
                    let Some((_, hash)) = ctx.read(&m.path)? else {
                        continue;
                    };
                    super::auto_file::trash(ctx, &m.path, hash, &capture)?;
                }
                v.certain += 1;
                continue;
            }
            // Phase 4: label every twin pair not already ruled out.
            v.uncertain += 1;
            if guarded {
                v.inbound_guarded += 1;
            }
            let mdoc = FrontmatterDoc::new(text.as_str());
            for t in &group {
                let Some((tt, _)) = read_text(ctx, t)? else {
                    continue;
                };
                if lists(&mdoc, NOT_DUP_KEY, t)
                    || lists(&FrontmatterDoc::new(tt.as_str()), NOT_DUP_KEY, &m.path)
                {
                    continue;
                }
                v.labels_added += u32::from(label(ctx, &m.path, t, dry)?);
                v.labels_added += u32::from(label(ctx, t, &m.path, dry)?);
            }
            place_rows(ctx, &rows, day.as_deref(), &m.path, dry, &mut v)?;
            continue;
        }

        // Phase 5: no twin.
        let (Some(date), Some(day)) = (published, day) else {
            v.skipped_no_date += 1;
            continue;
        };
        let folder = super::materialize::day_folder(ctx.config, date)?;
        let dst = super::materialize::join(&folder, cv_text::vpath::basename(&m.path));
        if ctx.read(&dst)?.is_some() {
            v.skipped_conflict += 1;
            continue;
        }
        if !dry {
            let def = kind_def(ctx.config, KIND).map_err(StepError::failed)?;
            let tags = get_str_list(def, "tags").map_err(StepError::failed)?;
            ctx.modify(&m.path, |old| {
                let mut d = FrontmatterDoc::new(old);
                let mut changed = false;
                for tg in &tags {
                    let c = tg.trim().trim_start_matches('#').trim();
                    if !c.is_empty() {
                        changed |= d.append_to_list("tags", &JsValue::Str(c.to_owned()))?;
                    }
                }
                changed |= apply_trim_label(&mut d, KIND)?;
                Ok(changed.then(|| d.into_text()))
            })?;
        }
        if rows.is_empty() {
            v.rows_created += 1;
            if !dry {
                let item = new_row(ctx.config, &m, &text, &day, &dst);
                ctx.state().insert_tracker_item(&item)?;
            }
        } else {
            place_rows(ctx, &rows, Some(&day), &dst, dry, &mut v)?;
        }
        if !dry {
            let Some((_, hash)) = ctx.read(&m.path)? else {
                continue;
            };
            let (s2, d2) = (m.path.clone(), dst.clone());
            ctx.with_writer(&[m.path.as_str(), dst.as_str()], move |vw| {
                vw.rename(&s2, &d2, hash)
            })??;
        }
        v.no_twin += 1;
    }

    let mut out = StepOutcome::default();
    out.set_vars(&v);
    Ok(out)
}

/// The intake row created for a no-twin note with no matching row (brief pin 5).
fn new_row(config: &ConfigSet, m: &Meta, text: &str, day: &str, dst: &str) -> TrackerItem {
    let host = bare_host(&m.source);
    let src = config
        .sources()
        .blog
        .iter()
        .find(|b| host.is_some() && bare_host(&b.link) == host);
    let title = FrontmatterDoc::new(text)
        .get_str_trimmed("title")
        .unwrap_or_else(|| bare(cv_text::vpath::basename(&m.path)).to_owned());
    TrackerItem {
        source_kind: KIND.into(),
        source_key: src.map_or_else(
            || format!("backfill:{}", host.clone().unwrap_or_default()),
            |b| b.link.clone(),
        ),
        item_id: m.keys.configured.clone(),
        title,
        url: m.source.clone(),
        first_seen_ms: now_ms(),
        day: day.to_owned(),
        outcome: TrackerOutcome::Note,
        score: None,
        full_body: None,
        note_path: Some(dst.to_owned()),
        source_name: src.map(|b| b.name.clone()),
        duration_s: None,
        word_count: None,
        access: None,
        complete: None,
        full_word_count: None,
    }
}
