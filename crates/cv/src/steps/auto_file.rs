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
//! 2. **Kind** (first match wins): a Works note (`core/works`, the `works.toml` `matchClause`
//!    semantics, see [`works_match`]) whose tags hold a `kinds.toml` key; a `[[newsletter]]` row's
//!    linked blog host or a `[[blog]]` row host (`kind` of the newsletter row, else `blog`); a
//!    `kinds.toml` kind whose `hosts` holds the clip host. A match spawns `file-as-kind { kind }`
//!    as a child (move + tags + lint); no match leaves the clip in place.
//!
//! The step never moves the clip itself. Outcome vars: `action` (`replaced|merged|duplicate|new|
//! missing`), `target`, `kind`, `child_job_id`. A clip that already sits in its kind's folder with
//! all of the kind's tags spawns no child and reports `kind` with `filed = "already"` (`action`
//! unchanged). A matched `[[blog]]` row's `publication` is written to the clip when absent, before
//! the child is spawned.

use cv_actions::pipelines::SourcesConfig;
use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access;
use cv_note::{FrontmatterDoc, JsValue};
use serde::Serialize;

use super::file::kind_def;
use super::params::{expand, get_str, get_str_list};
use super::{StepCtx, StepError, StepModule, StepOutcome};
use crate::supersede::{self, Action, Incoming, Origin, SupersedeIndex};

/// The action a kind match spawns.
const FILE_ACTION: &str = "file-as-kind";
/// The folder whose notes are the Works index.
pub(in crate::steps) const WORKS_FOLDER: &str = "core/works";

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

/// One Works note's filters and tags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::steps) struct Work {
    pub(in crate::steps) path: String,
    pub(in crate::steps) author_filter: Option<String>,
    pub(in crate::steps) source_prefixes: Vec<String>,
    pub(in crate::steps) tags: Vec<String>,
    /// The Works note's `title` (plan round-9-feedback-todos Decision 2: the `publication` of a
    /// fanfic clip it files).
    pub(in crate::steps) title: Option<String>,
}

/// The kind whose clips take the matched Work's `title` as `publication` (Decision 2).
pub(in crate::steps) const FANFIC_KIND: &str = "fanfic";

