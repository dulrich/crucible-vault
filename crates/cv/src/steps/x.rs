//! `x_discover`, `x_metadata` and `x_backfill` (plan `v0-automation-pipelines` WP-5): X oEmbed
//! metadata notes under `_x_metadata/`.
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/xApi.ts` (`ensureXMetadataNote :252-300`,
//! `findExistingXMetadataNote :158-167`), `workflows/XMetadataFetchWorkflow.ts` (stamp the
//! source notes' `x-metadata` list), `workflows/XPostDiscoverWorkflow.ts` (`collectXStatuses`)
//! with the inbox guard of `utils/xDiscoverTrigger.ts:28-45`, and `workflows/XBackfillWorkflow.ts
//! :35-60`. Dropped as Obsidian-induced: the `x-post::<id>` resource lock and note locks (the
//! single writer's exact-name absence check + CAS writes replace them), the metadata-cache reads,
//! and the discover/backfill enqueue pacing.
//!
//! **Inline, not enqueue:** Crucible's discover/backfill enqueue one `x_metadata_fetch` job per
//! status. Here a step can only spawn a configured *action* (`ChildSpawner`), and no
//! `x_metadata` action id is guaranteed to exist, so `x_discover`/`x_backfill` run the same
//! find-or-create + stamp **inline per status**, bounded by `limit`. A 429/5xx defers the whole
//! job; on resume, notes already written are found, never refetched.
//!
//! Outcome vars — `x_metadata`: `status_id`, `metadata_path`, `created`, `tombstone`, `linked`;
//! `x_discover`: `found`, `ensured`; `x_backfill`: `pending`, `ensured`, `remaining`.

use std::collections::BTreeSet;

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::x::{XPost, folder_slug, metadata_note, tombstone_note};
use cv_note::{FrontmatterDoc, JsValue};
use cv_providers::web::FetchError;
use cv_providers::web::x as api;
use cv_text::url_canon::{canonical_x_status_url, extract_x_status_from_url};
use serde::Serialize;

use super::metadata::{Ensured, MetadataKind, iso_now, link_metadata, link_onto, str_prop};
use super::params::{check_keys, check_placeholders, expand, get_str};
use super::web::{
    api_base, block_on, client, create_or_found, find_in_subfolders, link_targets, record_usage,
};
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// Statuses ensured per `x_discover`/`x_backfill` run by default.
pub const DEFAULT_LIMIT: i64 = 50;
/// The frontmatter list stamped onto source notes.
const LIST_KEY: &str = "x-metadata";
/// The list is inserted after this key when present (Crucible's X notes carry it).
const ANCHOR: &str = "x-status-id";

fn limit(t: &toml::Table) -> Result<usize, String> {
    match t.get("limit") {
        None => Ok(DEFAULT_LIMIT as usize),
        Some(toml::Value::Integer(n)) if *n > 0 => Ok(*n as usize),
        Some(_) => Err("`limit` must be an integer > 0".into()),
    }
}

/// Parsed `x_metadata` / `x_discover` / `x_backfill` params.
pub(crate) enum Params {
    /// `x_metadata`.
    Metadata { status_id: Option<String> },
    /// `x_discover`.
    Discover { limit: usize },
    /// `x_backfill`.
    Backfill { limit: usize },
}

/// `x_metadata` / `x_discover` / `x_backfill` (unknown keys disarm, I5).
pub(crate) struct X;

impl StepModule for X {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, kind.as_str())?;
        Ok(match kind {
            StepKind::XMetadata => {
                let status_id = get_str(t, "status_id")?;
                if let Some(s) = &status_id {
                    check_placeholders(s)?;
                }
                Params::Metadata { status_id }
            }
            StepKind::XDiscover => Params::Discover { limit: limit(t)? },
            _ => Params::Backfill { limit: limit(t)? },
        })
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p {
            Params::Metadata { status_id } => metadata(status_id.as_deref(), ctx),
            Params::Discover { limit } => discover(limit, ctx),
            Params::Backfill { limit } => backfill(limit, ctx),
        }
    }
}

/// `x_discover` outcome vars.
#[derive(Serialize)]
struct DiscoverVars {
    found: usize,
    ensured: usize,
}

/// `x_backfill` outcome vars.
#[derive(Serialize)]
struct BackfillVars {
    pending: usize,
    ensured: usize,
    remaining: usize,
}

/// The X [`MetadataKind`].
const KIND: MetadataKind = MetadataKind {
    id_var: "status_id",
    anchor: ANCHOR,
    list_key: LIST_KEY,
    default_root: "_x_metadata",
    valid: valid_id,
};

fn metadata_root(ctx: &StepCtx<'_>) -> String {
    KIND.root(&ctx.config.pipelines().x.metadata_root)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}

/// `ensureXMetadataNote`: `(path, created, tombstone)`. An existing note (or tombstone) is found
/// without a fetch; 404/403 tombstones (done, never the breaker); 429/5xx/network defer.
fn ensure_note(
    ctx: &mut StepCtx<'_>,
    id: &str,
    canonical_url: &str,
) -> Result<(String, bool, bool), StepError> {
    let root = metadata_root(ctx);
    let unavailable = format!("{root}/_unavailable/");
    if let Some(p) = find_in_subfolders(ctx, &root, &format!("{id}.md"))? {
        let tomb = p.starts_with(&unavailable);
        return Ok((p, false, tomb));
    }
    let c = client()?;
    let base = api_base(api::SERVICE, api::DEFAULT_BASE);
    let r = block_on(api::oembed(c, &base, canonical_url))?;
    let o = match r {
        Ok(o) => {
            record_usage(ctx, api::SERVICE, 1);
            o
        }
        Err(FetchError::NotFound { .. }) => {
            record_usage(ctx, api::SERVICE, 1);
            let path = format!("{unavailable}{id}.md");
            let made = create_or_found(
                ctx,
                &path,
                tombstone_note(id, canonical_url, &iso_now()).into_bytes(),
            )?;
            return Ok((path, made, true));
        }
        Err(e) => return Err(StepError::from_fetch(&e)),
    };
    let path = format!(
        "{root}/{}/{id}.md",
        folder_slug(&o.author_url, canonical_url)
    );
    let post = XPost::from(o);
    let body = metadata_note(id, canonical_url, &post, &iso_now()).into_bytes();
    let created = create_or_found(ctx, &path, body)?;
    Ok((path, created, false))
}

