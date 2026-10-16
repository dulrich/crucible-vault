//! Step module `localize` (plan `v0-core-daemon` WP-11): localize a note's attachments and repair
//! its broken attachment refs.
//!
//! Ported from Crucible `src/localizeAttachments.ts` (`AttachmentLocalizer.localizeNote`,
//! `repairNote`, `processRemote`, `processLocal`, `writeAttachment`, `downloadRemote`,
//! `convertImage`) and `src/attachmentRepair.ts` (the pure ref/repair domain, ported function by
//! function below with the same names in snake case). Behaviours kept, with their Crucible
//! source:
//! - attachments are named by a deterministic MD5 of the *final, post-conversion* bytes through
//!   the `{{md5}}_MD5.{{ext}}` name template (`src/AGENTS.md:23,49`); an existing file with that
//!   name is reused, never rewritten (Crucible rewrote it with identical bytes via
//!   `modifyBinary` — a no-op write, dropped);
//! - the web clipper's `data:image` lazy placeholders are stripped (`src/AGENTS.md:24,51`);
//! - rewritten markdown embeds carry an empty alt (`src/AGENTS.md:25,53`);
//! - one `_MD5`-gated ref-rewrite chokepoint ([`rewrite_localized_attachment_refs`]); repair
//!   counts only replacements that landed, and only when the note write itself landed (CAS
//!   success) (`src/AGENTS.md:26`).
//!
//! Dropped (Obsidian-induced): the re-read-before-write and the spanning `withMaterializing`
//! flag (`src/AGENTS.md:22,47`). Crucible needed both because `vault.modify` has no
//! compare-and-swap and its own debounced auto-lint/auto-localize handlers had to be suppressed
//! for the whole download window. Here the note is read once with its hash, every download and
//! conversion happens, and the single note write is a CAS against that hash (I2): a concurrent
//! edit yields a conflict and the step fails, never a clobber; the runner's cycle guard (I5)
//! replaces the materializing flag. Also dropped: the metadata cache as the ref source
//! (`parseAttachmentRefsFromCache`) — refs are parsed from the note text ([`parse_attachment_refs`],
//! fenced code skipped as the cache does) and resolved through the index's file list with
//! `cv_index::Resolver` (`getFirstLinkpathDest`'s replacement); notices/spinners/debug-log.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

use cv_actions::{ConfigSet, Step};
use cv_vault::VaultError;
use regex::Regex;
use serde_json::{Map, Value};

use super::{StepCtx, StepError, StepOutcome};

// ---------------------------------------------------------------------------------------------
// Pure domain (port of src/attachmentRepair.ts)
// ---------------------------------------------------------------------------------------------

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
        r"!\[\[[^\]\n]+\]\]|!\[[^\]\n]*\]\([^)\n]+\)|\[\[[^\]\n]+\]\]|\[[^\[\]\n]*\]\([^)\n]+\)",
    )
}

/// Crucible `REMOTE_MD_IMAGE_RE`.
fn remote_md_image_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"!\[([^\]]*)\]\((https?://[^)\s]+)\)")
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

fn percent_decode(s: &str) -> String {
    match percent_encoding::percent_decode_str(s).decode_utf8() {
        Ok(d) => d.into_owned(),
        Err(_) => s.to_owned(),
    }
}

fn strip_angle(s: &str) -> &str {
    let s = s.strip_prefix('<').unwrap_or(s);
    s.strip_suffix('>').unwrap_or(s)
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
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
        dest.to_owned()
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
            link: url.as_str().to_owned(),
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

/// Crucible `md5HexForBytes` (deterministic content MD5, `src/AGENTS.md:23,49`).
pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::Digest;
    hex::encode(md5::Md5::digest(bytes))
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
    fn native(self) -> &'static [&'static str] {
        match self {
            Media::Images => &["avif", "bmp", "gif", "jpeg", "jpg", "png", "svg", "webp"],
            Media::Audio => &["flac", "m4a", "mp3", "ogg", "wav", "webm", "3gp"],
            Media::Video => &["mkv", "mov", "mp4", "ogv", "webm"],
            Media::Pdf => &["pdf"],
        }
    }
    fn key(self) -> &'static str {
        match self {
            Media::Images => "images",
            Media::Audio => "audio",
            Media::Video => "video",
            Media::Pdf => "pdf",
        }
    }
}

