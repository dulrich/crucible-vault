//! The repair phases of `intake_reconcile`, run after the backfill phases in
//! [`super::reconcile_backfill`] and the boilerplate phase. Every phase is dry-aware: with `dry`
//! it computes the same counts and writes nothing to `state.db` or the vault.
//!
//! - **E. Merge** ([`merge_mail_twins`]): each `via: email` note with a `message-id` finds its
//!   `.eml` (processed Maildir, matched by Message-ID) and derives the one post identity with
//!   [`cv_ingest::mail::canonical_post_url`] (blog row link from the resolver, as `track_mail`
//!   does); a note whose message is gone, unparseable or yields no canonical URL is skipped. The
//!   canonical identity is matched in the supersede index (kept current with the derived identities). **Only a
//!   mail↔RSS pair is merged**: when every match is RSS-origin (`via: rss`, or `via: email` with
//!   no `message-id`, i.e. an RSS note the supersede engine already rewrote, or a
//!   note with no `via: email`/`via: rss` that a `blog` tracker row points at) the pair goes
//!   through [`crate::supersede`]; a match with any other note (a clip, another mail note) is
//!   `dup_other` and left untouched. The resolved pair follows `auto_file`'s containment rule
//!   (D25 item 2): `Replaced`, or `Kept` with the mail body contained in the RSS note
//!   (`cv_text::similar::containment` x 100 >= `threshold_percent`), makes the mail note
//!   redundant: its tracker rows are re-pointed to the survivor, the survivor's rows are
//!   finalized on its resolved-body evidence (D25 item 3), and only then does the mail note move
//!   to `.trash/` (`dup_merged`). Otherwise both stay and the mail note gets
//!   `duplicate-of: [[<survivor sans .md>]]` (`dup_marked`). A decision of "distinct" is
//!   `dup_distinct`, a note already carrying `duplicate-of` is skipped, a lost CAS race is
//!   `dup_conflict`. Re-associate-then-trash means an interrupted run never leaves a row
//!   pointing at a missing note, and a rerun finishes the pair.
//! - **F. Publication** ([`publication`]): each note linked from a tracker row (`note_path`) with
//!   no `publication` gets its source row's (`short`, else name; `channel` for YouTube),
//!   inserted after `author`, never overwriting (`publication_backfilled`). A note none of whose
//!   rows maps to a source row is `publication_no_row`; a lost CAS race `publication_conflict`.
//!   Tracker rows never carry a `[[serial]]` source, so serial rows join separately (round 11
//!   E1): an indexed note (outside `.trash/`) that matches a `[[serial]]` row by
//!   `SerialSource::matches` (its `author` list + `source`) and has no `publication` gets the
//!   first matching row's, through the same write; a tracker-linked note's own row wins, and
//!   each note is written at most once per pass.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use cv_actions::pipelines::SupersedeConfig;
use cv_ingest::mail;
use cv_note::{FrontmatterDoc, JsValue};
use cv_state::TrackerItem;
use cv_vault::VaultError;
use serde::Serialize;

use super::mail::{mail_blog_link, resolve_newsletter};
use super::reconcile_backfill::{Shared, message_map, read_text};
use super::{StepCtx, StepError};
use crate::supersede::{self, Action, Incoming, Origin, SupersedeIndex};

/// The repair phases' outcome vars (aggregates only; flattened into the reconcile vars).
#[derive(Default, Serialize)]
pub(super) struct Counts {
    dup_merged: u32,
    dup_marked: u32,
    dup_other: u32,
    dup_distinct: u32,
    dup_conflict: u32,
    publication_backfilled: u32,
    publication_no_row: u32,
    publication_conflict: u32,
}

/// An RSS-origin note: `via: rss`, or `via: email` with no `message-id` (an RSS note a mail
/// replaced in place: the engine rewrites `via` but never adds a message id), or — plan
/// round-9-feedback-todos WP-2, pre-D25 RSS notes carry no `via` — any other note with a `blog`
/// tracker row (`has_blog_row`).
fn is_rss_origin(text: &str, has_blog_row: bool) -> bool {
    let doc = FrontmatterDoc::new(text);
    match doc
        .get_str("via")
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("rss") => true,
        Some("email") => doc.get("message-id").ok().flatten().is_none(),
        _ => has_blog_row,
    }
}

/// Whether a `blog` tracker row points at `path`.
fn has_blog_row(ctx: &StepCtx<'_>, path: &str) -> Result<bool, StepError> {
    Ok(ctx
        .state()
        .tracker_items_for_note(path)?
        .iter()
        .any(|r| r.source_kind == "blog"))
}

