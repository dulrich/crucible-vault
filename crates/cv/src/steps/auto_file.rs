//! `auto_file` (plan `auto-intake-sprint` WP-5): dedup a fresh web clip through the supersede
//! engine ([`crate::supersede`], origin `Clip`), then file it by kind.
//!
//! 1. **Dedup.** The clip's own body (never an inherited completeness: audience != completeness)
//!    is resolved with `own_path = clip`, so a sole clip resolves `New` and the engine can never
//!    return the clip itself (asserted; a hard step failure in release).
//!    - `Replaced`: the target took the clip's body; the clip is moved to `.trash/` (recoverable,
//!      CAS on the hash read at the start). Done.
//!    - `Kept` with the clip's text contained in the target (same threshold and shingling as the
//!      engine): the clip is moved to `.trash/`. Done.
//!    - `Kept` otherwise: the clip stays and gets `duplicate-of: "[[target]]"`; filing continues.
//!    - `New`: filing continues.
//!
//!    The clip is removed only in the first two cases; anything ambiguous keeps it. The engine owns
//!    finalizing and ticking tracker rows; this step never ticks.
//! 2. **Kind** (first match wins):
//!    1. a `sources.toml` `[[serial]]` row, in config order, that matches the clip
//!       ([`cv_actions::pipelines::SerialSource::matches`]: prefix, else author) and whose `kind` is
//!       a `kinds.toml` key (a row with an undefined kind is skipped with a warning; plan
//!       round-11-feedback-todos Decision 4);
//!    2. a `[[newsletter]]` row's linked blog host or a `[[blog]]` row host (`kind` of the
//!       newsletter row, else `blog`);
//!    3. (2b) a `[[newsletter]]` row whose `hosts` lists the clip host (that row's `kind`;
//!       Decision 7);
//!    4. a YouTube video `source` (plan round-19-yt-cleanup-intake WP-2): `pipelines.toml`
//!       `[youtube] short_kind` (default `post`) when its `## Transcript` text is under
//!       `MIN_REFINE_WORDS`, else `long_kind` (default `transcript`); not gated by `enabled`;
//!    5. a `kinds.toml` kind whose `hosts` holds the clip host.
//!
//!    A match spawns `file-as-kind { kind }` as a child (move + tags + lint); no match leaves the
//!    clip in place.
//!
//! 3. **Enrichment** (plan round-16 WP-5, filing first): `localize` + describe run afterwards, as
//!    the `clip-enrich` child, on the note's final path: the supersede target (`Replaced`), or the
//!    clip left in place (no kind match, `filed = "already"`, `duplicate`). A filed clip is
//!    enriched by `file-as-kind`'s last step on the moved path; a trashed clip never is.
//!
//! The step never moves the clip itself. Outcome vars: `action` (`replaced|merged|duplicate|new|
//! missing`), `target`, `kind`, `child_job_id`, `enrich_job_id`. A clip that already sits in its kind's folder with
//! all of the kind's tags spawns no child and reports `kind` with `filed = "already"` (`action`
//! unchanged). The matched serial row's `publication()` (any kind), else the matched host row's,
//! is written to the clip when absent, before the child is spawned.

use cv_actions::pipelines::{SerialSource, SourcesConfig};
use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access;
use cv_note::{FrontmatterDoc, JsValue};
use cv_text::url_canon::{bare_host_name, canonical_bare_host};
use serde::Serialize;

use super::file::kind_def;
use super::params::{expand, get_str, get_str_list};
use super::{StepCtx, StepError, StepModule, StepOutcome};
use crate::supersede::{self, Action, Incoming, Origin, SupersedeIndex};

/// The action a kind match spawns.
const FILE_ACTION: &str = "file-as-kind";

/// The enrichment action (localize + describe under one shared key, plan round-16 WP-5) spawned
/// on the clip's final path when no `file-as-kind` child moves it.
const ENRICH_ACTION: &str = "clip-enrich";

/// The `auto_file` step (no params; unknown keys disarm, I5).
pub(crate) struct AutoFile;

