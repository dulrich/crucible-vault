//! The HTML-stage pre-pass ([`strip_html_chrome`]): `<head>`/`<style>`/`<script>`, preheader spacer
//! elements and pixel/icon/avatar `<img>`s.

use super::links::{is_pixel, is_profile_url, resolve_link};
use super::markdown::is_icon;
use super::{SMALL_PX, is_spacer_char};
use crate::html::attr;

/// An unquoted attribute value in an open tag also ends at the tag's `>` or a self-closing `/`.
const TAG_STOPS: &[char] = &['>', '/'];

/// Strips newsletter chrome from message HTML before conversion (see the module doc): `<head>`,
/// `<style>` and `<script>` elements, preheader spacer elements, and pixel/icon/avatar `<img>`s
/// (with their wrapping `<a>` when it held only that image). Idempotent; never touches text.
#[must_use]
pub fn strip_html_chrome(html: &str) -> String {
    let mut s = html.to_owned();
    for tag in ["head", "style", "script"] {
        s = remove_elements(&s, tag);
    }
    for tag in ["span", "p", "div"] {
        s = remove_spacer_elements(&s, tag);
    }
    strip_small_images(&s, false)
}

/// Whether the byte after `<name` ends the tag name (so `<head` does not match `<header`).
fn tag_boundary(lower: &str, after: usize) -> bool {
    matches!(
        lower.as_bytes().get(after),
        Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
    )
}

/// Byte offset of the next `<name` tag at or after `from` (name boundary respected).
fn find_open(lower: &str, name: &str, from: usize) -> Option<usize> {
    let open = format!("<{name}");
    let mut i = from;
    while let Some(rel) = lower.get(i..)?.find(&open) {
        let at = i + rel;
        if tag_boundary(lower, at + open.len()) {
            return Some(at);
        }
        i = at + open.len();
    }
    None
}

/// Offset one past the `>` of the `</name …>` closing tag at or after `from`.
fn find_close_end(lower: &str, name: &str, from: usize) -> Option<usize> {
    let close = format!("</{name}");
    let mut i = from;
    while let Some(rel) = lower.get(i..)?.find(&close) {
        let at = i + rel;
        if tag_boundary(lower, at + close.len()) {
            return lower[at..].find('>').map(|g| at + g + 1);
        }
        i = at + close.len();
    }
    None
}

/// Removes every `<name>…</name>` element (an unclosed one is left alone).
fn remove_elements(s: &str, name: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(at) = find_open(&lower, name, i) {
        let Some(end) = find_close_end(&lower, name, at) else {
            break;
        };
        out.push_str(&s[i..at]);
        i = end;
    }
    out.push_str(&s[i..]);
    out
}

/// Number of spacer units (spacer chars and spacer entities) in `inner`, or `None` when it holds
/// anything else than whitespace and spacers.
fn spacer_units(inner: &str) -> Option<usize> {
    let mut units = 0;
    let mut it = inner.char_indices();
    while let Some((i, c)) = it.next() {
        if c == '&' {
            let rest = &inner[i + 1..];
            let semi = rest.find(';').filter(|&e| e <= 8)?;
            let ent = rest[..semi].to_ascii_lowercase();
            let ok = matches!(
                ent.as_str(),
                "nbsp"
                    | "shy"
                    | "zwnj"
                    | "zwj"
                    | "#847"
                    | "#x34f"
                    | "#173"
                    | "#xad"
                    | "#160"
                    | "#xa0"
                    | "#8203"
                    | "#8204"
                    | "#8205"
                    | "#x200b"
                    | "#x200c"
                    | "#x200d"
                    | "#65279"
                    | "#xfeff"
            );
            if !ok {
                return None;
            }
            units += 1;
            for _ in 0..semi + 1 {
                it.next();
            }
        } else if is_spacer_char(c) {
            units += 1;
        } else if !c.is_whitespace() {
            return None;
        }
    }
    Some(units)
}

