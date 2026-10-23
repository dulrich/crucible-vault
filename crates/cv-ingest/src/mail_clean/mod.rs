//! Newsletter-chrome cleanup for email bodies (plan `intake-followups-mail-youtube` WP-1).
//!
//! [`clean_body`] is a pure, idempotent markdown pass over what [`crate::html_to_markdown`]
//! produces for a newsletter. It exists because captured mail notes carried wrapper chrome:
//! tracking redirects, button rows, subscribe prompts and the unsubscribe footer. Nothing here
//! has an Obsidian or Crucible ancestor (Crucible never ingested mail bodies); the structure was
//! read from public Substack/beehiiv/HubSpot newsletter mail.
//!
//! What it does: (1) cuts the unsubscribe footer (the last `unsubscribe` link's paragraph to the
//! end, only when a short tail follows); (2) drops chrome lines (like/comment/restack/share
//! buttons, "Forwarded this email?", "Upgrade to paid", "Read in app", "View in browser", …) and
//! tracking pixels; (3) rewrites every link target: a Substack `/redirect/<n>/<payload>` is
//! unwrapped to its target (the payload is `base64url(JSON {"e": <url>, …}).<sig>`, decodable
//! offline; an undecodable one keeps the anchor text and drops the link, never the tracker), an
//! `/app-link/…` is canonicalized to its post URL when the query carries one (else anchor text),
//! beehiiv/HubSpot wrappers unwrap only when the target is literally in the URL, and tracking
//! query params are stripped (`utm_*` everywhere; `r`, `s`, `triedRedirect`, `publication_id`,
//! `post_id`, `isFreemail`, `token` on Substack hosts). Prose paragraphs are never removed or
//! altered other than link-target rewriting.
//!
//! Round 2 (plan `intake-round-2-clean-dedupe-reading` WP-1, measured on 32 newsletter messages)
//! also removes the residue round 1 left: [`strip_html_chrome`] is an HTML-stage pre-pass that cuts
//! `<head>`/`<style>`/`<script>` elements (the CSS that leaked into the body), preheader spacer
//! elements (U+034F, soft hyphen, zero-width and NBSP runs of at least 4) and every `<img>` that is
//! a tracking pixel, an icon or an avatar, with its wrapping `<a>` when the anchor held only that
//! image. A pixel/icon/avatar is: a known tracker or icon URL (`substack.com/icon/…`, a filename
//! containing `icon`), an image whose every known dimension is **at most 64 px** (`width`/`height`
//! attributes, inline `style` `width`/`height`, or the `w_N`/`h_N` resize hints of an image-CDN
//! URL; this includes 1x1), or any image inside a link to a profile (`substack.com/@…`,
//! `/profile/…`, also behind a redirect wrapper). Avatars and icons are removed, never resized.
//! Publication logos (for example 183x40) and post-body images are kept.
//!
//! At the markdown stage [`clean_body`] additionally drops preheader spacer-only lines, the
//! referral block ("Invite your friends and earn rewards" heading, its paragraph and its
//! `Invite Friends`/`Subscribed` link lines), a CSS-shaped line (`@media`/`@font-face`/rule runs,
//! the backstop for style that leaked anyway), and the same pixel/icon/avatar images (by URL
//! shape, with the 64 px threshold above). [`clean_body_with`] takes a caller-supplied
//! `Fn(&str) -> Option<(u32, u32)>` giving the decoded size of an already-localized image, and
//! drops a markdown image (and its wrapping link) reported at 64x64 or smaller; the hook keeps
//! this crate free of I/O.
//!
//! The paywall-marker verdict must be taken on the **pre-cleanup** markdown by the caller (removing
//! an "Upgrade to paid" line must never turn a partial message complete). `strip_html_chrome` only
//! removes styling, spacers and small images, none of which can carry a paywall marker, so the
//! caller may take the verdict on the markdown of the stripped HTML.
//!
//! Pure: no I/O, no async.
//!
//! Layout: this file holds the line-level markdown pass; [`markdown`] scans links/images and
//! classifies chrome lines, [`links`] resolves link targets (redirects, tracking params, pixels),
//! [`html`] is the HTML-stage pre-pass.

mod html;
mod links;
mod markdown;

use crate::boilerplate::fence_mask;
pub use html::strip_html_chrome;
pub(crate) use links::resolve_link;
use markdown::{is_chrome_line, norm_label, only_items, rewrite_line};
#[cfg(test)]
use {links::*, markdown::*};

/// Looks up the decoded `(width, height)` of an already-localized image by its markdown target.
pub type Dims<'a> = &'a dyn Fn(&str) -> Option<(u32, u32)>;

/// Images at most this many pixels in every known dimension are pixels, icons or avatars.
const SMALL_PX: u32 = 64;

/// Cleans newsletter chrome out of `md` (see the module doc). Idempotent.
#[must_use]
pub fn clean_body(md: &str) -> String {
    clean_body_with(md, &|_| None)
}

