//! `track_youtube` (plan `v0-automation-pipelines` WP-7): the YouTube channel tracker.
//!
//! Per tracked `sources.toml` `[[youtube]]` row (not `skip`/`ignore`, `UC…` id only): the
//! uploads playlist's first page (`playlistItems.list` on `UU…`, Crucible `youtubeApi.ts:340`,
//! 1 unit), new ids (not in the [`SeenSet`]) → `videos.list` in batches of
//! [`api::MAX_IDS`] (1 unit each) → the `_yt_metadata` note (WP-4 find-or-create, no refetch) →
//! the Ignore Shorts predicate ([`cv_ingest::tracker::is_ignored_short`]; a match records
//! `ignored_ids` + `tracker_items(Ignored)`, no line) → else one daily intake line
//! ([`super::emit_line`]).
//!
//! Failures: a missing key fails `NoApiKey` once (never the breaker); every channel failing with
//! a service error settles deferred (opening the `youtube-api` breaker); partial failures
//! succeed with a `failed` count. Outcome vars: `sources`, `new`, `lines`, `ignored`, `failed`.
//!
//! | param | type | default | meaning |
//! |---|---|---|---|
//! | `limit_per_source` | int 1..=50 | 50 | uploads fetched per channel (the feed window) |

use std::collections::{BTreeSet, HashMap};

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::tracker::{is_ignored_short, uploads_playlist_id, youtube_ids_in_text};
use cv_ingest::youtube::{parse_iso8601_duration, watch_url};
use cv_providers::web::FetchError;
use cv_providers::web::youtube::{self as api, PlaylistItem};
use cv_state::TrackerOutcome;
use serde::Serialize;

use super::super::params::check_keys;
use super::super::web::{block_on, client, record_usage, secret};
use super::super::youtube::{
    API_KEY_SECRET, find_metadata_note, metadata_root, no_key, write_metadata_note, youtube,
};
use super::super::{StepCtx, StepError, StepModule, StepOutcome};
use super::{
    Candidate, SeenSet, emit_cancelled, emit_line, fetch_bounded, md_files_under, now_ms, record,
};

/// `tracker_items.source_kind` and `ignored_ids.kind` of YouTube videos.
pub const KIND: &str = "youtube";
/// The index prop carrying a captured note's video id.
pub const PROP: &str = "yt-video-id";
/// Crucible's orchestration folder (intake notes, `ignored.md`), read-only seeding input.
pub const CRUCIBLE_ROOT: &str = "_crucible/orchestration";

const DEFAULT_LIMIT: u32 = 50;

fn limit(t: &toml::Table) -> Result<u32, String> {
    match t.get("limit_per_source") {
        None => Ok(DEFAULT_LIMIT),
        Some(toml::Value::Integer(n)) if (1..=50).contains(n) => {
            u32::try_from(*n).map_err(|e| e.to_string())
        }
        Some(_) => Err("track_youtube: `limit_per_source` must be an integer 1..=50".into()),
    }
}

/// The `track_youtube` step (param `limit_per_source`; unknown keys disarm, I5).
pub(crate) struct TrackYoutube;

impl StepModule for TrackYoutube {
    type Params = u32;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<u32, String> {
        check_keys(t, kind, "track_youtube")?;
        limit(t)
    }

    fn run(max: u32, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(max, ctx)
    }
}

/// `track_youtube` outcome vars.
#[derive(Default, Serialize)]
struct Vars {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
    sources: usize,
    new: usize,
    lines: usize,
    ignored: usize,
    failed: usize,
}

/// First-run seed: ids of existing `_yt_metadata/**/<id>.md` notes plus the video ids in
/// Crucible's `_crucible/orchestration/**` notes (intake notes, `ignored.md`).
fn seed(ctx: &StepCtx<'_>) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for p in md_files_under(ctx, &metadata_root(ctx)) {
        let stem = p.rsplit('/').next().unwrap_or(&p).trim_end_matches(".md");
        if stem.len() == 11 {
            ids.insert(stem.to_owned());
        }
    }
    for p in md_files_under(ctx, CRUCIBLE_ROOT) {
        if let Ok(text) = std::fs::read_to_string(ctx.vault_root.join(&p)) {
            ids.extend(youtube_ids_in_text(&text));
        }
    }
    ids
}

