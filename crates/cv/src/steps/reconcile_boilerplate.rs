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
//!   `unknown-sender`) has its body put through [`cv_ingest::boilerplate::strip_tiered`] with the seeded
//!   set; written (CAS) only on a change, with `description` recomputed from the stripped body
//!   ([`super::reconcile_backfill::redescribed`]). Counts `bp_stripped`, `bp_guarded` (an
//!   anchored boilerplate line exists but the half-body guard kept the body), `bp_conflict`
//!   (CAS lost, skipped). A second run writes nothing.
//!
//! - **RSS** (plan `intake-removals-sources-rescan` WP-5): every `blog` tracker row with a note
//!   is learned once per `post-id` from the note body with generated blocks (top block, inserted
//!   headings, the `[!removed]` fold) excluded, under the feed's publication record name; then
//!   each note's recurring head/tail lines are folded ([`apply`]). Counts `bp_rss_learned`,
//!   `bp_rss_folded` (notes written), `bp_lines_folded`.
//!
//! **Keys.** Learned lines are keyed by the publication record name for every source kind (mail:
//! the newsletter `source_key`, already the record name; RSS: the record owning the feed). The
//! `_seen` table's `message_id` holds a mail Message-ID as is and an RSS post as `rss:<post-id>`,
//! so both kinds of one record share the line counts without colliding; the RSS share threshold
//! counts every learned item of the record. **Labels** are looked up by the note's `publication`
//! frontmatter value (the value `removal_labels.publication` records; at capture, the value the
//! note is written with); a note without `publication` gets no label tiers.
//!
//! **Tiers** ([`cv_ingest::boilerplate::plan`]), per normalized line, for mail and RSS alike:
//! a Keep label vetoes everything (never stripped, never folded) > a Delete label (stripped when
//! in an anchored head/tail run) > recurrence (mail: stripped at count >= 2; RSS: folded as a
//! `chrome` head / `footer` tail group at >= 3 posts and >= 30 % of the record's learned items).
//! Counts `bp_lines_stripped` (mail recurrence), `bp_lines_deleted` / `bp_notes_deleted`,
//! `bp_lines_vetoed` / `bp_notes_vetoed`, `bp_publications` (records whose notes changed),
//! `bp_label_unknown` (label rows with an unknown verdict, ignored). Generated blocks and the
//! existing fold are never learned, stripped or moved; a second run writes nothing.
//!
//! `dry` learns into in-memory sets only and writes nothing to `state.db` or the vault.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use cv_ingest::boilerplate::{self, Labels, LineSet, Plan, Recur};
use cv_ingest::mail;
use cv_note::FrontmatterDoc;
use cv_text::blocks;
use serde::Serialize;

use super::mail::{KIND, UNKNOWN, raw_markdown, resolve_newsletter};
use super::reconcile_backfill::{Shared, message_map, redescribed};
use super::tracker::blogs;
use super::{StepCtx, StepError};

/// The boilerplate phase's outcome vars (aggregates only; flattened into the reconcile vars).
#[derive(Default, Serialize)]
pub(super) struct Counts {
    bp_learned: u32,
    bp_stripped: u32,
    bp_guarded: u32,
    bp_conflict: u32,
    bp_rss_learned: u32,
    bp_rss_folded: u32,
    bp_lines_folded: u32,
    bp_lines_stripped: u32,
    bp_lines_deleted: u32,
    bp_notes_deleted: u32,
    bp_lines_vetoed: u32,
    bp_notes_vetoed: u32,
    bp_publications: u32,
    bp_label_unknown: u32,
}

fn n32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Counts {
    /// Adds one changed note and its tier counts.
    fn note(&mut self, p: &Plan, recur: Recur) {
        match recur {
            Recur::Mail => {
                self.bp_stripped += 1;
                self.bp_lines_stripped += n32(p.recurred);
            }
            Recur::Rss { .. } => {
                self.bp_rss_folded += 1;
                self.bp_lines_folded += n32(p.recurred);
            }
        }
        self.bp_lines_deleted += n32(p.deleted);
        self.bp_notes_deleted += u32::from(p.deleted > 0);
        self.bp_lines_vetoed += n32(p.vetoed);
        self.bp_notes_vetoed += u32::from(p.vetoed > 0);
    }
}

/// The `_seen` id of an RSS post (a Message-ID never starts with `rss:`).
pub(super) fn rss_seen_id(post_id: &str) -> String {
    format!("rss:{post_id}")
}