/// Phase E: each `via: email` note whose `.eml` yields a canonical identity is merged with its
/// RSS twin. A note with no `.eml` or no canonical URL is skipped.
pub(super) fn merge_mail_twins(
    ctx: &mut StepCtx<'_>,
    sh: &Shared,
    sidx: &mut Option<SupersedeIndex>,
    c: &mut Counts,
) -> Result<(), StepError> {
    let paths: BTreeSet<String> = {
        let reader = ctx.index()?;
        reader
            .query()
            .md_props_with_key("via")?
            .into_iter()
            .filter(|r| {
                matches!(&r.value, serde_json::Value::String(s) if s.trim().eq_ignore_ascii_case("email"))
            })
            .map(|r| r.path)
            .collect()
    };
    if paths.is_empty() {
        return Ok(());
    }
    let cfg = ctx.config.pipelines().mail.clone();
    let map: HashMap<_, _> = if cfg.maildir.trim().is_empty() {
        HashMap::new()
    } else {
        message_map(Path::new(cfg.maildir.trim()), &cfg.processed)
    };
    let sources = ctx.config.sources().clone();
    let sup_cfg = ctx.config.pipelines().supersede.clone();
    super::reconcile_backfill::index(sidx, ctx)?;
    for path in paths {
        let Some(text) = read_text(ctx, &path)? else {
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        let post = doc
            .get_str("message-id")
            .and_then(|m| map.get(&m).cloned())
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| mail::parse_eml(&b));
        let Some(post) = post else {
            continue;
        };
        let blog_link = resolve_newsletter(&post, &sources).and_then(|n| mail_blog_link(&post, n));
        let Some(canon) = mail::canonical_post_url(&post, blog_link) else {
            continue;
        };
        if let Some(idx) = sidx.as_mut() {
            idx.insert_identity(&path, &canon);
        }
        let idx = super::reconcile_backfill::index(sidx, ctx)?;
        merge(ctx, sh, idx, &sup_cfg, &path, &canon, c)?;
    }
    Ok(())
}

