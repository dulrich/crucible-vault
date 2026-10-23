//! Attachment-ref domain: parsing, rewriting, repair planning and path templating of a note's
//! attachment refs — the pure half of the daemon's `localize` step,
//! shared with the `write_attachment` RPC.
//!
//! Ported from Crucible `src/attachmentRepair.ts` (function by function, same names in snake
//! case) and the pure helpers of `src/localizeAttachments.ts`; the `localize` step module
//! (`crates/cv/src/steps/localize/mod.rs`) lists the behaviours kept and dropped. Folder slugs
//! come from the one [`crate::template::slugify`].
//! Measured: no existing attachment folder changes name.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use chrono::NaiveDateTime;
use regex::Regex;

use crate::template::{AttachmentTemplateContext, apply_attachment_template};
use crate::vpath::{basename, percent_decode, split};

/// Crucible `MD5_NAME_RE` (`/_MD5\.[A-Za-z0-9]+$/`).
pub fn is_md5_name(name: &str) -> bool {
    match name.rsplit_once("_MD5.") {
        Some((_, ext)) => !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric()),
        None => false,
    }
}

/// Crucible `PREFIX_REPAIR_MIN_STEM_LENGTH`: a broken managed basename's stem must keep at least
/// 8 hex chars (32 bits of the hash) before prefix recovery is attempted.
pub const PREFIX_REPAIR_MIN_STEM_LENGTH: usize = 8;

fn re(cell: &'static OnceLock<Regex>, src: &str) -> &'static Regex {
    cell.get_or_init(|| match Regex::new(src) {
        Ok(r) => r,
        // The patterns are literals covered by tests; an invalid one is a programming error.
        Err(e) => panic!("invalid built-in regex {src:?}: {e}"),
    })
}

/// Crucible `MARKDOWN_ATTACHMENT_REF_RE`: embeds first (alternation order is load-bearing), then
/// wiki links, then markdown links whose text excludes `[` (so a linked image
/// `[![](img)](href)` yields the nested embed).
fn attachment_ref_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"!\[\[[^\]\n]+\]\]|!\[[^\]\n]*\]\((?:\\.|[^)\\\n])+\)|\[\[[^\]\n]+\]\]|\[[^\[\]\n]*\]\((?:\\.|[^)\\\n])+\)",
    )
}

/// A managed (`<md5>_MD5.<image ext>`) image name, for the `images` outcome var (plan
/// `v0-automation-pipelines` WP-6: the image-describe trigger seam).
fn managed_image_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"(?i)[0-9a-f]{32}_MD5\.(?:png|jpe?g|gif|webp|bmp|svg|avif)\b",
    )
}

/// Crucible `REMOTE_MD_IMAGE_RE`, plus an optional CommonMark link title (`"t"` / `'t'`) after the
/// URL — Substack clips title every image, and Crucible's pattern missed those.
fn remote_md_image_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r#"!\[([^\]]*)\]\((https?://(?:\\.|[^)\s\\])+)(?:[ \t]+(?:"[^"\n]*"|'[^'\n]*'))?[ \t]*\)"#,
    )
}

/// Crucible `DATA_URI_IMAGE_RE` (placeholder plus trailing inline whitespace).
fn data_uri_image_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"!\[[^\]]*\]\(\s*data:image/[^)]*\)[ \t]*")
}

/// Ref syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// `[[…]]` / `![[…]]`.
    Wiki,
    /// `[…](…)` / `![…](…)`.
    Md,
}

/// Crucible `AttachmentMatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentMatch {
    /// The ref's exact source text.
    pub original: String,
    /// The link target (md: `<>` stripped and %-decoded unless remote; wiki: before `|`).
    pub link: String,
    /// Wiki or markdown.
    pub syntax: Syntax,
    /// `http(s)://` target.
    pub is_remote: bool,
    /// `!`-prefixed embed (repair must never turn a link into an embed or back).
    pub is_embed: bool,
    /// Wiki alias / markdown text (kept on repaired links).
    pub display_text: Option<String>,
}

