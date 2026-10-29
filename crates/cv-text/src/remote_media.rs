//! Remote-media classification and the HTML media-tag scanner (plan remote-media-sprint WP-1).
//!
//! [`classify_remote`] names what a remote URL is from its host and path extension alone; the
//! `cv` crate sniffs the content-type at download time for [`RemoteKind::Unknown`]. The video and
//! post host rules extend the old `is_embed_page` (whose truth table stays fixed); the extension
//! rule follows Crucible `classifyLocalizeMediaType` (`webm` is video) via [`Media::native`].
//! [`scan_html_media`] finds `<img>`, `<video>`, `<audio>` and `<iframe>` elements with a remote
//! source outside fenced code; malformed elements are skipped, never guessed.

use std::ops::Range;

use crate::attachments::{Media, in_ranges};
use crate::vpath::basename;

/// A video-page provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoProvider {
    /// YouTube (all hosts, incl. `youtu.be` and `youtube-nocookie.com`).
    YouTube,
    /// Vimeo.
    Vimeo,
}

/// What a remote URL points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteKind {
    /// An image file (by extension).
    Image,
    /// A provider's video page; `id` is the provider's video id (YouTube: the 11-char id).
    VideoPage {
        /// The provider.
        provider: VideoProvider,
        /// The video id.
        id: String,
    },
    /// A video file (by extension; `webm` and `m3u8` included).
    VideoFile,
    /// An audio file (by extension; `opus` and `aac` included).
    AudioFile,
    /// A PDF.
    Pdf,
    /// An X/Twitter post (`/status/` or `x.com/i/…`).
    Post,
    /// An iframe-style embed that is neither video nor post. [`classify_remote`] never returns
    /// it for a bare URL; [`scan_html_media`] callers assign it to `<iframe>` srcs that classify
    /// as [`RemoteKind::Unknown`].
    Embed,
    /// Nothing usable in the URL; the caller sniffs the content-type.
    Unknown,
}

/// The host-level page shape of a URL (shared with `is_embed_page`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageShape {
    /// A YouTube video-page path; the id when it is a valid 11-char id.
    YouTube(Option<String>),
    /// A Vimeo video page.
    Vimeo(String),
    /// An X/Twitter post-ish path; `status` when the path holds `/status/`.
    Post { status: bool },
}

fn is_youtube_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn first_segment(path: &str) -> &str {
    path.trim_start_matches('/').split('/').next().unwrap_or("")
}

pub(crate) fn page_shape(link: &str) -> Option<PageShape> {
    let u = url::Url::parse(link.trim()).ok()?;
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    let path = u.path();
    match host {
        "youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            if path == "/watch" {
                let id = u
                    .query_pairs()
                    .find(|(k, _)| k == "v")
                    .map(|(_, v)| v.into_owned());
                return Some(PageShape::YouTube(id.filter(|i| is_youtube_id(i))));
            }
            ["/shorts/", "/embed/", "/live/"]
                .iter()
                .find_map(|p| path.strip_prefix(p))
                .map(|rest| {
                    let id = first_segment(rest);
                    PageShape::YouTube(is_youtube_id(id).then(|| id.to_owned()))
                })
        }
        "youtu.be" => (path.len() > 1).then(|| {
            let id = first_segment(path);
            PageShape::YouTube(is_youtube_id(id).then(|| id.to_owned()))
        }),
        "vimeo.com" | "player.vimeo.com" => {
            let rest = if host == "player.vimeo.com" {
                path.strip_prefix("/video/")?
            } else {
                path
            };
            let id = first_segment(rest);
            (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                .then(|| PageShape::Vimeo(id.to_owned()))
        }
        "x.com" | "twitter.com" | "mobile.twitter.com" => {
            let status = path.contains("/status/");
            (status || (host == "x.com" && path.starts_with("/i/")))
                .then_some(PageShape::Post { status })
        }
        _ => None,
    }
}

