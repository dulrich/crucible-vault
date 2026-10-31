//! Step module `yt_clip_tidy` (plan `round-19-yt-cleanup-intake` WP-2, backfill scope WP-3):
//! deletes a clipped YouTube note's thumbnail embed and the clipped description below it, ahead of
//! `## Transcript`, via [`cv_note::strip_youtube_preamble`]. The one delete-not-fold exception to
//! D29: the description is kept in the linked `_yt_metadata` note and the frontmatter
//! `description`.
//!
//! | key | values | default |
//! |---|---|---|
//! | `scope` | `"note"` (the run's active note) \| `"backfill"` (every indexed YouTube note, in this one step) | `"note"` |
//! | `dry_run` | backfill only: compute every count, write nothing; a job param `dry_run` wins | `false` |
//!
//! **Note scope** runs on the active note only, and only when its `source` is a YouTube URL (a
//! note that merely embeds a video is never touched). Leading `%% … %%` managed regions, the
//! frontmatter and everything at or after `## Transcript` stay byte-identical; a body that does not
//! fully match the shape is left alone, so a second run is a no-op. Written through
//! [`StepCtx::modify`] (CAS, I2). Timestamp cleanup is not done here — it stays on the kind `post`
//! `cleanup_transcript` lint path. Like `transcript_refine`, there is no editor hold for a custom
//! step; the CAS hash is the protection. Outcome vars: `stripped` (bool).
//!
//! **Backfill scope** (a scope of this step rather than its own: the strip, the composition and the
//! publication fill share `tidy` and `with_publication`) walks every indexed note outside the
//! system folders (`.trash/`, `_yt_metadata/`, `_raw_transcript/`, `_custom/`, …) whose `source`
//! is a YouTube URL, in path order, and per note in one CAS modify (no network, no child jobs):
//! (a) [`tidy`]; (b) on a `post`-tagged note, [`cv_note::cleanup_youtube_transcript`] on the
//! result; (b2) on that same note, [`cv_note::restore_transcript_heading`] re-inserts a
//! `## Transcript` heading the cleanup dropped when the managed prefix still links `#Transcript`
//! (plan round-20-transcript-heading-auto-holds WP-1); (c) `publication` from the note's linked `yt-metadata` note (its `publication`, else its
//! `channelTitle`) when the note's own is absent or blank, inserted after `author`. A partial shape
//! changes nothing. Outcome vars: `scanned`, `stripped`, `cleaned`, `heading_restored`, `publication_filled`,
//! `no_metadata` (publication wanted, no usable linked metadata note), `conflict`, `unchanged`,
//! `dry_run`.

use std::collections::BTreeSet;

use cv_actions::{ConfigSet, StepKind};
use cv_note::{FrontmatterDoc, JsValue};
use serde::Serialize;

use super::intake_enrich::{is_user_path, is_youtube_source};
use super::params::{check_keys, get_bool, get_str};
use super::web::link_targets;
use super::youtube::{metadata_publication, with_publication};
use super::{StepCtx, StepError, StepModule, StepOutcome, job_flag};

/// The backfill's metadata-link frontmatter key (the `yt_metadata` list key).
const META_KEY: &str = "yt-metadata";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Note,
    Backfill,
}

/// Parsed step params.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Params {
    scope: Scope,
    dry_run: bool,
}

/// The `yt_clip_tidy` step.
pub(crate) struct YtClipTidy;

impl StepModule for YtClipTidy {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, "yt_clip_tidy")?;
        let scope = match get_str(t, "scope")?.as_deref() {
            None | Some("note") => Scope::Note,
            Some("backfill") => Scope::Backfill,
            Some(s) => {
                return Err(format!(
                    "yt_clip_tidy step: `scope` {s:?} is not note|backfill"
                ));
            }
        };
        let dry_run = get_bool(t, "dry_run")?;
        if scope == Scope::Note && dry_run.is_some() {
            return Err("yt_clip_tidy step: `dry_run` needs scope = \"backfill\"".into());
        }
        Ok(Params {
            scope,
            dry_run: dry_run.unwrap_or(false),
        })
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        match p.scope {
            Scope::Note => run_note(ctx),
            Scope::Backfill => backfill(job_flag(ctx, "dry_run").unwrap_or(p.dry_run), ctx),
        }
    }
}

fn run_note(ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let path = ctx
        .context
        .active_path
        .clone()
        .ok_or_else(|| StepError::failed("yt_clip_tidy: needs an active note"))?;
    ctx.read_required(&path)?;
    let written = ctx.modify(&path, |text| Ok(tidy(text)))?;
    let mut out = StepOutcome::default();
    if written.is_some() {
        out.push_output(path);
    }
    out.set_vars(&Vars {
        stripped: written.is_some(),
    });
    Ok(out)
}

/// Outcome vars of a note run.
#[derive(Serialize)]
struct Vars {
    stripped: bool,
}

/// The tidied note text, or `None` when `text` is not a YouTube clip with the preamble shape.
pub(crate) fn tidy(text: &str) -> Option<String> {
    let doc = FrontmatterDoc::new(text);
    if !doc
        .get_str_trimmed("source")
        .is_some_and(|s| is_youtube_source(&s))
    {
        return None;
    }
    let body = doc.body();
    let fm = &text[..text.len() - body.len()];
    cv_note::strip_youtube_preamble(body).map(|b| format!("{fm}{b}"))
}

