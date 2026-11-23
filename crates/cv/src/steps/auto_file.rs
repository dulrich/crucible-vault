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
//!    1. a `sources.toml` record matching the clip, walked in the one publication order
//!       ([`SourcesConfig::matches`]: any record's URL prefix, then any record's host in canonical
//!       form, then a prefix-less record's author, config order within each step). A prefix- or
//!       author-matched record files as its `kind` when that is a `kinds.toml` key; one with an
//!       undefined kind is skipped with a warning and the next match is tried (plan
//!       round-11-feedback-todos Decision 4). A host-matched record files as its `kind`, else
//!       `blog`;
//!    2. a YouTube video `source` (plan round-19-yt-cleanup-intake WP-2): `pipelines.toml`
//!       `[youtube] short_kind` (default `post`) when its `## Transcript` text is under
//!       `MIN_REFINE_WORDS`, else `long_kind` (default `transcript`); not gated by `enabled`;
//!    3. a `kinds.toml` kind whose `hosts` holds the clip host.
//!
//!    A match spawns `file-as-kind { kind }` as a child (move + tags + lint); no match leaves the
//!    clip in place.
//!
//!    **Link-only transcript** (Decision 6): when the kept target is a complete, free tracker post
//!    whose body is one single-link line and the clip has at least `MIN_REFINE_WORDS` words
//!    ([`super::transcript_link::is_link_only`]), the clip gets `transcript-of: "[[target]]"` (no
//!    `duplicate-of`) and its body under one `## Transcript` section, is filed under the configured
//!    long kind, and the post then gets `transcript: "[[clip]]"` at the clip's filed path (and the
//!    clip's `publication` when its own is blank); `action = "transcript"`. The two edits are
//!    [`super::transcript_link::link_pair`]'s.
//! 3. **Enrichment** (plan round-16 WP-5, filing first): `localize` + describe run afterwards, as
//!    the `clip-enrich` child, on the note's final path: the supersede target (`Replaced`), or the
//!    clip left in place (no kind match, `filed = "already"`, `duplicate`). A filed clip is
//!    enriched by `file-as-kind`'s last step on the moved path; a trashed clip never is.
//!
//! The step never moves the clip itself. Outcome vars: `action` (`replaced|merged|duplicate|transcript|new|
//! missing|skipped`), `target`, `kind`, `child_job_id`, `enrich_job_id`. A clip that already sits in its kind's folder with
//! all of the kind's tags spawns no child and reports `kind` with `filed = "already"` (`action`
//! unchanged). The publication of the record that gave the kind is written to the clip when
//! absent, before the child is spawned.

use cv_actions::pipelines::{Publication, SourcesConfig};
use cv_actions::sources::PubMatch;
use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access;
use cv_note::{FrontmatterDoc, JsValue};
use cv_text::url_canon::{canonical_bare_host, configured_host};
use serde::Serialize;

use super::file::kind_def;
use super::params::{expand, get_str, get_str_list};
use super::transcript_link::{LinkPair, link_pair, publication_of, target_is_link_only};
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

/// The clip facts of a note's frontmatter (`source`, `author`).
pub(in crate::steps) fn clip_of(doc: &FrontmatterDoc) -> Clip {
    let source = doc
        .get("source")
        .ok()
        .flatten()
        .map(|v| cv_note::js_text(&v))
        .unwrap_or_default()
        .trim()
        .to_owned();
    Clip {
        host: canonical_bare_host(&source),
        authors: doc.get_texts("author"),
        source,
    }
}

/// The publication of a note with clip facts `clip`: [`SourcesConfig::resolve`]'s record, else
/// the tracker-row publication `tracked`.
pub(in crate::steps) fn publication_for(
    sources: &SourcesConfig,
    clip: &Clip,
    tracked: Option<&str>,
) -> Option<String> {
    let authors: Vec<&str> = clip.authors.iter().map(String::as_str).collect();
    sources
        .resolve(&authors, &clip.source)
        .map(|m| m.publication().publication().to_owned())
        .or_else(|| tracked.map(str::to_owned))
}