/// Classifies a remote URL by host (video pages, posts) and then by path extension.
#[must_use]
pub fn classify_remote(url: &str) -> RemoteKind {
    match page_shape(url) {
        Some(PageShape::YouTube(Some(id))) => {
            return RemoteKind::VideoPage {
                provider: VideoProvider::YouTube,
                id,
            };
        }
        Some(PageShape::Vimeo(id)) => {
            return RemoteKind::VideoPage {
                provider: VideoProvider::Vimeo,
                id,
            };
        }
        Some(PageShape::Post { .. }) => return RemoteKind::Post,
        Some(PageShape::YouTube(None)) | None => {}
    }
    let Ok(u) = url::Url::parse(url.trim()) else {
        return RemoteKind::Unknown;
    };
    let Some((_, ext)) = basename(u.path()).rsplit_once('.') else {
        return RemoteKind::Unknown;
    };
    let ext = ext.to_ascii_lowercase();
    let e = ext.as_str();
    if e == "webm" || e == "m3u8" || Media::Video.native().contains(&e) {
        RemoteKind::VideoFile
    } else if Media::Images.native().contains(&e) {
        RemoteKind::Image
    } else if e == "opus" || e == "aac" || Media::Audio.native().contains(&e) {
        RemoteKind::AudioFile
    } else if Media::Pdf.native().contains(&e) {
        RemoteKind::Pdf
    } else {
        RemoteKind::Unknown
    }
}

/// An HTML media element's tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlMediaTag {
    /// `<img>`.
    Img,
    /// `<video>`.
    Video,
    /// `<audio>`.
    Audio,
    /// `<iframe>`.
    Iframe,
}

impl HtmlMediaTag {
    fn name(self) -> &'static str {
        match self {
            HtmlMediaTag::Img => "img",
            HtmlMediaTag::Video => "video",
            HtmlMediaTag::Audio => "audio",
            HtmlMediaTag::Iframe => "iframe",
        }
    }
}

/// One HTML media element with a remote source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlMedia {
    /// Byte span of the whole element (the tag for `<img>` and self-closing forms).
    pub span: Range<usize>,
    /// The tag.
    pub tag: HtmlMediaTag,
    /// The remote `src` (for video/audio without one: the first remote `<source src>`).
    pub src: String,
    /// The remote `poster`, if any.
    pub poster: Option<String>,
}

/// A parsed start tag: lowercase name, end (after `>`), self-closing flag, attributes
/// (lowercase names).
struct StartTag {
    name: String,
    end: usize,
    self_closing: bool,
    attrs: Vec<(String, String)>,
}

impl StartTag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

/// Parses the start tag at `i` (`s[i] == '<'`); `None` when malformed.
fn parse_start_tag(s: &str, i: usize) -> Option<StartTag> {
    let b = s.as_bytes();
    let mut j = i + 1;
    let name_start = j;
    while j < b.len() && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j == name_start {
        return None;
    }
    let name = s[name_start..j].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while j < b.len() && is_ws(b[j]) {
            j += 1;
        }
        match b.get(j)? {
            b'>' => {
                return Some(StartTag {
                    name,
                    end: j + 1,
                    self_closing: false,
                    attrs,
                });
            }
            b'/' if b.get(j + 1) == Some(&b'>') => {
                return Some(StartTag {
                    name,
                    end: j + 2,
                    self_closing: true,
                    attrs,
                });
            }
            b'<' => return None,
            _ => {}
        }
        let a = j;
        while j < b.len() && !is_ws(b[j]) && !matches!(b[j], b'=' | b'>' | b'<') {
            if b[j] == b'/' && b.get(j + 1) == Some(&b'>') {
                break;
            }
            j += 1;
        }
        if j == a {
            // A stray `/` or similar: skip it.
            j += 1;
            continue;
        }
        let key = s[a..j].to_ascii_lowercase();
        let mut k = j;
        while k < b.len() && is_ws(b[k]) {
            k += 1;
        }
        if b.get(k) != Some(&b'=') {
            attrs.push((key, String::new()));
            continue;
        }
        k += 1;
        while k < b.len() && is_ws(b[k]) {
            k += 1;
        }
        let value = match b.get(k)? {
            &q @ (b'"' | b'\'') => {
                let close = s[k + 1..].find(q as char)? + k + 1;
                j = close + 1;
                &s[k + 1..close]
            }
            _ => {
                let v = k;
                while k < b.len() && !is_ws(b[k]) && b[k] != b'>' {
                    k += 1;
                }
                j = k;
                &s[v..k]
            }
        };
        attrs.push((key, value.trim().to_owned()));
    }
}

