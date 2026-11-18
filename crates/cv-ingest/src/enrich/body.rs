//! The body model: the generated blocks stripped from a note body, and the structure strip.
//! Decisions recorded here:
//! - **Generated blocks** are wrapped in `%% cv-enrich %%` … `%% /cv-enrich %%`; the block whose
//!   callout header is `> [!removed]-` is the fold, the other the top block. A re-run strips both
//!   and carries the old fold's content into the new fold (plan E2: content already folded stays).

use cv_text::blocks;

use super::squeeze;
use crate::fold::{FOLD_HEADER, parse_meta};

pub(super) struct Stripped {
    pub(super) lines: Vec<String>,
    /// Content lines of the prior fold callout (after its header), as stored (`> `-prefixed).
    pub(super) prior_fold: Vec<String>,
    /// The prior fold(s)' `%% cv-fold … %%` group metadata lines, in order (new layout only).
    pub(super) prior_meta: Vec<String>,
    pub(super) had_generated: bool,
}

pub(super) fn strip_generated(rest: &str) -> Stripped {
    let raw: Vec<&str> = rest.split('\n').collect();
    let mut lines = Vec::with_capacity(raw.len());
    let mut prior_fold = Vec::new();
    let mut prior_meta = Vec::new();
    let (mut had_top, mut had_fold) = (false, false);
    // Only a well-formed open…close pair is generated; an unterminated open marker and
    // everything after it stay ordinary body lines (never discard user text).
    let generated = blocks::generated(&raw);
    let mut i = 0;
    while i < raw.len() {
        if let Some(j) = generated.close_at(i) {
            let mut inner = &raw[i + 1..j];
            // The blank line ending the callout is layout, not fold content (older notes lack it).
            while inner.last().is_some_and(|l| l.trim().is_empty()) {
                inner = &inner[..inner.len() - 1];
            }
            match inner.iter().position(|l| l.trim_end() == FOLD_HEADER) {
                Some(h) => {
                    had_fold = true;
                    prior_meta.extend(
                        inner[..h]
                            .iter()
                            .filter(|l| parse_meta(l).is_some())
                            .map(|s| (*s).to_owned()),
                    );
                    if !prior_fold.is_empty() {
                        prior_fold.push(">".to_owned());
                    }
                    prior_fold.extend(inner[h + 1..].iter().map(|s| (*s).to_owned()));
                }
                None => had_top = true,
            }
            i = j + 1;
            continue;
        }
        if !generated.classes[i].is_generated() {
            lines.push(raw[i].to_owned());
        }
        i += 1;
    }
    if had_top {
        let lead = lines.iter().take_while(|l| l.trim().is_empty()).count();
        lines.drain(..lead);
    }
    if had_fold {
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.push(String::new());
    }
    Stripped {
        lines,
        prior_fold,
        prior_meta,
        had_generated: had_top || had_fold,
    }
}

pub(super) fn link_safe(title: &str) -> String {
    squeeze(&title.replace(['[', ']', '|', '#', '^'], ""))
}

/// Whether a frontmatter-free note body carries a generated block: a well-formed managed
/// top block (`> [!abstract]-`) or `> [!removed]-` fold. Built on the one block parser
/// (`strip_generated`); an unterminated marker is ordinary text.
#[must_use]
pub fn has_generated_block(body: &str) -> bool {
    strip_generated(body).had_generated
}

/// A frontmatter-free body without the enrich pass's structure: every well-formed managed top
/// block (and one blank line after it) and every inserted heading (the marker, its heading line
/// and one blank line after it) are removed; `> [!removed]-` folds and every other line are kept
/// verbatim. For a body that is re-sectioned (a clip that becomes a transcript): a stale chapter
/// heading would otherwise end the new section at its first line.
#[must_use]
pub fn strip_structure(body: &str) -> String {
    let raw: Vec<&str> = body.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(raw.len());
    let generated = blocks::generated(&raw);
    let mut i = 0;
    while i < raw.len() {
        if let Some(j) = generated.close_at(i) {
            if raw[i + 1..j].iter().any(|l| l.trim_end() == FOLD_HEADER) {
                out.extend_from_slice(&raw[i..=j]);
                i = j + 1;
            } else {
                i = j + 1;
                if i < raw.len() && raw[i].trim().is_empty() {
                    i += 1;
                }
            }
            continue;
        }
        if !generated.classes[i].is_generated() {
            out.push(raw[i]);
        }
        i += 1;
    }
    out.join("\n")
}
