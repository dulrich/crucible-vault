//! The intake supersede engine (plan `auto-intake-sprint` WP-3).
//!
//! An incoming item (a tracker post, a mail, a clip) is resolved against the vault before its
//! caller creates or records anything:
//!
//! 1. **identity**: index notes whose `source` (canonicalized on both sides,
//!    [`cv_ingest::supersede::identity_keys`]) or `post-id` equals one of the item's keys;
//! 2. otherwise **containment**: notes from the same source (index `source` host, and an
//!    overlapping `author` when both have one, plus `tracker_items` rows of the caller's source
//!    with a `note_path`) dated within ±`[supersede] window_days` of the item (a missing date is in
//!    window; date equality is never required); the best containment either way ≥ the threshold.
//!
//! The pair is then decided ([`cv_ingest::supersede::decide`]): `Kept` (the existing note stays),
//! `Replaced` (the incoming body replaces the note's body in place; frontmatter is kept except
//! `access`/`complete`/`word-count`/`via`; the old text is first saved to
//! `<state dir>/superseded/<hash>-<ms>.md`, and a failed save aborts to `Kept`), or `New` (no
//! match; the caller creates its own note). The caller's own path is excluded from every
//! candidate set (audit B1), so a sole clip resolves `New`, never itself.
//!
//! Finalize (audit M2): the tracker rows of the resolved note (by `note_path`, else by the
//! incoming URLs) get the **resolved** body's `access`/`complete`, and are ticked (`status = x`)
//! only when that body is complete. A match alone never ticks.
//!
//! **Precondition:** callers hold [`cv_actions::INTAKE_IDENTITY_KEY`] (a step-intrinsic queue
//! resource key, [`cv_actions::StepKind::intrinsic_resource_keys`]) from resolve through their
//! own create/record, so no two intake jobs resolve and commit concurrently.

use std::collections::{BTreeSet, HashMap};

use chrono::NaiveDate;
use cv_actions::pipelines::SupersedeConfig;
use cv_ingest::access::Access;
use cv_ingest::supersede::{Decision, Side, decide, identity_keys, overlap};
use cv_note::{FrontmatterDoc, JsValue, calculate_word_count};
use cv_text::similar::Shingles;

use crate::steps::{StepCtx, StepError};

/// Where an incoming item came from (the replaced note's `via`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A feed tracker (`via: rss`).
    Tracker,
    /// A newsletter mail (`via: email`).
    Mail,
    /// A web clip (`via: clip`).
    Clip,
}

impl Origin {
    /// The `via` frontmatter value.
    #[must_use]
    pub fn via(self) -> &'static str {
        match self {
            Origin::Tracker => "rss",
            Origin::Mail => "email",
            Origin::Clip => "clip",
        }
    }
}

/// An item to resolve.
#[derive(Debug, Clone)]
pub struct Incoming<'a> {
    /// The caller's own vault path (a clip already written), excluded from every candidate set.
    pub own_path: Option<&'a str>,
    /// The item's URLs (identity keys are derived from each).
    pub urls: Vec<String>,
    /// The caller's `tracker_items` source (`source_kind`, `source_key`), when it has one.
    pub source: Option<(&'a str, &'a str)>,
    /// Author names (bare or `[[wikilinks]]`).
    pub authors: Vec<String>,
    /// The source host; `None` = the first URL's canonical domain.
    pub host: Option<String>,
    /// Published date (`None` = every candidate is in window).
    pub published: Option<NaiveDate>,
    /// The delivered body, markdown.
    pub body_md: &'a str,
    /// Publication audience, when probed.
    pub access: Option<Access>,
    /// Whether `body_md` is the whole item (`None` = unknown).
    pub complete: Option<bool>,
    /// The source row's `publication` (plan intake-round-2-clean-dedupe-reading WP-3): set on a
    /// replaced note only when it has none, never overwritten.
    pub publication: Option<&'a str>,
    /// Where the item came from.
    pub origin: Origin,
}

/// The engine's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// An existing note stays as it is (the incoming item is its duplicate).
    Kept,
    /// The existing note's body was replaced by the incoming one.
    Replaced,
    /// No match: the caller keeps or creates its own note.
    New,
}

/// The resolved note and the evidence of its (resolved) body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The resolved existing note (`None` for [`Action::New`]); never the caller's own path.
    pub path: Option<String>,
    /// What happened.
    pub action: Action,
    /// The resolved body's `access` (`Kept`: the note's; `Replaced`: the incoming, else the note's).
    pub access: Option<String>,
    /// The resolved body's `complete`.
    pub complete: Option<bool>,
}

impl Resolved {
    fn new_item() -> Self {
        Self {
            path: None,
            action: Action::New,
            access: None,
            complete: None,
        }
    }
}