/// Crucible `extFromMime`.
fn ext_from_mime(mime: &str) -> Option<&'static str> {
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
fn ext_from_url(u: &str) -> Option<String> {
    let url = url::Url::parse(u).ok()?;
    let last = url.path().rsplit('/').next()?;
    let (_, ext) = last.rsplit_once('.')?;
    (!ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then(|| ext.to_ascii_lowercase())
}

/// Crucible `guessRemoteOriginalName`: last path segment without its extension.
fn guess_remote_original_name(u: &str) -> String {
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

/// Crucible `slugify` (`src/utils.ts:129`). Deviation: no NFKD fold (no Unicode-normalization
/// crate pinned); non-ASCII letters become `-` like any other non-`[a-z0-9]` run.
pub fn slugify(v: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in v.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_owned()
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
pub(crate) const DEFAULT_FOLDER_TEMPLATE: &str = "{{folder}}/_attachments/{{slug}}";
/// Default `name_template`; shared with the `write_attachment` RPC.
pub(crate) const DEFAULT_NAME_TEMPLATE: &str = "{{md5}}_MD5.{{ext}}";

/// Splits a note path into `(folder, basename-without-.md)`.
pub(crate) fn note_folder_basename(path: &str) -> (&str, &str) {
    let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
    (folder, file.strip_suffix(".md").unwrap_or(file))
}

/// Resolves the vault path of an attachment for `note_path` from the templates
/// (folder template + name template, normalized). Shared by [`Run::write_attachment`] and the
/// daemon's `write_attachment` RPC.
pub(crate) fn resolve_attachment_path(
    folder_template: &str,
    name_template: &str,
    note_path: &str,
    original: &str,
    ext: &str,
    md5: &str,
) -> String {
    let (note_folder, note_basename) = note_folder_basename(note_path);
    let tc = TemplateCtx {
        note_basename,
        note_folder,
        original,
        ext,
        md5,
    };
    let folder = normalize_path(&apply_template(folder_template, &tc));
    let name = apply_template(name_template, &tc);
    normalize_path(&format!("{folder}/{name}"))
}

/// Crucible `AttachmentTemplateContext`.
struct TemplateCtx<'a> {
    note_basename: &'a str,
    note_folder: &'a str,
    original: &'a str,
    ext: &'a str,
    md5: &'a str,
}

/// Crucible `applyAttachmentTemplate` (tokens `folder slug name title ext original md5 date time
/// today now`). `{{datetime:…}}` (moment formats) is rejected at validation.
fn apply_template(t: &str, c: &TemplateCtx<'_>) -> String {
    let now = chrono::Local::now();
    let tokens = [
        ("folder", c.note_folder.to_owned()),
        ("slug", slugify(c.note_basename)),
        ("name", c.note_basename.to_owned()),
        ("title", c.note_basename.to_owned()),
        ("ext", c.ext.to_owned()),
        ("original", c.original.to_owned()),
        ("md5", c.md5.to_owned()),
        ("date", now.format("%Y-%m-%d").to_string()),
        ("time", now.format("%H:%M").to_string()),
        ("today", now.format("%Y-%m-%d").to_string()),
        ("now", now.format("%Y-%m-%dT%H:%M:%S").to_string()),
    ];
    let mut out = t.to_owned();
    for (k, v) in tokens {
        out = out.replace(&format!("{{{{{k}}}}}"), &v);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------------------------

/// Target image format (Crucible `ImageConvertFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// WebP (the `image` crate encodes lossless WebP only — quality is ignored).
    Webp,
    /// JPEG at the clamped quality.
    Jpeg,
}

/// Parsed step params (defaults = Crucible `DEFAULT_SETTINGS`, `src/types.ts:794-816`).
#[derive(Debug, Clone)]
struct Params {
    path: Option<String>,
    repair: bool,
    folder_template: String,
    name_template: String,
    enabled: Vec<Media>,
    whitelists: HashMap<&'static str, Vec<String>>,
    convert_images: bool,
    image_format: ImageFormat,
    image_quality: Option<f64>,
    timeout: Duration,
    max_bytes: u64,
}

const KEYS: &[&str] = &[
    "path",
    "mode",
    "folder_template",
    "name_template",
    "images",
    "audio",
    "video",
    "pdf",
    "images_whitelist",
    "audio_whitelist",
    "video_whitelist",
    "pdf_whitelist",
    "convert_images",
    "image_format",
    "image_quality",
    "timeout_secs",
    "max_bytes",
];

fn parse_params(t: Option<&toml::Table>) -> Result<Params, String> {
    let empty = toml::Table::new();
    let t = t.unwrap_or(&empty);
    for k in t.keys() {
        if !KEYS.contains(&k.as_str()) {
            return Err(format!("localize: unknown param {k:?}"));
        }
    }
    let s = |k: &str| -> Result<Option<String>, String> {
        match t.get(k) {
            None => Ok(None),
            Some(toml::Value::String(v)) => Ok(Some(v.clone())),
            Some(_) => Err(format!("localize: {k} must be a string")),
        }
    };
    let b = |k: &str, d: bool| -> Result<bool, String> {
        match t.get(k) {
            None => Ok(d),
            Some(toml::Value::Boolean(v)) => Ok(*v),
            Some(_) => Err(format!("localize: {k} must be a bool")),
        }
    };
    let u = |k: &str, d: u64| -> Result<u64, String> {
        match t.get(k) {
            None => Ok(d),
            Some(toml::Value::Integer(v)) if *v > 0 => Ok(u64::try_from(*v).unwrap_or(d)),
            Some(_) => Err(format!("localize: {k} must be a positive integer")),
        }
    };
    let repair = match s("mode")?.as_deref() {
        None | Some("localize") => false,
        Some("repair") => true,
        Some(o) => return Err(format!("localize: mode {o:?} (want localize|repair)")),
    };
    let folder_template = s("folder_template")?.unwrap_or_else(|| DEFAULT_FOLDER_TEMPLATE.into());
    let name_template = s("name_template")?.unwrap_or_else(|| DEFAULT_NAME_TEMPLATE.into());
    for tpl in [&folder_template, &name_template] {
        if tpl.contains("{{datetime:") {
            return Err("localize: {{datetime:…}} templates are not supported".into());
        }
    }
    if !name_template.contains("{{md5}}") {
        return Err("localize: name_template must contain {{md5}}".into());
    }
    let mut enabled = Vec::new();
    let mut whitelists = HashMap::new();
    for (m, d) in [
        (Media::Images, true),
        (Media::Audio, false),
        (Media::Video, false),
        (Media::Pdf, false),
    ] {
        if b(m.key(), d)? {
            enabled.push(m);
        }
        let wk = format!("{}_whitelist", m.key());
        let wl = match t.get(&wk) {
            None => m.native().iter().map(|s| (*s).to_owned()).collect(),
            Some(toml::Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_ascii_lowercase)
                        .ok_or_else(|| format!("localize: {wk} must be strings"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err(format!("localize: {wk} must be an array")),
        };
        whitelists.insert(m.key(), wl);
    }
    let image_format = match s("image_format")?.as_deref() {
        None | Some("webp") => ImageFormat::Webp,
        Some("jpeg") => ImageFormat::Jpeg,
        Some(o) => return Err(format!("localize: image_format {o:?} (want webp|jpeg)")),
    };
    let image_quality = match t.get("image_quality") {
        None => None,
        Some(toml::Value::Integer(v)) => Some(*v as f64),
        Some(toml::Value::Float(v)) => Some(*v),
        Some(_) => return Err("localize: image_quality must be a number".into()),
    };
    Ok(Params {
        path: s("path")?,
        repair,
        folder_template,
        name_template,
        enabled,
        whitelists,
        convert_images: b("convert_images", false)?,
        image_format,
        image_quality,
        timeout: Duration::from_secs(u("timeout_secs", 30)?),
        max_bytes: u("max_bytes", 50 * 1024 * 1024)?,
    })
}

impl Params {
    /// Crucible `isEligibleAttached`.
    fn eligible(&self, ext: &str) -> bool {
        let Some(m) = classify_media(ext) else {
            return false;
        };
        self.enabled.contains(&m)
            && self
                .whitelists
                .get(m.key())
                .is_some_and(|w| w.iter().any(|e| e.eq_ignore_ascii_case(ext)))
    }
}

/// Validates the step's params (all optional; an empty table is valid).
pub fn validate(step: &Step, _config: &ConfigSet) -> Result<(), String> {
    parse_params(step.table()).map(|_| ())
}

// ---------------------------------------------------------------------------------------------
// I/O: download + convert
// ---------------------------------------------------------------------------------------------

/// Why one download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DownloadError {
    /// Connect/timeout: the network (or host) is unreachable.
    Unreachable,
    /// Anything else (HTTP status, body error, too large).
    Failed(String),
}

fn download(
    client: &reqwest::Client,
    handle: &tokio::runtime::Handle,
    url: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, String), DownloadError> {
    let classify = |e: reqwest::Error| {
        if e.is_connect() || e.is_timeout() {
            DownloadError::Unreachable
        } else {
            DownloadError::Failed(e.to_string())
        }
    };
    handle.block_on(async {
        let resp = client.get(url).send().await.map_err(classify)?;
        if !resp.status().is_success() {
            return Err(DownloadError::Failed(format!("http {}", resp.status())));
        }
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(DownloadError::Failed("too large".into()));
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let bytes = resp.bytes().await.map_err(classify)?;
        if bytes.len() as u64 > max_bytes {
            return Err(DownloadError::Failed("too large".into()));
        }
        let ext = ext_from_mime(&ctype)
            .map(str::to_owned)
            .or_else(|| ext_from_url(url))
            .unwrap_or_else(|| "bin".into());
        Ok((bytes.to_vec(), ext))
    })
}

/// Crucible `convertImage`: re-encode, and keep the result only when it is smaller; any decode
/// or encode failure keeps the source bytes. Deviation: WebP output is lossless (the pinned
/// `image` crate has no lossy WebP encoder), so `image_quality` applies to JPEG only.
pub fn convert_image(
    bytes: Vec<u8>,
    src_ext: &str,
    target: ImageFormat,
    quality: Option<f64>,
) -> (Vec<u8>, String) {
    let Ok(img) = image::load_from_memory(&bytes) else {
        return (bytes, src_ext.to_owned());
    };
    let mut out = Vec::new();
    let (res, ext) = match target {
        ImageFormat::Webp => {
            let rgba = image::DynamicImage::ImageRgba8(img.to_rgba8());
            let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
            (rgba.write_with_encoder(enc), "webp")
        }
        ImageFormat::Jpeg => {
            let q = (clamp_image_quality(quality) * 100.0).round() as u8;
            let rgb = image::DynamicImage::ImageRgb8(img.to_rgb8());
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
            (rgb.write_with_encoder(enc), "jpg")
        }
    };
    if res.is_err() || out.is_empty() || out.len() >= bytes.len() {
        return (bytes, src_ext.to_owned());
    }
    (out, ext.to_owned())
}

// ---------------------------------------------------------------------------------------------
// The step
// ---------------------------------------------------------------------------------------------

/// The note being processed.
struct Note<'a> {
    path: &'a str,
    basename: &'a str,
    folder: &'a str,
}

impl<'a> Note<'a> {
    fn new(path: &'a str) -> Self {
        let (folder, basename) = note_folder_basename(path);
        Self {
            path,
            basename,
            folder,
        }
    }
}

/// Vault paths + a resolver, from the index (`getFirstLinkpathDest`'s replacement).
struct VaultView {
    paths: Vec<String>,
    resolver: cv_index::Resolver,
    note_id: i64,
    /// For each attachment path: the set of notes linking to it (Crucible `resolvedLinks`).
    reader: cv_index::Reader,
}

impl VaultView {
    fn open(index_db: &std::path::Path, note: &str) -> Result<Self, cv_index::Error> {
        let reader = match cv_index::Reader::open_current(index_db) {
            Err(cv_index::Error::GenerationMissing { .. }) => {
                cv_index::Reader::open_current(index_db)?
            }
            r => r?,
        };
        let paths = cv_index::attachment_vault_paths(&reader.query())?;
        let mut resolver = cv_index::Resolver::default();
        let mut note_id = -1;
        for (i, p) in paths.iter().enumerate() {
            let id = i as i64;
            resolver.add(id, p);
            if p == note {
                note_id = id;
            }
        }
        Ok(Self {
            paths,
            resolver,
            note_id,
            reader,
        })
    }

    fn resolve(&self, note: &str, link: &str) -> Option<&str> {
        let target = link.split('#').next().unwrap_or("");
        let id = self.resolver.resolve(note, self.note_id, target)?;
        if id == self.note_id {
            return None;
        }
        usize::try_from(id)
            .ok()
            .and_then(|i| self.paths.get(i))
            .map(String::as_str)
    }

    /// Crucible `hasOtherAttachmentReferrer`, from the index's backlinks.
    fn has_other_referrer(&self, attachment: &str, note: &str) -> bool {
        match self.reader.query().backlinks(attachment) {
            Ok(rows) => rows.iter().any(|r| r.src_path != note),
            // Unknown → keep the file (copy semantics are the safe side).
            Err(_) => true,
        }
    }
}

/// Run counters (reported in `vars`).
#[derive(Debug, Default)]
struct Counts {
    downloaded: u64,
    reused: u64,
    stripped: u64,
    failed: u64,
    unreachable: u64,
    remote_attempts: u64,
    created: Vec<String>,
}

/// Per-run state shared by the remote/local processors.
struct Run<'p> {
    p: &'p Params,
    client: Option<reqwest::Client>,
    handle: Option<tokio::runtime::Handle>,
    counts: Counts,
}

impl Run<'_> {
    /// Crucible `writeAttachment`: md5 of the final bytes → templated folder/name; an existing
    /// file of that name is reused (content-MD5 naming makes it byte-identical).
    fn write_attachment(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &Note<'_>,
        bytes: Vec<u8>,
        ext: &str,
        original: &str,
    ) -> Result<String, StepError> {
        let md5 = md5_hex(&bytes);
        let target = resolve_attachment_path(
            &self.p.folder_template,
            &self.p.name_template,
            note.path,
            original,
            ext,
            &md5,
        );
        if ctx.read(&target)?.is_some() {
            self.counts.reused += 1;
            return Ok(target);
        }
        let t = target.clone();
        match ctx.with_writer(&[target.as_str()], move |vw| vw.create(&t, &bytes))? {
            Ok(_) => {
                self.counts.created.push(target.clone());
                Ok(target)
            }
            Err(VaultError::Exists(_)) => {
                self.counts.reused += 1;
                Ok(target)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn maybe_convert(&self, bytes: Vec<u8>, ext: &str) -> (Vec<u8>, String) {
        if self.p.convert_images && classify_media(ext) == Some(Media::Images) {
            convert_image(bytes, ext, self.p.image_format, self.p.image_quality)
        } else {
            (bytes, ext.to_owned())
        }
    }

    /// Crucible `processRemote`. `Ok(None)` = left as-is (counted when it was a failure).
    fn process_remote(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &Note<'_>,
        m: &AttachmentMatch,
    ) -> Result<Option<String>, StepError> {
        let (Some(client), Some(handle)) = (self.client.as_ref(), self.handle.as_ref()) else {
            return Err(StepError::failed("no async runtime"));
        };
        self.counts.remote_attempts += 1;
        let (bytes, ext) = match download(client, handle, &m.link, self.p.max_bytes) {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(error = ?e, "localize: download failed (left as-is)");
                self.counts.failed += 1;
                if e == DownloadError::Unreachable {
                    self.counts.unreachable += 1;
                }
                return Ok(None);
            }
        };
        if !self.p.eligible(&ext) {
            // Crucible: "ext not eligible (left as-is)" — not a failure.
            return Ok(None);
        }
        let (bytes, ext) = self.maybe_convert(bytes, &ext);
        let original = guess_remote_original_name(&m.link);
        let before = self.counts.reused;
        match self.write_attachment(ctx, note, bytes, &ext, &original) {
            Ok(target) => {
                if self.counts.reused == before {
                    self.counts.downloaded += 1;
                }
                Ok(Some(format_ref(m, &target)))
            }
            Err(e) => {
                tracing::debug!(error = %e, "localize: attachment write failed (left as-is)");
                self.counts.failed += 1;
                Ok(None)
            }
        }
    }

    /// Crucible `processLocal`: re-home an eligible local attachment under its `_MD5` name in the
    /// note's attachment folder. Returns the new ref and the old path to retire. Deviation: the
    /// old file is retired only after the note write landed (Crucible trashed it first).
    fn process_local(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &Note<'_>,
        m: &AttachmentMatch,
        view: Option<&VaultView>,
    ) -> Result<Option<(String, String)>, StepError> {
        let Some(resolved) = view.and_then(|v| v.resolve(note.path, &m.link)) else {
            return Ok(None); // UNRESOLVED (left as-is)
        };
        let resolved = resolved.to_owned();
        let name = basename(&resolved);
        let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
        let ext = ext.to_ascii_lowercase();
        if !self.p.eligible(&ext) {
            return Ok(None);
        }
        let expected = normalize_path(&apply_template(
            &self.p.folder_template,
            &TemplateCtx {
                note_basename: note.basename,
                note_folder: note.folder,
                original: stem,
                ext: &ext,
                md5: "",
            },
        ));
        let parent = resolved.rsplit_once('/').map_or("", |(f, _)| f);
        if is_md5_name(name) && parent == expected {
            return Ok(None); // already localized
        }
        let Some((bytes, _)) = ctx.read(&resolved)? else {
            return Ok(None);
        };
        let (bytes, out_ext) = self.maybe_convert(bytes, &ext);
        let stem = stem.to_owned();
        let target = self.write_attachment(ctx, note, bytes, &out_ext, &stem)?;
        Ok(Some((format_ref(m, &target), resolved)))
    }
}

fn ctx_note_path(p: &Params, ctx: &StepCtx<'_>) -> Result<String, StepError> {
    let from_params = ctx
        .params
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_owned);
    p.path
        .clone()
        .or(from_params)
        .or_else(|| ctx.context.active_path.clone())
        .ok_or_else(|| StepError::failed("localize: no target note (path or active_path)"))
}

/// Crucible `repairNote`'s count: `repaired = resolved − (planned − applied)`, and zero unless
/// the note write landed (planned-not-landed replacements fold into `unrepairable`).
fn repair_counts(
    resolved: u64,
    unrepairable: u64,
    planned: u64,
    applied: u64,
    landed: bool,
) -> (u64, u64) {
    if !landed {
        return (0, unrepairable + resolved);
    }
    let missed = planned.saturating_sub(applied);
    (resolved.saturating_sub(missed), unrepairable + missed)
}

/// Executes one `localize` step (mode `localize` or `repair`) on the target note.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let p = parse_params(step.table()).map_err(StepError::failed)?;
    let path = ctx_note_path(&p, ctx)?;
    let mut vars = Map::new();
    if !path.ends_with(".md") {
        vars.insert("changed".into(), Value::Bool(false));
        return Ok(StepOutcome {
            vars,
            ..StepOutcome::default()
        });
    }
    let note = Note::new(&path);
    let Some((bytes, hash)) = ctx.read(&path)? else {
        return Err(StepError::failed(format!("localize: {path} not found")));
    };
    let content = String::from_utf8(bytes)
        .map_err(|_| StepError::failed(format!("localize: {path} is not UTF-8")))?;
    let matches = parse_attachment_refs(&content);
    let placeholders = if !p.repair && p.enabled.contains(&Media::Images) {
        strip_data_uri_image_placeholders(&content).1
    } else {
        0
    };
    let mut run = Run {
        p: &p,
        client: None,
        handle: tokio::runtime::Handle::try_current().ok(),
        counts: Counts::default(),
    };
    if matches.iter().any(|m| m.is_remote) {
        if run.handle.is_none() {
            return Err(StepError::failed("no async runtime"));
        }
        run.client = Some(
            reqwest::Client::builder()
                .timeout(p.timeout)
                .connect_timeout(p.timeout)
                .build()
                .map_err(|e| StepError::failed(format!("http client: {e}")))?,
        );
    }
    let needs_view = p.repair || matches.iter().any(|m| !m.is_remote);
    let view = if needs_view {
        match VaultView::open(ctx.index_db, &path) {
            Ok(v) => Some(v),
            Err(e) if p.repair => return Err(StepError::failed(format!("localize: index: {e}"))),
            Err(e) => {
                tracing::debug!(error = %e, "localize: no index; local refs left as-is");
                None
            }
        }
    } else {
        None
    };

    let mut replacements: Vec<Replacement> = Vec::new();
    // (ref text replaced, old local path) for re-homed local attachments.
    let mut retire: Vec<(String, String)> = Vec::new();
    let (mut resolved_count, mut unrepairable) = (0_u64, 0_u64);
    if p.repair {
        let expected = normalize_path(&apply_template(
            &p.folder_template,
            &TemplateCtx {
                note_basename: note.basename,
                note_folder: note.folder,
                original: "",
                ext: "",
                md5: "",
            },
        ));
        let pix = view
            .as_ref()
            .map(|v| AttachmentPathIndex::build(&v.paths))
            .unwrap_or_default();
        for m in &matches {
            if !m.is_remote
                && view
                    .as_ref()
                    .and_then(|v| v.resolve(&path, &m.link))
                    .is_some()
            {
                continue;
            }
            let new_ref = if m.is_remote {
                run.process_remote(ctx, &note, m)?
            } else {
                resolve_local_attachment_repair(&m.link, &expected, &pix)
                    .ok()
                    .map(|t| format_ref(m, &t))
            };
            match new_ref {
                Some(r) => {
                    resolved_count += 1;
                    if r != m.original {
                        replacements.push(Replacement {
                            from: m.original.clone(),
                            to: r,
                        });
                    }
                }
                None => unrepairable += 1,
            }
        }
    } else {
        for m in &matches {
            let new_ref = if m.is_remote {
                run.process_remote(ctx, &note, m)?
            } else {
                match run.process_local(ctx, &note, m, view.as_ref())? {
                    Some((r, old)) => {
                        retire.push((m.original.clone(), old));
                        Some(r)
                    }
                    None => None,
                }
            };
            if let Some(r) = new_ref
                && r != m.original
            {
                replacements.push(Replacement {
                    from: m.original.clone(),
                    to: r,
                });
            }
        }
    }

    let c = &run.counts;
    if c.remote_attempts > 0 && c.unreachable == c.remote_attempts {
        return Err(StepError::Deferred {
            retry_after_ms: 60_000,
            service: Some("network".into()),
            cause: super::DeferCause::Transient(cv_state::ServiceFailureKind::Timeout),
        });
    }

    let rewritten = rewrite_localized_attachment_refs(&content, &replacements);
    let mut updated = rewritten.content;
    if placeholders > 0 {
        updated = strip_data_uri_image_placeholders(&updated).0;
    }
    let changed = updated != content;
    let mut output_paths = run.counts.created.clone();
    if changed {
        // The one note write, CAS against the hash read at the start (I2).
        ctx.write(&path, updated.into_bytes(), Some(hash))?;
        output_paths.push(path.clone());
        // Retire re-homed local sources no other note references (Crucible processLocal's
        // trash, after the write so a conflict never strands the note's refs).
        let applied: HashSet<&str> = rewritten.applied_from.iter().map(String::as_str).collect();
        for (from, old) in retire {
            let keep = !applied.contains(from.as_str())
                || view
                    .as_ref()
                    .is_none_or(|v| v.has_other_referrer(&old, &path));
            if keep {
                continue;
            }
            if let Some((_, h)) = ctx.read(&old)? {
                let o = old.clone();
                let r: Result<(), VaultError> =
                    ctx.with_writer(&[old.as_str()], move |vw| vw.delete(&o, h))?;
                if let Err(e) = r {
                    tracing::debug!(error = %e, "localize: could not retire old attachment");
                }
            }
        }
    }
    run.counts.stripped = if changed { placeholders as u64 } else { 0 };
    let c = &run.counts;
    let applied = rewritten.applied_from.len() as u64;
    vars.insert("changed".into(), Value::Bool(changed));
    vars.insert("downloaded".into(), c.downloaded.into());
    vars.insert("reused".into(), c.reused.into());
    vars.insert("stripped".into(), c.stripped.into());
    vars.insert("failed".into(), c.failed.into());
    vars.insert(
        "localized".into(),
        (if changed { applied } else { 0 }).into(),
    );
    if p.repair {
        let (repaired, unrep) = repair_counts(
            resolved_count,
            unrepairable,
            replacements.len() as u64,
            applied,
            changed || replacements.is_empty(),
        );
        vars.insert("repaired".into(), repaired.into());
        vars.insert("unrepairable".into(), unrep.into());
    }
    Ok(StepOutcome {
        output_paths,
        vars,
        ..StepOutcome::default()
    })
}

/// Ported vectors of Crucible `tests/localizeAttachments.edge.test.mjs` (names kept).
#[cfg(test)]
mod vectors {
    use super::*;

    fn r(from: &str, to: &str) -> Replacement {
        Replacement {
            from: from.into(),
            to: to.into(),
        }
    }
    fn resolve(link: &str, exp: &str, paths: &[&str]) -> Result<String, RepairFailure> {
        resolve_local_attachment_repair(link, exp, &AttachmentPathIndex::build(paths))
    }
    fn plan(link: &str, exp: &str, paths: &[&str]) -> Option<String> {
        resolve(link, exp, paths).ok()
    }
    fn m(syntax: Syntax, is_embed: bool, display: Option<&str>) -> AttachmentMatch {
        AttachmentMatch {
            original: String::new(),
            link: String::new(),
            syntax,
            is_remote: false,
            is_embed,
            display_text: display.map(str::to_owned),
        }
    }

    #[test]
    fn rewrite_only_replaces_full_markdown_image_ranges() {
        let content = "Numeric alt stays intact: ![2024-01-15](photos/2024-01-15.png)\nNumeric filename localizes: ![1](photos/1.png)\nPlain numeric filename text stays intact: photos/1.png";
        let res = rewrite_localized_attachment_refs(
            content,
            &[
                r(
                    "![1](photos/1.png)",
                    "![](Notes/_attachments/post/abc123_MD5.png)",
                ),
                r("photos/1.png", "BROKEN"),
            ],
        );
        assert_eq!(
            res.content,
            "Numeric alt stays intact: ![2024-01-15](photos/2024-01-15.png)\nNumeric filename localizes: ![](Notes/_attachments/post/abc123_MD5.png)\nPlain numeric filename text stays intact: photos/1.png"
        );
        assert_eq!(res.applied_from, ["![1](photos/1.png)"]);
    }

    #[test]
    fn rewrite_keeps_numeric_heavy_sibling_filenames_separate() {
        let content = "![shot 1](assets/2024-01-15-1.png)\n![shot 10](assets/2024-01-15-10.png)";
        let res = rewrite_localized_attachment_refs(
            content,
            &[r(
                "![shot 1](assets/2024-01-15-1.png)",
                "![](assets/hash1_MD5.png)",
            )],
        );
        assert_eq!(
            res.content,
            "![](assets/hash1_MD5.png)\n![shot 10](assets/2024-01-15-10.png)"
        );
    }

    #[test]
    fn strip_removes_only_image_data_placeholders() {
        let (c, n) = strip_data_uri_image_placeholders(
            "A ![](data:image/gif;base64,R0lGODlhAQABAAAAACw=) ![](real.png)",
        );
        assert_eq!(n, 1);
        assert_eq!(c, "A ![](real.png)");
    }

    #[test]
    fn clamp_image_quality_handles_nan_undefined_and_bounds() {
        assert!((clamp_image_quality(Some(f64::NAN)) - 0.85).abs() < 1e-9);
        assert!((clamp_image_quality(None) - 0.85).abs() < 1e-9);
        assert!((clamp_image_quality(Some(10.0)) - 0.3).abs() < 1e-9);
        assert!((clamp_image_quality(Some(120.0)) - 1.0).abs() < 1e-9);
        assert!((clamp_image_quality(Some(72.0)) - 0.72).abs() < 1e-9);
    }

    #[test]
    fn md5_hex_handles_empty_and_numeric_bytes() {
        assert_eq!(md5_hex(&[]), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(
            md5_hex(&[1, 2, 3, 4, 5]),
            "7cfdd07889b3295d6a550914ab35e068"
        );
    }

    #[test]
    fn repoint_rewrites_moved_embed_prefixes_md_wiki_encoded_raw() {
        let content = "![](_resources/Clippings/example-person/3ff_MD5.webp)\n![[_resources/Clippings/example-person/abc_MD5.png]]\nProse mentioning _resources/Clippings/example-person/ stays untouched.";
        assert_eq!(
            repoint_attachment_folder_prefix(
                content,
                "_resources/Clippings/example-person",
                "_resources/daily/day/2026-06-13/example-person"
            ),
            "![](_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp)\n![[_resources/daily/day/2026-06-13/example-person/abc_MD5.png]]\nProse mentioning _resources/Clippings/example-person/ stays untouched."
        );
    }

    #[test]
    fn repoint_handles_pct20_prefixes_and_is_idempotent() {
        let (o, n) = ("_resources/My Clips/post", "_resources/daily/post");
        let md = "![](_resources/My%20Clips/post/x_MD5.webp)";
        let once = repoint_attachment_folder_prefix(md, o, n);
        assert_eq!(once, "![](_resources/daily/post/x_MD5.webp)");
        assert_eq!(repoint_attachment_folder_prefix(&once, o, n), once);
        assert_eq!(repoint_attachment_folder_prefix(md, o, o), md);
    }

    #[test]
    fn plan_prefers_expected_folder_then_unique_name() {
        let exp = "_resources/daily/day/2026-06-13/example-person";
        let paths = [
            "_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp",
            "_resources/other/elsewhere/3ff_MD5.webp",
        ];
        assert_eq!(
            plan("_resources/Clippings/example-person/3ff_MD5.webp", exp, &paths).as_deref(),
            Some("_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp")
        );
        assert_eq!(
            plan(
                "_resources/Clippings/x/uniq_MD5.png",
                exp,
                &["_resources/wherever/uniq_MD5.png"]
            )
            .as_deref(),
            Some("_resources/wherever/uniq_MD5.png")
        );
        assert_eq!(
            plan(
                "_resources/Clippings/x/dup.png",
                exp,
                &["a/dup.png", "b/dup.png"]
            ),
            None
        );
        assert_eq!(
            plan("_resources/Clippings/x/gone_MD5.png", exp, &paths),
            None
        );
        assert_eq!(
            plan(
                "_resources/Clippings/x/a%20b_MD5.png",
                exp,
                &["_resources/keep/a b_MD5.png"]
            )
            .as_deref(),
            Some("_resources/keep/a b_MD5.png")
        );
    }

    fn links(pairs: &[(&str, &[(&str, u32)])]) -> HashMap<String, HashMap<String, u32>> {
        pairs
            .iter()
            .map(|(s, t)| {
                (
                    (*s).to_owned(),
                    t.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn has_other_referrer_detects_a_second_note() {
        let l = links(&[
            (
                "daily/day/2026-05-23/Sync.md",
                &[("_resources/_blog_metadata/k/300b_MD5.png", 1)],
            ),
            (
                "_blog_metadata/k/2022-06-05-sync.md",
                &[("_resources/_blog_metadata/k/300b_MD5.png", 1)],
            ),
        ]);
        assert!(has_other_attachment_referrer(
            &l,
            "_resources/_blog_metadata/k/300b_MD5.png",
            "daily/day/2026-05-23/Sync.md"
        ));
    }

    #[test]
    fn has_other_referrer_false_when_only_excluded_note() {
        let l = links(&[
            (
                "daily/day/2026-05-23/Sync.md",
                &[("_resources/old/300b_MD5.png", 2)],
            ),
            ("unrelated/note.md", &[("_resources/other/aaa_MD5.png", 1)]),
        ]);
        assert!(!has_other_attachment_referrer(
            &l,
            "_resources/old/300b_MD5.png",
            "daily/day/2026-05-23/Sync.md"
        ));
    }

    #[test]
    fn has_other_referrer_ignores_zero_counts_and_empty_maps() {
        let l = links(&[("a.md", &[("_resources/x_MD5.png", 0)]), ("b.md", &[])]);
        assert!(!has_other_attachment_referrer(
            &l,
            "_resources/x_MD5.png",
            "c.md"
        ));
        assert!(!has_other_attachment_referrer(
            &HashMap::new(),
            "_resources/x_MD5.png",
            "c.md"
        ));
    }

    #[test]
    fn prefix_repair_min_stem_length_is_8() {
        assert_eq!(PREFIX_REPAIR_MIN_STEM_LENGTH, 8);
    }

    const EXP: &str = "_resources/notes/post";
    const TRUNC: &str = "_resources/Clippings/x/abcdef12_MD5.pn";

    #[test]
    fn unique_prefix_hit_recovers_truncated_basename() {
        let paths = ["_resources/elsewhere/abcdef1234567890_MD5.png"];
        assert_eq!(resolve(TRUNC, EXP, &paths).as_deref(), Ok(paths[0]));
        assert_eq!(plan(TRUNC, EXP, &paths).as_deref(), Some(paths[0]));
    }

    #[test]
    fn prefix_ambiguity_reports_ambiguous() {
        let paths = [
            "_resources/elsewhere/abcdef1234567890_MD5.png",
            "_resources/other/abcdef12ffffffffff_MD5.png",
        ];
        assert_eq!(resolve(TRUNC, EXP, &paths), Err(RepairFailure::Ambiguous));
    }

    #[test]
    fn below_min_stem_no_prefix_recovery() {
        let paths = ["_resources/elsewhere/abcdef1234567890_MD5.png"];
        assert_eq!(
            resolve("_resources/Clippings/x/abc_MD5.pn", EXP, &paths),
            Err(RepairFailure::Missing)
        );
    }

    #[test]
    fn exact_basename_wins_over_prefix() {
        let paths = [
            "_resources/exact/abcdef12_MD5.png",
            "_resources/elsewhere/abcdef1234567890_MD5.png",
        ];
        assert_eq!(
            resolve("_resources/Clippings/x/abcdef12_MD5.png", EXP, &paths).as_deref(),
            Ok(paths[0])
        );
    }

    #[test]
    fn prefix_ignores_non_managed_candidate() {
        let paths = ["_resources/elsewhere/abcdef1234567890.png"];
        assert_eq!(resolve(TRUNC, EXP, &paths), Err(RepairFailure::Missing));
    }

    #[test]
    fn tier2_md5_ambiguity_prefers_expected_folder() {
        let paths = [
            "_resources/notes/post/sub/dup_MD5.png",
            "_resources/other/dup_MD5.png",
            "_resources/third/dup_MD5.png",
        ];
        assert_eq!(
            resolve("_resources/Clippings/x/dup_MD5.png", EXP, &paths).as_deref(),
            Ok(paths[0])
        );
    }

    #[test]
    fn tier2_md5_ambiguity_shortest_path() {
        let paths = [
            "_resources/a/much/deeper/nested/folder/dup_MD5.png",
            "_resources/b/dup_MD5.png",
        ];
        assert_eq!(
            resolve("_resources/Clippings/x/dup_MD5.png", EXP, &paths).as_deref(),
            Ok("_resources/b/dup_MD5.png")
        );
    }

    #[test]
    fn tier2_md5_ambiguity_equal_length_lexicographic() {
        let paths = ["_resources/bbb/dup_MD5.png", "_resources/aaa/dup_MD5.png"];
        assert_eq!(
            resolve("_resources/Clippings/x/dup_MD5.png", EXP, &paths).as_deref(),
            Ok("_resources/aaa/dup_MD5.png")
        );
    }

    #[test]
    fn tier2_non_md5_duplicates_bail_ambiguous() {
        assert_eq!(
            resolve(
                "_resources/Clippings/x/dup.png",
                EXP,
                &["a/dup.png", "b/dup.png"]
            ),
            Err(RepairFailure::Ambiguous)
        );
    }

    #[test]
    fn tier3_identical_full_basenames_auto_resolve() {
        let paths = [
            "_resources/notes/post/abcdef1234567890_MD5.png",
            "_resources/elsewhere/abcdef1234567890_MD5.png",
        ];
        assert_eq!(resolve(TRUNC, EXP, &paths).as_deref(), Ok(paths[0]));
    }

    #[test]
    fn tier3_differing_basenames_bail_ambiguous() {
        let paths = [
            "_resources/elsewhere/abcdef1234567890_MD5.png",
            "_resources/other/abcdef12ffffffffff_MD5.png",
        ];
        assert_eq!(resolve(TRUNC, EXP, &paths), Err(RepairFailure::Ambiguous));
    }

    #[test]
    fn plain_missing_ref_is_missing() {
        assert_eq!(
            resolve("_resources/x/nothing_MD5.png", EXP, &["a/b.png"]),
            Err(RepairFailure::Missing)
        );
    }

    #[test]
    fn format_embed_empty_alt() {
        assert_eq!(
            format_embed(Syntax::Wiki, "folder/x_MD5.png"),
            "![[folder/x_MD5.png]]"
        );
        assert_eq!(
            format_embed(Syntax::Md, "folder/x_MD5.png"),
            "![](folder/x_MD5.png)"
        );
        assert_eq!(
            format_embed(Syntax::Md, "a folder/x_MD5.png"),
            "![](a%20folder/x_MD5.png)"
        );
    }

    #[test]
    fn format_link_preserves_display_text() {
        assert_eq!(
            format_link(Syntax::Wiki, "folder/x_MD5.pdf", None),
            "[[folder/x_MD5.pdf]]"
        );
        assert_eq!(
            format_link(Syntax::Wiki, "folder/x_MD5.pdf", Some("the source PDF")),
            "[[folder/x_MD5.pdf|the source PDF]]"
        );
        assert_eq!(
            format_link(Syntax::Md, "folder/x_MD5.pdf", Some("the source PDF")),
            "[the source PDF](folder/x_MD5.pdf)"
        );
        assert_eq!(
            format_link(Syntax::Md, "a folder/x_MD5.pdf", Some("doc")),
            "[doc](a%20folder/x_MD5.pdf)"
        );
    }

    #[test]
    fn format_ref_dispatches_on_is_embed() {
        assert_eq!(
            format_ref(&m(Syntax::Wiki, true, None), "x_MD5.png"),
            "![[x_MD5.png]]"
        );
        assert_eq!(
            format_ref(&m(Syntax::Md, false, Some("the report")), "x_MD5.pdf"),
            "[the report](x_MD5.pdf)"
        );
    }

    #[test]
    fn parse_collects_embeds_tagged_is_embed() {
        let ms = parse_attachment_refs("![[a_MD5.png]]");
        assert_eq!(ms.len(), 1);
        assert!(ms[0].is_embed);
        assert_eq!(ms[0].syntax, Syntax::Wiki);
    }

    #[test]
    fn parse_collects_links_only_when_managed() {
        let ms =
            parse_attachment_refs("[[attachments/a_MD5.pdf|the report]] [[notes/other-note.md]]");
        assert_eq!(ms.len(), 1);
        assert!(!ms[0].is_embed);
        assert_eq!(ms[0].syntax, Syntax::Wiki);
        assert_eq!(ms[0].display_text.as_deref(), Some("the report"));
        assert_eq!(ms[0].link, "attachments/a_MD5.pdf");
    }

    #[test]
    fn parse_markdown_managed_link_keeps_display_and_syntax() {
        let ms = parse_attachment_refs("[the report](attachments/a_MD5.pdf)");
        assert_eq!(ms.len(), 1);
        assert!(!ms[0].is_embed);
        assert_eq!(ms[0].syntax, Syntax::Md);
        assert_eq!(ms[0].display_text.as_deref(), Some("the report"));
    }

    #[test]
    fn parse_dedupes_identical_original() {
        assert_eq!(
            parse_attachment_refs("![[a_MD5.png]] and ![[a_MD5.png]]").len(),
            1
        );
    }

    #[test]
    fn parse_finds_remote_markdown_images() {
        let ms = parse_attachment_refs("![alt](https://example.com/pic.png)");
        assert_eq!(ms.len(), 1);
        assert!(ms[0].is_remote && ms[0].is_embed);
    }

    #[test]
    fn parse_skips_fenced_code_and_data_uris() {
        let ms = parse_attachment_refs("```\n![[a_MD5.png]]\n```\n![](data:image/gif;base64,AA)");
        assert!(ms.is_empty());
    }

    #[test]
    fn rewrite_wiki_link_to_managed_attachment_lands() {
        let res = rewrite_localized_attachment_refs(
            "See [[attachments/old/a_MD5.pdf|the report]] for details.",
            &[r(
                "[[attachments/old/a_MD5.pdf|the report]]",
                "[[attachments/new/a_MD5.pdf|the report]]",
            )],
        );
        assert_eq!(
            res.content,
            "See [[attachments/new/a_MD5.pdf|the report]] for details."
        );
        assert_eq!(
            res.applied_from,
            ["[[attachments/old/a_MD5.pdf|the report]]"]
        );
    }

    #[test]
    fn rewrite_pct20_markdown_link_lands() {
        let res = rewrite_localized_attachment_refs(
            "The [source PDF](attachments/old%20folder/a_MD5.pdf) is here.",
            &[r(
                "[source PDF](attachments/old%20folder/a_MD5.pdf)",
                "[source PDF](attachments/new%20folder/a_MD5.pdf)",
            )],
        );
        assert_eq!(
            res.content,
            "The [source PDF](attachments/new%20folder/a_MD5.pdf) is here."
        );
        assert_eq!(
            res.applied_from,
            ["[source PDF](attachments/old%20folder/a_MD5.pdf)"]
        );
    }

    #[test]
    fn rewrite_mixed_note_moves_only_managed_refs() {
        let content = "![[attachments/old/img_MD5.png]]\n[[attachments/old/doc_MD5.pdf|the doc]]\nSee also [[Some Other Note]] and [more](Other%20Note.md).";
        let res = rewrite_localized_attachment_refs(
            content,
            &[
                r(
                    "![[attachments/old/img_MD5.png]]",
                    "![[attachments/new/img_MD5.png]]",
                ),
                r(
                    "[[attachments/old/doc_MD5.pdf|the doc]]",
                    "[[attachments/new/doc_MD5.pdf|the doc]]",
                ),
            ],
        );
        assert_eq!(
            res.content,
            "![[attachments/new/img_MD5.png]]\n[[attachments/new/doc_MD5.pdf|the doc]]\nSee also [[Some Other Note]] and [more](Other%20Note.md)."
        );
        assert_eq!(res.applied_from.len(), 2);
    }

    #[test]
    fn rewrite_never_touches_ordinary_note_links() {
        let content = "Check [[Some Other Note]] and [the text](Other%20Note.md) for more.";
        let res = rewrite_localized_attachment_refs(
            content,
            &[
                r("[[Some Other Note]]", "[[Renamed Note]]"),
                r("[the text](Other%20Note.md)", "[the text](Renamed.md)"),
            ],
        );
        assert_eq!(res.content, content);
        assert!(res.applied_from.is_empty());
    }

    #[test]
    fn rewrite_is_idempotent() {
        let content = "See [[attachments/new/a_MD5.pdf|the report]] for details.";
        let res = rewrite_localized_attachment_refs(
            content,
            &[r(
                "[[attachments/old/a_MD5.pdf|the report]]",
                "[[attachments/new/a_MD5.pdf|the report]]",
            )],
        );
        assert_eq!(res.content, content);
        assert!(res.applied_from.is_empty());
    }

    #[test]
    fn repoint_wiki_link_alongside_embeds() {
        let content = "![](_resources/Clippings/example-person/3ff_MD5.webp)\n[[_resources/Clippings/example-person/doc_MD5.pdf|Open: the source PDF]]";
        assert_eq!(
            repoint_attachment_folder_prefix(
                content,
                "_resources/Clippings/example-person",
                "_resources/daily/day/2026-06-13/example-person"
            ),
            "![](_resources/daily/day/2026-06-13/example-person/3ff_MD5.webp)\n[[_resources/daily/day/2026-06-13/example-person/doc_MD5.pdf|Open: the source PDF]]"
        );
    }

    #[test]
    fn repoint_pct20_markdown_link_idempotent() {
        let (o, n) = ("_resources/My Clips/post", "_resources/daily/post");
        let md = "[the source](attachments/_resources/My%20Clips/post/x_MD5.pdf)";
        let once = repoint_attachment_folder_prefix(md, o, n);
        assert_eq!(
            once,
            "[the source](attachments/_resources/daily/post/x_MD5.pdf)"
        );
        assert_eq!(repoint_attachment_folder_prefix(&once, o, n), once);
    }

    #[test]
    fn repoint_never_rewrites_ordinary_note_links() {
        let content = "[[_resources/Clippings/example-person/Some Note]]\n[a note](_resources/Clippings/example-person/Some%20Note.md)";
        assert_eq!(
            repoint_attachment_folder_prefix(
                content,
                "_resources/Clippings/example-person",
                "_resources/daily/day/2026-06-13/example-person"
            ),
            content
        );
    }

    #[test]
    fn fragment_suffixed_managed_ref_resolves() {
        assert_eq!(
            resolve(
                "_resources/old/uniq_MD5.png#page=2",
                "attachments/x",
                &["_resources/keep/uniq_MD5.png"]
            )
            .as_deref(),
            Ok("_resources/keep/uniq_MD5.png")
        );
    }

    #[test]
    fn alias_suffixed_managed_ref_resolves() {
        assert_eq!(
            resolve(
                "_resources/old/uniq_MD5.png|Open: pasted image",
                "attachments/x",
                &["_resources/keep/uniq_MD5.png"]
            )
            .as_deref(),
            Ok("_resources/keep/uniq_MD5.png")
        );
    }

    #[test]
    fn fragment_suffixed_tier2_md5_duplicates_auto_resolve() {
        assert_eq!(
            resolve(
                "_resources/old/dup_MD5.png#frag",
                "attachments/x",
                &["b/dup_MD5.png", "a/dup_MD5.png"]
            )
            .as_deref(),
            Ok("a/dup_MD5.png")
        );
    }

    #[test]
    fn rewrite_replaces_image_nested_in_markdown_link() {
        let img = "![](https://example.com/uploads/logo-1200x628-1.jpg)";
        let content = format!("Before\n[{img}](https://link.example.com/click/abc)\nAfter");
        let res = rewrite_localized_attachment_refs(
            &content,
            &[r(img, "![](_resources/n/dd8e_MD5.webp)")],
        );
        assert_eq!(res.applied_from, [img]);
        assert_eq!(
            res.content,
            "Before\n[![](_resources/n/dd8e_MD5.webp)](https://link.example.com/click/abc)\nAfter"
        );
    }

    #[test]
    fn repoint_nested_embed_leaves_outer_href() {
        assert_eq!(
            repoint_attachment_folder_prefix(
                "[![](_resources/Clippings/note/dd8e_MD5.webp)](https://link.example.com/click/abc)",
                "_resources/Clippings/note",
                "_resources/daily/day/2026-10-13/note"
            ),
            "[![](_resources/daily/day/2026-10-13/note/dd8e_MD5.webp)](https://link.example.com/click/abc)"
        );
    }

    #[test]
    fn repair_counts_only_landed_writes() {
        // Landed, all applied.
        assert_eq!(repair_counts(2, 1, 2, 2, true), (2, 1));
        // Landed, one planned replacement did not apply → folded into unrepairable.
        assert_eq!(repair_counts(2, 0, 2, 1, true), (1, 1));
        // CAS failed / nothing written → nothing repaired.
        assert_eq!(repair_counts(2, 0, 2, 2, false), (0, 2));
    }

    #[test]
    fn templates_and_slug() {
        assert_eq!(slugify("My Post: Ünïcode  2024!"), "my-post-n-code-2024");
        assert_eq!(
            normalize_path("/_attachments//my-note/"),
            "_attachments/my-note"
        );
        let c = TemplateCtx {
            note_basename: "My Note",
            note_folder: "",
            original: "pic",
            ext: "png",
            md5: "abc",
        };
        assert_eq!(
            normalize_path(&apply_template("{{folder}}/_attachments/{{slug}}", &c)),
            "_attachments/my-note"
        );
        assert_eq!(apply_template("{{md5}}_MD5.{{ext}}", &c), "abc_MD5.png");
        assert!(is_md5_name("abc_MD5.png") && !is_md5_name("abc_MD5.") && !is_md5_name("abc.png"));
    }

    #[test]
    fn validate_accepts_empty_and_rejects_bad_params() {
        assert!(parse_params(None).is_ok());
        let d = parse_params(None).unwrap();
        assert_eq!(d.folder_template, DEFAULT_FOLDER_TEMPLATE);
        assert_eq!(d.name_template, DEFAULT_NAME_TEMPLATE);
        assert_eq!(
            resolve_attachment_path(
                DEFAULT_FOLDER_TEMPLATE,
                DEFAULT_NAME_TEMPLATE,
                "notes/My Note.md",
                "",
                "png",
                "abc"
            ),
            format!("notes/_attachments/{}/abc_MD5.png", slugify("My Note"))
        );
        let bad: toml::Table = toml::from_str("mode = \"nope\"").unwrap();
        assert!(parse_params(Some(&bad)).is_err());
        let unknown: toml::Table = toml::from_str("bogus = 1").unwrap();
        assert!(parse_params(Some(&unknown)).is_err());
    }
}