/// Removes `<name>` elements holding only a run (at least 4) of preheader spacers.
fn remove_spacer_elements(s: &str, name: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(at) = find_open(&lower, name, i) {
        let Some(gt) = lower[at..].find('>').map(|g| at + g + 1) else {
            break;
        };
        let inner_end = lower[gt..].find('<').map_or(lower.len(), |r| gt + r);
        let removable = spacer_units(&s[gt..inner_end]).is_some_and(|n| n >= 4)
            && lower[inner_end..].starts_with(&format!("</{name}"));
        if let (true, Some(end)) = (removable, find_close_end(&lower, name, inner_end)) {
            out.push_str(&s[i..at]);
            i = end;
        } else {
            out.push_str(&s[i..gt]);
            i = gt;
        }
    }
    out.push_str(&s[i..]);
    out
}

/// The value of attribute `name` in the open tag `tag` (quotes handled, entities left as is).
/// A pixel size: digits with an optional `px` (a percentage or `auto` is unknown).
fn parse_px(v: &str) -> Option<u32> {
    let v = v.trim().to_ascii_lowercase();
    let v = v.trim_end_matches("!important").trim();
    v.strip_suffix("px").unwrap_or(v).trim().parse().ok()
}

/// The pixel `width`/`height` an `<img>` tag declares (attribute and inline style).
fn declared_dims(tag: &str) -> Vec<u32> {
    let style = attr(tag, "style", TAG_STOPS).unwrap_or_default();
    let mut out = Vec::new();
    for prop in ["width", "height"] {
        if let Some(px) = attr(tag, prop, TAG_STOPS).and_then(parse_px) {
            out.push(px);
        }
        for decl in style.split(';') {
            if let Some((k, v)) = decl.split_once(':')
                && k.trim().eq_ignore_ascii_case(prop)
                && let Some(px) = parse_px(v)
            {
                out.push(px);
            }
        }
    }
    out
}

/// Whether an `<img>` open tag is a pixel, icon or avatar.
fn is_chrome_img(tag: &str) -> bool {
    let src = attr(tag, "src", TAG_STOPS)
        .unwrap_or_default()
        .replace("&amp;", "&");
    if is_pixel(&src) || is_icon(&src) {
        return true;
    }
    let dims = declared_dims(tag);
    !dims.is_empty() && dims.iter().all(|d| *d <= SMALL_PX)
}

/// `s` without its tags (rough: for the "anchor held nothing" test).
fn text_of(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Drops chrome `<img>`s (all of them when `force`, inside a profile link); an anchor left with
/// no text and no image is dropped with them.
fn strip_small_images(s: &str, force: bool) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    loop {
        let img = find_open(&lower, "img", i);
        let a = find_open(&lower, "a", i);
        let (at, is_img) = match (img, a) {
            (Some(x), Some(y)) if y < x => (y, false),
            (Some(x), _) => (x, true),
            (None, Some(y)) => (y, false),
            (None, None) => break,
        };
        let Some(gt) = lower[at..].find('>').map(|g| at + g + 1) else {
            break;
        };
        out.push_str(&s[i..at]);
        let tag = &s[at..gt];
        if is_img {
            if !(force || is_chrome_img(tag)) {
                out.push_str(tag);
            }
            i = gt;
        } else if let Some(end) = find_close_end(&lower, "a", gt) {
            let close_at = lower[gt..end].rfind("</a").map_or(end, |r| gt + r);
            let inner = &s[gt..close_at];
            let href = attr(tag, "href", TAG_STOPS)
                .unwrap_or_default()
                .replace("&amp;", "&");
            let profile = resolve_link(&href).is_some_and(|t| is_profile_url(&t));
            let kept = strip_small_images(inner, force || profile);
            if kept == inner || !text_of(&kept).trim().is_empty() || lower_has_img(&kept) {
                out.push_str(tag);
                out.push_str(&kept);
                out.push_str(&s[close_at..end]);
            }
            i = end;
        } else {
            out.push_str(tag);
            i = gt;
        }
    }
    out.push_str(&s[i..]);
    out
}

fn lower_has_img(s: &str) -> bool {
    find_open(&s.to_ascii_lowercase(), "img", 0).is_some()
}