/// The body's lines outside generated blocks, joined (what RSS learning reads).
pub(super) fn plain_body(body: &str) -> String {
    let raw: Vec<&str> = body.split('\n').collect();
    let classes = blocks::generated(&raw).classes;
    raw.iter()
        .zip(&classes)
        .filter(|(_, c)| !c.is_generated())
        .map(|(l, _)| *l)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Applies the tiers to a whole note `text` (frontmatter kept byte-for-byte) through
/// [`boilerplate::apply`]: `None` when nothing changes or the fold is refused.
pub(super) fn apply(
    text: &str,
    set: &LineSet,
    labels: &Labels,
    recur: Recur,
) -> (Option<String>, Plan) {
    let doc = FrontmatterDoc::new(text);
    let body = doc.body();
    let prefix = &text[..text.len() - body.len()];
    match boilerplate::apply(body, set, labels, recur) {
        Ok((changed, plan)) => (changed.map(|b| format!("{prefix}{b}")), plan),
        Err(e) => {
            tracing::warn!(error = %e, "boilerplate: fold refused; note left unchanged");
            (None, Plan::default())
        }
    }
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

/// One publication's labels from `state.db` and the count of ignored unknown-verdict rows.
fn load_labels(ctx: &StepCtx<'_>, publication: &str) -> Result<(Labels, usize), StepError> {
    let (keep, delete, unknown) = ctx.state().publication_labels(publication)?;
    if unknown > 0 {
        tracing::warn!(
            unknown,
            "boilerplate: removal labels with an unknown verdict ignored"
        );
    }
    Ok((
        Labels {
            keep: keep.into_iter().collect(),
            delete: delete.into_iter().collect(),
        },
        unknown,
    ))
}

/// Capture: learns `body` (the cleaned, pre-strip body) for `source_key` (the record name) once by
/// `message_id`, then strips it with the updated set and `publication`'s labels.
pub(super) fn learn_and_strip(
    ctx: &StepCtx<'_>,
    source_key: &str,
    publication: &str,
    message_id: &str,
    body: &str,
) -> Result<String, StepError> {
    let (labels, _) = load_labels(ctx, publication)?;
    let mut db = ctx.state();
    db.learn_mail_boilerplate(source_key, message_id, &line_keys(body))?;
    let set = LineSet::from_entries(db.mail_boilerplate_lines(source_key)?);
    Ok(boilerplate::strip_tiered(body, &set, &labels))
}

/// RSS capture: learns the captured note `text`'s body (before enrich) for the record once by
/// `post-id`, then applies the tiers (recurring head/tail lines folded); returns the text to write.
pub(crate) fn learn_and_fold_rss(
    ctx: &StepCtx<'_>,
    record: &str,
    publication: &str,
    post_id: &str,
    text: &str,
) -> Result<String, StepError> {
    let (labels, _) = load_labels(ctx, publication)?;
    let body = plain_body(FrontmatterDoc::new(text).body());
    let (set, posts) = {
        let mut db = ctx.state();
        db.learn_mail_boilerplate(record, &rss_seen_id(post_id), &line_keys(&body))?;
        (
            LineSet::from_entries(db.mail_boilerplate_lines(record)?),
            db.mail_boilerplate_seen_count(record)?,
        )
    };
    Ok(apply(text, &set, &labels, Recur::Rss { posts })
        .0
        .unwrap_or_else(|| text.to_owned()))
}

/// In-run learned sets (loaded from `state.db` on first use; dry runs never persist).
struct Sets {
    dry: bool,
    sets: HashMap<String, LineSet>,
    /// Learned items per key (the RSS share denominator).
    posts: HashMap<String, u32>,
    seen: HashSet<(String, String)>,
    labels: HashMap<String, Labels>,
}

impl Sets {
    fn set(&mut self, ctx: &StepCtx<'_>, key: &str) -> Result<&mut LineSet, StepError> {
        if !self.sets.contains_key(key) {
            let entries = ctx.state().mail_boilerplate_lines(key)?;
            let posts = ctx.state().mail_boilerplate_seen_count(key)?;
            self.sets
                .insert(key.to_owned(), LineSet::from_entries(entries));
            self.posts.insert(key.to_owned(), posts);
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
        *self.posts.entry(key.to_owned()).or_insert(0) += 1;
        if !self.dry {
            ctx.state()
                .learn_mail_boilerplate(key, mid, &line_keys(body))?;
        }
        self.seen.insert(id);
        Ok(true)
    }

    /// The labels of `publication` (cached; unknown verdicts counted once per publication).
    fn labels(
        &mut self,
        ctx: &StepCtx<'_>,
        publication: &str,
        c: &mut Counts,
    ) -> Result<&Labels, StepError> {
        if !self.labels.contains_key(publication) {
            let (l, unknown) = load_labels(ctx, publication)?;
            c.bp_label_unknown += n32(unknown);
            self.labels.insert(publication.to_owned(), l);
        }
        Ok(self
            .labels
            .get(publication)
            .unwrap_or_else(|| unreachable!()))
    }
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

/// The source kind of one strip target (its [`Recur`] is resolved once the sets are seeded).
#[derive(Clone, Copy)]
enum Source {
    Mail,
    Rss,
}

/// One note the tiers apply to, under its record's learned set `key`.
struct Target {
    key: String,
    path: String,
    source: Source,
}

/// The boilerplate phase (after the re-clean, before the repair phases).
pub(super) fn run(ctx: &mut StepCtx<'_>, sh: &Shared, c: &mut Counts) -> Result<(), StepError> {
    let mut sets = Sets {
        dry: sh.dry,
        sets: HashMap::new(),
        posts: HashMap::new(),
        seen: HashSet::new(),
        labels: HashMap::new(),
    };
    let cfg = ctx.config.pipelines().mail.clone();
    let map = if cfg.maildir.trim().is_empty() {
        HashMap::new()
    } else {
        message_map(Path::new(cfg.maildir.trim()), &cfg.processed)
    };
    let sources = ctx.config.sources().clone();
    // Seed from the processed messages.
    let mut files: Vec<_> = map.values().cloned().collect();
    files.sort();
    for f in files {
        let Some(post) = std::fs::read(&f).ok().and_then(|b| mail::parse_eml(&b)) else {
            continue;
        };
        let Some(nl) = resolve_newsletter(&post, &sources) else {
            continue;
        };
        let body = mail::clean_body(&raw_markdown(&post));
        c.bp_learned += u32::from(sets.learn(ctx, &nl.name, &post.message_id, &body)?);
    }
    // Known-sender notes: learn the ones without an `.eml` from their current body; RSS notes:
    // learn once per `post-id` under the feed's record. Then apply the tiers.
    let rows: Vec<_> = ctx
        .state()
        .tracker_items_with_note()?
        .into_iter()
        .filter(|r| {
            (r.source_kind == KIND && r.source_key != UNKNOWN) || r.source_kind == blogs::KIND
        })
        .collect();
    let mut targets: Vec<Target> = Vec::new();
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
        let body = plain_body(doc.body());
        if r.source_kind == blogs::KIND {
            let Some(rec) = sources.by_feed(&r.source_key) else {
                continue;
            };
            let learned = sets.learn(ctx, &rec.name, &rss_seen_id(&r.item_id), &body)?;
            c.bp_rss_learned += u32::from(learned);
            targets.push(Target {
                key: rec.name.clone(),
                path,
                source: Source::Rss,
            });
            continue;
        }
        if !is_email(&doc) {
            continue;
        }
        if !map.contains_key(&r.item_id) {
            c.bp_learned += u32::from(sets.learn(ctx, &r.source_key, &r.item_id, &body)?);
        }
        targets.push(Target {
            key: r.source_key,
            path,
            source: Source::Mail,
        });
    }
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for Target { key, path, source } in targets {
        let Some((bytes, hash)) = ctx.read(&path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let publication = FrontmatterDoc::new(text.as_str())
            .get_str("publication")
            .map(|p| p.trim().to_owned())
            .unwrap_or_default();
        let labels = sets.labels(ctx, &publication, c)?.clone();
        sets.set(ctx, &key)?;
        let recur = match source {
            Source::Mail => Recur::Mail,
            Source::Rss => Recur::Rss {
                posts: sets.posts.get(&key).copied().unwrap_or(0),
            },
        };
        let set = sets.set(ctx, &key)?;
        let (changed, plan) = apply(&text, set, &labels, recur);
        let new = match (changed, recur) {
            (None, Recur::Mail) => {
                if plan.guarded || anchored_boiler(FrontmatterDoc::new(text.as_str()).body(), set) {
                    c.bp_guarded += 1;
                }
                continue;
            }
            (None, Recur::Rss { .. }) => continue,
            (Some(b), Recur::Mail) => redescribed(&b, true).unwrap_or(b),
            (Some(b), Recur::Rss { .. }) => b,
        };
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
        c.note(&plan, recur);
        touched.insert(key);
    }
    c.bp_publications += n32(touched.len());
    Ok(())
}