impl StepModule for AutoFile {
    type Params = ();

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<(), String> {
        super::no_params(kind, t)
    }

    fn run((): (), ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(ctx)
    }
}

/// `auto_file` outcome vars.
#[derive(Default, Serialize)]
struct Vars {
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filed: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enrich_job_id: Option<String>,
}

/// The clip facts the kind rules read (every field optional).
#[derive(Debug, Clone, Default)]
pub(in crate::steps) struct Clip {
    /// Trimmed `source` text (a list is comma-joined, as the base expression's `toString`).
    pub(in crate::steps) source: String,
    /// The `author` entries, raw.
    pub(in crate::steps) authors: Vec<String>,
    /// Canonical host, `www.` stripped.
    pub(in crate::steps) host: Option<String>,
}

/// A configured host (`www.fanfiction.net`, or a full URL) in the same form as
/// [`canonical_bare_host`]: a full URL goes through it, a bare host through `bare_host_name`.
fn configured_host(s: &str) -> Option<String> {
    let s = s.trim();
    if s.contains("://") {
        canonical_bare_host(s)
    } else {
        bare_host_name(s)
    }
}

/// The row a clip host matched in `sources.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::steps) struct HostRow {
    /// The matched `[[blog]]` row's `(name, link)`.
    pub(in crate::steps) blog: Option<(String, String)>,
    /// The kind: a linked newsletter row's, else `blog`.
    pub(in crate::steps) kind: Option<String>,
    /// Paywall markers of the matched rows.
    pub(in crate::steps) markers: Vec<String>,
    /// The matched `[[blog]]` row's `publication` (`short`, else `name`; plan
    /// intake-round-2-clean-dedupe-reading WP-5b).
    pub(in crate::steps) publication: Option<String>,
}

/// Matches the clip host against the `[[newsletter]]` rows' linked blog host and the `[[blog]]`
/// rows' host (feed URL host); failing those, step (2b): the first `[[newsletter]]` row whose
/// `hosts` lists it (its `kind`, markers and publication; no blog key).
pub(in crate::steps) fn host_row(host: &str, sources: &SourcesConfig) -> Option<HostRow> {
    let Some(blog) = sources
        .blog
        .iter()
        .find(|b| canonical_bare_host(&b.link).as_deref() == Some(host))
    else {
        let n = sources.newsletter.iter().find(|n| {
            n.hosts
                .iter()
                .filter_map(|h| configured_host(h))
                .any(|h| h == host)
        })?;
        return Some(HostRow {
            blog: None,
            kind: Some(n.kind.clone()),
            markers: n.paywall_markers.clone(),
            publication: Some(n.publication().to_owned()),
        });
    };
    let mut row = HostRow {
        blog: Some((blog.name.clone(), blog.link.trim().to_owned())),
        kind: Some("blog".into()),
        markers: blog.paywall_markers.clone(),
        publication: Some(blog.publication().to_owned()),
    };
    if let Some(n) = sources
        .newsletter
        .iter()
        .find(|n| n.blog.as_deref() == Some(blog.name.as_str()))
    {
        row.kind = Some(n.kind.clone());
        row.markers.extend(n.paywall_markers.iter().cloned());
    }
    Some(row)
}

/// The first `[[serial]]` row (config order) that matches the clip and whose `kind` `is_kind`;
/// a matching row with an undefined kind is skipped with a warning (never coerced).
pub(in crate::steps) fn serial_hit<'a>(
    serial: &'a [SerialSource],
    clip: &Clip,
    is_kind: impl Fn(&str) -> bool,
) -> Option<&'a SerialSource> {
    let authors: Vec<&str> = clip.authors.iter().map(String::as_str).collect();
    serial
        .iter()
        .filter(|s| s.matches(&authors, &clip.source))
        .find(|s| {
            let ok = is_kind(s.kind.trim());
            if !ok {
                tracing::warn!(serial = %s.name, kind = %s.kind, "auto_file: serial row kind is not defined in kinds.toml; row skipped");
            }
            ok
        })
}