/// One indexed note's source facts.
#[derive(Debug, Clone, Default)]
struct NoteMeta {
    host: Option<String>,
    authors: BTreeSet<String>,
    date: Option<NaiveDate>,
}

/// The identity map and per-note source facts, built once per step run.
#[derive(Debug, Clone, Default)]
pub struct SupersedeIndex {
    by_key: HashMap<String, BTreeSet<String>>,
    notes: HashMap<String, NoteMeta>,
}

fn json_strings(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(a) => a.iter().flat_map(json_strings).collect(),
        _ => Vec::new(),
    }
}

/// `[[Name|alias]]` / `Name` → `name`.
fn norm_author(a: &str) -> String {
    let a = a.trim().trim_start_matches("[[").trim_end_matches("]]");
    a.split('|').next().unwrap_or(a).trim().to_lowercase()
}

fn host_of(url: &str) -> Option<String> {
    cv_text::canonicalize_url(url)
        .map(|c| c.domain.trim_start_matches("www.").to_lowercase())
        .filter(|d| !d.is_empty())
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim().get(..10)?, "%Y-%m-%d").ok()
}

impl SupersedeIndex {
    /// Scans the index once: every note's `source` and `post-id` (identity), host, `author` and
    /// `published`/`created` date.
    pub fn build(ctx: &StepCtx<'_>) -> Result<Self, StepError> {
        let reader = ctx.index()?;
        let q = reader.query();
        let rows = |key: &str| {
            q.md_props_with_key(key)
                .map_err(|e| StepError::failed(format!("index: {e}")))
        };
        let mut idx = Self::default();
        for r in rows("source")? {
            for s in json_strings(&r.value) {
                idx.insert_identity(&r.path, &s);
                if let Some(h) = host_of(&s) {
                    idx.notes.entry(r.path.clone()).or_default().host = Some(h);
                }
            }
        }
        for r in rows("post-id")? {
            for s in json_strings(&r.value) {
                idx.insert_identity(&r.path, &s);
            }
        }
        for r in rows("author")? {
            let m = idx.notes.entry(r.path.clone()).or_default();
            m.authors.extend(
                json_strings(&r.value)
                    .iter()
                    .map(|a| norm_author(a))
                    .filter(|a| !a.is_empty()),
            );
        }
        // `published` wins over `created`.
        for key in ["created", "published"] {
            for r in rows(key)? {
                if let Some(d) = json_strings(&r.value).first().and_then(|s| parse_date(s)) {
                    idx.notes.entry(r.path.clone()).or_default().date = Some(d);
                }
            }
        }
        Ok(idx)
    }

    /// Records `path` under every identity key of `value` (a URL or a post id).
    pub fn insert_identity(&mut self, path: &str, value: &str) {
        for k in identity_keys(value) {
            self.by_key.entry(k).or_default().insert(path.to_owned());
        }
    }

    /// Notes matching any identity key of `urls`, sorted, `exclude` left out.
    #[must_use]
    pub fn identity_matches(&self, urls: &[String], exclude: Option<&str>) -> Vec<String> {
        let mut out = BTreeSet::new();
        for u in urls {
            for k in identity_keys(u) {
                if let Some(ps) = self.by_key.get(&k) {
                    out.extend(ps.iter().filter(|p| Some(p.as_str()) != exclude).cloned());
                }
            }
        }
        out.into_iter().collect()
    }

    /// Whether any note matches `url` by identity.
    #[must_use]
    pub fn has_identity(&self, url: &str) -> bool {
        !self.identity_matches(&[url.to_owned()], None).is_empty()
    }
}

/// An existing note's text split into what the engine needs.
struct Existing {
    side: Side,
    access: Option<String>,
    complete: Option<bool>,
}

