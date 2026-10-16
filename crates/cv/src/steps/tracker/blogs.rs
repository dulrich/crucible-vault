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
//! error settles deferred (`fetch_disposition`). Outcome vars: `sources`, `new`, `lines`,
//! `notes`, `failed`, `invalid` (+ `disabled` when `[blogs] enabled = false`). No step params.

use std::collections::{BTreeSet, VecDeque};

use cv_actions::pipelines::{BlogSource, BodyMode};
use cv_actions::{ConfigSet, Step};
use cv_ingest::blogs::{self as policy, BlogPost};
use cv_state::TrackerOutcome;
use serde_json::json;

use super::super::file::kind_def;
use super::super::materialize::{expand, get_str, get_str_list, join, now};
use super::super::web::{client, create_or_found};
use super::super::{StepCtx, StepError, StepOutcome, lint};
use super::{Candidate, SeenSet, emit_line_with, fetch_bounded, md_files_under, record_with};

/// `tracker_items.source_kind` / `ignored_ids.kind` of blog posts.
pub const KIND: &str = "blog";
/// The index prop carrying a captured note's post id.
pub const PROP: &str = "post-id";
/// Breaker / usage service of feed GETs.
pub const SERVICE: &str = "blog-feed";
/// Crucible's blog metadata root (read-only seeding input).
pub const METADATA_ROOT: &str = "_blog_metadata";
/// The `kinds.toml` kind a captured post is filed as.
pub const NOTE_KIND: &str = "blog";

/// Validates the step's params (none accepted; unknown keys disarm, I5).
pub fn validate(step: &Step, _config: &ConfigSet) -> Result<(), String> {
    crate::steps::reject_params(step)
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

fn vars(out: &mut StepOutcome, counts: [(&str, usize); 6]) {
    for (k, v) in counts {
        out.vars.insert(k.into(), json!(v));
    }
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
    for t in &row.tags {
        let t = t.trim().trim_start_matches('#');
        if !t.is_empty() && !tags.iter().any(|x| x == t) {
            tags.push(t.to_owned());
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

/// The path for `post`'s note in `folder`: `<slug>.md`, or `<slug>-<hash>.md` when another
/// note holds the slug. `(path, exists)`.
fn note_path(
    ctx: &StepCtx<'_>,
    folder: &str,
    post: &BlogPost,
) -> Result<(String, bool), StepError> {
    let slug = policy::note_slug(post);
    let first = join(folder, &format!("{slug}.md"));
    match is_post_note(ctx, &first, post)? {
        None => Ok((first, false)),
        Some(true) => Ok((first, true)),
        Some(false) => {
            let alt = join(
                folder,
                &format!(
                    "{slug}-{}.md",
                    cv_text::url_canon::short_hash(&post.post_id)
                ),
            );
            let exists = is_post_note(ctx, &alt, post)?.is_some();
            Ok((alt, exists))
        }
    }
}

/// Writes (or recovers) `post`'s captured note and lints it. Returns whether it was written.
fn write_note(
    ctx: &mut StepCtx<'_>,
    row: &BlogSource,
    post: &BlogPost,
    out_paths: &mut Vec<String>,
) -> Result<bool, StepError> {
    let (folder, tags) = note_target(ctx, row)?;
    let (path, exists) = note_path(ctx, &folder, post)?;
    if exists {
        return Ok(false);
    }
    let created = now().format("%Y-%m-%d").to_string();
    let text = policy::captured_note(post, &tags, &created);
    if !create_or_found(ctx, &path, text.into_bytes())? {
        return Ok(false);
    }
    out_paths.push(path.clone());
    let mut t = toml::Table::new();
    t.insert("type".into(), "lint".into());
    t.insert("path".into(), path.into());
    if kind_def(ctx.config, NOTE_KIND).is_ok() {
        t.insert("kind".into(), NOTE_KIND.into());
    }
    let step = Step::from_table(t).map_err(StepError::failed)?;
    lint::execute(&step, ctx)?;
    #[cfg(test)]
    if super::hook::fail_now() {
        return Err(StepError::failed("injected failure after note write"));
    }
    Ok(true)
}

/// Executes the step.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    validate(step, ctx.config).map_err(StepError::failed)?;
    let mut out = StepOutcome::default();
    let cfg = ctx.config.pipelines().blogs.clone();
    if !cfg.enabled {
        out.vars.insert("disabled".into(), json!(true));
        vars(
            &mut out,
            [
                ("sources", 0),
                ("new", 0),
                ("lines", 0),
                ("notes", 0),
                ("failed", 0),
                ("invalid", 0),
            ],
        );
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
    let index = cv_index::Reader::open_current(ctx.index_db).ok();
    let source_seen = |url: &str| {
        index.as_ref().is_some_and(|r| {
            r.query()
                .files_with_prop("source", &json!(url))
                .is_ok_and(|rows| !rows.is_empty())
        })
    };

    let c = client()?;
    let bodies = fetch_bounded(
        rows.iter().map(|r| r.link.trim().to_owned()).collect(),
        |u| async move { c.get_text(SERVICE, &u).await },
    )?;

    let (mut new, mut lines, mut notes, mut failed) = (0, 0, 0, 0);
    let mut deferred = None;
    for (row, body) in rows.iter().zip(bodies) {
        let xml = match body {
            Ok(x) => x,
            Err(e) => {
                failed += 1;
                tracing::warn!(source = %row.name, error = %e, "track_blogs: feed failed");
                let d = super::super::fetch_disposition(&e);
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
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tracker_items_for_source(KIND, &key, window.saturating_mul(4).max(64))
            .map_err(|e| StepError::failed(format!("state: {e}")))?
            .into_iter()
            .rev()
            .filter_map(|r| r.full_body)
            .collect();
        // Feeds list newest first; surface oldest first.
        for post in posts.into_iter().rev() {
            let cand = Candidate {
                source_kind: KIND.into(),
                source_key: key.clone(),
                source_name: row.name.clone(),
                item_id: post.post_id.clone(),
                title: post.title.clone(),
                url: post.url.clone(),
            };
            let full = Some(post.has_body());
            let recorded = ctx
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tracker_item(KIND, &key, &post.post_id)
                .map_err(|e| StepError::failed(format!("state: {e}")))?
                .is_some();
            if recorded {
                continue;
            }
            let already = seen.contains(&post.post_id)
                || seen.contains(&post.url_id)
                || source_seen(&post.url);
            let auto = automatable(row.body, &evidence, window) && post.has_body();
            if already {
                // Replay: this run's own captured note (crash between write and record).
                let mut outcome = TrackerOutcome::Skipped;
                if auto {
                    let (folder, _) = note_target(ctx, row)?;
                    if note_path(ctx, &folder, &post)?.1 {
                        outcome = TrackerOutcome::Note;
                    }
                }
                record_with(ctx, &cand, outcome, full)?;
                evidence.push_back(post.has_body());
                continue;
            }
            new += 1;
            seen.insert(&post.post_id);
            if auto {
                if write_note(ctx, row, &post, &mut out.output_paths)? {
                    notes += 1;
                }
                record_with(ctx, &cand, TrackerOutcome::Note, full)?;
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
    vars(
        &mut out,
        [
            ("sources", rows.len()),
            ("new", new),
            ("lines", lines),
            ("notes", notes),
            ("failed", failed),
            ("invalid", invalid),
        ],
    );
    Ok(out)
}

#[cfg(test)]
#[path = "blogs_tests.rs"]
mod tests;
