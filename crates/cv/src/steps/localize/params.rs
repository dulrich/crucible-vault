//! The `localize` step's params (defaults = Crucible `DEFAULT_SETTINGS`).

use std::collections::HashMap;
use std::time::Duration;

use cv_actions::StepKind;
use cv_text::attachments::{DEFAULT_FOLDER_TEMPLATE, DEFAULT_NAME_TEMPLATE, Media, classify_media};

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
pub(crate) struct Params {
    pub(super) path: Option<String>,
    /// `Some(folder)` = folder scope (plan `usability-sprint-captures-links` WP-2 backfill).
    pub(super) folder_scope: Option<String>,
    pub(super) limit: u64,
    /// Folder scope only: select notes by their source-relative image refs instead of remote
    /// ones (plan `round-15-relative-images-pdf-viewer` WP-1 backfill).
    pub(super) relative_sources: bool,
    pub(super) repair: bool,
    pub(super) folder_template: String,
    pub(super) name_template: String,
    pub(super) enabled: Vec<Media>,
    pub(super) whitelists: HashMap<&'static str, Vec<String>>,
    pub(super) convert_images: bool,
    pub(super) image_format: ImageFormat,
    pub(super) image_quality: Option<f64>,
    pub(super) timeout: Duration,
    pub(super) max_bytes: u64,
}

pub(super) fn parse_params(t: &toml::Table) -> Result<Params, String> {
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
    let relative_sources = b("relative_sources", false)?;
    if relative_sources && folder_scope.is_none() {
        return Err("localize: relative_sources requires scope = \"folder\"".into());
    }
    Ok(Params {
        path: s("path")?,
        folder_scope,
        limit: u("limit", 50)?,
        relative_sources,
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
    /// Default params localizing `path` (inline localize, e.g. the blog tracker's captures).
    pub(crate) fn note(path: String) -> Self {
        let mut p = parse_params(&toml::Table::new()).unwrap_or_else(|_| unreachable!());
        p.path = Some(path);
        p
    }
}
