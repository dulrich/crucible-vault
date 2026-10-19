//! The `localize` step's I/O half: attachment download + conversion, the index view (vault
//! paths + resolver) and the per-run remote/local processors (Crucible `processRemote`,
//! `processLocal`, `writeAttachment`, `downloadRemote`, `convertImage`).

use std::time::Duration;

use cv_providers::FailureKind;
use cv_providers::web::{FetchError, FetchRequest, Limited, WebClient};
use cv_text::attachments::{
    AttachmentMatch, Media, clamp_image_quality, classify_media, ext_from_mime, ext_from_url,
    format_ref, guess_remote_original_name, is_embed_page, is_md5_name, is_page_mime,
    resolve_attachment_folder, resolve_attachment_path,
};
use cv_text::vpath;
use cv_vault::VaultError;

use super::super::util::md5_hex;
use super::super::{StepCtx, StepError};
use super::params::{ImageFormat, Params};

/// Local wall-clock time for the attachment templates' date tokens.
fn local_now() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}

/// The breaker key of localize's attachment downloads (shared host gate, B8).
pub(crate) const SERVICE: &str = "localize";

/// Why one download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DownloadError {
    /// Connect/timeout: the network (or host) is unreachable.
    Unreachable,
    /// HTTP 429 / 5xx: the host asks us to come back later (not a property of the image).
    Throttled(FetchError),
    /// The response's type (Content-Type, else the URL extension) is not an eligible
    /// attachment; the body was not read.
    NotEligible,
    /// Anything else (HTTP status, body error, too large).
    Failed(String),
}

