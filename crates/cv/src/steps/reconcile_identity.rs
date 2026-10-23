//! The round-9 repair phases of `intake_reconcile` (plan `round-9-feedback-todos` WP-2), run after
//! the round-2 repair phases in [`super::reconcile_repair`]. Every phase is dry-aware: with `dry`
//! it computes the same counts and writes nothing. No note is moved or deleted; every write is a
//! CAS write and a lost race is counted and skipped.
//!
//! - **G. Forum post-id re-key** ([`forum_rekey`]): Decision 3 keys a forum post URL (a thread
//!   page with a `#post-<P>` fragment) as `https://<host>/posts/<P>`. A note whose `source` has
//!   that shape and whose stored `post-id` is still the obsolete page-level key (the key of the
//!   `source` with its fragment dropped) gets `post-id` = the new key (`forum_rekeyed`,
//!   `forum_rekey_conflict`). Any other stored `post-id` (lint settings, a hand edit) is left.
//! - **H. Spurious `duplicate-of`** ([`forum_duplicates`], audit M2): a note's `duplicate-of` is
//!   removed only when the note and its target both have a forum post `source`, shared the
//!   obsolete page-level key, share no identity key after the re-key, and have no content match
//!   (containment either way below the supersede `threshold_percent`, the engine's content rule).
//!   Every other `duplicate-of` is untouched. Counts `dup_of_removed`, `dup_of_kept` (a forum
//!   candidate kept on content or identity evidence), `dup_of_conflict`.
//! - **I. Fanfic publication** ([`fanfic_publication`], Decision 2): a note tagged `fanfic` (in
//!   frontmatter) without `publication` is matched against the Works notes exactly as `auto_file`
//!   matches a clip; a fanfic match whose Work has a `title` sets `publication` to it, never
//!   overwriting (`fanfic_publication_backfilled`, `fanfic_publication_no_work`,
//!   `fanfic_publication_conflict`).

use std::collections::BTreeSet;

use cv_note::{FrontmatterDoc, JsValue};
use cv_text::{PostIdOptions, post_id_from_url};
use serde::Serialize;

use super::auto_file::{Clip, FANFIC_KIND, WORKS_FOLDER, read_works, work_publication, works_hit};
use super::file::kind_def;
use super::reconcile_backfill::{Shared, read_text};
use super::{StepCtx, StepError};

/// The round-9 phases' outcome vars (aggregates only; flattened into the reconcile vars).
#[derive(Default, Serialize)]
pub(super) struct Counts {
    forum_rekeyed: u32,
    forum_rekey_conflict: u32,
    dup_of_removed: u32,
    dup_of_kept: u32,
    dup_of_conflict: u32,
    fanfic_publication_backfilled: u32,
    fanfic_publication_no_work: u32,
    fanfic_publication_conflict: u32,
}

/// `(obsolete page-level key, post key)` of a forum post `source`; `None` when Decision 3 does
/// not change the key (not a forum post URL, or already the `/posts/<P>` form).
pub(super) fn forum_keys(source: &str) -> Option<(String, String)> {
    let opts = PostIdOptions::default();
    let s = source.trim();
    let bare = s.split_once('#').map_or(s, |(b, _)| b);
    let old = post_id_from_url(bare, &opts);
    let new = post_id_from_url(s, &opts);
    (old != new && new.contains("/posts/")).then_some((old, new))
}

/// Paths of markdown notes carrying frontmatter `key`.
fn paths_with(ctx: &StepCtx<'_>, key: &str) -> Result<BTreeSet<String>, StepError> {
    let reader = ctx.index()?;
    Ok(reader
        .query()
        .md_paths_with_prop_key(key)
        .map_err(|e| StepError::failed(format!("index: {e}")))?
        .into_iter()
        .collect())
}