/// The kind whose `hosts` holds `host` (kinds in key order).
pub(in crate::steps) fn hosts_kind(
    config: &ConfigSet,
    host: &str,
) -> Result<Option<String>, StepError> {
    let Some(kinds) = config
        .table("kinds.toml")
        .get("kinds")
        .and_then(toml::Value::as_table)
    else {
        return Ok(None);
    };
    for (name, def) in kinds {
        let Some(def) = def.as_table() else { continue };
        let hosts = get_str_list(def, "hosts").map_err(StepError::failed)?;
        if hosts
            .iter()
            .filter_map(|h| configured_host(h))
            .any(|h| h == host)
        {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

/// Moves `path` to `.trash/` (recoverable, CAS on `hash`), recording that `by` absorbed it so
/// the `deleted` notification carries `replaced_by` (round 11 WP-1, Decision 1).
pub(crate) fn trash(
    ctx: &mut StepCtx<'_>,
    path: &str,
    hash: cv_vault::ContentHash,
    by: &str,
) -> Result<(), StepError> {
    let (p, by) = (path.to_owned(), by.to_owned());
    ctx.with_writer(&[path], move |w| w.delete_replaced(&p, hash, &by))??;
    Ok(())
}

/// Sets `publication` on the clip when absent (one CAS modify; never overwrites).
fn set_publication(ctx: &mut StepCtx<'_>, path: &str, p: &str) -> Result<(), StepError> {
    ctx.modify(path, |old| {
        let mut d = FrontmatterDoc::new(old);
        if d.get("publication").ok().flatten().is_some() {
            return Ok(None);
        }
        d.insert_after("author", "publication", &JsValue::Str(p.to_owned()))?;
        Ok(Some(d.into_text()))
    })?;
    Ok(())
}

/// Whether the clip already sits in `kind`'s folder (expanded as `file-as-kind`'s `move` does)
/// and carries all of the kind's tags, so filing it would change nothing.
fn already_filed(ctx: &StepCtx<'_>, path: &str, kind: &str) -> Result<bool, StepError> {
    let def = kind_def(ctx.config, kind).map_err(StepError::failed)?;
    let Some(folder) = get_str(def, "folder").map_err(StepError::failed)? else {
        return Ok(false);
    };
    let folder = expand(ctx, &folder)?;
    if cv_text::vpath::parent(path) != folder.trim().trim_matches('/') {
        return Ok(false);
    }
    let want = get_str_list(def, "tags").map_err(StepError::failed)?;
    let Some((bytes, _)) = ctx.read(path)? else {
        return Ok(false);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(false);
    };
    let have: Vec<String> = FrontmatterDoc::new(text.as_str())
        .get_texts("tags")
        .iter()
        .map(|t| t.trim().trim_start_matches('#').to_owned())
        .collect();
    Ok(want
        .iter()
        .all(|w| have.iter().any(|h| h == w.trim().trim_start_matches('#'))))
}

/// Executes the step.
fn run(ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut out = StepOutcome::default();
    let mut vars = Vars {
        action: "missing",
        ..Vars::default()
    };
    let Some(clip_path) = ctx
        .context
        .active_path
        .clone()
        .filter(|p| !p.trim().is_empty())
    else {
        out.set_vars(&vars);
        return Ok(out);
    };
    // Only notes are filed: an attachment (a clipped `.webp`, say) is a no-op, not a UTF-8 failure.
    if !clip_path.ends_with(".md") {
        vars.action = "skipped";
        out.set_vars(&vars);
        return Ok(out);
    }
    // Machine folders (`_raw_transcript/`, `_custom/`, …) are never filed, superseded or trashed.
    if clip_path.starts_with('_') {
        vars.action = "skipped";
        out.set_vars(&vars);
        return Ok(out);
    }
    let Some((bytes, clip_hash)) = ctx.read(&clip_path)? else {
        out.set_vars(&vars);
        return Ok(out);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| StepError::failed(format!("{clip_path} is not UTF-8")))?;
    let doc = FrontmatterDoc::new(text.as_str());
    let get = |k: &str| doc.get(k).ok().flatten();
    let source = get("source")
        .map(|v| cv_note::js_text(&v))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let clip = Clip {
        host: canonical_bare_host(&source),
        authors: doc.get_texts("author"),
        source,
    };
    let published = get("published")
        .map(|v| cv_note::js_text(&v))
        .and_then(|s| s.trim().get(..10).map(str::to_owned))
        .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
    let body = doc.body().to_owned();

    let sources = ctx.config.sources().clone();
    let row = clip.host.as_deref().and_then(|h| host_row(h, &sources));

    // ---- dedup --------------------------------------------------------------------------------
    let words = body.split_whitespace().count();
    let markers = row.as_ref().map(|r| r.markers.clone()).unwrap_or_default();
    let complete = access::completeness(
        None,
        u32::try_from(words).unwrap_or(u32::MAX),
        None,
        words > 0,
        access::marker_hit(&body, &markers),
        ctx.config.pipelines().blogs.complete_percent,
    );
    let sup_cfg = ctx.config.pipelines().supersede.clone();
    let idx = SupersedeIndex::build(ctx).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "auto_file: supersede index unavailable");
        SupersedeIndex::default()
    });
    let blog_key = row
        .as_ref()
        .and_then(|r| r.blog.as_ref())
        .map(|(_, link)| link.clone());
    let inc = Incoming {
        own_path: Some(&clip_path),
        urls: Some(clip.source.clone())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect(),
        source: blog_key.as_deref().map(|k| ("blog", k)),
        authors: clip.authors.clone(),
        host: clip.host.clone(),
        published,
        body_md: &body,
        access: None,
        complete,
        publication: row.as_ref().and_then(|r| r.publication.as_deref()),
        origin: Origin::Clip,
    };
    let resolved = supersede::resolve(ctx, &idx, &sup_cfg, &inc)?;
    debug_assert!(
        resolved.path.as_deref() != Some(clip_path.as_str()),
        "auto_file: supersede resolved a clip to itself"
    );
    if resolved.path.as_deref() == Some(clip_path.as_str()) {
        return Err(StepError::failed(
            "auto_file: supersede resolved a clip to itself",
        ));
    }
    vars.action = "new";
    vars.target.clone_from(&resolved.path);
    match (resolved.action, resolved.path) {
        (Action::Replaced, Some(target)) => {
            trash(ctx, &clip_path, clip_hash, &target)?;
            // The target took the clip's body: enrich it in place (the trashed clip never is).
            vars.enrich_job_id = enrich(ctx, &target);
            out.push_output(target);
            vars.action = "replaced";
            out.set_vars(&vars);
            return Ok(out);
        }
        (Action::Kept, Some(target)) => {
            let target_body = ctx
                .read(&target)?
                .and_then(|(b, _)| String::from_utf8(b).ok())
                .map(|t| FrontmatterDoc::new(t.as_str()).body().to_owned());
            let contained = target_body.is_some_and(|tb| {
                cv_text::similar::containment(&body, &tb) * 100.0
                    >= f64::from(sup_cfg.threshold_percent.min(100))
            });
            if contained {
                trash(ctx, &clip_path, clip_hash, &target)?;
                vars.action = "merged";
                out.set_vars(&vars);
                return Ok(out);
            }
            let link = format!("[[{}]]", target.strip_suffix(".md").unwrap_or(&target));
            ctx.modify(&clip_path, |old| {
                let mut d = FrontmatterDoc::new(old);
                Ok(d.set("duplicate-of", &JsValue::Str(link.clone()))?
                    .then(|| d.into_text()))
            })?;
            out.push_output(clip_path.clone());
            vars.action = "duplicate";
        }
        _ => {}
    }

    // ---- kind ---------------------------------------------------------------------------------
    file_by_kind(
        ctx,
        &clip_path,
        &clip,
        &body,
        row.as_ref(),
        &sources,
        &mut vars,
    )?;
    // No `file-as-kind` child: the clip stays where it is, so it is enriched in place. A filed
    // clip is enriched by `file-as-kind`'s own last step, on the moved path.
    if vars.child_job_id.is_none() {
        vars.enrich_job_id = enrich(ctx, &clip_path);
    }
    out.set_vars(&vars);
    Ok(out)
}