/// Rows the duration backfill looks at per run (10 `videos.list` calls at most).
const BACKFILL_LIMIT: usize = 500;

/// Length backfill (plan intake-type-column-length-backfill WP-1): `line`/`note` rows recorded
/// without `duration_s` get it from `videos.list`, in [`api::MAX_IDS`] batches. An id a
/// successful response omits (deleted/private), or whose duration does not parse, is marked `0`
/// (rendered blank, never re-queried). A failed batch leaves its rows NULL for the next run and
/// stops the backfill; it never affects the step's outcome or counters.
fn backfill_durations(
    ctx: &mut StepCtx<'_>,
    c: &cv_providers::web::WebClient,
    key: &str,
) -> Result<(), StepError> {
    let missing = ctx
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .tracker_items_missing_duration(KIND, BACKFILL_LIMIT)
        .map_err(|e| StepError::failed(format!("state: {e}")))?;
    for chunk in missing.chunks(api::MAX_IDS) {
        let ids: Vec<&str> = chunk.iter().map(|(_, i)| i.as_str()).collect();
        let videos = match block_on(youtube().videos(c, key, &ids))? {
            Ok(vs) => {
                record_usage(ctx, api::SERVICE, 1);
                vs
            }
            Err(e) => {
                tracing::warn!(error = %e, "track_youtube: duration backfill batch failed");
                break;
            }
        };
        let durations: HashMap<String, i64> = videos
            .into_iter()
            .map(|v| {
                let d = parse_iso8601_duration(&v.content_details.duration).unwrap_or(0);
                (v.id, d)
            })
            .collect();
        let mut st = ctx
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (source_key, id) in chunk {
            let d = durations.get(id).copied().unwrap_or(0);
            st.fill_tracker_duration(KIND, source_key, id, d)
                .map_err(|e| StepError::failed(format!("state: {e}")))?;
        }
    }
    Ok(())
}

