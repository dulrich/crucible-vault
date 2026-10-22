//! `track_blogs` (plan `v0-automation-pipelines` WP-8): the blog feed tracker.
//!
//! Per tracked `sources.toml` `[[blog]]` row (not `skip`/`ignore`, method `rss`, http(s) link):
//! one GET of the feed, RSS/Atom parsing ([`cv_ingest::feed`]) and Crucible's post policy
//! ([`cv_ingest::blogs`]: post-id, canon, body mode, podcast vs article). Each post not in the
//! [`SeenSet`] (kind `blog`: `tracker_items` + `ignored_ids` + index `post-id`, plus index
//! `source` = the post URL) is surfaced once:
//!
//! - **automatable** (plan Decisions locked: `body = "full"`, or `auto` with each of the
//!   source's last `[blogs] automatable_window` posts carrying a full body) → a captured blog
//!   note in the `blog` kind's folder (default `{period.day}`), shaped like a filed blog entry,
//!   then linted (the `lint` step in-process) and recorded `Note`;
//! - otherwise → one daily intake line ([`super::emit_line_with`]), recorded `Line`.
//!
//! Automatable evidence lives in `tracker_items.full_body` of the source's rows (`source_key` =
//! feed URL): every fetched post gets a row — new posts with their outcome, already-seen posts
//! as `Skipped` — carrying whether it had a full body, so the window survives restarts.
//!
//! Crucible reference: `src/orchestration/utils/blogs.ts` (`fetchBlogFeed`, `parseRssOrAtom`),
//! `feedSources.ts:170-200` (seen-set from `post-id`/`source`), `FeedTrackerWorkflow.ts`.
//! Dropped (Obsidian-induced): `_blog_metadata` notes (no longer written), per-run intake notes.
//!
//! Failures: an invalid row (unsupported method, non-http(s) link) is counted `invalid` and
//! logged; a feed failing (HTTP or parse) is counted `failed`; every feed failing with a service
//! error settles deferred (`StepError::from_fetch`). Outcome vars: `sources`, `new`, `lines`,
//! `notes`, `failed`, `invalid`, `localize_failed`, `probe_failed` (+ `disabled` when `[blogs] enabled = false`).
//! No step params. A full-body captured note is localized inline (remote images → `_MD5`
//! attachments); a localize failure is logged and counted, never fails the capture.
//!
//! Access/completeness (plan `auto-intake-sprint` WP-2): each new `/p/<slug>` post is probed at
//! `/api/v1/posts/<slug>` ([`PROBE_SERVICE`], one bounded batch per feed); the row and the captured
//! note carry `access`/`complete` ([`cv_ingest::access::completeness`]), and a `note` outcome that
//! is complete is recorded done (`status = x`). A probe failure is counted `probe_failed` and the
//! post stays open (`access = unknown`); it never fails the step or holds back the line/note.

use std::collections::{BTreeSet, HashMap, VecDeque};

use cv_actions::pipelines::{BlogSource, BodyMode};
use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access::{self, Access};
use cv_ingest::blogs::{self as policy, BlogPost};
use cv_protocol::TrackerOutcome;
use serde::Serialize;

use super::super::file::kind_def;
use super::super::materialize::{join, now};
use super::super::params::{expand, get_str, get_str_list};
use super::super::util::{fetch_bounded, md_files_under};
use super::super::web::{client, create_or_found};
use super::super::{StepCtx, StepError, StepModule, StepOutcome, lint, localize};
use super::substack;
use super::{Candidate, SeenSet, emit_line_with, record_with};
use crate::supersede::{
    self, Action as SupersedeAction, Incoming, Origin, Resolved, SupersedeIndex,
};

/// `tracker_items.source_kind` / `ignored_ids.kind` of blog posts.
pub const KIND: &str = "blog";
/// The index prop carrying a captured note's post id.
pub const PROP: &str = "post-id";
/// Breaker / usage service of feed GETs.
pub const SERVICE: &str = "blog-feed";
/// Breaker / usage service of Substack post probes (plan auto-intake-sprint WP-2).
pub const PROBE_SERVICE: &str = "substack-api";
/// Crucible's blog metadata root (read-only seeding input).
pub const METADATA_ROOT: &str = "_blog_metadata";
/// The `kinds.toml` kind a captured post is filed as.
pub const NOTE_KIND: &str = "blog";