fn load(ctx: &StepCtx<'_>, path: &str) -> Result<Option<Existing>, StepError> {
    let Some((bytes, _)) = ctx.read(path)? else {
        return Ok(None);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    let doc = FrontmatterDoc::new(text.as_str());
    let access = match doc.get("access") {
        Ok(Some(JsValue::Str(s))) => Some(s),
        _ => None,
    };
    let complete = match doc.get("complete") {
        Ok(Some(JsValue::Bool(b))) => Some(b),
        _ => None,
    };
    let body = doc.body();
    let side = Side {
        words: words(body),
        complete,
        shingles: Shingles::from_text(body),
    };
    Ok(Some(Existing {
        side,
        access,
        complete,
    }))
}

fn words(body: &str) -> u64 {
    u64::try_from(calculate_word_count(body)).unwrap_or(u64::MAX)
}

/// Content candidates of `inc` (same source, in window), own path excluded.
fn content_candidates(
    ctx: &StepCtx<'_>,
    idx: &SupersedeIndex,
    cfg: &SupersedeConfig,
    inc: &Incoming<'_>,
) -> Result<BTreeSet<String>, StepError> {
    let in_window = |d: Option<NaiveDate>| match (d, inc.published) {
        (Some(a), Some(b)) => (a - b).num_days().abs() <= i64::from(cfg.window_days),
        _ => true,
    };
    let host = inc
        .host
        .clone()
        .or_else(|| inc.urls.iter().find_map(|u| host_of(u)));
    let authors: BTreeSet<String> = inc
        .authors
        .iter()
        .map(|a| norm_author(a))
        .filter(|a| !a.is_empty())
        .collect();
    let mut out = BTreeSet::new();
    if let Some(h) = &host {
        for (p, m) in &idx.notes {
            if m.host.as_ref() != Some(h) || !in_window(m.date) {
                continue;
            }
            if !authors.is_empty() && !m.authors.is_empty() && m.authors.is_disjoint(&authors) {
                continue;
            }
            out.insert(p.clone());
        }
    }
    if let Some((kind, key)) = inc.source {
        let rows = ctx
            .state()
            .tracker_items_for_source(kind, key, 512)
            .map_err(|e| StepError::failed(format!("state: {e}")))?;
        for r in rows {
            let Some(p) = r.note_path else { continue };
            let date = idx
                .notes
                .get(&p)
                .and_then(|m| m.date)
                .or_else(|| parse_date(&r.day));
            if in_window(date) {
                out.insert(p);
            }
        }
    }
    if let Some(own) = inc.own_path {
        out.remove(own);
    }
    Ok(out)
}

/// Resolves `inc` against the vault (see the module doc). Replaces in place on `Replace`, and
/// finalizes the resolved note's tracker rows. The caller holds `intake::identity`.
pub fn resolve(
    ctx: &mut StepCtx<'_>,
    idx: &SupersedeIndex,
    cfg: &SupersedeConfig,
    inc: &Incoming<'_>,
) -> Result<Resolved, StepError> {
    let incoming = Side::new(words(inc.body_md), inc.complete, inc.body_md);
    let mut chosen: Option<(String, Existing, Decision)> = None;
    for p in idx.identity_matches(&inc.urls, inc.own_path) {
        if let Some(e) = load(ctx, &p)? {
            let d = decide(&e.side, &incoming, true, cfg.threshold_percent);
            chosen = Some((p, e, d));
            break;
        }
    }
    if chosen.is_none() {
        let mut best: Option<(f64, String, Existing)> = None;
        for p in content_candidates(ctx, idx, cfg, inc)? {
            let Some(e) = load(ctx, &p)? else { continue };
            let c = overlap(&e.side.shingles, &incoming.shingles);
            if c * 100.0 >= f64::from(cfg.threshold_percent.min(100))
                && best.as_ref().is_none_or(|(b, _, _)| c > *b)
            {
                best = Some((c, p, e));
            }
        }
        if let Some((_, p, e)) = best {
            let d = decide(&e.side, &incoming, false, cfg.threshold_percent);
            chosen = Some((p, e, d));
        }
    }
    let resolved = match chosen {
        None | Some((_, _, Decision::Distinct)) => Resolved::new_item(),
        Some((p, e, Decision::Keep)) => kept(p, &e),
        Some((p, e, Decision::Replace)) => {
            if replace(ctx, &p, inc, incoming.words)? {
                Resolved {
                    path: Some(p),
                    action: Action::Replaced,
                    access: inc.access.map(|a| a.as_str().to_owned()).or(e.access),
                    complete: inc.complete,
                }
            } else {
                kept(p, &e)
            }
        }
    };
    debug_assert!(
        resolved.path.is_none() || resolved.path.as_deref() != inc.own_path,
        "supersede resolved an item to its own path"
    );
    if let Some(p) = &resolved.path {
        finalize(ctx, p, &inc.urls, &resolved)?;
    }
    Ok(resolved)
}

fn kept(path: String, e: &Existing) -> Resolved {
    Resolved {
        path: Some(path),
        action: Action::Kept,
        access: e.access.clone(),
        complete: e.complete,
    }
}

/// The note text with its body replaced and the engine-owned keys set.
fn replaced_text(old: &str, inc: &Incoming<'_>, words: u64) -> Result<String, StepError> {
    let fm_err = |e: cv_note::FmError| StepError::failed(format!("frontmatter: {e}"));
    let doc = FrontmatterDoc::new(old);
    let head_len = old.len() - doc.body().len();
    let mut text = old[..head_len].to_owned();
    text.push('\n');
    text.push_str(inc.body_md.trim());
    text.push('\n');
    let mut doc = FrontmatterDoc::new(text);
    #[allow(clippy::cast_precision_loss)]
    doc.set("word-count", &JsValue::Num(words as f64))
        .map_err(fm_err)?;
    if let Some(a) = inc.access {
        doc.insert_after("word-count", "access", &JsValue::Str(a.as_str().to_owned()))
            .map_err(fm_err)?;
    }
    match inc.complete {
        Some(c) => {
            let anchor = if doc.get("access").map_err(fm_err)?.is_some() {
                "access"
            } else {
                "word-count"
            };
            doc.insert_after(anchor, "complete", &JsValue::Bool(c))
                .map_err(fm_err)?;
        }
        None => {
            doc.remove("complete").map_err(fm_err)?;
        }
    }
    doc.insert_after(
        "complete",
        "via",
        &JsValue::Str(inc.origin.via().to_owned()),
    )
    .map_err(fm_err)?;
    if let Some(p) = inc.publication
        && doc.get("publication").map_err(fm_err)?.is_none()
    {
        // After `author`; `insert_after` appends at the block end when `author` is absent.
        doc.insert_after("author", "publication", &JsValue::Str(p.to_owned()))
            .map_err(fm_err)?;
    }
    Ok(doc.into_text())
}

/// Replaces `path`'s body in place (CAS through [`StepCtx::modify`]). `false` when the old text
/// could not be saved first (the replace is aborted).
fn replace(
    ctx: &mut StepCtx<'_>,
    path: &str,
    inc: &Incoming<'_>,
    words: u64,
) -> Result<bool, StepError> {
    // `modify` borrows `ctx` mutably: the closure reaches the state db through its own handle.
    let state = std::sync::Arc::clone(ctx.state);
    let mut save_failed = false;
    ctx.modify(path, |old| {
        let new = replaced_text(old, inc, words)?;
        if new == old {
            return Ok(None);
        }
        let saved = {
            let probe = StateProbe(&state);
            save_old_with(&probe, path, old)
        };
        if let Err(e) = saved {
            tracing::warn!(error = %e, "supersede: saving the old body failed; replace aborted");
            save_failed = true;
            return Ok(None);
        }
        Ok(Some(new))
    })?;
    Ok(!save_failed)
}

/// The state handle `save_old_with` reads the db path from (a `modify` closure cannot borrow
/// the whole `StepCtx`).
struct StateProbe<'a>(&'a std::sync::Mutex<cv_state::StateDb>);