/// [`clean_body`] plus the image-dimension hook: a markdown image (and its wrapping link) whose
/// target `dims` reports at 64x64 or smaller is dropped. Idempotent.
#[must_use]
pub fn clean_body_with(md: &str, dims: Dims<'_>) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let fenced = fence_mask(&lines);
    let end = footer_start(&lines, &fenced).unwrap_or(lines.len());
    let referral = referral_mask(&lines[..end], &fenced[..end]);
    let mut out: Vec<String> = Vec::with_capacity(end);
    for (i, line) in lines[..end].iter().enumerate() {
        if fenced[i] {
            out.push((*line).to_owned());
        } else if referral[i]
            || is_spacer_line(line)
            || is_css_line(line)
            || is_chrome_line(line, dims)
        {
            out.push(String::new());
        } else {
            out.push(rewrite_line(line, dims));
        }
    }
    collapse_blanks(&out, &fenced[..end])
}

/// Characters newsletters use as invisible preheader padding.
fn is_spacer_char(c: char) -> bool {
    matches!(
        c,
        '\u{034f}' | '\u{00ad}' | '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{00a0}'
    )
}

/// A non-blank line made only of whitespace and spacer characters.
fn is_spacer_line(line: &str) -> bool {
    !line.trim().is_empty() && line.chars().all(|c| c.is_whitespace() || is_spacer_char(c))
}

/// A long line that is structurally CSS: at least 200 bytes, no backtick or markdown link, and
/// either an at-rule (`@media`/`@font-face`/`@import`) with a rule body, or at least 3
/// `selector { prop: val; }` groups. Prose quoting inline code is never matched.
fn is_css_line(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 200 || t.contains('`') || t.contains("](") {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    let at_rule = ["@media", "@font-face", "@import"]
        .iter()
        .any(|a| lower.contains(a));
    let groups = t
        .split('}')
        .filter(|g| {
            g.split_once('{')
                .is_some_and(|(_, b)| (b.contains(':') && b.contains(';')) || has_declaration(b))
        })
        .count();
    t.contains('{') && ((at_rule && groups >= 1) || groups >= 3)
}

/// Whether a block body holds a `prop: value` declaration (a `:` directly after a word
/// character, followed by a value), so minified CSS without a `;` still counts.
fn has_declaration(body: &str) -> bool {
    let b = body.as_bytes();
    b.iter().enumerate().any(|(i, &c)| {
        c == b':'
            && i > 0
            && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'-')
            && body[i + 1..]
                .trim_start()
                .chars()
                .next()
                .is_some_and(|n| !matches!(n, '}' | '{'))
    })
}

/// The lines of the referral block: an "Invite your friends" heading, then the paragraph and
/// link-only lines that follow it.
fn referral_mask(lines: &[&str], fenced: &[bool]) -> Vec<bool> {
    let mut mask = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        let heading = t.starts_with('#') || t.starts_with("**");
        if fenced[i]
            || !heading
            || !norm_label(t.trim_start_matches('#')).starts_with("invite your friends")
        {
            i += 1;
            continue;
        }
        mask[i] = true;
        let mut j = i + 1;
        loop {
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            let start = j;
            while j < lines.len() && !lines[j].trim().is_empty() && !fenced[j] {
                j += 1;
            }
            if start == j || !referral_block(&lines[start..j]) {
                break;
            }
            mask[start..j].iter_mut().for_each(|m| *m = true);
        }
        i = j.max(i + 1);
    }
    mask
}

/// Whether a paragraph belongs to the referral block: the "earn rewards" blurb, or link-only
/// lines labelled `Invite Friends`/`Subscribed`.
fn referral_block(block: &[&str]) -> bool {
    let flat = norm_label(&block.join(" "));
    if flat.contains("earn rewards") || flat.contains("share it with your friends") {
        return true;
    }
    block.iter().all(|l| {
        only_items(l).is_some_and(|items| {
            items.iter().all(|sp| {
                !sp.image
                    && matches!(
                        norm_label(&l[sp.inner.0..sp.inner.1]).as_str(),
                        "invite friends" | "subscribed" | "invite" | "refer a friend"
                    )
            })
        })
    })
}

/// Joins the lines, dropping whitespace-only runs down to one blank line (outside code fences)
/// and the leading/trailing blank lines.
fn collapse_blanks(lines: &[String], fenced: &[bool]) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut prev_blank = true;
    for (l, f) in lines.iter().zip(fenced) {
        let blank = !f && l.trim().is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push(if blank { "" } else { l });
        prev_blank = blank;
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// Most non-blank lines allowed after the unsubscribe paragraph for it to count as a footer.
const FOOTER_TAIL_MAX: usize = 15;

/// Index of the first line of the footer: the paragraph holding the last `unsubscribe` link
/// (plus a directly preceding `©` paragraph), when only a short tail follows it.
fn footer_start(lines: &[&str], fenced: &[bool]) -> Option<usize> {
    let hit = (0..lines.len()).rev().find(|&i| {
        !fenced[i]
            && lines[i].contains("](")
            && lines[i].to_ascii_lowercase().contains("unsubscribe")
    })?;
    let tail = lines[hit + 1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .count();
    if tail > FOOTER_TAIL_MAX {
        return None;
    }
    let mut start = hit;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    // A copyright paragraph directly above (separate from the unsubscribe paragraph).
    let mut p = start;
    while p > 0 && lines[p - 1].trim().is_empty() {
        p -= 1;
    }
    if p < start && p > 0 {
        let mut q = p;
        while q > 0 && !lines[q - 1].trim().is_empty() {
            q -= 1;
        }
        if lines[q].trim_start().starts_with('©') {
            start = q;
        }
    }
    Some(start)
}

#[cfg(test)]
#[path = "../mail_clean_tests.rs"]
mod tests;
