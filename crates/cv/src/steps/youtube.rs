//! `yt_metadata` and `yt_channel_enrich` (plan `v0-automation-pipelines` WP-4): YouTube metadata
//! notes under `_yt_metadata/`.
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/youtubeApi.ts`
//! (`ingestYoutubeVideoMetadata :547-568`, `ensureMetadataNote :483-529`, `linkMetadataToNote
//! :666-676`, `resolveChannelFolder :388-406`, `ensureChannelAboutNote :800-850`) and the chain
//! in `workflows/YoutubeMetadataFetchWorkflow.ts:33-72,134-155`. Dropped as Obsidian-induced: the
//! note/resource locks (the job's resource keys + the single writer's exact-name absence check
//! replace them) and the metadata-cache reads (the note is read directly).
//!
//! `yt_metadata` outcome vars: `video_id`, `channel_id`, `metadata_path`, `created`, `tombstone`,
//! `linked`. `yt_channel_enrich` runs only when `pipelines.toml` `[youtube] channel_enrich` is on
//! and (`force` or the previous step set `created = true`).

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::youtube::{
    ChannelMeta, VideoMeta, channel_about_note, metadata_note, tombstone_note,
};
use cv_lint::{FrontmatterDoc, JsValue};
use cv_providers::web::FetchError;
use cv_providers::web::youtube::{self as api, YoutubeApi};
use cv_state::FailureReason;
use cv_text::slugify;
use serde::Serialize;
use serde_json::Value;

use super::params::{check_keys, check_placeholders, expand, get_bool, get_str};
use super::web::{
    append_link, block_on, client, create_or_found, find_in_subfolders, link_targets, record_usage,
    secret,
};
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// The secret name (env var or `~/.config/crucible-vault/YOUTUBE_API_KEY`).
pub const API_KEY_SECRET: &str = "YOUTUBE_API_KEY";

const DEFAULT_MAX_AGE_DAYS: i64 = 30;

fn templated(t: &toml::Table, key: &str) -> Result<Option<String>, String> {
    let v = get_str(t, key)?;
    if let Some(s) = &v {
        check_placeholders(s)?;
    }
    Ok(v)
}

fn max_age(t: &toml::Table) -> Result<i64, String> {
    match t.get("max_age_days") {
        None => Ok(DEFAULT_MAX_AGE_DAYS),
        Some(toml::Value::Integer(n)) if *n > 0 => Ok(*n),
        Some(_) => Err("`max_age_days` must be a positive integer".into()),
    }
}

/// Parsed `yt_metadata` / `yt_channel_enrich` params.
pub(crate) enum Params {
    /// `yt_metadata`.
    Metadata { video_id: Option<String> },
    /// `yt_channel_enrich`.
    ChannelEnrich {
        channel_id: Option<String>,
        force: bool,
        max_age_days: i64,
    },
}

/// `yt_metadata` / `yt_channel_enrich` (unknown keys disarm, I5).
pub(crate) struct Youtube;

impl StepModule for Youtube {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, kind.as_str())?;
        Ok(if kind == StepKind::YtChannelEnrich {
            Params::ChannelEnrich {
                channel_id: templated(t, "channel_id")?,
                force: get_bool(t, "force")?.unwrap_or(false),
                max_age_days: max_age(t)?,
            }
        } else {
            Params::Metadata {
                video_id: templated(t, "video_id")?,
            }
        })
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p {
            Params::ChannelEnrich {
                channel_id,
                force,
                max_age_days,
            } => channel_enrich(channel_id.as_deref(), force, max_age_days, ctx),
            Params::Metadata { video_id } => metadata(video_id.as_deref(), ctx),
        }
    }
}

/// `yt_metadata` outcome vars.
#[derive(Serialize)]
struct MetaVars {
    video_id: String,
    created: bool,
    tombstone: bool,
    linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_id: Option<String>,
}

/// `yt_channel_enrich` outcome vars.
#[derive(Serialize)]
struct EnrichVars {
    enriched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    about_path: Option<String>,
}