fn metadata(status_id: Option<&str>, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let explicit = match status_id {
        Some(s) => expand(ctx, s)?.trim().to_owned(),
        None => String::new(),
    };
    let mut out = StepOutcome::default();
    let resolve = |doc: Option<&FrontmatterDoc>| {
        let source = doc
            .and_then(|d| str_prop(d.get("source").ok().flatten()))
            .and_then(|s| extract_x_status_from_url(&s));
        let id = if explicit.is_empty() {
            doc.and_then(|d| str_prop(d.get(ANCHOR).ok().flatten()))
                .or_else(|| source.as_ref().map(|r| r.status_id.clone()))
                .unwrap_or_default()
        } else {
            explicit.clone()
        };
        (id, source)
    };
    link_metadata(&KIND, ctx, &mut out, resolve, |ctx, id, source| {
        let handle = source.filter(|r| r.status_id == id).and_then(|r| r.handle);
        let url = canonical_x_status_url(handle.as_deref(), id);
        let (path, created, tombstone) = ensure_note(ctx, id, &url)?;
        Ok(Ensured {
            path,
            created,
            tombstone,
            output: created,
            extra: (),
        })
    })?;
    Ok(out)
}

/// `isPathUnderFolder`: `path` is `folder` or under it (boundary-safe).
fn under(path: &str, folder: &str) -> bool {
    let root = folder.trim_matches('/');
    !root.is_empty() && (path == root || path.starts_with(&format!("{root}/")))
}

/// `collectXStatuses`: distinct statuses in `content`, first-seen URL wins → `(id, canonical)`.
fn collect_statuses(content: &str) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in cv_ingest::extract_urls(content) {
        if let Some(r) = extract_x_status_from_url(&raw)
            && seen.insert(r.status_id.clone())
        {
            let url = canonical_x_status_url(r.handle.as_deref(), &r.status_id);
            out.push((r.status_id, url));
        }
    }
    out
}

fn discover(lim: usize, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut out = StepOutcome::default();
    out.set_vars(&DiscoverVars {
        found: 0,
        ensured: 0,
    });
    let inbox = ctx.config.pipelines().x.inbox.clone().unwrap_or_default();
    let Some(note) = ctx.context.active_path.clone() else {
        return Ok(out);
    };
    if !note.ends_with(".md") || !under(&note, &inbox) {
        return Ok(out);
    }
    let Some((bytes, _)) = ctx.read(&note)? else {
        return Ok(out);
    };
    let statuses = collect_statuses(&String::from_utf8_lossy(&bytes));
    let found = statuses.len();
    let mut ensured = 0usize;
    for (id, url) in statuses.into_iter().take(lim) {
        let (path, created, _) = ensure_note(ctx, &id, &url)?;
        if created {
            out.push_output(path.clone());
        }
        if link_onto(&KIND, ctx, &note, &path)? {
            out.push_output(note.clone());
        }
        ensured += 1;
    }
    out.set_vars(&DiscoverVars { found, ensured });
    Ok(out)
}

fn backfill(lim: usize, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let records = ctx
        .state()
        .link_records_with_x()
        .map_err(|e| StepError::failed(format!("link records: {e}")))?;
    // Aggregate by status id: first record's URL, union of source notes.
    let mut agg: Vec<(String, String, Vec<String>)> = Vec::new();
    for r in records {
        let id = r.x_status_id.as_deref().unwrap_or("").trim().to_owned();
        if !valid_id(&id) {
            continue;
        }
        let i = match agg.iter().position(|(a, _, _)| *a == id) {
            Some(i) => i,
            None => {
                let handle = extract_x_status_from_url(&r.canonical_url)
                    .filter(|x| x.status_id == id)
                    .and_then(|x| x.handle);
                agg.push((
                    id.clone(),
                    canonical_x_status_url(handle.as_deref(), &id),
                    Vec::new(),
                ));
                agg.len() - 1
            }
        };
        for s in link_targets(Some(&JsValue::Arr(
            r.source_notes.into_iter().map(JsValue::Str).collect(),
        ))) {
            let p = format!("{s}.md");
            if !agg[i].2.contains(&p) {
                agg[i].2.push(p);
            }
        }
    }
    let root = metadata_root(ctx);
    let mut pending = Vec::new();
    for a in agg {
        if find_in_subfolders(ctx, &root, &format!("{}.md", a.0))?.is_none() {
            pending.push(a);
        }
    }
    let mut out = StepOutcome::default();
    let total = pending.len();
    let mut ensured = 0usize;
    for (id, url, sources) in pending.into_iter().take(lim) {
        let (path, created, _) = ensure_note(ctx, &id, &url)?;
        if created {
            out.push_output(path.clone());
        }
        for s in sources {
            if link_onto(&KIND, ctx, &s, &path)? {
                out.push_output(s);
            }
        }
        ensured += 1;
    }
    out.set_vars(&BackfillVars {
        pending: total,
        ensured,
        remaining: total.saturating_sub(ensured),
    });
    Ok(out)
}

#[cfg(test)]
#[path = "x_tests.rs"]
mod tests;
