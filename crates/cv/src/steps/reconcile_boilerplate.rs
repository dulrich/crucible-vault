//! Per-sender newsletter boilerplate (plan `round-10-feedback-todos` WP-2, Decision 1): the
//! capture hook ([`learn_and_strip`], used by `track_mail`) and the `intake_reconcile` phase
//! ([`run`]) that seeds the learned sets and strips existing notes.
//!
//! - **Seed**: every processed `.eml` (the [`super::reconcile_backfill::message_map`] path) of a
//!   configured newsletter is cleaned exactly as capture does ([`super::mail::raw_markdown`] →
//!   [`cv_ingest::mail::clean_body`], pre-strip) and learned once per Message-ID under the
//!   newsletter's `source_key`; a linked `via: email` note of a known sender whose `.eml` is gone
//!   is learned from its current body once, by its Message-ID, before its first strip. Sets are
//!   never re-derived from stripped notes (learning is keyed by Message-ID, so a stripped body is
//!   never learned again). Counts `bp_learned`.
//! - **Strip**: every `via: email` note linked from a `mail` row of a known sender (not
//!   `unknown-sender`) has its body put through [`cv_ingest::boilerplate::strip`] with the seeded
//!   set; written (CAS) only on a change, with `description` recomputed from the stripped body
//!   ([`super::reconcile_backfill::redescribed`]). Counts `bp_stripped`, `bp_guarded` (an
//!   anchored boilerplate line exists but the half-body guard kept the body), `bp_conflict`
//!   (CAS lost, skipped). A second run writes nothing.
//!
//! `dry` learns into in-memory sets only and writes nothing to `state.db` or the vault.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use cv_ingest::boilerplate::{self, LineSet};
use cv_ingest::mail;
use cv_note::FrontmatterDoc;
use serde::Serialize;

use super::mail::{KIND, UNKNOWN, raw_markdown, resolve_newsletter};
use super::reconcile_backfill::{Shared, message_map, redescribed};
use super::{StepCtx, StepError};

/// The boilerplate phase's outcome vars (aggregates only; flattened into the reconcile vars).
#[derive(Default, Serialize)]
pub(super) struct Counts {
    bp_learned: u32,
    bp_stripped: u32,
    bp_guarded: u32,
    bp_conflict: u32,
}

/// The distinct persisted line keys of one body.
fn line_keys(body: &str) -> Vec<u64> {
    let keys: BTreeSet<u64> = body
        .lines()
        .filter_map(boilerplate::normalize_line)
        .map(|n| boilerplate::line_key(&n))
        .collect();
    keys.into_iter().collect()
}

/// Capture: learns `body` (the cleaned, pre-strip body) for `source_key` once by `message_id`,
/// then strips it with the updated set.
pub(super) fn learn_and_strip(
    ctx: &StepCtx<'_>,
    source_key: &str,
    message_id: &str,
    body: &str,
) -> Result<String, StepError> {
    let mut db = ctx.state();
    db.learn_mail_boilerplate(source_key, message_id, &line_keys(body))?;
    let set = LineSet::from_entries(db.mail_boilerplate_lines(source_key)?);
    Ok(boilerplate::strip(body, &set))
}

/// In-run learned sets (loaded from `state.db` on first use; dry runs never persist).
struct Sets {
    dry: bool,
    sets: HashMap<String, LineSet>,
    seen: HashSet<(String, String)>,
}

impl Sets {
    fn set(&mut self, ctx: &StepCtx<'_>, key: &str) -> Result<&mut LineSet, StepError> {
        if !self.sets.contains_key(key) {
            let entries = ctx.state().mail_boilerplate_lines(key)?;
            self.sets
                .insert(key.to_owned(), LineSet::from_entries(entries));
        }
        Ok(self.sets.get_mut(key).unwrap_or_else(|| unreachable!()))
    }

