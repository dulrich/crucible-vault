//! `media_cleanup`: the one step of the built-in `removals-media-cleanup` action (plan
//! `intake-removals-sources-rescan` WP-4, audit M4), enqueued by `resolve_removals` after the
//! note write with the managed media files the deleted lines linked (`paths` job param).
//!
//! The step holds [`cv_actions::ATTACHMENTS_KEY`] (intrinsic), so it never runs between a
//! localize run's attachment selection and its note commit. Each candidate: not a managed
//! media name ([`is_managed_media`], the `tidy`/`localize` rule) or under `.trash/` →
//! `media_skipped` before any filesystem check (so a user action naming the step can never
//! trash a note or an unmanaged file); missing / not a file / unreadable → `media_skipped`; still linked (fresh index) → `media_referenced`; younger
//! than the `tidy` minimum age → `media_too_young` (left to the daily sweep); else the unchanged
//! `tidy` writer-side re-check [`super::tidy::sweep_one`] trashes it with `reason = orphan` →
//! `media_trashed` (a refused re-check → `media_skipped`). Outcome vars and logs carry counts
//! only, never paths.

use std::os::unix::fs::MetadataExt;

use cv_actions::{ConfigSet, StepKind};
use serde::Serialize;
use serde_json::Value;

use super::params::check_keys;
use crate::steps::{StepCtx, StepError, StepModule, StepOutcome};

/// Whether vault path `path` names a managed media file (`…_MD5.<media ext>`): the one rule
/// shared by `resolve_removals`' candidate selection and this step.
pub(crate) fn is_managed_media(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    cv_text::attachments::is_md5_name(name) && cv_text::attachments::classify_media(ext).is_some()
}

/// The step (no step params; the candidates come from the `paths` job param).
pub(crate) struct MediaCleanup;

/// Outcome vars.
#[derive(Debug, Default, Serialize)]
struct Vars {
    media_trashed: u32,
    media_too_young: u32,
    media_referenced: u32,
    media_skipped: u32,
}

impl StepModule for MediaCleanup {
    type Params = ();

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<(), String> {
        check_keys(t, kind, "media_cleanup")
    }

    fn run((): (), ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        let paths: Vec<String> = match ctx.params.get("paths") {
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()
                .ok_or_else(|| StepError::failed("media_cleanup: `paths` must be strings"))?,
            _ => return Err(StepError::failed("media_cleanup needs a `paths` job param")),
        };
        let min_age_ms = super::tidy::configured_min_age_ms(ctx.config);
        let now = crate::now_ms();
        let mut v = Vars::default();
        for path in paths {
            if !is_managed_media(&path) || path.starts_with(".trash/") {
                v.media_skipped += 1;
                continue;
            }
            let Ok(md) = ctx.vault_root.join(&path).symlink_metadata() else {
                v.media_skipped += 1;
                continue;
            };
            let Some((_, hash)) = ctx.read(&path).ok().flatten().filter(|_| md.is_file()) else {
                v.media_skipped += 1;
                continue;
            };
            let referenced = {
                let reader = ctx.index()?;
                cv_index::referenced_elsewhere(&reader.query(), &path, "").unwrap_or(true)
            };
            let mtime_ms = md.mtime() * 1000 + md.mtime_nsec() / 1_000_000;
            if referenced {
                v.media_referenced += 1;
            } else if now - mtime_ms < min_age_ms {
                v.media_too_young += 1;
            } else {
                let root = ctx.vault_root.to_path_buf();
                let ok = ctx.with_writer(&[], move |vw| {
                    super::tidy::sweep_one(vw, &root, &path, hash, min_age_ms, now)
                })?;
                if ok == super::tidy::Sweep::Trashed {
                    v.media_trashed += 1;
                } else {
                    v.media_skipped += 1;
                }
            }
        }
        tracing::info!(
            media_trashed = v.media_trashed,
            media_too_young = v.media_too_young,
            media_referenced = v.media_referenced,
            media_skipped = v.media_skipped,
            "removals media cleanup"
        );
        let mut out = StepOutcome::default();
        out.set_vars(&v);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use cv_actions::{ConfigSet, ResourceSpec};
    use cv_vault::test_support::LiveFixture;
    use serde_json::json;

    use crate::steps::testkit::{ActionBuilder, StepEnv};

    /// Old, unlinked, non-managed files (a note, an unmanaged image, a managed name under
    /// `.trash/`) are skipped and stay in place.
    #[test]
    fn non_managed_paths_are_skipped_and_kept() {
        let files = [
            ("notes/plain.md", "# plain\n"),
            ("media/plain.png", "png"),
            (".trash/old_MD5.png", "png"),
        ];
        let action = ActionBuilder::new(
            "removals-media-cleanup",
            vec![],
            ResourceSpec {
                note: false,
                keys: vec![],
            },
        )
        .build();
        let e = StepEnv::new(LiveFixture::new(files), ConfigSet::new(), action);
        let old = SystemTime::now() - Duration::from_secs(72 * 3600);
        for (p, _) in files {
            std::fs::File::options()
                .write(true)
                .open(e.fx.path(p))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let o = e
            .run(
                "type = \"media_cleanup\"",
                json!({"paths": files.map(|(p, _)| p)}),
                cv_protocol::ActionContext::default(),
            )
            .unwrap();
        assert_eq!(o.vars["media_skipped"], 3);
        assert_eq!(o.vars["media_trashed"], 0);
        for (p, _) in files {
            assert!(e.fx.path(p).exists(), "{p}");
        }
    }
}