pub(crate) fn metadata_root(ctx: &StepCtx<'_>) -> String {
    let r = ctx
        .config
        .pipelines()
        .youtube
        .metadata_root
        .trim_matches('/');
    if r.is_empty() {
        "_yt_metadata".to_owned()
    } else {
        r.to_owned()
    }
}

fn utf8(b: Vec<u8>) -> Result<String, StepError> {
    String::from_utf8(b).map_err(|_| StepError::failed("note is not UTF-8"))
}

fn fm_err(e: cv_lint::FmError) -> StepError {
    StepError::failed(e.to_string())
}

pub(crate) fn no_key() -> StepError {
    StepError::Failed {
        message: format!("YouTube Data API key missing (set {API_KEY_SECRET})"),
        reason: Some(FailureReason::NoApiKey),
    }
}

/// Crucible `fetched_at` (`new Date().toISOString()`).
fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

pub(crate) fn youtube() -> YoutubeApi {
    YoutubeApi::new(super::web::api_base(api::SERVICE, api::DEFAULT_BASE))
}

/// `resolveChannelFolder`: the `sources.toml` display name for a tracked channel, else the
/// channel title, else the channel id.
fn channel_folder(ctx: &StepCtx<'_>, channel_id: &str, title: &str) -> String {
    let tracked = ctx
        .config
        .sources()
        .youtube
        .iter()
        .find(|r| !channel_id.is_empty() && r.id == channel_id)
        .map(|r| slugify(&r.channel))
        .filter(|s| !s.is_empty());
    tracked
        .or_else(|| Some(slugify(title)).filter(|s| !s.is_empty()))
        .or_else(|| Some(channel_id.to_owned()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "unknown-channel".to_owned())
}

fn str_prop(v: Option<JsValue>) -> Option<String> {
    match v? {
        JsValue::Str(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
        JsValue::Num(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Ensures the metadata note for `id` exists: `(path, created, tombstone, channel_id)`. Reused by
/// the tracker (WP-7).
pub(crate) fn ensure_metadata_note(
    ctx: &mut StepCtx<'_>,
    id: &str,
) -> Result<(String, bool, bool, Option<String>), StepError> {
    if let Some((p, tombstone)) = find_metadata_note(ctx, id)? {
        return Ok((p, false, tombstone, None));
    }
    let key = secret(API_KEY_SECRET).ok_or_else(no_key)?;
    let c = client()?;
    let r = block_on(youtube().videos(c, &key, &[id]))?;
    let video = match r {
        Ok(items) => {
            record_usage(ctx, api::SERVICE, 1);
            items.into_iter().next()
        }
        Err(FetchError::NotFound { .. }) => {
            record_usage(ctx, api::SERVICE, 1);
            None
        }
        Err(e) => return Err(StepError::from_fetch(&e)),
    };
    write_metadata_note(ctx, id, video)
}

/// Finds an existing metadata note for `id`: `(path, tombstone)`.
pub(crate) fn find_metadata_note(
    ctx: &StepCtx<'_>,
    id: &str,
) -> Result<Option<(String, bool)>, StepError> {
    let root = metadata_root(ctx);
    Ok(
        find_in_subfolders(ctx, &root, &format!("{id}.md"))?.map(|p| {
            let tombstone = p.starts_with(&format!("{root}/_unavailable/"));
            (p, tombstone)
        }),
    )
}

/// Writes the metadata note for `id` from an already fetched `videos.list` item (`None` = the
/// video is unavailable → tombstone), find-or-create, no refetch: `(path, created, tombstone,
/// channel_id)` (`channel_id` only when created). The tracker (WP-7) calls this with batched
/// results.
pub(crate) fn write_metadata_note(
    ctx: &mut StepCtx<'_>,
    id: &str,
    video: Option<api::Video>,
) -> Result<(String, bool, bool, Option<String>), StepError> {
    let root = metadata_root(ctx);
    let Some(v) = video else {
        let path = format!("{root}/_unavailable/{id}.md");
        create_or_found(ctx, &path, tombstone_note(id, &iso_now()).into_bytes())?;
        return Ok((path, false, true, None));
    };
    let meta = VideoMeta {
        video_id: id.to_owned(),
        title: v.snippet.title,
        description: v.snippet.description,
        duration: v.content_details.duration,
        channel_id: v.snippet.channel_id,
        channel_title: v.snippet.channel_title,
        published_at: v.snippet.published_at,
        tags: v.snippet.tags,
        category_id: v.snippet.category_id,
        default_language: v.snippet.default_language,
        live_broadcast_content: v.snippet.live_broadcast_content,
        view_count: v.statistics.view_count,
        like_count: v.statistics.like_count,
        comment_count: v.statistics.comment_count,
    };
    let folder = channel_folder(ctx, &meta.channel_id, &meta.channel_title);
    let path = format!("{root}/{folder}/{id}.md");
    let created = create_or_found(ctx, &path, metadata_note(&meta, &iso_now()).into_bytes())?;
    let channel = Some(meta.channel_id).filter(|c| created && !c.is_empty());
    Ok((path, created, false, channel))
}

fn metadata(video_id: Option<&str>, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let active = ctx.context.active_path.clone();
    let note = match &active {
        Some(p) => ctx.read(p)?,
        None => None,
    };
    let doc = match &note {
        Some((b, _)) => Some(FrontmatterDoc::new(utf8(b.clone())?)),
        None => None,
    };
    let explicit = match video_id {
        Some(s) => expand(ctx, s)?.trim().to_owned(),
        None => String::new(),
    };
    let id = if explicit.is_empty() {
        let from_prop = doc
            .as_ref()
            .and_then(|d| d.get("yt-video-id").ok().flatten())
            .and_then(|v| str_prop(Some(v)));
        let from_source = || {
            doc.as_ref()
                .and_then(|d| str_prop(d.get("source").ok().flatten()))
                .and_then(|s| cv_text::url_canon::extract_video_id_from_url(&s))
        };
        from_prop.or_else(from_source).unwrap_or_default()
    } else {
        explicit
    };
    let mut out = StepOutcome::default();
    let mut v = MetaVars {
        video_id: id.clone(),
        created: false,
        tombstone: false,
        linked: false,
        metadata_path: None,
        channel_id: None,
    };
    if id.is_empty() || id.contains('/') || id.contains("..") {
        // Crucible `no-video-id`: nothing to do.
        out.set_vars(&v);
        return Ok(out);
    }

    // (a) Already linked to a live metadata note for this video → done, no write.
    if let Some(d) = &doc {
        for target in link_targets(d.get("yt-metadata").map_err(fm_err)?.as_ref()) {
            if target.rsplit('/').next() == Some(id.as_str())
                && ctx.read(&format!("{target}.md"))?.is_some()
            {
                v.metadata_path = Some(format!("{target}.md"));
                out.set_vars(&v);
                return Ok(out);
            }
        }
    }

    // (b)/(c) Find or create.
    let (path, created, tombstone, channel) = ensure_metadata_note(ctx, &id)?;
    v.metadata_path = Some(path.clone());
    v.created = created;
    v.tombstone = tombstone;
    v.channel_id = channel;
    if created || tombstone {
        out.push_output(path.clone());
    }

    // (d) Link onto the source note: re-read after the fetch and CAS-written through
    // `modify` (SR7), so an edit made during the fetch is kept.
    if let (Some(p), Some(_)) = (active, note) {
        let link = format!("[[{}]]", path.strip_suffix(".md").unwrap_or(&path));
        let linked = ctx.modify(&p, |cur| {
            let mut d = FrontmatterDoc::new(cur.to_owned());
            Ok(append_link(&mut d, "yt-video-id", "yt-metadata", &link)?.then(|| d.into_text()))
        })?;
        if linked.is_some() {
            out.push_output(p);
            v.linked = true;
        }
    }
    out.set_vars(&v);
    Ok(out)
}

fn channel_enrich(
    channel_id: Option<&str>,
    force: bool,
    max_age_days: i64,
    ctx: &mut StepCtx<'_>,
) -> Result<StepOutcome, StepError> {
    let mut out = StepOutcome::default();
    out.set_vars(&EnrichVars {
        enriched: false,
        about_path: None,
    });
    let created = ctx.vars.get("created") == Some(&Value::Bool(true));
    if !ctx.config.pipelines().youtube.channel_enrich || !(force || created) {
        return Ok(out);
    }
    let tpl = channel_id.unwrap_or("{vars.channel_id}");
    let channel_id = expand(ctx, tpl)?.trim().to_owned();
    if channel_id.is_empty() || channel_id.contains('/') {
        return Ok(out);
    }
    let root = metadata_root(ctx);
    let existing = find_about(ctx, &root, &channel_id)?;
    if let Some((p, age_days)) = &existing
        && !force
        && age_days.is_some_and(|a| a < max_age_days)
    {
        out.set_vars(&EnrichVars {
            enriched: false,
            about_path: Some(p.clone()),
        });
        return Ok(out);
    }
    let key = secret(API_KEY_SECRET).ok_or_else(no_key)?;
    let c = client()?;
    let items = block_on(youtube().channels(c, &key, &[channel_id.as_str()]))?
        .map_err(|e| StepError::from_fetch(&e))?;
    record_usage(ctx, api::SERVICE, 1);
    let ch = items
        .into_iter()
        .next()
        .ok_or_else(|| StepError::failed(format!("YouTube channel {channel_id} not found")))?;
    let id = if ch.id.is_empty() {
        channel_id.clone()
    } else {
        ch.id.clone()
    };
    let meta = ChannelMeta {
        channel_id: id.clone(),
        thumbnail_url: ch.snippet.thumbnails.best().map(str::to_owned),
        title: ch.snippet.title,
        description: ch.snippet.description,
        custom_url: ch.snippet.custom_url,
        published_at: ch.snippet.published_at,
        country: ch.snippet.country,
        subscriber_count: ch.statistics.subscriber_count,
        video_count: ch.statistics.video_count,
        view_count: ch.statistics.view_count,
    };
    let body = channel_about_note(&meta, &iso_now());
    // The about note is regenerated whole after the fetch: written through `modify` (SR7) so
    // the CAS hash is the one read now, not the one read before the round-trip.
    let path = match existing {
        Some((p, _)) => {
            ctx.modify(&p, |_| Ok(Some(body.clone())))?;
            p
        }
        None => {
            let p = format!("{root}/{}/about.md", channel_folder(ctx, &id, &meta.title));
            if ctx.read(&p)?.is_some() {
                ctx.modify(&p, |_| Ok(Some(body.clone())))?;
            } else {
                create_or_found(ctx, &p, body.into_bytes())?;
            }
            p
        }
    };
    out.push_output(path.clone());
    out.set_vars(&EnrichVars {
        enriched: true,
        about_path: Some(path),
    });
    Ok(out)
}

/// `findExistingChannelAboutNote` + `channelAboutAgeMs`: `(path, age in whole days)`.
fn find_about(
    ctx: &StepCtx<'_>,
    root: &str,
    channel_id: &str,
) -> Result<Option<(String, Option<i64>)>, StepError> {
    let Ok(rd) = std::fs::read_dir(ctx.vault_root.join(root)) else {
        return Ok(None);
    };
    let mut dirs: Vec<String> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    dirs.sort();
    for d in dirs {
        let p = format!("{root}/{d}/about.md");
        let Some((bytes, _)) = ctx.read(&p)? else {
            continue;
        };
        let doc = FrontmatterDoc::new(String::from_utf8_lossy(&bytes).into_owned());
        if str_prop(doc.get("channelId").ok().flatten()).as_deref() != Some(channel_id) {
            continue;
        }
        let age = str_prop(doc.get("fetched_at").ok().flatten())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days());
        return Ok(Some((p, age)));
    }
    Ok(None)
}

#[cfg(test)]
#[path = "youtube_tests.rs"]
mod tests;
