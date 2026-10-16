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

use cv_actions::{ConfigSet, Step};
use cv_ingest::youtube::{
    ChannelMeta, VideoMeta, channel_about_note, metadata_note, slugify, tombstone_note,
};
use cv_lint::{FrontmatterDoc, JsValue};
use cv_providers::web::FetchError;
use cv_providers::web::youtube::{self as api, YoutubeApi};
use cv_state::FailureReason;
use serde_json::{Value, json};

use super::materialize::{check_keys, check_placeholders, expand, get_bool, get_str, table};
use super::web::{
    append_link, block_on, client, create_or_found, find_in_subfolders, link_targets, record_usage,
    secret,
};
use super::{StepCtx, StepError, StepOutcome, fetch_disposition};

/// The secret name (env var or `~/.config/crucible-vault/YOUTUBE_API_KEY`).
pub const API_KEY_SECRET: &str = "YOUTUBE_API_KEY";

const META_KEYS: &[&str] = &["video_id"];
const ENRICH_KEYS: &[&str] = &["channel_id", "force", "max_age_days"];
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

/// Validates the step's params (unknown keys disarm, I5).
pub fn validate(step: &Step, _config: &ConfigSet) -> Result<(), String> {
    let t = table(step);
    match step {
        Step::YtChannelEnrich(_) => {
            check_keys(&t, ENRICH_KEYS, "yt_channel_enrich")?;
            templated(&t, "channel_id")?;
            get_bool(&t, "force")?;
            max_age(&t).map(|_| ())
        }
        _ => {
            check_keys(&t, META_KEYS, "yt_metadata")?;
            templated(&t, "video_id").map(|_| ())
        }
    }
}

/// Executes the step.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    validate(step, ctx.config).map_err(StepError::failed)?;
    match step {
        Step::YtChannelEnrich(_) => channel_enrich(&table(step), ctx),
        _ => metadata(&table(step), ctx),
    }
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
        Err(e) => return Err(fetch_disposition(&e)),
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

fn metadata(t: &toml::Table, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let active = ctx.context.active_path.clone();
    let note = match &active {
        Some(p) => ctx.read(p)?,
        None => None,
    };
    let doc = match &note {
        Some((b, _)) => Some(FrontmatterDoc::new(utf8(b.clone())?)),
        None => None,
    };
    let explicit = match get_str(t, "video_id").map_err(StepError::failed)? {
        Some(s) => expand(ctx, &s)?.trim().to_owned(),
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
    let set = |out: &mut StepOutcome, k: &str, v: Value| {
        out.vars.insert(k.to_owned(), v);
    };
    set(&mut out, "video_id", json!(id));
    set(&mut out, "created", json!(false));
    set(&mut out, "tombstone", json!(false));
    set(&mut out, "linked", json!(false));
    if id.is_empty() || id.contains('/') || id.contains("..") {
        // Crucible `no-video-id`: nothing to do.
        return Ok(out);
    }

    // (a) Already linked to a live metadata note for this video → done, no write.
    if let Some(d) = &doc {
        for target in link_targets(d.get("yt-metadata").map_err(fm_err)?.as_ref()) {
            if target.rsplit('/').next() == Some(id.as_str())
                && ctx.read(&format!("{target}.md"))?.is_some()
            {
                set(&mut out, "metadata_path", json!(format!("{target}.md")));
                return Ok(out);
            }
        }
    }

    // (b)/(c) Find or create.
    let (path, created, tombstone, channel) = ensure_metadata_note(ctx, &id)?;
    set(&mut out, "metadata_path", json!(path));
    set(&mut out, "created", json!(created));
    set(&mut out, "tombstone", json!(tombstone));
    if let Some(c) = channel {
        set(&mut out, "channel_id", json!(c));
    }
    if created || tombstone {
        out.output_paths.push(path.clone());
    }

    // (d) Link onto the source note (CAS).
    if let (Some(p), Some((_, hash)), Some(mut d)) = (active, note, doc) {
        let link = format!("[[{}]]", path.strip_suffix(".md").unwrap_or(&path));
        if append_link(&mut d, "yt-video-id", "yt-metadata", &link)? {
            ctx.write(&p, d.into_text().into_bytes(), Some(hash))?;
            out.output_paths.push(p);
            set(&mut out, "linked", json!(true));
        }
    }
    Ok(out)
}

fn channel_enrich(t: &toml::Table, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let force = get_bool(t, "force")
        .map_err(StepError::failed)?
        .unwrap_or(false);
    let max_age_days = max_age(t).map_err(StepError::failed)?;
    let mut out = StepOutcome::default();
    out.vars.insert("enriched".into(), json!(false));
    let created = ctx.vars.get("created") == Some(&Value::Bool(true));
    if !ctx.config.pipelines().youtube.channel_enrich || !(force || created) {
        return Ok(out);
    }
    let tpl = get_str(t, "channel_id")
        .map_err(StepError::failed)?
        .unwrap_or_else(|| "{vars.channel_id}".to_owned());
    let channel_id = expand(ctx, &tpl)?.trim().to_owned();
    if channel_id.is_empty() || channel_id.contains('/') {
        return Ok(out);
    }
    let root = metadata_root(ctx);
    let existing = find_about(ctx, &root, &channel_id)?;
    if let Some((p, _, age_days)) = &existing
        && !force
        && age_days.is_some_and(|a| a < max_age_days)
    {
        out.vars.insert("about_path".into(), json!(p));
        return Ok(out);
    }
    let key = secret(API_KEY_SECRET).ok_or_else(no_key)?;
    let c = client()?;
    let items = block_on(youtube().channels(c, &key, &[channel_id.as_str()]))?
        .map_err(|e| fetch_disposition(&e))?;
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
    let body = channel_about_note(&meta, &iso_now()).into_bytes();
    let path = match existing {
        Some((p, hash, _)) => {
            ctx.write(&p, body, Some(hash))?;
            p
        }
        None => {
            let p = format!("{root}/{}/about.md", channel_folder(ctx, &id, &meta.title));
            match ctx.read(&p)? {
                Some((_, hash)) => {
                    ctx.write(&p, body, Some(hash))?;
                }
                None => {
                    create_or_found(ctx, &p, body)?;
                }
            }
            p
        }
    };
    out.output_paths.push(path.clone());
    out.vars.insert("about_path".into(), json!(path));
    out.vars.insert("enriched".into(), json!(true));
    Ok(out)
}

/// `findExistingChannelAboutNote` + `channelAboutAgeMs`: `(path, hash, age in whole days)`.
fn find_about(
    ctx: &StepCtx<'_>,
    root: &str,
    channel_id: &str,
) -> Result<Option<(String, cv_vault::ContentHash, Option<i64>)>, StepError> {
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
        let Some((bytes, hash)) = ctx.read(&p)? else {
            continue;
        };
        let doc = FrontmatterDoc::new(String::from_utf8_lossy(&bytes).into_owned());
        if str_prop(doc.get("channelId").ok().flatten()).as_deref() != Some(channel_id) {
            continue;
        }
        let age = str_prop(doc.get("fetched_at").ok().flatten())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days());
        return Ok(Some((p, hash, age)));
    }
    Ok(None)
}

#[cfg(test)]
#[path = "youtube_tests.rs"]
mod tests;