fn save_old_with(s: &StateProbe<'_>, path: &str, text: &str) -> std::io::Result<bool> {
    let db = crate::sync::lock(s.0).path();
    let Some(dir) = db.as_deref().and_then(std::path::Path::parent) else {
        tracing::warn!("supersede: state.db has no path; old body not saved");
        return Ok(false);
    };
    let dir = dir.join("superseded");
    std::fs::create_dir_all(&dir)?;
    let hash = cv_text::url_canon::short_hash(path);
    let ms = chrono::Utc::now().timestamp_millis();
    let file = dir.join(format!("{hash}-{ms}.md"));
    std::fs::write(&file, text)?;
    tracing::info!(path_hash = %hash, bytes = text.len(), "supersede: old body saved");
    Ok(true)
}

/// Writes the resolved evidence to the resolved note's tracker rows (by `note_path`, else by
/// URL), then ticks them only when the resolved body is complete (audit M2).
fn finalize(ctx: &StepCtx<'_>, path: &str, urls: &[String], r: &Resolved) -> Result<(), StepError> {
    let st = |e: cv_state::StateError| StepError::failed(format!("state: {e}"));
    let mut db = ctx.state();
    let mut rows = db.tracker_items_for_note(path).map_err(st)?;
    if rows.is_empty() {
        for u in urls {
            rows.extend(db.tracker_items_for_url(u).map_err(st)?);
        }
    }
    finalize_rows(&mut db, rows, r)
}

/// Finalize's evidence-then-tick write for an already-resolved row set (shared with
/// `intake_finalize`, plan intake-followups-mail-youtube WP-4): every row gets `r`'s
/// `access`/`complete` first, and is ticked only when `r.complete == Some(true)`.
pub(crate) fn finalize_rows(
    db: &mut cv_state::StateDb,
    rows: Vec<cv_state::TrackerItem>,
    r: &Resolved,
) -> Result<(), StepError> {
    let st = |e: cv_state::StateError| StepError::failed(format!("state: {e}"));
    for row in rows {
        let access = r.access.clone().or(row.access.clone());
        db.set_tracker_completeness(
            &row.source_kind,
            &row.source_key,
            &row.item_id,
            access.as_deref(),
            r.complete.or(row.complete),
            row.full_word_count,
        )
        .map_err(st)?;
        if r.complete == Some(true) {
            db.set_tracker_item_status(&row.source_kind, &row.source_key, &row.item_id, Some('x'))
                .map_err(st)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "supersede_tests.rs"]
mod tests;