/// The `track_blogs` step (no params; unknown keys disarm, I5).
pub(crate) struct TrackBlogs;

impl StepModule for TrackBlogs {
    type Params = ();

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<(), String> {
        super::super::no_params(kind, t)
    }

    fn run((): (), ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(ctx)
    }
}

/// `track_blogs` outcome vars.
#[derive(Default, Serialize)]
struct Vars {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
    sources: usize,
    new: usize,
    lines: usize,
    notes: usize,
    failed: usize,
    invalid: usize,
    localize_failed: usize,
    probe_failed: usize,
}

fn canon_opts() -> cv_text::url_canon::PostIdOptions {
    cv_text::url_canon::PostIdOptions::default()
}

fn unquote(v: &str) -> String {
    v.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_owned()
}

/// First-run seed: `post-id`/`source` of `_blog_metadata/**` notes, and every URL (as written
/// and canonicalized) in Crucible's `_crucible/orchestration/**` notes. Filed blog notes are
/// matched per candidate through the index (`post-id`, `source`).
fn seed(ctx: &StepCtx<'_>) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let add_url = |ids: &mut BTreeSet<String>, u: &str| {
        let u = u.trim();
        if u.is_empty() {
            return;
        }
        ids.insert(u.to_owned());
        ids.insert(cv_text::url_canon::post_id_from_url(u, &canon_opts()));
    };
    for p in md_files_under(ctx, METADATA_ROOT) {
        let Ok(text) = std::fs::read_to_string(ctx.vault_root.join(&p)) else {
            continue;
        };
        for line in text.lines().take(60) {
            if let Some(v) = line.strip_prefix("post-id:") {
                let v = unquote(v);
                if !v.is_empty() {
                    ids.insert(v);
                }
            } else if let Some(v) = line.strip_prefix("source:") {
                add_url(&mut ids, &unquote(v));
            }
        }
    }
    for p in md_files_under(ctx, super::youtube::CRUCIBLE_ROOT) {
        if let Ok(text) = std::fs::read_to_string(ctx.vault_root.join(&p)) {
            for u in cv_ingest::extract_urls(&text) {
                add_url(&mut ids, &u);
            }
        }
    }
    ids
}

fn body_mode(m: BodyMode) -> policy::BodyMode {
    match m {
        BodyMode::Auto => policy::BodyMode::Auto,
        BodyMode::Full => policy::BodyMode::Full,
        BodyMode::Snippet => policy::BodyMode::Snippet,
    }
}

fn row_is_valid(r: &BlogSource) -> Result<(), String> {
    if !r.method.trim().eq_ignore_ascii_case("rss") {
        return Err(format!("unsupported method {:?}", r.method));
    }
    let l = r.link.trim();
    if cv_text::canonicalize_url(l).is_none() {
        return Err("link is not an http(s) URL".into());
    }
    Ok(())
}

/// Whether the source is automatable for its next post: `full` always; `auto` when the last
/// `window` evidence entries (oldest → newest) are all full bodies.
fn automatable(mode: BodyMode, evidence: &VecDeque<bool>, window: usize) -> bool {
    match mode {
        BodyMode::Full => true,
        BodyMode::Snippet => false,
        BodyMode::Auto => {
            evidence.len() >= window && evidence.iter().rev().take(window).all(|b| *b)
        }
    }
}