/// Crucible `AttachmentReplacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// Exact source text to replace.
    pub from: String,
    /// New ref.
    pub to: String,
}

/// Crucible `RewriteAttachmentRefsResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteResult {
    /// Rewritten content.
    pub content: String,
    /// The `from` values whose replacement actually landed (a subset of the request).
    pub applied_from: Vec<String>,
}

/// Crucible `extractRefLinkTarget`.
fn extract_ref_link_target(r: &str) -> Option<&str> {
    let body = r.strip_prefix('!').unwrap_or(r);
    if let Some(inner) = body.strip_prefix("[[") {
        let end = inner.find([']', '|']).unwrap_or(inner.len());
        let t = &inner[..end];
        return (!t.is_empty()).then_some(t);
    }
    if body.starts_with('[') && body.ends_with(')') {
        let close = body.find("](")?;
        let t = &body[close + 2..body.len() - 1];
        return (!t.is_empty()).then_some(t);
    }
    None
}

/// Unescapes a CommonMark backslash-escaped URL for download: `\(`/`\)` become `%28`/`%29`
/// (so the URL stays one token), any other `\x` becomes `x`.
fn unescape_md_url(u: &str) -> String {
    let mut out = String::with_capacity(u.len());
    let mut it = u.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('(') => out.push_str("%28"),
            Some(')') => out.push_str("%29"),
            Some(x) => out.push(x),
            None => out.push('\\'),
        }
    }
    out
}

fn strip_angle(s: &str) -> &str {
    let s = s.strip_prefix('<').unwrap_or(s);
    s.strip_suffix('>').unwrap_or(s)
}

/// Crucible `isManagedAttachmentLink`: the gate that keeps ordinary note links out of the
/// rewrite — true only when the target decodes to a `…_MD5.ext` basename.
fn is_managed_attachment_link(r: &str) -> bool {
    let Some(target) = extract_ref_link_target(r) else {
        return false;
    };
    let decoded = percent_decode(strip_angle(target));
    let path = decoded.split('#').next().unwrap_or("");
    is_md5_name(basename(path))
}

/// Crucible `rewriteLocalizedAttachmentRefs` — **the** ref-rewrite chokepoint. Embeds are
/// rewritten when listed; non-embed links only when they target a managed attachment.
pub fn rewrite_localized_attachment_refs(
    content: &str,
    replacements: &[Replacement],
) -> RewriteResult {
    if replacements.is_empty() {
        return RewriteResult {
            content: content.to_owned(),
            applied_from: Vec::new(),
        };
    }
    let mut by_original: HashMap<&str, &str> = HashMap::new();
    for r in replacements {
        by_original.entry(r.from.as_str()).or_insert(r.to.as_str());
    }
    let mut updated = String::with_capacity(content.len());
    let mut cursor = 0;
    let mut applied_from = Vec::new();
    for m in attachment_ref_re().find_iter(content) {
        let original = m.as_str();
        if !original.starts_with('!') && !is_managed_attachment_link(original) {
            continue;
        }
        let Some(to) = by_original.get(original) else {
            continue;
        };
        updated.push_str(&content[cursor..m.start()]);
        updated.push_str(to);
        cursor = m.end();
        applied_from.push(original.to_owned());
    }
    if cursor == 0 {
        return RewriteResult {
            content: content.to_owned(),
            applied_from: Vec::new(),
        };
    }
    updated.push_str(&content[cursor..]);
    RewriteResult {
        content: updated,
        applied_from,
    }
}