/// Case-insensitive find of `needle` (ASCII) in `s` from `from`, at a tag-name boundary.
fn find_tag(s: &str, from: usize, needle: &str) -> Option<usize> {
    let b = s.as_bytes();
    let n = needle.as_bytes();
    let mut i = from;
    while i + n.len() <= b.len() {
        if b[i..i + n.len()].eq_ignore_ascii_case(n)
            && b.get(i + n.len())
                .is_none_or(|&c| !c.is_ascii_alphanumeric())
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_http(v: &str) -> bool {
    let l = v.get(..8).unwrap_or(v).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// Every `<img>`, `<video>`, `<audio>` and `<iframe>` element with a remote (`http(s)`) source
/// outside fenced code, in document order. Unclosed, `src`-less and nested same-name elements
/// are skipped.
#[must_use]
pub fn scan_html_media(content: &str) -> Vec<HtmlMedia> {
    let fences = crate::attachments::fenced_ranges(content);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = content[i..].find('<') {
        let at = i + off;
        i = at + 1;
        if in_ranges(&fences, at) {
            continue;
        }
        let Some(tag) = parse_start_tag(content, at) else {
            continue;
        };
        let kind = match tag.name.as_str() {
            "img" => HtmlMediaTag::Img,
            "video" => HtmlMediaTag::Video,
            "audio" => HtmlMediaTag::Audio,
            "iframe" => HtmlMediaTag::Iframe,
            _ => continue,
        };
        let poster = tag.attr("poster").filter(|p| is_http(p)).map(str::to_owned);
        let own_src = tag.attr("src").filter(|v| is_http(v)).map(str::to_owned);
        let (end, src) = if kind == HtmlMediaTag::Img || tag.self_closing {
            (tag.end, own_src)
        } else {
            let open = format!("<{}", kind.name());
            let close_tag = format!("</{}", kind.name());
            let Some(close) = find_tag(content, tag.end, &close_tag) else {
                continue;
            };
            let Some(gt) = content[close..].find('>') else {
                continue;
            };
            let end = close + gt + 1;
            if find_tag(&content[..close], tag.end, &open).is_some() {
                // Nested same-name element: skip the whole run, inner element included.
                i = end;
                continue;
            }
            let src = own_src.or_else(|| {
                (kind != HtmlMediaTag::Iframe)
                    .then(|| first_source_src(content, tag.end, close))
                    .flatten()
            });
            (end, src)
        };
        let Some(src) = src else {
            continue;
        };
        out.push(HtmlMedia {
            span: at..end,
            tag: kind,
            src,
            poster,
        });
        i = end;
    }
    out
}

/// An attribute of the start tag that opens `element` (an [`HtmlMedia`] span's text), e.g. an
/// `<img>`'s `alt`; `None` when absent or the tag is malformed.
#[must_use]
pub fn html_attr(element: &str, name: &str) -> Option<String> {
    let t = parse_start_tag(element, element.find('<')?)?;
    t.attr(&name.to_ascii_lowercase()).map(str::to_owned)
}

/// Whether `content` has any remote media the `localize` step converts, outside fenced code:
/// a remote image or `data:image` placeholder ([`crate::attachments::has_remote_image_refs`]),
/// any other remote markdown embed (video/post pages, audio, pdf, …), or an HTML media element
/// with a remote source (the backfill candidate predicate, plan remote-media-sprint WP-3).
#[must_use]
pub fn has_remote_media_refs(content: &str) -> bool {
    crate::attachments::has_remote_image_refs(content)
        || crate::attachments::has_remote_md_embed(content)
        || !scan_html_media(content).is_empty()
}

/// The first remote `<source src>` in `content[from..to]`.
fn first_source_src(content: &str, from: usize, to: usize) -> Option<String> {
    let inner = &content[..to];
    let mut i = from;
    while let Some(at) = find_tag(inner, i, "<source") {
        i = at + 1;
        if let Some(t) = parse_start_tag(inner, at)
            && let Some(src) = t.attr("src").filter(|v| is_http(v))
        {
            return Some(src.to_owned());
        }
    }
    None
}

#[cfg(test)]
#[path = "remote_media_tests.rs"]
mod tests;