/// The kind of a YouTube clip (plan round-19-yt-cleanup-intake WP-2): `short_kind` when its
/// `## Transcript` text (outside managed enrich blocks) is under
/// [`cv_ingest::transcript::MIN_REFINE_WORDS`] words — the floor refine skips, so every Short gets
/// the mechanical `cleanup_transcript` lint — else `long_kind` (also with no transcript section).
/// The word count sums every segment, a split section included.
fn youtube_kind(yt: &cv_actions::pipelines::YoutubePipeline, body: &str) -> String {
    use cv_ingest::transcript::{Eligibility, MIN_REFINE_WORDS, eligibility};
    let e = eligibility(body);
    let short = e != Eligibility::NoSection && e.words() < MIN_REFINE_WORDS;
    let kind = if short { &yt.short_kind } else { &yt.long_kind };
    kind.trim().to_owned()
}

/// Enqueues [`ENRICH_ACTION`] (localize + describe) on `path`; a failure to enqueue is logged,
/// never fatal (filing already happened; the backfills cover the images later).
fn enrich(ctx: &StepCtx<'_>, path: &str) -> Option<String> {
    match ctx
        .children
        .spawn_child_at(ENRICH_ACTION, serde_json::Map::new(), path)
    {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "auto_file: could not enqueue clip-enrich");
            None
        }
    }
}