/// The row a clip host matched in `sources.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::steps) struct HostRow {
    /// The matched record's `(name, feed link)`.
    pub(in crate::steps) blog: Option<(String, String)>,
    /// The record's kind ([`Publication::kind_or_blog`]).
    pub(in crate::steps) kind: Option<String>,
    /// The record's paywall markers.
    pub(in crate::steps) markers: Vec<String>,
    /// The record's `publication` (`short`, else `name`; plan
    /// intake-round-2-clean-dedupe-reading WP-5b).
    pub(in crate::steps) publication: Option<String>,
}

/// The record [`SourcesConfig::by_host`] matches for the clip host, as a [`HostRow`] (plan
/// sources-publication-model WP-2).
pub(in crate::steps) fn host_row(host: &str, sources: &SourcesConfig) -> Option<HostRow> {
    let PubMatch::Host {
        publication: p,
        feed,
    } = sources.by_host(host)?
    else {
        return None;
    };
    Some(HostRow {
        blog: feed.map(|l| (p.name.clone(), l.to_owned())),
        kind: Some(p.kind_or_blog().to_owned()),
        markers: p.paywall_markers.clone(),
        publication: Some(p.publication().to_owned()),
    })
}

/// The first of [`SourcesConfig::work_matches`] for the note whose `kind` `is_kind`; a matching
/// record with an undefined kind is skipped with a warning (never coerced).
pub(in crate::steps) fn serial_hit<'a>(
    sources: &'a SourcesConfig,
    clip: &Clip,
    is_kind: impl Fn(&str) -> bool,
) -> Option<&'a Publication> {
    let authors: Vec<&str> = clip.authors.iter().map(String::as_str).collect();
    sources
        .work_matches(&authors, &clip.source)
        .map(PubMatch::publication)
        .find(|s| {
            let kind = s.kind.as_deref().unwrap_or("").trim();
            let ok = is_kind(kind);
            if !ok {
                tracing::warn!(serial = %s.name, %kind, "auto_file: serial row kind is not defined in kinds.toml; row skipped");
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

/// What [`trash`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trashed {
    /// The note is in the trash.
    Done,
    /// Decision 3 (plan folder-links-follow): the note lies outside the vault root, so the
    /// automation guard left it alone. Callers must not declare or act on a trash.
    SkippedOutside,
}

/// Moves `path` to `.trash/` (recoverable, CAS on `hash`), recording that `by` absorbed it so
/// the `deleted` notification carries `replaced_by` (round 11 WP-1, Decision 1).
pub(crate) fn trash(
    ctx: &mut StepCtx<'_>,
    path: &str,
    hash: cv_vault::ContentHash,
    by: &str,
) -> Result<Trashed, StepError> {
    let (p, by) = (path.to_owned(), by.to_owned());
    match ctx.with_writer(&[path], move |w| w.delete_replaced(&p, hash, &by))? {
        Ok(()) => Ok(Trashed::Done),
        Err(e) if e.is_automation_skip() => Ok(Trashed::SkippedOutside),
        Err(e) => Err(e.into()),
    }
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
    let clip = clip_of(&doc);
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
            if trash(ctx, &clip_path, clip_hash, &target)? == Trashed::SkippedOutside {
                vars.action = "skipped_outside";
                out.set_vars(&vars);
                return Ok(out);
            }
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
            // Link-only post (plan intake-removals-sources-rescan Decision 6): the clip is the
            // post's transcript, not its duplicate; it continues through the clip pipeline.
            if let Some(tb) = &target_body
                && target_is_link_only(ctx, &target, tb, words)?
            {
                let pair = link_pair(&target);
                ctx.modify(&clip_path, |old| Ok(pair.clip(old)?))?;
                out.push_output(clip_path.clone());
                vars.action = "transcript";
                // The configured long kind whatever the host says; the publication rules still apply.
                let long = ctx.config.pipelines().youtube.long_kind.trim().to_owned();
                let (_, publication) = kind_rules(ctx, &clip, &body)?;
                file_clip(ctx, &clip_path, &long, publication.as_deref(), &mut vars)?;
                let final_path = final_clip_path(ctx, &clip_path, vars.kind.as_deref());
                link_post_note(ctx, &pair, &target, &final_path, &clip_path)?;
                if vars.child_job_id.is_none() {
                    vars.enrich_job_id = enrich(ctx, &clip_path);
                }
                out.set_vars(&vars);
                return Ok(out);
            }
            let contained = target_body.is_some_and(|tb| {
                cv_text::similar::containment(&body, &tb) * 100.0
                    >= f64::from(sup_cfg.threshold_percent.min(100))
            });
            if contained {
                vars.action = match trash(ctx, &clip_path, clip_hash, &target)? {
                    Trashed::Done => "merged",
                    Trashed::SkippedOutside => "skipped_outside",
                };
                out.set_vars(&vars);
                return Ok(out);
            }
            let link = cv_text::wikilink::link_to(&target);
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
    let (kind, publication) = kind_rules(ctx, &clip, &body)?;
    if let Some(kind) = kind {
        file_clip(ctx, &clip_path, &kind, publication.as_deref(), &mut vars)?;
    }
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

/// Step 2's kind rules: the clip's kind (first match wins, module doc) and the publication filing
/// writes when absent (that of the record that gave the kind; none for the fallbacks).
fn kind_rules(
    ctx: &StepCtx<'_>,
    clip: &Clip,
    body: &str,
) -> Result<(Option<String>, Option<String>), StepError> {
    let sources: &SourcesConfig = ctx.config.sources();
    if let Some((kind, p)) = record_kind(sources, clip, |k| kind_def(ctx.config, k).is_ok()) {
        let publication = Some(p.publication().trim().to_owned()).filter(|p| !p.is_empty());
        return Ok((Some(kind), publication));
    }
    let kind = if cv_text::url_canon::is_youtube_video(&clip.source) {
        Some(youtube_kind(&ctx.config.pipelines().youtube, body))
    } else if let Some(h) = &clip.host {
        hosts_kind(ctx.config, h)?
    } else {
        None
    };
    Ok((kind, None))
}

/// The first record of [`SourcesConfig::matches`] for the clip that gives a kind, with that kind:
/// a host match files as [`Publication::kind_or_blog`]; a prefix or author match needs a `kind`
/// that `is_kind`, else it is skipped with a warning (never coerced).
pub(in crate::steps) fn record_kind<'a>(
    sources: &'a SourcesConfig,
    clip: &Clip,
    is_kind: impl Fn(&str) -> bool,
) -> Option<(String, &'a Publication)> {
    let authors: Vec<&str> = clip.authors.iter().map(String::as_str).collect();
    sources
        .matches(&authors, &clip.source)
        .find_map(|m| match m {
            PubMatch::Host { publication: p, .. } => Some((p.kind_or_blog().to_owned(), p)),
            PubMatch::Prefix(p) | PubMatch::Author(p) => {
                let kind = p.kind.as_deref().unwrap_or("").trim();
                if is_kind(kind) {
                    Some((kind.to_owned(), p))
                } else {
                    tracing::warn!(serial = %p.name, %kind, "auto_file: serial row kind is not defined in kinds.toml; row skipped");
                    None
                }
            }
        })
}

/// Files the clip under `kind` through [`spawn_file_as_kind`] (sets `vars.kind`, `vars.filed`,
/// `vars.child_job_id`); a kind that cannot be filed, or an already-filed clip, leaves it in place.
fn file_clip(
    ctx: &mut StepCtx<'_>,
    clip_path: &str,
    kind: &str,
    publication: Option<&str>,
    vars: &mut Vars,
) -> Result<(), StepError> {
    match spawn_file_as_kind(ctx, clip_path, kind, publication, ChildAt::Inherited)? {
        Filing::Refused => {}
        Filing::Already => {
            vars.filed = Some("already");
            vars.kind = Some(kind.to_owned());
        }
        Filing::Spawned(id) => {
            vars.child_job_id = id;
            vars.kind = Some(kind.to_owned());
        }
    }
    Ok(())
}

/// Where a `file-as-kind` child points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::steps) enum ChildAt {
    /// The parent job's context (its active path and id).
    Inherited,
    /// The clip's path, with no bound id.
    Clip,
}

/// What [`spawn_file_as_kind`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::steps) enum Filing {
    /// `kind` is not defined in `kinds.toml` or not a `file-as-kind` choice: nothing written.
    Refused,
    /// The clip already sits in the kind's folder with all its tags.
    Already,
    /// The child was enqueued (`None`: the enqueue failed, logged).
    Spawned(Option<String>),
}