/// The captured note's folder (the `blog` kind's `folder`, default `{period.day}`) and tags.
fn note_target(ctx: &StepCtx<'_>, row: &BlogSource) -> Result<(String, Vec<String>), StepError> {
    let (folder, mut tags) = match kind_def(ctx.config, NOTE_KIND) {
        Ok(def) => (
            get_str(def, "folder")
                .map_err(StepError::failed)?
                .unwrap_or_else(|| "{period.day}".into()),
            get_str_list(def, "tags").map_err(StepError::failed)?,
        ),
        Err(_) => ("{period.day}".into(), vec![NOTE_KIND.to_owned()]),
    };
    if tags.is_empty() {
        tags.push(NOTE_KIND.into());
    }
    for t in cv_text::template::normalize_tags(&row.tags) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    Ok((expand(ctx, &folder)?, tags))
}

/// Whether the note at `path` is this post's captured note.
fn is_post_note(ctx: &StepCtx<'_>, path: &str, post: &BlogPost) -> Result<Option<bool>, StepError> {
    Ok(ctx.read(path)?.map(|(b, _)| {
        let t = String::from_utf8_lossy(&b);
        t.lines().any(|l| {
            l.strip_prefix("post-id:")
                .is_some_and(|v| unquote(v) == post.post_id)
        })
    }))
}

/// The path for `post`'s note in `folder`: `<name>.md` (the title filename), or
/// `<name> <hash>.md` when another note holds the name (deterministic, so re-runs stay
/// idempotent). `(path, exists)`.
fn note_path(
    ctx: &StepCtx<'_>,
    folder: &str,
    post: &BlogPost,
) -> Result<(String, bool), StepError> {
    let slug = policy::note_name(post);
    let first = join(folder, &format!("{slug}.md"));
    match is_post_note(ctx, &first, post)? {
        None => Ok((first, false)),
        Some(true) => Ok((first, true)),
        Some(false) => {
            let alt = join(
                folder,
                &format!(
                    "{slug} {}.md",
                    cv_text::url_canon::short_hash(&post.post_id)
                ),
            );
            let exists = is_post_note(ctx, &alt, post)?.is_some();
            Ok((alt, exists))
        }
    }
}

/// Writes (or recovers) `post`'s captured note and lints it. Returns whether it was written,
/// and the note's vault-relative path.
fn write_note(
    ctx: &mut StepCtx<'_>,
    row: &BlogSource,
    post: &BlogPost,
    ev: &Evidence,
    out_paths: &mut Vec<String>,
    localize_failed: &mut usize,
) -> Result<(bool, String), StepError> {
    let (folder, tags) = note_target(ctx, row)?;
    let (path, exists) = note_path(ctx, &folder, post)?;
    if exists {
        return Ok((false, path));
    }
    let created = now().format("%Y-%m-%d").to_string();
    let text = policy::captured_note(
        post,
        &tags,
        &created,
        ev.access,
        ev.complete,
        Some(row.publication()),
    );
    if !create_or_found(ctx, &path, text.into_bytes())? {
        return Ok((false, path));
    }
    out_paths.push(path.clone());
    // Inline localize (plan usability-sprint-captures-links WP-1): remote images become local
    // `_MD5` attachments now; a failure leaves the remote refs and never fails the capture.
    if let Err(e) = localize::run(&localize::Params::note(path.clone()), ctx) {
        *localize_failed += 1;
        tracing::warn!(path = %path, error = %e, "track_blogs: inline localize failed");
    }
    let kind = kind_def(ctx.config, NOTE_KIND)
        .is_ok()
        .then(|| NOTE_KIND.to_owned());
    lint::run(&lint::Params::note(path.clone(), kind), ctx)?;
    #[cfg(test)]
    if super::hook::fail_now() {
        return Err(StepError::failed("injected failure after note write"));
    }
    Ok((true, path))
}

/// A probe's result: `Some((access, full words))`, or `None` when it failed (error, non-200,
/// unparseable body).
type Probe = Option<(Access, Option<u32>)>;

