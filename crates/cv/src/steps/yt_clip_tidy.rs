//! Step module `yt_clip_tidy` (plan `round-19-yt-cleanup-intake` WP-2): deletes a clipped YouTube
//! note's thumbnail embed and the clipped description below it, ahead of `## Transcript`, via
//! [`cv_note::strip_youtube_preamble`]. The one delete-not-fold exception to D29: the description
//! is kept in the linked `_yt_metadata` note and the frontmatter `description`.
//!
//! No params (a stray key disarms the action). Runs on the active note only, and only when its
//! `source` is a YouTube URL (a note that merely embeds a video is never touched). Leading
//! `%% … %%` managed regions, the frontmatter and everything at or after `## Transcript` stay
//! byte-identical; a body that does not fully match the shape is left alone, so a second run is a
//! no-op. Written through [`StepCtx::modify`] (CAS, I2). Timestamp cleanup is not done here — it
//! stays on the kind `post` `cleanup_transcript` lint path. Like `transcript_refine`, there is no
//! editor hold for a custom step; the CAS hash is the protection.
//!
//! Outcome vars: `stripped` (bool).

use cv_actions::{ConfigSet, StepKind};
use cv_note::FrontmatterDoc;
use serde::Serialize;

use super::intake_enrich::is_youtube_source;
use super::{StepCtx, StepError, StepModule, StepOutcome, no_params};

/// The `yt_clip_tidy` step.
pub(crate) struct YtClipTidy;

impl StepModule for YtClipTidy {
    type Params = ();

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<(), String> {
        no_params(kind, t)
    }

    fn run((): (), ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
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
}

/// Outcome vars.
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
