//! The round-2 repair phases of `intake_reconcile` (plan `intake-round-2-clean-dedupe-reading`
//! WP-6), run after the backfill phases in [`super::reconcile_backfill`] (phase C there is the
//! re-clean r2). Every phase is dry-aware: with `dry` it computes the same counts and writes
//! nothing to `state.db` or the vault.
//!
//! - **D. Re-key** ([`rekey_and_merge`]): each `via: email` note with a `message-id` finds its
//!   `.eml` (processed Maildir, matched by Message-ID) and recomputes the one post identity with
//!   [`cv_ingest::mail::canonical_post_url`] (blog row link from the resolver, as `track_mail`
//!   does). A differing `source` (and `post-id`, when the note has one) is rewritten to it
//!   (`mail_rekeyed`). With no canonical URL, a current `source` that fails
//!   [`cv_ingest::mail::is_clean_identity`] is removed together with `post-id`
//!   (`mail_token_stripped`). A note whose message is gone or unparseable counts `rekey_skipped`
//!   and nothing else changes for it. A CAS conflict is `rekey_conflict` (skipped, not retried).
//! - **E. Merge** (same loop, per re-keyed or already canonical note): the canonical identity is
//!   matched in the supersede index (kept current with the re-keyed identities). **Only a
//!   mail↔RSS pair is merged**: when every match is RSS-origin (`via: rss`, or `via: email` with
//!   no `message-id`, i.e. an RSS note the supersede engine already rewrote, or — round 9 — a
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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use cv_actions::pipelines::SupersedeConfig;
use cv_ingest::mail;
use cv_note::{FrontmatterDoc, JsValue};
use cv_state::TrackerItem;
use cv_vault::VaultError;
use serde::Serialize;

use super::mail::resolve_newsletter;
use super::reconcile_backfill::{Shared, message_map, read_text};
use super::{StepCtx, StepError};
use crate::supersede::{self, Action, Incoming, Origin, SupersedeIndex};

/// The repair phases' outcome vars (aggregates only; flattened into the reconcile vars).
#[derive(Default, Serialize)]
pub(super) struct Counts {
    mail_rekeyed: u32,
    mail_token_stripped: u32,
    rekey_skipped: u32,
    rekey_conflict: u32,
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

/// The note text with `source` set to `canon` (and `post-id` too when the note has one);
/// `None` when it already holds.
fn rekeyed_text(text: &str, canon: &str) -> Option<String> {
    let mut doc = FrontmatterDoc::new(text);
    let v = JsValue::Str(canon.to_owned());
    let mut changed = doc.insert_after("title", "source", &v).ok()?;
    if doc.get("post-id").ok().flatten().is_some() {
        changed |= doc.set("post-id", &v).ok()?;
    }
    changed.then(|| doc.into_text())
}

/// The note text without `source` and `post-id`; `None` when it has neither.
fn stripped_text(text: &str) -> Option<String> {
    let mut doc = FrontmatterDoc::new(text);
    let a = doc.remove("source").ok()?;
    let b = doc.remove("post-id").ok()?;
    (a || b).then(|| doc.into_text())
}

/// Phases D and E.
pub(super) fn rekey_and_merge(
    ctx: &mut StepCtx<'_>,
    sh: &Shared,
    sidx: &mut Option<SupersedeIndex>,
    c: &mut Counts,
) -> Result<(), StepError> {
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
    if paths.is_empty() {
        return Ok(());
    }
    let cfg = ctx.config.pipelines().mail.clone();
    let map: HashMap<_, _> = if cfg.maildir.trim().is_empty() {
        HashMap::new()
    } else {
        message_map(Path::new(cfg.maildir.trim()), &cfg.processed)
    };
    let newsletters = ctx.config.sources().newsletter.clone();
    let blogs = ctx.config.sources().blog.clone();
    let sup_cfg = ctx.config.pipelines().supersede.clone();
    super::reconcile_backfill::index(sidx, ctx)?;
    for path in paths {
        let Some((bytes, hash)) = ctx.read(&path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        let post = doc
            .get_str("message-id")
            .and_then(|m| map.get(&m).cloned())
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| mail::parse_eml(&b));
        let Some(post) = post else {
            c.rekey_skipped += 1;
            continue;
        };
        let blog_link = resolve_newsletter(&post, &newsletters, &blogs)
            .and_then(|n| n.blog.clone())
            .and_then(|b| blogs.iter().find(|r| r.name == b).map(|r| r.link.clone()));
        let current = doc.get_str("source");
        let Some(canon) = mail::canonical_post_url(&post, blog_link.as_deref()) else {
            if current
                .as_deref()
                .is_some_and(|u| !mail::is_clean_identity(u))
                || (current.is_none() && doc.get("post-id").ok().flatten().is_some())
            {
                let Some(new) = stripped_text(&text) else {
                    continue;
                };
                if sh.dry {
                    c.mail_token_stripped += 1;
                    continue;
                }
                match ctx.write(&path, new.into_bytes(), Some(hash)) {
                    Ok(_) => c.mail_token_stripped += 1,
                    Err(e) if e.is_conflict() => c.rekey_conflict += 1,
                    Err(e) => return Err(e),
                }
            }
            continue;
        };
        if let Some(new) = rekeyed_text(&text, &canon) {
            if sh.dry {
                c.mail_rekeyed += 1;
            } else {
                match ctx.write(&path, new.into_bytes(), Some(hash)) {
                    Ok(_) => c.mail_rekeyed += 1,
                    Err(e) if e.is_conflict() => {
                        c.rekey_conflict += 1;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
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
        "blog" => s
            .blog
            .iter()
            .find(|b| b.link.trim() == r.source_key)
            .map(|b| b.publication().to_owned()),
        "mail" => s
            .newsletter
            .iter()
            .find(|n| n.name == r.source_key)
            .map(|n| n.publication().to_owned())
            .or_else(|| {
                s.blog
                    .iter()
                    .find(|b| b.name == r.source_key)
                    .map(|b| b.publication().to_owned())
            }),
        "youtube" => s
            .youtube
            .iter()
            .find(|y| y.id == r.source_key)
            .map(|y| y.publication().to_owned()),
        _ => None,
    }
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
    for (path, rows) in by_note {
        let Some(text) = read_text(ctx, &path)? else {
            continue;
        };
        if FrontmatterDoc::new(text.as_str())
            .get("publication")
            .ok()
            .flatten()
            .is_some()
        {
            continue;
        }
        let Some(p) = rows.iter().find_map(|r| publication_of_row(ctx, r)) else {
            c.publication_no_row += 1;
            continue;
        };
        if sh.dry {
            c.publication_backfilled += 1;
            continue;
        }
        match ctx.modify(&path, |old| {
            let mut d = FrontmatterDoc::new(old);
            if d.get("publication").ok().flatten().is_some() {
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