/// Retroactive Ignore Shorts (plan round-8-intake-views-anchor WP-1, "Decisions locked" 2): a
/// `line` row with NULL status, a known duration (`> 0`), no `ignored_ids` row, and a channel +
/// duration matching [`is_ignored_short`] (the tracker's own predicate, over the row's
/// `source_key` and recorded `source_name`) becomes `ignored` with status `-` and gets an
/// `ignored_ids` row (reason `short`), so it is never eligible again. At most
/// [`BACKFILL_LIMIT`] candidates per source per run. Returns rows changed.
fn cancel_listed_shorts(
    ctx: &StepCtx<'_>,
    channels: &[String],
    max_seconds: u32,
) -> Result<usize, StepError> {
    let err = |e: cv_state::StateError| StepError::failed(format!("state: {e}"));
    if channels.iter().all(|c| c.trim().is_empty()) {
        return Ok(0);
    }
    let mut st = ctx
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut changed = 0;
    for (source_key, source_name) in st.tracker_sources(KIND).map_err(err)? {
        let names = [source_name.as_deref().unwrap_or("")];
        // Channel match only (any duration under the cap); per-row durations are checked below.
        if !is_ignored_short(&source_key, &names, Some(0), channels, max_seconds) {
            continue;
        }
        let rows = st
            .tracker_items_cancel_candidates(KIND, &source_key, max_seconds, BACKFILL_LIMIT)
            .map_err(err)?;
        for (id, duration) in rows {
            if is_ignored_short(&source_key, &names, Some(duration), channels, max_seconds)
                && st
                    .cancel_tracker_item(KIND, &source_key, &id, "short", now_ms())
                    .map_err(err)?
            {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// Executes the step.
pub(crate) fn run(max: u32, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut out = StepOutcome::default();
    if !ctx.config.pipelines().youtube.enabled {
        out.set_vars(&Vars {
            disabled: true,
            ..Vars::default()
        });
        return Ok(out);
    }
    let rows: Vec<_> = ctx
        .config
        .sources()
        .youtube
        .iter()
        .filter(|r| !r.priority.is_excluded())
        .filter_map(|r| uploads_playlist_id(&r.id).map(|pl| (r.clone(), pl)))
        .collect();
    let key = secret(API_KEY_SECRET).ok_or_else(no_key)?;
    let mut seen = SeenSet::load(ctx, KIND, PROP, seed)?;
    super::heal_source_names(
        ctx,
        KIND,
        rows.iter()
            .map(|(r, _)| (r.id.as_str(), r.channel.as_str())),
    )?;
    let shorts = ctx.config.pipelines().ignore_shorts.clone();

    let c = client()?;
    let k2 = key.clone();
    let lists = fetch_bounded(rows.iter().map(|(_, pl)| pl.clone()).collect(), |pl| {
        let k = k2.clone();
        let yt = youtube();
        async move { yt.playlist_items(c, &k, &pl, max).await }
    })?;

    let (mut new, mut lines, mut ignored, mut failed) = (0, 0, 0, 0);
    let mut deferred = None;
    let mut fail = |e: &FetchError, failed: &mut usize| {
        *failed += 1;
        let d = StepError::from_fetch(e);
        tracing::warn!(error = %e, "track_youtube: channel failed");
        if matches!(d, StepError::Deferred { .. }) && deferred.is_none() {
            deferred = Some(d);
        }
    };
    for ((row, _), list) in rows.iter().zip(lists) {
        let items: Vec<PlaylistItem> = match list {
            Ok(v) => {
                record_usage(ctx, api::SERVICE, 1);
                v
            }
            Err(e) => {
                if matches!(e, FetchError::NotFound { .. }) {
                    record_usage(ctx, api::SERVICE, 1);
                }
                fail(&e, &mut failed);
                continue;
            }
        };
        // Oldest first, so the day's lines read chronologically.
        let mut fresh: Vec<(String, String)> = Vec::new();
        for it in items.into_iter().rev() {
            let id = it.content_details.video_id.trim().to_owned();
            if id.is_empty() || fresh.iter().any(|(f, _)| *f == id) || seen.contains(&id) {
                continue;
            }
            fresh.push((id, it.snippet.title));
        }
        if fresh.is_empty() {
            continue;
        }
        let mut videos = HashMap::new();
        let mut batch_err = None;
        for chunk in fresh.chunks(api::MAX_IDS) {
            let ids: Vec<&str> = chunk.iter().map(|(i, _)| i.as_str()).collect();
            match block_on(youtube().videos(c, &key, &ids))? {
                Ok(vs) => {
                    record_usage(ctx, api::SERVICE, 1);
                    videos.extend(vs.into_iter().map(|v| (v.id.clone(), v)));
                }
                Err(e) => {
                    batch_err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = batch_err {
            fail(&e, &mut failed);
            continue;
        }
        for (id, feed_title) in fresh {
            new += 1;
            let video = videos.remove(&id);
            let duration = video
                .as_ref()
                .and_then(|v| parse_iso8601_duration(&v.content_details.duration));
            let channel_title = video
                .as_ref()
                .map(|v| v.snippet.channel_title.clone())
                .unwrap_or_default();
            let title = video
                .as_ref()
                .map(|v| v.snippet.title.clone())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or(feed_title);
            let available = video.is_some();
            if find_metadata_note(ctx, &id)?.is_none() {
                write_metadata_note(ctx, &id, video)?;
            }
            let cand = Candidate {
                source_kind: KIND.into(),
                source_key: row.id.clone(),
                source_name: row.channel.clone(),
                item_id: id.clone(),
                title,
                url: watch_url(&id),
                duration_s: duration,
                word_count: None,
            };
            seen.insert(&id);
            if !available {
                record(ctx, &cand, TrackerOutcome::Skipped)?;
                continue;
            }
            let names = [row.channel.as_str(), channel_title.as_str()];
            if is_ignored_short(
                &row.id,
                &names,
                duration,
                &shorts.channels,
                shorts.max_seconds,
            ) {
                ctx.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert_ignored_id(KIND, &id, "short", now_ms())
                    .map_err(|e| StepError::failed(format!("state: {e}")))?;
                emit_cancelled(ctx, &cand, &mut out.output_paths)?;
                ignored += 1;
                continue;
            }
            if emit_line(ctx, &cand, &mut out.output_paths)? != super::Emitted::AlreadyRecorded {
                lines += 1;
            }
        }
    }
    backfill_durations(ctx, c, &key)?;
    cancel_listed_shorts(ctx, &shorts.channels, shorts.max_seconds)?;
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
        ignored,
        failed,
    });
    Ok(out)
}

#[cfg(test)]
#[path = "youtube_tests.rs"]
mod tests;