/// Probes, in one bounded batch (`fetch_bounded`, as for feeds), every `/p/<slug>` post of a
/// feed that is neither recorded nor `seen` (plan auto-intake-sprint WP-2). Keyed by post id;
/// posts without a probe URL are absent. A failure is a `None` value, never an error.
fn probe_new_posts(
    ctx: &StepCtx<'_>,
    posts: &[BlogPost],
    key: &str,
    seen: impl Fn(&BlogPost) -> bool,
) -> Result<HashMap<String, Probe>, StepError> {
    let mut targets = Vec::new();
    for p in posts {
        let Some(u) = substack::probe_url(&p.url) else {
            continue;
        };
        let recorded = ctx
            .state()
            .tracker_item(KIND, key, &p.post_id)
            .map_err(|e| StepError::failed(format!("state: {e}")))?
            .is_some();
        if !recorded && !seen(p) {
            targets.push((p.post_id.clone(), u));
        }
    }
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let c = client()?;
    let bodies = fetch_bounded(
        targets.iter().map(|(_, u)| u.clone()).collect(),
        |u| async move { c.get_text(PROBE_SERVICE, &u).await },
    )?;
    Ok(targets
        .into_iter()
        .zip(bodies)
        .map(|((id, u), body)| {
            let probe = match body {
                Ok(b) => substack::parse_post_meta(&b)
                    .map(|(aud, wc)| (Access::from_substack_audience(&aud), wc)),
                Err(e) => {
                    tracing::warn!(url = %u, error = %e, "track_blogs: post probe failed");
                    None
                }
            };
            (id, probe)
        })
        .collect())
}

/// A new post's access/completeness evidence.
struct Evidence {
    access: Option<Access>,
    complete: Option<bool>,
    full_words: Option<u32>,
    probe_failed: bool,
}

/// The evidence of `post` from its probe (`None` = not probed) and the row's paywall markers.
fn evidence_for(
    post: &BlogPost,
    probe: Option<&Probe>,
    row: &BlogSource,
    complete_percent: u32,
) -> Evidence {
    let (access, full_words, probe_failed) = match probe {
        None => (None, None, false),
        Some(None) => (Some(Access::Unknown), None, true),
        Some(Some((a, wc))) => (Some(*a), *wc, false),
    };
    let delivered = post.body_html.as_deref().map_or(0, |h| {
        u32::try_from(cv_ingest::word_count(h)).unwrap_or(u32::MAX)
    });
    let marker = post.body_html.as_deref().is_some_and(|h| {
        access::marker_hit(&cv_ingest::html::html_to_text(h), &row.paywall_markers)
    });
    Evidence {
        access,
        complete: access::completeness(
            access,
            delivered,
            full_words,
            post.has_body(),
            marker,
            complete_percent,
        ),
        full_words,
        probe_failed,
    }
}

/// Runs the supersede engine for a feed post whose identity matches an existing note (origin
/// `Tracker`, no own path). The post was not probed (identity-matched posts are filtered out of
/// the probe batch), so a probeable post's incoming `complete` is only ever `false` (marker or
/// no body), never an unverified `true`.
fn supersede_existing(
    ctx: &mut StepCtx<'_>,
    sidx: &SupersedeIndex,
    cfg: &cv_actions::pipelines::SupersedeConfig,
    row: &BlogSource,
    key: &str,
    post: &BlogPost,
    complete_percent: u32,
) -> Result<Resolved, StepError> {
    let ev = evidence_for(post, None, row, complete_percent);
    let complete = if substack::probe_url(&post.url).is_some() {
        ev.complete.filter(|c| !*c)
    } else {
        ev.complete
    };
    let body_md = post
        .body_html
        .as_deref()
        .map(cv_ingest::html_to_markdown)
        .unwrap_or_default();
    let inc = Incoming {
        own_path: None,
        urls: vec![post.url.clone()],
        source: Some((KIND, key)),
        authors: post.authors.clone(),
        host: None,
        published: chrono::NaiveDate::parse_from_str(
            post.published.get(..10).unwrap_or(""),
            "%Y-%m-%d",
        )
        .ok(),
        body_md: &body_md,
        access: None,
        complete,
        publication: Some(row.publication()),
        origin: Origin::Tracker,
    };
    supersede::resolve(ctx, sidx, cfg, &inc)
}