/// Crucible `repointAttachmentFolderPrefix`: swaps an attachment folder prefix inside embeds and
/// managed-attachment links (raw and `%20`-encoded); idempotent. Ported for parity with the
/// vectors; the note-lifecycle hook that called it in Crucible is not part of this WP.
pub fn repoint_attachment_folder_prefix(content: &str, old: &str, new: &str) -> String {
    if old.is_empty() || old == new {
        return content.to_owned();
    }
    let raw_old = format!("{old}/");
    let raw_new = format!("{new}/");
    let enc_old = raw_old.replace(' ', "%20");
    let enc_new = raw_new.replace(' ', "%20");
    attachment_ref_re()
        .replace_all(content, |c: &regex::Captures<'_>| {
            let r = &c[0];
            if !r.starts_with('!') && !is_managed_attachment_link(r) {
                return r.to_owned();
            }
            let mut out = r.to_owned();
            if out.contains(&raw_old) {
                out = out.replace(&raw_old, &raw_new);
            }
            if enc_old != raw_old && out.contains(&enc_old) {
                out = out.replace(&enc_old, &enc_new);
            }
            out
        })
        .into_owned()
}

/// Why a broken ref could not be repaired (Crucible `AttachmentRepairFailureReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairFailure {
    /// No candidate.
    Missing,
    /// Several non-identical candidates.
    Ambiguous,
    /// The remote re-download failed.
    RemoteDownloadFailed,
}

/// Crucible `AttachmentPathIndex` (built once per repair pass).
#[derive(Debug, Default)]
pub struct AttachmentPathIndex {
    by_path: HashSet<String>,
    by_basename: HashMap<String, Vec<String>>,
    managed_by_stem: Vec<(String, String)>,
}

fn managed_stem(name: &str) -> Option<&str> {
    is_md5_name(name).then(|| name.rsplit_once("_MD5.").map_or("", |(s, _)| s))
}

impl AttachmentPathIndex {
    /// Crucible `buildAttachmentPathIndex`.
    pub fn build<S: AsRef<str>>(paths: &[S]) -> Self {
        let mut ix = Self::default();
        for p in paths {
            let p = p.as_ref();
            ix.by_path.insert(p.to_owned());
            let base = basename(p);
            ix.by_basename
                .entry(base.to_owned())
                .or_default()
                .push(p.to_owned());
            if let Some(stem) = managed_stem(base) {
                ix.managed_by_stem.push((stem.to_owned(), p.to_owned()));
            }
        }
        ix.managed_by_stem.sort();
        ix
    }

    /// Crucible `stemPrefixMatches` (lower bound + contiguous walk).
    fn stem_prefix_matches(&self, prefix: &str) -> Vec<&str> {
        let lo = self
            .managed_by_stem
            .partition_point(|(s, _)| s.as_str() < prefix);
        self.managed_by_stem[lo..]
            .iter()
            .take_while(|(s, _)| s.starts_with(prefix))
            .map(|(_, p)| p.as_str())
            .collect()
    }
}

/// Crucible `pickAmongIdenticalContent`: expected folder, else shortest, else lexicographic.
fn pick_among_identical_content(candidates: &[&str], expected_folder: &str) -> String {
    if !expected_folder.is_empty() {
        let pre = format!("{expected_folder}/");
        if let Some(p) = candidates.iter().find(|p| p.starts_with(&pre)) {
            return (*p).to_owned();
        }
    }
    let mut sorted = candidates.to_vec();
    sorted.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
    sorted.first().map(|s| (*s).to_owned()).unwrap_or_default()
}