/// One attachment download through the shared [`WebClient::fetch_limited`] (host gate +
/// breaker key [`SERVICE`]), keeping localize's decide-from-headers rule: a page
/// (`text/*`, XHTML) is never an attachment, whatever the URL's extension says; otherwise the
/// extension comes from the Content-Type, else the URL, and must be `eligible` — all before the
/// body is read.
fn download(
    client: &WebClient,
    handle: &tokio::runtime::Handle,
    url: &str,
    max_bytes: u64,
    timeout: Duration,
    eligible: impl Fn(&str) -> bool,
) -> Result<(Vec<u8>, String), DownloadError> {
    let mut req = FetchRequest::get(SERVICE, url);
    req.timeout = Some(timeout);
    let decide = |ctype: &str| {
        if is_page_mime(ctype) {
            return None;
        }
        let ext = ext_from_mime(ctype)
            .map(str::to_owned)
            .or_else(|| ext_from_url(url))
            .unwrap_or_else(|| "bin".into());
        eligible(&ext).then_some(ext)
    };
    match handle.block_on(client.fetch_limited(req, max_bytes, decide)) {
        Ok(Limited::Body(bytes, ext)) => Ok((bytes, ext)),
        Ok(Limited::Rejected) => Err(DownloadError::NotEligible),
        Err(e) => Err(match e.failure().kind {
            FailureKind::Connect | FailureKind::Timeout => DownloadError::Unreachable,
            FailureKind::RateLimited | FailureKind::Server => DownloadError::Throttled(e),
            _ => DownloadError::Failed(e.to_string()),
        }),
    }
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

/// Vault paths + a resolver, from the index (`getFirstLinkpathDest`'s replacement).
pub(super) struct VaultView {
    pub(super) paths: Vec<String>,
    pub(super) resolver: cv_index::Resolver,
    pub(super) note_id: i64,
    /// For each attachment path: the set of notes linking to it (Crucible `resolvedLinks`).
    pub(super) reader: cv_index::Reader,
}

impl VaultView {
    pub(super) fn open(index_db: &std::path::Path, note: &str) -> Result<Self, cv_index::Error> {
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

    pub(super) fn resolve(&self, note: &str, link: &str) -> Option<&str> {
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
    pub(super) fn has_other_referrer(&self, attachment: &str, note: &str) -> bool {
        match self.reader.query().backlinks(attachment) {
            Ok(rows) => rows.iter().any(|r| r.src_path != note),
            // Unknown → keep the file (copy semantics are the safe side).
            Err(_) => true,
        }
    }
}

/// Run counters (reported in `vars`).
#[derive(Debug, Default)]
pub(super) struct Counts {
    pub(super) downloaded: u64,
    pub(super) reused: u64,
    pub(super) stripped: u64,
    pub(super) failed: u64,
    pub(super) unreachable: u64,
    /// Downloads refused with 429/5xx (also counted in `failed`).
    pub(super) throttled: u64,
    /// The first throttled download's failure (a note-scope run defers on it).
    pub(super) first_throttle: Option<FetchError>,
    /// Remote refs left as-is because they are not eligible attachments (embed pages, or a
    /// response type outside the whitelists) — not failures.
    pub(super) ineligible: u64,
    pub(super) remote_attempts: u64,
    pub(super) created: Vec<String>,
}

/// Per-run state shared by the remote/local processors.
pub(super) struct Run<'p> {
    pub(super) p: &'p Params,
    pub(super) client: Option<&'static WebClient>,
    pub(super) handle: Option<tokio::runtime::Handle>,
    pub(super) counts: Counts,
}

impl Run<'_> {
    /// Crucible `writeAttachment`: md5 of the final bytes → templated folder/name; an existing
    /// file of that name is reused (content-MD5 naming makes it byte-identical).
    pub(super) fn write_attachment(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        bytes: Vec<u8>,
        ext: &str,
        original: &str,
    ) -> Result<String, StepError> {
        let md5 = md5_hex(&bytes);
        let target = resolve_attachment_path(
            &self.p.folder_template,
            &self.p.name_template,
            note,
            original,
            ext,
            &md5,
            local_now(),
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

    pub(super) fn maybe_convert(&self, bytes: Vec<u8>, ext: &str) -> (Vec<u8>, String) {
        if self.p.convert_images && classify_media(ext) == Some(Media::Images) {
            convert_image(bytes, ext, self.p.image_format, self.p.image_quality)
        } else {
            (bytes, ext.to_owned())
        }
    }

    /// Crucible `processRemote`. `Ok(None)` = left as-is (counted when it was a failure).
    pub(super) fn process_remote(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        m: &AttachmentMatch,
    ) -> Result<Option<String>, StepError> {
        let (Some(client), Some(handle)) = (self.client, self.handle.as_ref()) else {
            return Err(StepError::failed("no async runtime"));
        };
        if is_embed_page(&m.link) {
            self.counts.ineligible += 1;
            return Ok(None);
        }
        self.counts.remote_attempts += 1;
        let p = self.p;
        let (bytes, ext) = match download(client, handle, &m.link, p.max_bytes, p.timeout, |e| {
            p.eligible(e)
        }) {
            Ok(d) => d,
            Err(DownloadError::NotEligible) => {
                // Crucible: "ext not eligible (left as-is)" — not a failure.
                self.counts.ineligible += 1;
                return Ok(None);
            }
            Err(e) => {
                tracing::debug!(error = ?e, "localize: download failed (left as-is)");
                self.counts.failed += 1;
                match e {
                    DownloadError::Unreachable => self.counts.unreachable += 1,
                    DownloadError::Throttled(f) => {
                        self.counts.throttled += 1;
                        self.counts.first_throttle.get_or_insert(f);
                    }
                    DownloadError::Failed(_) | DownloadError::NotEligible => {}
                }
                return Ok(None);
            }
        };
        if !self.p.eligible(&ext) {
            // Crucible: "ext not eligible (left as-is)" — not a failure.
            self.counts.ineligible += 1;
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
    pub(super) fn process_local(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        m: &AttachmentMatch,
        view: Option<&VaultView>,
    ) -> Result<Option<(String, String)>, StepError> {
        let Some(resolved) = view.and_then(|v| v.resolve(note, &m.link)) else {
            return Ok(None); // UNRESOLVED (left as-is)
        };
        let resolved = resolved.to_owned();
        let name = vpath::basename(&resolved);
        let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
        let ext = ext.to_ascii_lowercase();
        if !self.p.eligible(&ext) {
            return Ok(None);
        }
        let expected =
            resolve_attachment_folder(&self.p.folder_template, note, stem, &ext, local_now());
        let parent = vpath::parent(&resolved);
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