/// Files the clip at `clip_path` under `kind`: checks that `kind` is defined and a `file-as-kind`
/// choice, writes `publication` when the clip has none (never overwrites), then spawns
/// `file-as-kind { kind }` (move + tags + lint, then the clip pipeline) unless the clip is already
/// filed under it.
pub(in crate::steps) fn spawn_file_as_kind(
    ctx: &mut StepCtx<'_>,
    clip_path: &str,
    kind: &str,
    publication: Option<&str>,
    at: ChildAt,
) -> Result<Filing, StepError> {
    if kind_def(ctx.config, kind).is_err() {
        tracing::info!(%kind, "file-as-kind: kind is not defined in kinds.toml; clip left in place");
        return Ok(Filing::Refused);
    }
    let allowed = ctx
        .config
        .action(FILE_ACTION)
        .and_then(|a| a.params.iter().find(|p| p.name == "kind"))
        .is_some_and(|p| p.choices.iter().any(|c| c == kind));
    if !allowed {
        tracing::info!(%kind, "file-as-kind: kind is not a file-as-kind choice; clip left in place");
        return Ok(Filing::Refused);
    }
    if let Some(p) = publication {
        set_publication(ctx, clip_path, p)?;
    }
    if already_filed(ctx, clip_path, kind)? {
        return Ok(Filing::Already);
    }
    let mut params = serde_json::Map::new();
    params.insert("kind".into(), serde_json::Value::String(kind.to_owned()));
    let spawned = match at {
        ChildAt::Inherited => ctx.children.spawn_child(FILE_ACTION, params),
        ChildAt::Clip => ctx.children.spawn_child_at(FILE_ACTION, params, clip_path),
    };
    match spawned {
        Ok(id) => Ok(Filing::Spawned(Some(id))),
        Err(e) => {
            tracing::warn!(error = %e, "file-as-kind: could not enqueue; clip left in place");
            Ok(Filing::Spawned(None))
        }
    }
}