/// Crucible `resolveLocalAttachmentRepair` (index path; the Rust port has no separate naive
/// path): (1) expected folder, (2) unique exact basename (all-`_MD5` duplicates auto-resolve),
/// (3) unique managed-stem prefix of at least [`PREFIX_REPAIR_MIN_STEM_LENGTH`] chars.
pub fn resolve_local_attachment_repair(
    broken_link: &str,
    expected_folder: &str,
    index: &AttachmentPathIndex,
) -> Result<String, RepairFailure> {
    let cut = broken_link.split('#').next().unwrap_or("");
    let cut = cut.split('|').next().unwrap_or("");
    let decoded = percent_decode(strip_angle(cut));
    let base = basename(&decoded);
    if base.is_empty() {
        return Err(RepairFailure::Missing);
    }
    let expected = if expected_folder.is_empty() {
        base.to_owned()
    } else {
        format!("{expected_folder}/{base}")
    };
    if index.by_path.contains(&expected) {
        return Ok(expected);
    }
    let exact: Vec<&str> = index
        .by_basename
        .get(base)
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_default();
    match exact.len() {
        1 => return Ok(exact[0].to_owned()),
        n if n > 1 => {
            if is_md5_name(base) {
                return Ok(pick_among_identical_content(&exact, expected_folder));
            }
            return Err(RepairFailure::Ambiguous);
        }
        _ => {}
    }
    if let Some(stem) = managed_stem(base).or_else(|| {
        // A truncated ext (`abc_MD5.pn`) still carries the marker; `_MD5.` alone does not.
        base.rsplit_once("_MD5.").map(|(s, _)| s)
    }) && stem.len() >= PREFIX_REPAIR_MIN_STEM_LENGTH
    {
        let hits = index.stem_prefix_matches(stem);
        match hits.len() {
            1 => return Ok(hits[0].to_owned()),
            n if n > 1 => {
                let names: BTreeSet<&str> = hits.iter().map(|p| basename(p)).collect();
                if names.len() == 1 {
                    return Ok(pick_among_identical_content(&hits, expected_folder));
                }
                return Err(RepairFailure::Ambiguous);
            }
            _ => {}
        }
    }
    Err(RepairFailure::Missing)
}

/// Crucible `hasOtherAttachmentReferrer` over a `source → {target → count}` map (the index's
/// resolved links; see `has_other_referrer` for the step's index-backed form).
pub fn has_other_attachment_referrer(
    resolved_links: &HashMap<String, HashMap<String, u32>>,
    attachment_path: &str,
    exclude_note_path: &str,
) -> bool {
    resolved_links.iter().any(|(src, targets)| {
        src != exclude_note_path && targets.get(attachment_path).copied().unwrap_or(0) > 0
    })
}

/// Crucible `stripDataUriImagePlaceholders`: returns the content and the number stripped.
pub fn strip_data_uri_image_placeholders(content: &str) -> (String, usize) {
    let n = data_uri_image_re().find_iter(content).count();
    if n == 0 {
        return (content.to_owned(), 0);
    }
    (data_uri_image_re().replace_all(content, "").into_owned(), n)
}

/// Crucible `formatEmbed`: empty alt on the markdown form (a numeric alt is read as a width).
pub fn format_embed(syntax: Syntax, target: &str) -> String {
    match syntax {
        Syntax::Wiki => format!("![[{target}]]"),
        Syntax::Md => format!("![]({})", target.replace(' ', "%20")),
    }
}

/// Crucible `formatLink`: a link stays a link and keeps its display text.
pub fn format_link(syntax: Syntax, target: &str, display: Option<&str>) -> String {
    match syntax {
        Syntax::Wiki => match display {
            Some(d) => format!("[[{target}|{d}]]"),
            None => format!("[[{target}]]"),
        },
        Syntax::Md => {
            let text = display.unwrap_or_else(|| basename(target));
            format!("[{text}]({})", target.replace(' ', "%20"))
        }
    }
}

/// Crucible `formatRef`: dispatch on the original ref kind.
pub fn format_ref(m: &AttachmentMatch, target: &str) -> String {
    if m.is_embed {
        format_embed(m.syntax, target)
    } else {
        format_link(m.syntax, target, m.display_text.as_deref())
    }
}