fn js_text(v: &JsValue) -> String {
    match v {
        JsValue::Str(s) => s.clone(),
        JsValue::Num(n) => n.to_string(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Arr(a) => a.iter().map(js_text).collect::<Vec<_>>().join(","),
        JsValue::Null | JsValue::Obj(_) => String::new(),
    }
}

/// `list(v)`: an array's entries, a scalar as one entry, null as none.
fn js_list(v: Option<JsValue>) -> Vec<String> {
    match v {
        None | Some(JsValue::Null) => Vec::new(),
        Some(JsValue::Arr(a)) => a.iter().map(js_text).collect(),
        Some(other) => vec![js_text(&other)],
    }
}

/// Canonical host of `url`, lowercase, leading `www.` stripped.
fn host_of(url: &str) -> Option<String> {
    cv_text::canonicalize_url(url)
        .map(|c| c.domain.trim_start_matches("www.").to_lowercase())
        .filter(|d| !d.is_empty())
}

/// A configured host (`www.fanfiction.net`, or a full URL) in the same form as [`host_of`].
fn config_host(s: &str) -> Option<String> {
    let s = s.trim();
    if s.contains("://") {
        host_of(s)
    } else {
        Some(s.trim_start_matches("www.").to_lowercase()).filter(|h| !h.is_empty())
    }
}

/// The `works.toml` `matchClause` semantics, as raw string comparisons: a work with neither
/// filter matches nothing; the author filter (trimmed, `#` removed) must be among the clip's
/// authors (each trimmed, `#` removed); some non-empty prefix must start the trimmed `source`;
/// both are ANDed when both are set.
pub(in crate::steps) fn works_match(work: &Work, clip: &Clip) -> bool {
    let author_filter = work
        .author_filter
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty());
    let prefixes: Vec<&str> = work
        .source_prefixes
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if author_filter.is_none() && prefixes.is_empty() {
        return false;
    }
    if let Some(a) = author_filter {
        let want = a.replace('#', "");
        if !clip
            .authors
            .iter()
            .any(|c| c.trim().replace('#', "") == want)
        {
            return false;
        }
    }
    prefixes.is_empty() || prefixes.iter().any(|p| clip.source.starts_with(p))
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
/// rows' host (feed URL host).
pub(in crate::steps) fn host_row(host: &str, sources: &SourcesConfig) -> Option<HostRow> {
    let blog = sources
        .blog
        .iter()
        .find(|b| host_of(&b.link).as_deref() == Some(host))?;
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

/// The first Works note (sorted by path) that matches the clip and carries a tag that `is_kind`,
/// with that kind.
pub(in crate::steps) fn works_hit<'a>(
    works: &'a [Work],
    clip: &Clip,
    is_kind: impl Fn(&str) -> bool,
) -> Option<(&'a Work, String)> {
    let mut sorted: Vec<&Work> = works.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    sorted
        .into_iter()
        .filter(|w| works_match(w, clip))
        .find_map(|w| {
            w.tags
                .iter()
                .map(|t| t.trim().trim_start_matches('#'))
                .find(|t| is_kind(t))
                .map(|k| (w, k.to_owned()))
        })
}

/// The `publication` a Works match gives a clip: the Work's `title`, for the fanfic kind only.
pub(in crate::steps) fn work_publication(work: &Work, kind: &str) -> Option<String> {
    (kind == FANFIC_KIND)
        .then(|| work.title.as_deref().map(str::trim))
        .flatten()
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
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
            .filter_map(|h| config_host(h))
            .any(|h| h == host)
        {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

/// The Works notes: markdown files directly in `core/works`.
pub(in crate::steps) fn read_works(ctx: &StepCtx<'_>) -> Result<Vec<Work>, StepError> {
    let rows = ctx
        .index()?
        .query()
        .md_files_under(WORKS_FOLDER)
        .map_err(|e| StepError::failed(format!("index: {e}")))?;
    let mut out = Vec::new();
    for r in rows.into_iter().filter(|r| r.folder == WORKS_FOLDER) {
        let Some((bytes, _)) = ctx.read(&r.path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let doc = FrontmatterDoc::new(text);
        let get = |k: &str| doc.get(k).ok().flatten();
        let author_filter = get("author-filter")
            .map(|v| js_text(&v))
            .filter(|s| !s.trim().is_empty());
        out.push(Work {
            path: r.path.clone(),
            author_filter,
            source_prefixes: js_list(get("source-prefix-filter")),
            tags: js_list(get("tags")),
            title: get("title")
                .map(|v| js_text(&v).trim().to_owned())
                .filter(|s| !s.is_empty()),
        });
    }
    Ok(out)
}

/// Moves `path` to `.trash/` (recoverable, CAS on `hash`).
fn trash(ctx: &mut StepCtx<'_>, path: &str, hash: cv_vault::ContentHash) -> Result<(), StepError> {
    let p = path.to_owned();
    ctx.with_writer(&[path], move |w| w.delete(&p, hash))??;
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
    let have: Vec<String> = js_list(
        FrontmatterDoc::new(text.as_str())
            .get("tags")
            .ok()
            .flatten(),
    )
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
    let Some((bytes, clip_hash)) = ctx.read(&clip_path)? else {
        out.set_vars(&vars);
        return Ok(out);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| StepError::failed(format!("{clip_path} is not UTF-8")))?;
    let doc = FrontmatterDoc::new(text.as_str());
    let get = |k: &str| doc.get(k).ok().flatten();
    let source = get("source")
        .map(|v| js_text(&v))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let clip = Clip {
        host: host_of(&source),
        authors: js_list(get("author")),
        source,
    };
    let published = get("published")
        .map(|v| js_text(&v))
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
            trash(ctx, &clip_path, clip_hash)?;
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
                trash(ctx, &clip_path, clip_hash)?;
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
    let is_kind = |k: &str| kind_def(ctx.config, k).is_ok();
    let works = read_works(ctx)?;
    let mut work_pub = None;
    let kind = if let Some((w, k)) = works_hit(&works, &clip, is_kind) {
        work_pub = work_publication(w, &k);
        Some(k)
    } else if let Some(k) = row.as_ref().and_then(|r| r.kind.clone()) {
        Some(k)
    } else if let Some(h) = &clip.host {
        hosts_kind(ctx.config, h)?
    } else {
        None
    };
    let Some(kind) = kind else {
        out.set_vars(&vars);
        return Ok(out);
    };
    if !is_kind(&kind) {
        tracing::info!(%kind, "auto_file: kind is not defined in kinds.toml; clip left in place");
        out.set_vars(&vars);
        return Ok(out);
    }
    let allowed = ctx
        .config
        .action(FILE_ACTION)
        .and_then(|a| a.params.iter().find(|p| p.name == "kind"))
        .is_some_and(|p| p.choices.contains(&kind));
    if !allowed {
        tracing::info!(%kind, "auto_file: kind is not a file-as-kind choice; clip left in place");
        out.set_vars(&vars);
        return Ok(out);
    }
    // A fanfic Work's title wins over a host row's publication (Decision 2); never overwrites.
    if let Some(p) = work_pub
        .as_deref()
        .or_else(|| row.as_ref().and_then(|r| r.publication.as_deref()))
    {
        set_publication(ctx, &clip_path, p)?;
    }
    if already_filed(ctx, &clip_path, &kind)? {
        vars.filed = Some("already");
        vars.kind = Some(kind);
        out.set_vars(&vars);
        return Ok(out);
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
    out.set_vars(&vars);
    Ok(out)
}