/// Writes the post's `transcript` link to the clip at `final_path` (and the clip's publication
/// when the post has none).
fn link_post_note(
    ctx: &mut StepCtx<'_>,
    pair: &LinkPair,
    target: &str,
    final_path: &str,
    clip_path: &str,
) -> Result<(), StepError> {
    let publication = ctx
        .read(clip_path)?
        .and_then(|(b, _)| String::from_utf8(b).ok())
        .and_then(|t| publication_of(&t));
    ctx.modify(target, |old| {
        Ok(pair.post(old, final_path, publication.as_deref())?)
    })?;
    Ok(())
}

/// Where the clip will sit once its `file-as-kind` child ran: the kind's folder (expanded as the
/// `move` does) plus the clip's basename; the clip's current path when it is not moved. (The
/// writer rewrites links on a move, so the post's link follows either way.)
fn final_clip_path(ctx: &StepCtx<'_>, clip_path: &str, kind: Option<&str>) -> String {
    let dest = kind
        .and_then(|k| kind_def(ctx.config, k).ok())
        .and_then(|def| get_str(def, "folder").ok().flatten())
        .and_then(|f| expand(ctx, &f).ok())
        .map(|f| f.trim().trim_matches('/').to_owned())
        .filter(|f| !f.is_empty())
        .map(|f| format!("{f}/{}", cv_text::vpath::basename(clip_path)));
    dest.unwrap_or_else(|| clip_path.to_owned())
}