/// Phase G.
pub(super) fn forum_rekey(
    ctx: &mut StepCtx<'_>,
    sh: &Shared,
    c: &mut Counts,
) -> Result<(), StepError> {
    for path in paths_with(ctx, "post-id")? {
        let Some((bytes, hash)) = ctx.read(&path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let mut doc = FrontmatterDoc::new(text.as_str());
        let Some((old, new)) = doc.get_str_trimmed("source").and_then(|s| forum_keys(&s)) else {
            continue;
        };
        if doc.get_str_trimmed("post-id").as_deref() != Some(old.as_str()) {
            continue;
        }
        if sh.dry {
            c.forum_rekeyed += 1;
            continue;
        }
        if !doc.set("post-id", &JsValue::Str(new))? {
            continue;
        }
        match ctx.write(&path, doc.into_text().into_bytes(), Some(hash)) {
            Ok(_) => c.forum_rekeyed += 1,
            Err(e) if e.is_conflict() => c.forum_rekey_conflict += 1,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The vault path a `duplicate-of` value links to (`[[path]]`, `[[path|alias]]`, `.md` added).
fn link_target(v: &str) -> Option<String> {
    let inner = v.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    let p = inner.split(['|', '#']).next()?.trim();
    if p.is_empty() {
        return None;
    }
    Some(if p.ends_with(".md") {
        p.to_owned()
    } else {
        format!("{p}.md")
    })
}

/// Phase H.
pub(super) fn forum_duplicates(
    ctx: &mut StepCtx<'_>,
    sh: &Shared,
    c: &mut Counts,
) -> Result<(), StepError> {
    let threshold = f64::from(ctx.config.pipelines().supersede.threshold_percent.min(100));
    for path in paths_with(ctx, "duplicate-of")? {
        let Some(text) = read_text(ctx, &path)? else {
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        let Some(target) = doc
            .get_str_trimmed("duplicate-of")
            .and_then(|v| link_target(&v))
        else {
            continue;
        };
        let Some((a_old, a_new)) = doc.get_str_trimmed("source").and_then(|s| forum_keys(&s))
        else {
            continue;
        };
        let Some(t_text) = read_text(ctx, &target)? else {
            continue;
        };
        let t_doc = FrontmatterDoc::new(t_text.as_str());
        let Some((t_old, t_new)) = t_doc.get_str_trimmed("source").and_then(|s| forum_keys(&s))
        else {
            continue;
        };
        if a_old != t_old {
            continue;
        }
        let (a_body, t_body) = (doc.body(), t_doc.body());
        let content = cv_text::similar::containment(a_body, t_body)
            .max(cv_text::similar::containment(t_body, a_body))
            * 100.0
            >= threshold;
        if a_new == t_new || content {
            c.dup_of_kept += 1;
            continue;
        }
        if sh.dry {
            c.dup_of_removed += 1;
            continue;
        }
        let want = doc.get_str_trimmed("duplicate-of");
        match ctx.modify(&path, |old| {
            let mut d = FrontmatterDoc::new(old);
            if d.get_str_trimmed("duplicate-of") != want {
                return Ok(None);
            }
            Ok(d.remove("duplicate-of")?.then(|| d.into_text()))
        }) {
            Ok(Some(_)) => c.dup_of_removed += 1,
            Ok(None) => {}
            Err(e) if e.is_conflict() => c.dup_of_conflict += 1,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Phase I.
pub(super) fn fanfic_publication(
    ctx: &mut StepCtx<'_>,
    sh: &Shared,
    c: &mut Counts,
) -> Result<(), StepError> {
    let paths: BTreeSet<String> = {
        let reader = ctx.index()?;
        reader
            .query()
            .tagged(FANFIC_KIND)
            .map_err(|e| StepError::failed(format!("index: {e}")))?
            .into_iter()
            .filter(|r| r.source == "fm" && r.tag.trim_start_matches('#') == FANFIC_KIND)
            // The Works pages themselves carry the kind tag; they are not clips.
            .filter(|r| cv_text::vpath::parent(&r.path) != WORKS_FOLDER)
            .map(|r| r.path)
            .collect()
    };
    if paths.is_empty() {
        return Ok(());
    }
    let works = read_works(ctx)?;
    for path in paths {
        let Some(text) = read_text(ctx, &path)? else {
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        if doc.get("publication").ok().flatten().is_some() {
            continue;
        }
        let source = doc.get_str_trimmed("source").unwrap_or_default();
        let clip = Clip {
            host: None,
            authors: doc.get_strings("author"),
            source,
        };
        let is_kind = |k: &str| kind_def(ctx.config, k).is_ok();
        let Some(title) =
            works_hit(&works, &clip, is_kind).and_then(|(w, k)| work_publication(w, &k))
        else {
            c.fanfic_publication_no_work += 1;
            continue;
        };
        if sh.dry {
            c.fanfic_publication_backfilled += 1;
            continue;
        }
        match ctx.modify(&path, |old| {
            let mut d = FrontmatterDoc::new(old);
            if d.get("publication").ok().flatten().is_some() {
                return Ok(None);
            }
            d.insert_after("author", "publication", &JsValue::Str(title.clone()))?;
            Ok(Some(d.into_text()))
        }) {
            Ok(Some(_)) => c.fanfic_publication_backfilled += 1,
            Ok(None) => {}
            Err(e) if e.is_conflict() => c.fanfic_publication_conflict += 1,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