    /// Learns one message once; `true` when it was new.
    fn learn(
        &mut self,
        ctx: &StepCtx<'_>,
        key: &str,
        mid: &str,
        body: &str,
    ) -> Result<bool, StepError> {
        let id = (key.to_owned(), mid.to_owned());
        if self.seen.contains(&id) || ctx.state().mail_boilerplate_seen(key, mid)? {
            return Ok(false);
        }
        // Load the set before persisting, so the in-memory copy counts this message once.
        boilerplate::learn(self.set(ctx, key)?, body);
        if !self.dry {
            ctx.state()
                .learn_mail_boilerplate(key, mid, &line_keys(body))?;
        }
        self.seen.insert(id);
        Ok(true)
    }
}

/// `text` with its body stripped (frontmatter and surrounding newlines kept byte-for-byte);
/// `None` when nothing changes.
pub(super) fn stripped(text: &str, set: &LineSet) -> Option<String> {
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
    let out = format!("{prefix}{lead}{}{trail}", boilerplate::strip(core, set));
    (out != text).then_some(out)
}

/// Whether the first or last countable line of `body` is boilerplate (so an unchanged strip was
/// the half-body guard, not an absence of boilerplate).
fn anchored_boiler(body: &str, set: &LineSet) -> bool {
    let mut lines = body.lines().filter_map(boilerplate::normalize_line);
    let first = lines.next();
    let last = lines.next_back();
    [first, last]
        .into_iter()
        .flatten()
        .any(|n| set.count(&n) >= 2)
}

fn is_email(doc: &FrontmatterDoc) -> bool {
    doc.get_str("via")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("email"))
}

/// The boilerplate phase (after the re-clean, before the repair phases).
pub(super) fn run(ctx: &mut StepCtx<'_>, sh: &Shared, c: &mut Counts) -> Result<(), StepError> {
    let mut sets = Sets {
        dry: sh.dry,
        sets: HashMap::new(),
        seen: HashSet::new(),
    };
    let cfg = ctx.config.pipelines().mail.clone();
    let map = if cfg.maildir.trim().is_empty() {
        HashMap::new()
    } else {
        message_map(Path::new(cfg.maildir.trim()), &cfg.processed)
    };
    let newsletters = ctx.config.sources().newsletter.clone();
    let blogs = ctx.config.sources().blog.clone();
    // Seed from the processed messages.
    let mut files: Vec<_> = map.values().cloned().collect();
    files.sort();
    for f in files {
        let Some(post) = std::fs::read(&f).ok().and_then(|b| mail::parse_eml(&b)) else {
            continue;
        };
        let Some(nl) = resolve_newsletter(&post, &newsletters, &blogs) else {
            continue;
        };
        let body = mail::clean_body(&raw_markdown(&post));
        c.bp_learned += u32::from(sets.learn(ctx, &nl.name, &post.message_id, &body)?);
    }
    // Known-sender notes: learn the ones without an `.eml` from their current body, then strip.
    let rows: Vec<_> = ctx
        .state()
        .tracker_items_with_note()?
        .into_iter()
        .filter(|r| r.source_kind == KIND && r.source_key != UNKNOWN)
        .collect();
    let mut targets: Vec<(String, String)> = Vec::new();
    for r in rows {
        let Some(path) = r.note_path.clone() else {
            continue;
        };
        let Some((bytes, _)) = ctx.read(&path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let doc = FrontmatterDoc::new(text.as_str());
        if !is_email(&doc) {
            continue;
        }
        if !map.contains_key(&r.item_id) {
            c.bp_learned += u32::from(sets.learn(ctx, &r.source_key, &r.item_id, doc.body())?);
        }
        targets.push((r.source_key, path));
    }
    for (key, path) in targets {
        let Some((bytes, hash)) = ctx.read(&path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let set = sets.set(ctx, &key)?;
        let Some(body_new) = stripped(&text, set) else {
            if anchored_boiler(FrontmatterDoc::new(text.as_str()).body(), set) {
                c.bp_guarded += 1;
            }
            continue;
        };
        let new = redescribed(&body_new, true).unwrap_or(body_new);
        if !sh.dry {
            match ctx.write(&path, new.into_bytes(), Some(hash)) {
                Ok(_) => {}
                Err(e) if e.is_conflict() => {
                    c.bp_conflict += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        c.bp_stripped += 1;
    }
    Ok(())
}
