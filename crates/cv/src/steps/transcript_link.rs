//! The link-only transcript pair (plan intake-removals-sources-rescan Decision 6): a complete,
//! free tracker post whose body is one single-link line ([`is_link_only`]) and a clip that holds
//! its transcript. [`link_pair`] owns the two frontmatter edits — the clip's `transcript-of` and
//! `## Transcript` section, the post's `transcript` and publication — and the callers
//! (`auto_file`'s capture branch, the reconcile's phase G) own when and whether each is written.

use cv_note::{FmError, FrontmatterDoc, JsValue};
use cv_state::TrackerItem;

use super::{StepCtx, StepError};

/// The link-only predicate (Decision 6), pure: `rows` are the `blog` tracker rows resolving to the
/// target. True when there is at least one and every one is complete (`Some(true)`), free
/// (`access` is `free`; paid, founding, unknown or NULL do not qualify) with `word_count` 0; the
/// target's body satisfies [`cv_ingest::transcript::is_link_only_body`]; and the clip has at least
/// [`cv_ingest::transcript::MIN_REFINE_WORDS`] words. It also matches an audio-only episode:
/// nothing stored tells a video link from an audio one.
pub(in crate::steps) fn is_link_only(
    rows: &[TrackerItem],
    target_body: &str,
    clip_words: usize,
) -> bool {
    if clip_words < cv_ingest::transcript::MIN_REFINE_WORDS {
        return false;
    }
    let blog: Vec<&TrackerItem> = rows.iter().filter(|r| r.source_kind == "blog").collect();
    let rows_ok = !blog.is_empty()
        && blog.iter().all(|r| {
            r.complete == Some(true)
                && r.word_count == Some(0)
                && r.access
                    .as_deref()
                    .is_some_and(|a| a.trim().eq_ignore_ascii_case("free"))
        });
    rows_ok && cv_ingest::transcript::is_link_only_body(target_body)
}

/// [`is_link_only`] for `target`, reading its tracker rows (the `StateDb` guard is dropped before
/// returning).
pub(in crate::steps) fn target_is_link_only(
    ctx: &StepCtx<'_>,
    target: &str,
    target_body: &str,
    clip_words: usize,
) -> Result<bool, StepError> {
    let rows = ctx.state().tracker_items_for_note(target)?;
    Ok(is_link_only(&rows, target_body, clip_words))
}

/// The two text edits that link a transcript clip to the post at `post_path`.
pub(in crate::steps) fn link_pair(post_path: &str) -> LinkPair {
    LinkPair {
        post_link: cv_text::wikilink::link_to(post_path),
    }
}

/// See [`link_pair`].
pub(in crate::steps) struct LinkPair {
    post_link: String,
}

impl LinkPair {
    /// The clip edit: `transcript-of: "[[post]]"` replaces `duplicate-of`, and the body goes under
    /// one `## Transcript` section unless it already has one (then the clip's enrich stamp stays,
    /// else it is cleared in the same text: the text under refine/enrich changed). `None` when
    /// nothing changes. Frontmatter outside the named keys is byte-preserved.
    pub(in crate::steps) fn clip(&self, text: &str) -> Result<Option<String>, FmError> {
        let mut d = FrontmatterDoc::new(text);
        let mut changed = d.set("transcript-of", &JsValue::Str(self.post_link.clone()))?;
        changed |= d.remove("duplicate-of")?;
        let body = d.body().to_owned();
        if cv_ingest::transcript::locate(&body).is_none() {
            let mut out = d.into_text();
            let off = out.len() - body.len();
            // A clip enriched before it was linked carries the enrich pass's top block and chapter
            // headings; a `##` heading would end the new section at its first line, so they go
            // (the enrich re-runs after the refine).
            let body = cv_ingest::enrich::strip_structure(&body);
            let lead = body.len() - body.trim_start().len();
            out.truncate(off);
            out.push_str(&body[..lead]);
            out.push_str("## Transcript\n\n");
            out.push_str(&body[lead..]);
            let mut d = FrontmatterDoc::new(out);
            super::enrich_stamp::clear(&mut d)?;
            return Ok(Some(d.into_text()));
        }
        Ok(changed.then(|| d.into_text()))
    }

    /// The post edit: `transcript: "[[clip_path]]"` (when its `transcript` is blank) and, when the
    /// post's own `publication` is blank and `publication` is given, that publication (after
    /// `author`). `None` when nothing changes.
    pub(in crate::steps) fn post(
        &self,
        text: &str,
        clip_path: &str,
        publication: Option<&str>,
    ) -> Result<Option<String>, FmError> {
        let mut d = FrontmatterDoc::new(text);
        // A post that already names a transcript keeps it (never flap between two clips).
        let mut changed = false;
        if d.is_blank("transcript")? {
            let clip_link = cv_text::wikilink::link_to(clip_path);
            changed = d.set("transcript", &JsValue::Str(clip_link))?;
        }
        if let Some(p) = publication
            && crate::supersede::publication_blank(&d)
        {
            d.insert_after("author", "publication", &JsValue::Str(p.to_owned()))?;
            changed = true;
        }
        Ok(changed.then(|| d.into_text()))
    }
}

/// The non-blank `publication` of the note `text`.
pub(in crate::steps) fn publication_of(text: &str) -> Option<String> {
    let d = FrontmatterDoc::new(text);
    if crate::supersede::publication_blank(&d) {
        return None;
    }
    d.get("publication")
        .ok()
        .flatten()
        .map(|v| cv_note::js_text(&v))
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