/// Byte ranges of fenced code blocks (```` ``` ```` / `~~~`); refs inside are not refs (the
/// Obsidian metadata cache skips them, and `parseAttachmentRefsFromCache` read the cache).
fn fenced_ranges(content: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open: Option<(usize, &str)> = None;
    let mut pos = 0;
    for line in content.split_inclusive('\n') {
        let t = line.trim_start();
        let fence = if t.starts_with("```") {
            Some("```")
        } else if t.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        match (open, fence) {
            (None, Some(f)) => open = Some((pos, f)),
            (Some((start, f)), Some(g)) if f == g => {
                out.push((start, pos + line.len()));
                open = None;
            }
            _ => {}
        }
        pos += line.len();
    }
    if let Some((start, _)) = open {
        out.push((start, content.len()));
    }
    out
}

/// A remote link Obsidian renders as an embedded page (YouTube video, X/Twitter post), not an
/// attachment: never downloaded and never a backfill candidate.
pub fn is_embed_page(link: &str) -> bool {
    let Ok(u) = url::Url::parse(link.trim()) else {
        return false;
    };
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    let path = u.path();
    match host {
        "youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            path == "/watch"
                || ["/shorts/", "/embed/", "/live/"]
                    .iter()
                    .any(|p| path.starts_with(p))
        }
        "youtu.be" => path.len() > 1,
        "x.com" | "twitter.com" | "mobile.twitter.com" => path.contains("/status/"),
        _ => false,
    }
}

fn is_remote(link: &str) -> bool {
    let l = link.get(..8).unwrap_or(link).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// Splits a matched ref into (link, display text).
fn ref_parts(original: &str) -> Option<(String, Option<String>, Syntax)> {
    let body = original.strip_prefix('!').unwrap_or(original);
    if let Some(inner) = body.strip_prefix("[[").and_then(|s| s.strip_suffix("]]")) {
        let (link, display) = match inner.split_once('|') {
            Some((l, d)) => (l, Some(d.to_owned())),
            None => (inner, None),
        };
        return Some((link.trim().to_owned(), display, Syntax::Wiki));
    }
    let close = body.find("](")?;
    let text = &body[1..close];
    let dest = body.get(close + 2..body.len() - 1)?.trim();
    let dest = if let Some(rest) = dest.strip_prefix('<') {
        rest.split_once('>').map_or(rest, |(d, _)| d)
    } else {
        dest.split_whitespace().next().unwrap_or("")
    };
    let link = if is_remote(dest) {
        unescape_md_url(dest)
    } else {
        percent_decode(dest)
    };
    Some((link, Some(text.to_owned()), Syntax::Md))
}

/// Port of Crucible `parseAttachmentRefsFromCache`, over the note text instead of the metadata
/// cache: every embed (`data:` URIs excluded — the cache never lists them), every link whose
/// decoded basename is a managed `_MD5` name, then remote markdown images via
/// `REMOTE_MD_IMAGE_RE`; deduped by `original`.
pub fn parse_attachment_refs(content: &str) -> Vec<AttachmentMatch> {
    let fences = fenced_ranges(content);
    let in_fence = |i: usize| fences.iter().any(|&(s, e)| i >= s && i < e);
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for m in attachment_ref_re().find_iter(content) {
        if in_fence(m.start()) {
            continue;
        }
        let original = m.as_str();
        let Some((link, display, syntax)) = ref_parts(original) else {
            continue;
        };
        let is_embed = original.starts_with('!');
        if link.is_empty() || link.to_ascii_lowercase().starts_with("data:") {
            continue;
        }
        let remote = is_remote(&link);
        if is_embed {
            // Remote markdown images come from the REMOTE_MD_IMAGE_RE pass below.
            if remote && syntax == Syntax::Md {
                continue;
            }
        } else {
            let cut = link.split('#').next().unwrap_or("");
            let cut = cut.split('|').next().unwrap_or("");
            if !is_md5_name(basename(cut)) {
                continue;
            }
        }
        if !seen.insert(original.to_owned()) {
            continue;
        }
        let display_text = match (is_embed, syntax) {
            (true, Syntax::Md) => None,
            _ => display.filter(|d| !d.is_empty()),
        };
        out.push(AttachmentMatch {
            original: original.to_owned(),
            link,
            syntax,
            is_remote: remote,
            is_embed,
            display_text,
        });
    }
    for c in remote_md_image_re().captures_iter(content) {
        let (Some(all), Some(url)) = (c.get(0), c.get(2)) else {
            continue;
        };
        if in_fence(all.start()) || !seen.insert(all.as_str().to_owned()) {
            continue;
        }
        out.push(AttachmentMatch {
            original: all.as_str().to_owned(),
            link: unescape_md_url(url.as_str()),
            syntax: Syntax::Md,
            is_remote: true,
            is_embed: true,
            display_text: None,
        });
    }
    out
}

/// Crucible `clampImageQuality`: default 85, clamped to 30..=100, as a 0..1 fraction.
pub fn clamp_image_quality(q: Option<f64>) -> f64 {
    let v = match q {
        Some(v) if v.is_finite() => v,
        _ => 85.0,
    };
    v.clamp(30.0, 100.0) / 100.0
}

/// Crucible `classifyLocalizeMediaType` (`src/utils.ts:145`, formats from `src/types.ts:33`).
pub fn classify_media(ext: &str) -> Option<Media> {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    if e == "webm" {
        return Some(Media::Video);
    }
    [Media::Images, Media::Audio, Media::Video, Media::Pdf]
        .into_iter()
        .find(|m| m.native().contains(&e.as_str()))
}

/// A localize media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    /// Images.
    Images,
    /// Audio.
    Audio,
    /// Video.
    Video,
    /// PDF.
    Pdf,
}

