//! Dated review notes, the one writer (plan `tn-review-2026-11-10-remediation` WP-R8, review S4):
//! the path `<app folder>/reviews/<stem><date>.md`, a create-or-replace upsert with an explicit
//! overwrite policy, and the one guarded note link. `fanfic_title` (always overwrites: it holds
//! no user input) and `removals_review` (refuses unless `overwrite`: the user's ticks) write
//! through [`upsert`]; `materialize` shares the create-or-write step through [`Slot`].

use cv_actions::ConfigSet;
use cv_text::vpath;
use cv_vault::ContentHash;

use super::{StepCtx, StepError};

/// What to do when the review note already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overwrite {
    /// Replace it.
    Always,
    /// Fail the step (the note may hold the user's ticks).
    Refuse,
}

/// Where review notes live: `<app folder>/reviews` (`vault.toml` `[structure] app_folder`, plan
/// public-release WP-2; default `_crucible/reviews`).
pub(crate) fn review_dir(config: &ConfigSet) -> String {
    format!("{}/reviews", config.app_folder())
}

/// `<review dir>/<stem><date>.md`.
pub(crate) fn dated_path(config: &ConfigSet, stem: &str, date: &str) -> String {
    format!("{}/{stem}{date}.md", review_dir(config))
}

/// A note path read once: where [`Slot::write`] creates it (absent) or replaces it (present,
/// compare-and-swap on the hash read).
pub(crate) struct Slot {
    path: String,
    existing: Option<(Vec<u8>, ContentHash)>,
}

impl Slot {
    /// Reads `path`.
    pub(crate) fn read(ctx: &StepCtx<'_>, path: &str) -> Result<Self, StepError> {
        Ok(Self {
            path: path.to_owned(),
            existing: ctx.read(path)?,
        })
    }

    /// The bytes read, when the note exists.
    pub(crate) fn existing(&self) -> Option<&[u8]> {
        self.existing.as_ref().map(|(b, _)| b.as_slice())
    }

    /// Creates the note, or replaces what was read; `true` when it was created.
    pub(crate) fn write(self, ctx: &mut StepCtx<'_>, text: Vec<u8>) -> Result<bool, StepError> {
        match self.existing {
            Some((_, hash)) => {
                ctx.write(&self.path, text, Some(hash))?;
                Ok(false)
            }
            None => {
                ctx.create(&self.path, text)?;
                Ok(true)
            }
        }
    }
}

/// Reads the review note at `path` and applies `policy` to an existing one, before the caller
/// does any expensive work: [`Overwrite::Refuse`] on an existing note fails the step as
/// `"{step}: {path} already exists (it may hold ticks); run with overwrite = true to replace it"`.
pub(crate) fn claim(
    ctx: &StepCtx<'_>,
    step: &str,
    path: &str,
    policy: Overwrite,
) -> Result<Slot, StepError> {
    let slot = Slot::read(ctx, path)?;
    if slot.existing.is_some() && policy == Overwrite::Refuse {
        return Err(StepError::failed(format!(
            "{step}: {path} already exists (it may hold ticks); run with overwrite = true to replace it"
        )));
    }
    Ok(slot)
}

/// Writes the review note `text` at `path` under `policy` (one read, one create-or-replace).
pub(crate) fn upsert(
    ctx: &mut StepCtx<'_>,
    step: &str,
    path: &str,
    text: Vec<u8>,
    policy: Overwrite,
) -> Result<(), StepError> {
    claim(ctx, step, path, policy)?.write(ctx, text).map(drop)
}

/// A resolvable wikilink `[[path-without-.md]]`, or `[[path-without-.md|basename]]` with
/// `aliased`; a name with link syntax characters (`|`, `[`, `]`, `#`, `^`) falls back to a
/// backticked path, so no note path yields a broken link.
pub(crate) fn link(path: &str, aliased: bool) -> String {
    let stem = path.strip_suffix(".md").unwrap_or(path);
    if stem.contains(['|', '[', ']', '#', '^']) {
        return format!("`{path}`");
    }
    if aliased {
        format!("[[{stem}|{}]]", vpath::stem(path))
    } else {
        format!("[[{stem}]]")
    }
}

#[cfg(test)]
mod tests {
    use super::link;

    #[test]
    fn a_link_to_a_path_with_syntax_characters_is_not_a_broken_wikilink() {
        assert_eq!(link("A/n.md", false), "[[A/n]]");
        assert_eq!(link("A/n.md", true), "[[A/n|n]]");
        for p in ["A/a|b.md", "A/a#b.md", "A/a]b.md", "A/a[b.md"] {
            assert_eq!(link(p, false), format!("`{p}`"), "{p}");
            assert_eq!(link(p, true), format!("`{p}`"), "{p}");
        }
    }
}
