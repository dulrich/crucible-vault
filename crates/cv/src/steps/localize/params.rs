//! The `localize` step's params (defaults = Crucible `DEFAULT_SETTINGS`).
//!
//! Precedence (plan remote-media-sprint WP-2): a step param, else `vault.toml`
//! `[remote_media]`, else the built-in default (which `RemoteMediaConfig::default` holds).

use std::collections::HashMap;
use std::time::Duration;

use cv_actions::{MediaMode, RemoteMediaConfig, StepKind, VaultVideoMode, check_templates};
use cv_text::attachments::{Media, classify_media};

/// Target image format (Crucible `ImageConvertFormat`); the one definition is `cv_actions`'.
pub use cv_actions::ImageFormat;

/// Remote video handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoMode {
    /// Keep the link, show a localized thumbnail (WP-3 acts on it).
    Thumbnail,
    /// Keep the link only.
    Link,
    /// Download the video file — reachable only from the legacy step bool `video = true`.
    Localize,
}

impl From<VaultVideoMode> for VideoMode {
    fn from(m: VaultVideoMode) -> Self {
        match m {
            VaultVideoMode::Thumbnail => Self::Thumbnail,
            VaultVideoMode::Link => Self::Link,
        }
    }
}

/// Remote audio handling.
pub(crate) type AudioMode = MediaMode;
/// Remote PDF handling.
pub(crate) type PdfMode = MediaMode;

/// Parsed step params (defaults = Crucible `DEFAULT_SETTINGS`, `src/types.ts:794-816`).
#[derive(Debug, Clone)]
pub(crate) struct Params {
    pub(super) path: Option<String>,
    /// `Some(folder)` = folder scope (plan `usability-sprint-captures-links` WP-2 backfill).
    pub(super) folder_scope: Option<String>,
    pub(super) limit: u64,
    pub(super) repair: bool,
    pub(super) folder_template: String,
    pub(super) name_template: String,
    /// Media downloaded today: Images (unless a legacy `images = false`), Audio iff
    /// `audio == Localize`, Pdf iff `pdf == Localize`, Video iff `video == VideoMode::Localize`.
    pub(super) enabled: Vec<Media>,
    #[allow(dead_code)] // read by WP-3
    pub(super) video: VideoMode,
    #[allow(dead_code)] // read by WP-3
    pub(super) audio: AudioMode,
    #[allow(dead_code)] // read by WP-3
    pub(super) pdf: PdfMode,
    pub(super) whitelists: HashMap<&'static str, Vec<String>>,
    pub(super) convert_images: bool,
    pub(super) image_format: ImageFormat,
    pub(super) image_quality: Option<f64>,
    pub(super) timeout: Duration,
    pub(super) max_bytes: u64,
}

/// A link/localize mode param: the mode string, or a legacy bool (`true` = localize).
fn media_mode(t: &toml::Table, k: &str, d: MediaMode) -> Result<MediaMode, String> {
    match t.get(k) {
        None => Ok(d),
        Some(toml::Value::Boolean(true)) => Ok(MediaMode::Localize),
        Some(toml::Value::Boolean(false)) => Ok(MediaMode::Link),
        Some(toml::Value::String(s)) if s == "link" => Ok(MediaMode::Link),
        Some(toml::Value::String(s)) if s == "localize" => Ok(MediaMode::Localize),
        Some(_) => Err(format!("localize: {k} must be \"link\" or \"localize\"")),
    }
}

pub(super) fn parse_params(t: &toml::Table, rm: &RemoteMediaConfig) -> Result<Params, String> {
    if let Some(k) = super::super::params::unknown_key(StepKind::Localize, t) {
        return Err(format!("localize: unknown param {k:?}"));
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
    let folder_template = s("folder_template")?.unwrap_or_else(|| rm.folder_template.clone());
    let name_template = s("name_template")?.unwrap_or_else(|| rm.name_template.clone());
    check_templates(&folder_template, &name_template).map_err(|e| format!("localize: {e}"))?;
    // `images`: "localize" (the only mode) or a legacy bool (`false` = skip images).
    let images = match t.get("images") {
        None => true,
        Some(toml::Value::Boolean(v)) => *v,
        Some(toml::Value::String(s)) if s == "localize" => true,
        Some(_) => return Err("localize: images must be \"localize\"".into()),
    };
    let audio = media_mode(t, "audio", rm.audio)?;
    let pdf = media_mode(t, "pdf", rm.pdf)?;
    let video = match t.get("video") {
        None | Some(toml::Value::Boolean(false)) => VideoMode::from(rm.video),
        Some(toml::Value::Boolean(true)) => VideoMode::Localize,
        Some(toml::Value::String(s)) if s == "thumbnail" => VideoMode::Thumbnail,
        Some(toml::Value::String(s)) if s == "link" => VideoMode::Link,
        Some(_) => return Err("localize: video must be \"thumbnail\" or \"link\"".into()),
    };
    let mut enabled = Vec::new();
    let mut whitelists = HashMap::new();
    for m in [Media::Images, Media::Audio, Media::Video, Media::Pdf] {
        let on = match m {
            Media::Images => images,
            Media::Audio => audio == MediaMode::Localize,
            Media::Video => video == VideoMode::Localize,
            Media::Pdf => pdf == MediaMode::Localize,
        };
        if on {
            enabled.push(m);
        }
        let wk = format!("{}_whitelist", m.key());
        let wl = match t.get(&wk) {
            None => rm.whitelist(m).to_vec(),
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
        None => rm.image_format,
        Some("webp") => ImageFormat::Webp,
        Some("jpeg") => ImageFormat::Jpeg,
        Some(o) => return Err(format!("localize: image_format {o:?} (want webp|jpeg)")),
    };
    let image_quality = match t.get("image_quality") {
        None => rm.image_quality,
        Some(toml::Value::Integer(v)) => Some(*v as f64),
        Some(toml::Value::Float(v)) => Some(*v),
        Some(_) => return Err("localize: image_quality must be a number".into()),
    };
    let folder = s("folder")?;
    let folder_scope = match s("scope")?.as_deref() {
        None | Some("note") => {
            if folder.is_some() {
                return Err("localize: folder requires scope = \"folder\"".into());
            }
            None
        }
        Some("folder") => Some(
            folder
                .map(|f| f.trim_matches('/').to_owned())
                .ok_or("localize: scope = \"folder\" requires folder")?,
        ),
        Some(o) => return Err(format!("localize: scope {o:?} (want note|folder)")),
    };
    Ok(Params {
        path: s("path")?,
        folder_scope,
        limit: u("limit", 50)?,
        repair,
        folder_template,
        name_template,
        enabled,
        video,
        audio,
        pdf,
        whitelists,
        convert_images: b("convert_images", rm.convert_images)?,
        image_format,
        image_quality,
        timeout: Duration::from_secs(u("timeout_secs", rm.timeout_secs)?),
        max_bytes: u("max_bytes", rm.max_bytes)?,
    })
}

impl Params {
    /// Crucible `isEligibleAttached`.
    pub(super) fn eligible(&self, ext: &str) -> bool {
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

impl Params {
    /// Params localizing `path` under the effective `[remote_media]` (inline localize, e.g. the
    /// blog tracker's and mail's captures).
    pub(crate) fn note(path: String, rm: &RemoteMediaConfig) -> Self {
        let mut p = parse_params(&toml::Table::new(), rm).unwrap_or_else(|_| unreachable!());
        p.path = Some(path);
        p
    }
}