/// Executes the step.
pub(crate) fn run(ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut out = StepOutcome::default();
    let cfg = ctx.config.pipelines().blogs.clone();
    if !cfg.enabled {
        out.set_vars(&Vars {
            disabled: true,
            ..Vars::default()
        });
        return Ok(out);
    }
    let window = usize::try_from(cfg.automatable_window.max(1)).unwrap_or(usize::MAX);
    let mut invalid = 0;
    let rows: Vec<BlogSource> = ctx
        .config
        .sources()
        .blog
        .iter()
        .filter(|r| !r.priority.is_excluded())
        .filter(|r| match row_is_valid(r) {
            Ok(()) => true,
            Err(e) => {
                invalid += 1;
                tracing::warn!(source = %r.name, error = %e, "track_blogs: invalid row skipped");
                false
            }
        })
        .cloned()
        .collect();
    let mut seen = SeenSet::load(ctx, KIND, PROP, seed)?;
    super::heal_source_names(
        ctx,
        KIND,
        rows.iter().map(|r| (r.link.trim(), r.name.as_str())),
    )?;
    // Canonical identity seen-check (plan auto-intake-sprint WP-3): an index note whose
    // canonicalized `source`/`post-id` matches the post URL. Built once per run.
    let sidx = SupersedeIndex::build(ctx).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "track_blogs: supersede index unavailable");
        SupersedeIndex::default()
    });
    let sup_cfg = ctx.config.pipelines().supersede.clone();
    let source_seen = |url: &str| sidx.has_identity(url);

    let c = client()?;
    let bodies = fetch_bounded(
        rows.iter().map(|r| r.link.trim().to_owned()).collect(),
        |u| async move { c.get_text(SERVICE, &u).await },
    )?;

    let (mut new, mut lines, mut notes, mut failed, mut localize_failed) = (0, 0, 0, 0, 0);
    let mut probe_failed = 0;
    let mut deferred = None;
    for (row, body) in rows.iter().zip(bodies) {
        let xml = match body {
            Ok(x) => x,
            Err(e) => {
                failed += 1;
                tracing::warn!(source = %row.name, error = %e, "track_blogs: feed failed");
                let d = StepError::from_fetch(&e);
                if matches!(d, StepError::Deferred { .. }) && deferred.is_none() {
                    deferred = Some(d);
                }
                continue;
            }
        };
        let items = match cv_ingest::parse_feed(&xml) {
            Ok(v) => v,
            Err(e) => {
                failed += 1;
                tracing::warn!(source = %row.name, error = %e, "track_blogs: feed unparseable");
                continue;
            }
        };
        let key = row.link.trim().to_owned();
        let posts = policy::posts_from_feed(&items, row.canon.method(), body_mode(row.body));
        // Stored evidence, oldest → newest.
        let mut evidence: VecDeque<bool> = ctx
            .state()
            .tracker_items_for_source(KIND, &key, window.saturating_mul(4).max(64))
            .map_err(|e| StepError::failed(format!("state: {e}")))?
            .into_iter()
            .rev()
            .filter_map(|r| r.full_body)
            .collect();
        let probes = probe_new_posts(ctx, &posts, &key, |p| {
            seen.contains(&p.post_id) || seen.contains(&p.url_id) || source_seen(&p.url)
        })?;
        // Feeds list newest first; surface oldest first.
        for post in posts.into_iter().rev() {
            let mut cand = Candidate {
                source_kind: KIND.into(),
                source_key: key.clone(),
                source_name: row.name.clone(),
                item_id: post.post_id.clone(),
                title: post.title.clone(),
                url: post.url.clone(),
                duration_s: None,
                word_count: post
                    .body_html
                    .as_deref()
                    .map(|h| i64::try_from(cv_ingest::word_count(h)).unwrap_or(i64::MAX)),
                access: None,
                complete: None,
                full_word_count: None,
            };
            let full = Some(post.has_body());
            let recorded = ctx
                .state()
                .tracker_item(KIND, &key, &post.post_id)
                .map_err(|e| StepError::failed(format!("state: {e}")))?
                .is_some();
            if recorded {
                // Length backfill for rows recorded before v7: the store fills only a NULL
                // `word_count` of a `line`/`note` row (plan intake-type-column-length-backfill).
                if let Some(wc) = cand.word_count {
                    ctx.state()
                        .fill_tracker_word_count(KIND, &key, &post.post_id, wc)
                        .map_err(|e| StepError::failed(format!("state: {e}")))?;
                }
                continue;
            }
            let already = seen.contains(&post.post_id)
                || seen.contains(&post.url_id)
                || source_seen(&post.url);
            let auto = automatable(row.body, &evidence, window) && post.has_body();
            if already {
                // Replay: this run's own captured note (crash between write and record).
                let mut outcome = TrackerOutcome::Skipped;
                let mut path = None;
                // Identity match (plan auto-intake-sprint WP-3): the supersede engine decides
                // keep vs replace-in-place; the row carries the resolved body's evidence.
                if source_seen(&post.url) {
                    let r = supersede_existing(
                        ctx,
                        &sidx,
                        &sup_cfg,
                        row,
                        &key,
                        &post,
                        cfg.complete_percent,
                    )?;
                    cand.access = r.access.clone();
                    cand.complete = r.complete;
                    if r.action == SupersedeAction::Replaced {
                        outcome = TrackerOutcome::Note;
                        path.clone_from(&r.path);
                        if let Some(p) = &r.path {
                            out.output_paths.push(p.clone());
                            if let Err(e) = localize::run(&localize::Params::note(p.clone()), ctx) {
                                localize_failed += 1;
                                tracing::warn!(path = %p, error = %e, "track_blogs: inline localize failed");
                            }
                        }
                    }
                }
                if path.is_none() && auto {
                    let (folder, _) = note_target(ctx, row)?;
                    let (p, exists) = note_path(ctx, &folder, &post)?;
                    if exists {
                        outcome = TrackerOutcome::Note;
                        path = Some(p);
                    }
                }
                let inserted = record_with(ctx, &cand, outcome, full, None, path)?;
                // Ticked only on the resolved body's predicate (audit M2), never on a match alone.
                if inserted && outcome == TrackerOutcome::Note && cand.complete == Some(true) {
                    ctx.state()
                        .set_tracker_item_status(KIND, &key, &post.post_id, Some('x'))?;
                }
                evidence.push_back(post.has_body());
                continue;
            }
            new += 1;
            seen.insert(&post.post_id);
            let ev = evidence_for(&post, probes.get(&post.post_id), row, cfg.complete_percent);
            probe_failed += usize::from(ev.probe_failed);
            cand.access = ev.access.map(|a| a.as_str().to_owned());
            cand.complete = ev.complete;
            cand.full_word_count = ev.full_words.map(i64::from);
            if auto {
                let (written, path) = write_note(
                    ctx,
                    row,
                    &post,
                    &ev,
                    &mut out.output_paths,
                    &mut localize_failed,
                )?;
                if written {
                    notes += 1;
                }
                let score = super::score_item(ctx, &cand);
                let inserted =
                    record_with(ctx, &cand, TrackerOutcome::Note, full, score, Some(path))?;
                // Auto-done when complete (plan auto-intake-sprint Decisions locked).
                if inserted && ev.complete == Some(true) {
                    ctx.state()
                        .set_tracker_item_status(KIND, &key, &post.post_id, Some('x'))?;
                }
            } else if emit_line_with(ctx, &cand, full, &mut out.output_paths)?
                != super::Emitted::AlreadyRecorded
            {
                lines += 1;
            }
            evidence.push_back(post.has_body());
        }
    }
    if !rows.is_empty()
        && failed == rows.len()
        && let Some(d) = deferred
    {
        return Err(d);
    }
    out.output_paths.sort();
    out.output_paths.dedup();
    out.set_vars(&Vars {
        disabled: false,
        sources: rows.len(),
        new,
        lines,
        notes,
        failed,
        invalid,
        localize_failed,
        probe_failed,
    });
    Ok(out)
}

#[cfg(test)]
#[path = "blogs_tests.rs"]
mod tests;