/// Phase E for one mail note whose canonical identity is `canon`.
fn merge(
    ctx: &mut StepCtx<'_>,
    sh: &Shared,
    idx: &SupersedeIndex,
    cfg: &SupersedeConfig,
    path: &str,
    canon: &str,
    c: &mut Counts,
) -> Result<(), StepError> {
    let mut rss = 0u32;
    for m in idx.identity_matches(&[canon.to_owned()], Some(path)) {
        match read_text(ctx, &m)? {
            None => {}
            Some(t) if is_rss_origin(&t, has_blog_row(ctx, &m)?) => rss += 1,
            Some(_) => {
                c.dup_other += 1;
                return Ok(());
            }
        }
    }
    if rss == 0 {
        return Ok(());
    }
    let Some(text) = read_text(ctx, path)? else {
        return Ok(());
    };
    let doc = FrontmatterDoc::new(text.as_str());
    if doc.get("duplicate-of").ok().flatten().is_some() {
        return Ok(());
    }
    let body = doc.body().trim().to_owned();
    let publication = doc.get_str("publication");
    let complete = doc.get_bool("complete");
    let inc = Incoming {
        own_path: Some(path),
        urls: vec![canon.to_owned()],
        source: None,
        authors: Vec::new(),
        host: None,
        published: None,
        body_md: &body,
        access: None,
        complete,
        publication: publication.as_deref(),
        origin: Origin::Mail,
    };
    let resolved = if sh.dry {
        supersede::preview(ctx, idx, cfg, &inc)?
    } else {
        supersede::resolve(ctx, idx, cfg, &inc)?
    };
    let Some(survivor) = resolved.path.clone() else {
        c.dup_distinct += 1;
        return Ok(());
    };
    let redundant = match resolved.action {
        Action::Replaced => true,
        Action::Kept => read_text(ctx, &survivor)?.is_some_and(|t| {
            cv_text::similar::containment(&body, FrontmatterDoc::new(t.as_str()).body()) * 100.0
                >= f64::from(cfg.threshold_percent.min(100))
        }),
        Action::New => {
            c.dup_distinct += 1;
            return Ok(());
        }
    };
    if !redundant {
        if sh.dry {
            c.dup_marked += 1;
            return Ok(());
        }
        let link = format!("[[{}]]", survivor.strip_suffix(".md").unwrap_or(&survivor));
        match ctx.modify(path, |old| {
            let mut d = FrontmatterDoc::new(old);
            Ok(d.set("duplicate-of", &JsValue::Str(link.clone()))?
                .then(|| d.into_text()))
        }) {
            Ok(Some(_)) => c.dup_marked += 1,
            Ok(None) => {}
            Err(e) if e.is_conflict() => c.dup_conflict += 1,
            Err(e) => return Err(e),
        }
        return Ok(());
    }
    if sh.dry {
        c.dup_merged += 1;
        return Ok(());
    }
    // Re-associate before trash (audit M1): no row may ever point at a missing note.
    {
        let mut db = ctx.state();
        for r in db.tracker_items_for_note(path)? {
            db.set_tracker_note_path(&r.source_kind, &r.source_key, &r.item_id, &survivor)?;
        }
        let rows = db.tracker_items_for_note(&survivor)?;
        supersede::finalize_rows(&mut db, rows, &resolved)?;
    }
    let Some((_, hash)) = ctx.read(path)? else {
        return Ok(());
    };
    let p = path.to_owned();
    match ctx.with_writer(&[path], move |w| w.delete(&p, hash))? {
        Ok(()) => c.dup_merged += 1,
        Err(VaultError::Conflict { .. }) => c.dup_conflict += 1,
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// The `publication` of the source row a tracker row belongs to.
fn publication_of_row(ctx: &StepCtx<'_>, r: &TrackerItem) -> Option<String> {
    let s = ctx.config.sources();
    match r.source_kind.as_str() {
        "blog" => s.by_feed(&r.source_key).map(|b| b.publication().to_owned()),
        "mail" => {
            let renamed = s
                .mail_key_renames()
                .into_iter()
                .find(|(old, _)| *old == r.source_key)
                .map(|(_, new)| new);
            let key = renamed.as_deref().unwrap_or(&r.source_key);
            s.publications()
                .iter()
                .find(|n| n.name == key)
                .map(|n| n.publication().to_owned())
        }
        "youtube" => s
            .by_channel(&r.source_key)
            .map(|y| y.publication().to_owned()),
        _ => None,
    }
}

/// Notes (outside `.trash/`) whose indexed `source` and `author` match some work record (one
/// with a prefix or author, the former `[[serial]]` rows):
/// the index prefilter for phase F's serial half (the note's own frontmatter is re-checked).
fn serial_candidates(
    ctx: &StepCtx<'_>,
    serial: &[cv_actions::pipelines::Publication],
) -> Result<Vec<String>, StepError> {
    if serial.is_empty() {
        return Ok(Vec::new());
    }
    let reader = ctx.index()?;
    let q = reader.query();
    let mut authors: HashMap<String, Vec<String>> = HashMap::new();
    for r in q.md_props_with_key("author")? {
        let a = authors.entry(r.path).or_default();
        a.extend(cv_index::prop_texts(&r.value));
    }
    let mut out = Vec::new();
    for r in q.md_props_with_key("source")? {
        if r.path.starts_with(".trash/") {
            continue;
        }
        let a: Vec<&str> = authors
            .get(&r.path)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let source = cv_index::prop_text(&r.value);
        if serial.iter().any(|s| s.matches_note(&a, &source)) {
            out.push(r.path);
        }
    }
    Ok(out)
}

/// The publication of the first work record (config order) the note matches.
fn serial_publication(
    serial: &[cv_actions::pipelines::Publication],
    doc: &FrontmatterDoc,
) -> Option<String> {
    let authors = doc.get_texts("author");
    let authors: Vec<&str> = authors.iter().map(String::as_str).collect();
    let source = doc
        .get("source")
        .ok()
        .flatten()
        .map(|v| cv_note::js_text(&v))
        .unwrap_or_default();
    serial
        .iter()
        .find(|s| s.matches_note(&authors, &source))
        .map(|s| s.publication().to_owned())
}

/// Phase F.
pub(super) fn publication(
    ctx: &mut StepCtx<'_>,
    sh: &Shared,
    c: &mut Counts,
) -> Result<(), StepError> {
    let rows = ctx.state().tracker_items_with_note()?;
    let mut by_note: BTreeMap<String, Vec<TrackerItem>> = BTreeMap::new();
    for r in rows {
        if let Some(p) = r.note_path.clone() {
            by_note.entry(p).or_default().push(r);
        }
    }
    // Round 11 E1: notes matching a `[[serial]]` row join the candidates (each note once).
    let serial: Vec<_> = ctx
        .config
        .sources()
        .publications()
        .iter()
        .filter(|p| p.is_work())
        .cloned()
        .collect();
    let mut paths: BTreeSet<String> = by_note.keys().cloned().collect();
    paths.extend(serial_candidates(ctx, &serial)?);
    for path in paths {
        let Some(text) = read_text(ctx, &path)? else {
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        if !crate::supersede::publication_blank(&doc) {
            continue;
        }
        let rows = by_note.get(&path);
        let tracked = rows.and_then(|rs| rs.iter().find_map(|r| publication_of_row(ctx, r)));
        let Some(p) = tracked.or_else(|| serial_publication(&serial, &doc)) else {
            if rows.is_some() {
                c.publication_no_row += 1;
            }
            continue;
        };
        if sh.dry {
            c.publication_backfilled += 1;
            continue;
        }
        match ctx.modify(&path, |old| {
            let mut d = FrontmatterDoc::new(old);
            if !crate::supersede::publication_blank(&d) {
                return Ok(None);
            }
            d.insert_after("author", "publication", &JsValue::Str(p.clone()))?;
            Ok(Some(d.into_text()))
        }) {
            Ok(Some(_)) => c.publication_backfilled += 1,
            Ok(None) => {}
            Err(e) if e.is_conflict() => c.publication_conflict += 1,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