// ---- backfill ---------------------------------------------------------------------------------

/// What the backfill would do to one note.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The new text, `None` when nothing changes.
    pub new: Option<String>,
    pub stripped: bool,
    pub cleaned: bool,
    pub heading_restored: bool,
    pub filled: bool,
}

/// Whether the note's `tags` include `post` (a list, or a single string).
fn has_post_tag(text: &str) -> bool {
    match FrontmatterDoc::new(text).get("tags") {
        Ok(Some(JsValue::Arr(a))) => a
            .iter()
            .any(|v| matches!(v, JsValue::Str(s) if s.trim().trim_start_matches('#') == "post")),
        Ok(Some(JsValue::Str(s))) => s.trim().trim_start_matches('#') == "post",
        _ => false,
    }
}

/// The pure per-note backfill: strip, then (on a `post` tag) timestamp cleanup, then the
/// publication fill from `meta_publication` (the linked metadata note's, if any). Each step runs
/// on the previous one's result; a note that is not a YouTube clip is left alone.
pub(crate) fn plan(text: &str, meta_publication: Option<&str>) -> Result<Plan, StepError> {
    let mut p = Plan::default();
    let mut cur = text.to_owned();
    if let Some(t) = tidy(&cur) {
        cur = t;
        p.stripped = true;
    }
    let is_youtube = FrontmatterDoc::new(text)
        .get_str_trimmed("source")
        .is_some_and(|s| is_youtube_source(&s));
    if is_youtube && has_post_tag(&cur) {
        let cleaned = cv_note::cleanup_youtube_transcript(&cur);
        if cleaned != cur {
            cur = cleaned;
            p.cleaned = true;
        }
        // Plan round-20-transcript-heading-auto-holds WP-1 (audit E2): only a post-tagged YouTube
        // note, whose heading the cleanup is the only known remover of, gets it back.
        if let Some(t) = cv_note::restore_transcript_heading(&cur) {
            cur = t;
            p.heading_restored = true;
        }
    }
    if is_youtube
        && let Some(publication) = meta_publication
        && let Some(t) = with_publication(&cur, publication)?
    {
        cur = t;
        p.filled = true;
    }
    p.new = (cur != text).then_some(cur);
    Ok(p)
}

/// Whether the note's `publication` is absent or blank.
fn publication_blank(text: &str) -> bool {
    match FrontmatterDoc::new(text).get("publication") {
        Ok(None | Some(JsValue::Null)) => true,
        Ok(Some(JsValue::Str(s))) => s.trim().is_empty(),
        _ => false,
    }
}

#[derive(Serialize, Default)]
struct BackfillVars {
    scanned: u32,
    stripped: u32,
    cleaned: u32,
    heading_restored: u32,
    publication_filled: u32,
    no_metadata: u32,
    conflict: u32,
    unchanged: u32,
    dry_run: bool,
}

/// The linked metadata note's publication, when the note links one that exists.
fn linked_publication(ctx: &StepCtx<'_>, text: &str) -> Result<Option<String>, StepError> {
    let doc = FrontmatterDoc::new(text);
    let list = doc.get(META_KEY).ok().flatten();
    for target in link_targets(list.as_ref()) {
        if let Some((bytes, _)) = ctx.read(&format!("{target}.md"))? {
            return Ok(metadata_publication(&String::from_utf8_lossy(&bytes)));
        }
    }
    Ok(None)
}

fn backfill(dry_run: bool, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let paths: BTreeSet<String> = ctx
        .index()?
        .query()
        .md_props_with_key("source")?
        .into_iter()
        .filter(|r| is_youtube_source(&cv_index::prop_text(&r.value)) && is_user_path(&r.path))
        .map(|r| r.path)
        .collect();
    let mut v = BackfillVars {
        dry_run,
        ..BackfillVars::default()
    };
    for path in &paths {
        let Some((bytes, _)) = ctx.read(path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        v.scanned += 1;
        let wants_pub = publication_blank(&text);
        let meta = if wants_pub {
            linked_publication(ctx, &text)?
        } else {
            None
        };
        if wants_pub && meta.is_none() {
            v.no_metadata += 1;
        }
        let first = plan(&text, meta.as_deref())?;
        if first.new.is_none() {
            v.unchanged += 1;
            continue;
        }
        let applied = if dry_run {
            first
        } else {
            let mut last = Plan::default();
            match ctx.modify(path, |cur| {
                last = plan(cur, meta.as_deref())?;
                Ok(last.new.clone())
            }) {
                Ok(Some(_)) => last,
                Ok(None) => Plan::default(),
                Err(e) if e.is_conflict() => {
                    v.conflict += 1;
                    continue;
                }
                Err(e) => return Err(e),
            }
        };
        if applied.new.is_none() && !dry_run {
            v.unchanged += 1;
            continue;
        }
        v.stripped += u32::from(applied.stripped);
        v.cleaned += u32::from(applied.cleaned);
        v.heading_restored += u32::from(applied.heading_restored);
        v.publication_filled += u32::from(applied.filled);
    }
    let mut out = StepOutcome::default();
    out.set_vars(&v);
    Ok(out)
}
