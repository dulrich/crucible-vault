//! `ignore_video` (plan `v0-automation-pipelines` WP-9): Ignore Shorts + the manual
//! `youtube-ignore-video` command.
//!
//! Reads the **active note**'s `_yt_metadata` frontmatter (`videoId`, `channelId`,
//! `channelTitle`, `duration_seconds`; never the API) and records the video in `state.db`
//! `ignored_ids` under kind `youtube`:
//!
//! - `mode = "auto"` (default): applies the WP-7 predicate
//!   [`cv_ingest::tracker::is_ignored_short`] with `pipelines.toml` `[ignore_shorts]`; a match
//!   records reason `short` (the tracker's kind/reason), no match is a no-op success.
//! - `mode = "manual"`: always records reason `manual` (Crucible `ignoreYoutubeVideoCommand`,
//!   `internalCommands.ts:257-264` → `addIgnoredVideoId`, `utils/ignoredIds.ts:27-31`).
//!
//! Idempotent (an existing row is kept, first reason wins). The note is never edited or deleted;
//! Crucible's command does not touch the note either. Dropped as Obsidian-induced: the
//! `ignored-ids` resource lock (the state DB serializes) and the `ignored-ids.json` file.
//!
//! Outcome vars: `video_id`, `ignored` (bool: the video is ignored after this step), `reason`
//! (`short` | `manual` | `""` when not ignored).

use cv_actions::{ConfigSet, Step};
use cv_ingest::tracker::is_ignored_short;
use cv_lint::{FrontmatterDoc, JsValue};
use serde_json::Value;

use super::materialize::{check_keys, get_str, table};
use super::tracker::now_ms;
use super::{StepCtx, StepError, StepOutcome};

const KEYS: &[&str] = &["mode"];
/// The `ignored_ids` kind (shared with the WP-7 tracker).
const KIND: &str = "youtube";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Manual,
}

fn mode(t: &toml::Table) -> Result<Mode, String> {
    match get_str(t, "mode")?.as_deref() {
        None | Some("auto") => Ok(Mode::Auto),
        Some("manual") => Ok(Mode::Manual),
        Some(m) => Err(format!("ignore_video: unknown mode {m:?} (auto | manual)")),
    }
}

/// Validates the step's params (unknown keys disarm, I5).
pub fn validate(step: &Step, _config: &ConfigSet) -> Result<(), String> {
    let t = table(step);
    check_keys(&t, KEYS, "ignore_video")?;
    mode(&t).map(|_| ())
}

fn prop_str(doc: &FrontmatterDoc, key: &str) -> Result<String, StepError> {
    Ok(
        match doc.get(key).map_err(|e| StepError::failed(e.to_string()))? {
            Some(JsValue::Str(s)) => s.trim().to_owned(),
            _ => String::new(),
        },
    )
}

fn prop_int(doc: &FrontmatterDoc, key: &str) -> Result<Option<i64>, StepError> {
    Ok(
        match doc.get(key).map_err(|e| StepError::failed(e.to_string()))? {
            #[allow(clippy::cast_possible_truncation)]
            Some(JsValue::Num(n)) if n.is_finite() => Some(n as i64),
            Some(JsValue::Str(s)) => s.trim().parse().ok(),
            _ => None,
        },
    )
}

/// Executes the step.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    validate(step, ctx.config).map_err(StepError::failed)?;
    let mode = mode(&table(step)).map_err(StepError::failed)?;
    let path = ctx
        .context
        .active_path
        .clone()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| StepError::failed("ignore_video needs an active note"))?;
    let (bytes, _) = ctx
        .read(&path)?
        .ok_or_else(|| StepError::failed("ignore_video: active note not found"))?;
    let doc = FrontmatterDoc::new(String::from_utf8_lossy(&bytes).into_owned());
    let video_id = prop_str(&doc, "videoId")?;
    if video_id.is_empty() {
        return Err(StepError::failed(
            "No YouTube video id found on the target metadata note.",
        ));
    }

    let reason = match mode {
        Mode::Manual => Some("manual"),
        Mode::Auto => {
            let channel_id = prop_str(&doc, "channelId")?;
            let title = prop_str(&doc, "channelTitle")?;
            let tracked = ctx
                .config
                .sources()
                .youtube
                .iter()
                .find(|r| !channel_id.is_empty() && r.id == channel_id)
                .map(|r| r.channel.clone())
                .unwrap_or_default();
            let names = [title.as_str(), tracked.as_str()];
            let shorts = &ctx.config.pipelines().ignore_shorts;
            is_ignored_short(
                &channel_id,
                &names,
                prop_int(&doc, "duration_seconds")?,
                &shorts.channels,
                shorts.max_seconds,
            )
            .then_some("short")
        }
    };

    if let Some(r) = reason {
        ctx.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert_ignored_id(KIND, &video_id, r, now_ms())
            .map_err(|e| StepError::failed(format!("state: {e}")))?;
    }

    let mut out = StepOutcome::default();
    out.vars.insert("video_id".into(), Value::String(video_id));
    out.vars
        .insert("ignored".into(), Value::Bool(reason.is_some()));
    out.vars.insert(
        "reason".into(),
        Value::String(reason.unwrap_or_default().to_owned()),
    );
    Ok(out)
}