impl Media {
    /// The native extensions of this media type.
    #[must_use]
    pub fn native(self) -> &'static [&'static str] {
        match self {
            Media::Images => &["avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp"],
            Media::Audio => &["flac", "m4a", "mp3", "ogg", "wav", "webm", "3gp"],
            Media::Video => &["mkv", "mov", "mp4", "ogv", "webm"],
            Media::Pdf => &["pdf"],
        }
    }
    /// The params key (`images`, `audio`, …).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Media::Images => "images",
            Media::Audio => "audio",
            Media::Video => "video",
            Media::Pdf => "pdf",
        }
    }
}

/// A Content-Type naming a document page rather than a media file.
pub fn is_page_mime(mime: &str) -> bool {
    let head = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    head.starts_with("text/") || head == "application/xhtml+xml"
}

/// Crucible `extFromMime`.
pub fn ext_from_mime(mime: &str) -> Option<&'static str> {
    let head = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match head.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "audio/mpeg" => "mp3",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        "audio/mp4" => "m4a",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/ogg" => "ogv",
        "application/pdf" => "pdf",
        _ => return None,
    })
}

/// Crucible `extFromUrl`.
pub fn ext_from_url(u: &str) -> Option<String> {
    let url = url::Url::parse(u).ok()?;
    let last = basename(url.path());
    let (_, ext) = last.rsplit_once('.')?;
    (!ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| ext.to_ascii_lowercase())
}

/// Crucible `guessRemoteOriginalName`: last path segment without its extension.
pub fn guess_remote_original_name(u: &str) -> String {
    let Ok(url) = url::Url::parse(u) else {
        return "remote".into();
    };
    let last = url
        .path()
        .split('/')
        .rfind(|s| !s.is_empty())
        .unwrap_or("remote");
    match last.rsplit_once('.') {
        Some((stem, ext)) if !ext.is_empty() && !ext.contains('/') => stem.to_owned(),
        _ => last.to_owned(),
    }
}