/// Step 2: picks the clip's kind and spawns `file-as-kind` (sets `vars.kind`, `vars.filed`,
/// `vars.child_job_id`); no match, or an already-filed clip, leaves it in place.
fn file_by_kind(
    ctx: &mut StepCtx<'_>,
    clip_path: &str,
    clip: &Clip,
    body: &str,
    row: Option<&HostRow>,
    sources: &SourcesConfig,
    vars: &mut Vars,
) -> Result<(), StepError> {
    let is_kind = |k: &str| kind_def(ctx.config, k).is_ok();
    let mut work_pub = None;
    let kind = if let Some(s) = serial_hit(&sources.serial, clip, is_kind) {
        work_pub = Some(s.publication().trim().to_owned()).filter(|p| !p.is_empty());
        Some(s.kind.trim().to_owned())
    } else if let Some(k) = row.and_then(|r| r.kind.clone()) {
        Some(k)
    } else if cv_text::url_canon::is_youtube_video(&clip.source) {
        Some(youtube_kind(&ctx.config.pipelines().youtube, body))
    } else if let Some(h) = &clip.host {
        hosts_kind(ctx.config, h)?
    } else {
        None
    };
    let Some(kind) = kind else {
        return Ok(());
    };
    if !is_kind(&kind) {
        tracing::info!(%kind, "auto_file: kind is not defined in kinds.toml; clip left in place");
        return Ok(());
    }
    let allowed = ctx
        .config
        .action(FILE_ACTION)
        .and_then(|a| a.params.iter().find(|p| p.name == "kind"))
        .is_some_and(|p| p.choices.contains(&kind));
    if !allowed {
        tracing::info!(%kind, "auto_file: kind is not a file-as-kind choice; clip left in place");
        return Ok(());
    }
    // A serial row's publication (any kind, round-11 Decision 4) wins over a host row's; never
    // overwrites.
    if let Some(p) = work_pub
        .as_deref()
        .or_else(|| row.and_then(|r| r.publication.as_deref()))
    {
        set_publication(ctx, clip_path, p)?;
    }
    if already_filed(ctx, clip_path, &kind)? {
        vars.filed = Some("already");
        vars.kind = Some(kind);
        return Ok(());
    }
    let mut params = serde_json::Map::new();
    params.insert("kind".into(), serde_json::Value::String(kind.clone()));
    match ctx.children.spawn_child(FILE_ACTION, params) {
        Ok(id) => vars.child_job_id = Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "auto_file: could not enqueue file-as-kind; clip left in place");
        }
    }
    vars.kind = Some(kind);
    Ok(())
}