/// Obsidian `normalizePath` for a vault-relative path (`\`/`/` runs → `/`, no leading/trailing
/// `/`, NBSP → space); the vault root is `""`.
pub fn normalize_path(p: &str) -> String {
    p.replace('\u{a0}', " ")
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Default `folder_template` (Crucible `DEFAULT_SETTINGS`); shared with the `write_attachment` RPC.
pub const DEFAULT_FOLDER_TEMPLATE: &str = "{{folder}}/_attachments/{{slug}}";
/// Default `name_template`; shared with the `write_attachment` RPC.
pub const DEFAULT_NAME_TEMPLATE: &str = "{{md5}}_MD5.{{ext}}";

/// Splits a note path into `(folder, basename-without-.md)`.
pub fn note_folder_basename(path: &str) -> (&str, &str) {
    let (folder, file) = split(path);
    (folder, file.strip_suffix(".md").unwrap_or(file))
}

/// Resolves an attachment folder for `note_path` from `folder_template` (normalized), at `now`.
/// A template the engine rejects (a bad `{{datetime:…}}` format; params validation refuses
/// `{{datetime:` up front) renders verbatim.
#[must_use]
pub fn resolve_attachment_folder(
    folder_template: &str,
    note_path: &str,
    original: &str,
    ext: &str,
    now: NaiveDateTime,
) -> String {
    normalize_path(&render(
        folder_template,
        &ctx(note_path, original, ext, "", now),
    ))
}

/// Resolves the vault path of an attachment for `note_path` from the folder and name templates
/// (both through [`apply_attachment_template`], normalized). Shared by the `localize` step and
/// the daemon's `write_attachment` RPC.
#[must_use]
pub fn resolve_attachment_path(
    folder_template: &str,
    name_template: &str,
    note_path: &str,
    original: &str,
    ext: &str,
    md5: &str,
    now: NaiveDateTime,
) -> String {
    let c = ctx(note_path, original, ext, md5, now);
    let folder = normalize_path(&render(folder_template, &c));
    let name = render(name_template, &c);
    normalize_path(&format!("{folder}/{name}"))
}

fn ctx(
    note_path: &str,
    original: &str,
    ext: &str,
    md5: &str,
    now: NaiveDateTime,
) -> AttachmentTemplateContext {
    let (note_folder, note_basename) = note_folder_basename(note_path);
    AttachmentTemplateContext {
        now,
        note_basename: note_basename.to_owned(),
        note_folder_path: note_folder.to_owned(),
        original_name: original.to_owned(),
        ext: ext.to_owned(),
        md5: Some(md5.to_owned()),
    }
}

fn render(t: &str, c: &AttachmentTemplateContext) -> String {
    apply_attachment_template(t, c).unwrap_or_else(|_| t.to_owned())
}

/// Whether `content` has a remote markdown image or a `data:image` placeholder outside fenced
/// code (the folder-scope candidate predicate).
pub fn has_remote_image_refs(content: &str) -> bool {
    let fenced = fenced_ranges(content);
    let outside = |at: usize| !fenced.iter().any(|&(a, b)| at >= a && at < b);
    remote_md_image_re().captures_iter(content).any(|c| {
        c.get(0).is_some_and(|m| outside(m.start()))
            && c.get(2)
                .is_some_and(|u| is_remote(u.as_str()) && !is_embed_page(u.as_str()))
    }) || data_uri_image_re()
        .find_iter(content)
        .any(|m| outside(m.start()))
}

/// The number of managed (`<md5>_MD5.<image ext>`) image names in `content` (the `images`
/// outcome var; plan `v0-automation-pipelines` WP-6: the image-describe trigger seam). A regex
/// scan over note text with an image-extension allowlist and a word boundary — not the
/// name-level [`is_md5_name`] classification, which accepts any alphanumeric extension and
/// matches only a whole name.
#[must_use]
pub fn count_managed_images(content: &str) -> u64 {
    managed_image_re().find_iter(content).count() as u64
}
#[cfg(test)]
#[path = "attachments_tests.rs"]
mod vectors;
