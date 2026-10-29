//! Typed `vault.toml` (plan tidiness-trash-orphans WP-1): `[trash] location`, `retention_days`;
//! `[remote_media]` (plan remote-media-sprint WP-2): how remote images, video, audio and PDFs are
//! handled — the vault-wide defaults a `localize` step falls back to.
//!
//! New, not carried from Crucible: Obsidian owned the trash there (`.trash/` or the system trash
//! by its own setting); cv owns the vault, so the choice and the retention live here. The
//! `[remote_media]` keys mirror Crucible's localize settings (`src/types.ts:794-816`), moved from a
//! plugin settings tab into the vault config.

use cv_text::attachments::{DEFAULT_FOLDER_TEMPLATE, DEFAULT_NAME_TEMPLATE, Media};
use serde::Deserialize;

/// Where a recoverable delete moves the file (the one protocol definition, WP-R6).
pub use cv_protocol::TrashLocation;

/// `[trash]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrashConfig {
    /// Destination of recoverable deletes.
    pub location: TrashLocation,
    /// Days until a trashed file is deleted permanently; `0` = never.
    pub retention_days: u32,
}

impl Default for TrashConfig {
    fn default() -> Self {
        Self {
            location: TrashLocation::Vault,
            retention_days: 30,
        }
    }
}

/// `[remote_media] images` (the only value: remote images are localized).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesMode {
    /// Download into the vault.
    #[default]
    Localize,
}

/// `[remote_media] video`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VaultVideoMode {
    /// Keep the link and show a localized thumbnail.
    #[default]
    Thumbnail,
    /// Keep the link only.
    Link,
}

/// `[remote_media] audio` / `pdf`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaMode {
    /// Keep the remote link.
    #[default]
    Link,
    /// Download into the vault.
    Localize,
}

/// Image conversion target (Crucible `ImageConvertFormat`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// WebP (lossless only — quality is ignored).
    #[default]
    Webp,
    /// JPEG at the clamped quality.
    Jpeg,
}

fn native(m: Media) -> Vec<String> {
    m.native().iter().map(|s| (*s).to_owned()).collect()
}

/// `[remote_media]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RemoteMediaConfig {
    /// Remote images.
    pub images: ImagesMode,
    /// Remote video.
    pub video: VaultVideoMode,
    /// Remote audio.
    pub audio: MediaMode,
    /// Remote PDFs.
    pub pdf: MediaMode,
    /// Image extensions considered (default: the native set).
    pub images_whitelist: Vec<String>,
    /// Audio extensions considered.
    pub audio_whitelist: Vec<String>,
    /// Video extensions considered.
    pub video_whitelist: Vec<String>,
    /// PDF extensions considered.
    pub pdf_whitelist: Vec<String>,
    /// Re-encode downloaded images, keeping the result when smaller.
    pub convert_images: bool,
    /// Conversion target.
    pub image_format: ImageFormat,
    /// JPEG quality (clamped at use).
    pub image_quality: Option<f64>,
    /// Attachment folder template.
    pub folder_template: String,
    /// Attachment name template (must contain `{{md5}}`).
    pub name_template: String,
    /// Per-download timeout, seconds (> 0).
    pub timeout_secs: u64,
    /// Per-download size cap, bytes (> 0).
    pub max_bytes: u64,
}

impl Default for RemoteMediaConfig {
    fn default() -> Self {
        Self {
            images: ImagesMode::Localize,
            video: VaultVideoMode::Thumbnail,
            audio: MediaMode::Link,
            pdf: MediaMode::Link,
            images_whitelist: native(Media::Images),
            audio_whitelist: native(Media::Audio),
            video_whitelist: native(Media::Video),
            pdf_whitelist: native(Media::Pdf),
            convert_images: false,
            image_format: ImageFormat::Webp,
            image_quality: None,
            folder_template: DEFAULT_FOLDER_TEMPLATE.into(),
            name_template: DEFAULT_NAME_TEMPLATE.into(),
            timeout_secs: 30,
            max_bytes: 50 * 1024 * 1024,
        }
    }
}

/// The template rules shared by `[remote_media]` and the `localize` step params.
///
/// # Errors
/// A `{{datetime:…}}` template, or a name template without `{{md5}}`.
pub fn check_templates(folder_template: &str, name_template: &str) -> Result<(), String> {
    for tpl in [folder_template, name_template] {
        if tpl.contains("{{datetime:") {
            return Err("{{datetime:…}} templates are not supported".into());
        }
    }
    if !name_template.contains("{{md5}}") {
        return Err("name_template must contain {{md5}}".into());
    }
    Ok(())
}

impl RemoteMediaConfig {
    /// The whitelist of `m`.
    #[must_use]
    pub fn whitelist(&self, m: Media) -> &[String] {
        match m {
            Media::Images => &self.images_whitelist,
            Media::Audio => &self.audio_whitelist,
            Media::Video => &self.video_whitelist,
            Media::Pdf => &self.pdf_whitelist,
        }
    }

    fn check(&mut self) -> Result<(), String> {
        check_templates(&self.folder_template, &self.name_template)
            .map_err(|e| format!("remote_media: {e}"))?;
        if self.timeout_secs == 0 {
            return Err("remote_media: timeout_secs must be positive".into());
        }
        if self.max_bytes == 0 {
            return Err("remote_media: max_bytes must be positive".into());
        }
        if self.image_quality.is_some_and(|q| !q.is_finite()) {
            return Err("remote_media: image_quality must be a finite number".into());
        }
        for wl in [
            &mut self.images_whitelist,
            &mut self.audio_whitelist,
            &mut self.video_whitelist,
            &mut self.pdf_whitelist,
        ] {
            for e in wl.iter_mut() {
                *e = e.to_ascii_lowercase();
            }
        }
        Ok(())
    }
}

/// `vault.toml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VaultConfig {
    /// `[trash]`.
    pub trash: TrashConfig,
    /// `[remote_media]`.
    pub remote_media: RemoteMediaConfig,
}

impl VaultConfig {
    /// Parses and checks a `vault.toml` table (an unknown key or value is an error, never
    /// coerced).
    pub fn from_table(t: &toml::Table) -> Result<Self, String> {
        let mut c: Self = toml::Value::Table(t.clone())
            .try_into()
            .map_err(|e: toml::de::Error| e.message().to_owned())?;
        c.remote_media.check()?;
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<VaultConfig, String> {
        VaultConfig::from_table(&s.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn absent_is_defaults() {
        let c = parse("").unwrap();
        assert_eq!(c.trash.location, TrashLocation::Vault);
        assert_eq!(c.trash.retention_days, 30);
        assert_eq!(c.remote_media, RemoteMediaConfig::default());
    }

    #[test]
    fn valid_values_parse() {
        let c = parse("[trash]\nlocation = \"system\"\nretention_days = 0\n").unwrap();
        assert_eq!(c.trash.location, TrashLocation::System);
        assert_eq!(c.trash.retention_days, 0);
    }

    #[test]
    fn unknown_location_and_negative_days_rejected() {
        assert!(parse("[trash]\nlocation = \"bin\"\n").is_err());
        assert!(parse("[trash]\nretention_days = -1\n").is_err());
        assert!(parse("[trash]\nextra = 1\n").is_err());
    }

    #[test]
    fn remote_media_defaults() {
        let r = parse("[remote_media]\n").unwrap().remote_media;
        assert_eq!(r.images, ImagesMode::Localize);
        assert_eq!(r.video, VaultVideoMode::Thumbnail);
        assert_eq!(r.audio, MediaMode::Link);
        assert_eq!(r.pdf, MediaMode::Link);
        assert_eq!(r.whitelist(Media::Audio).len(), Media::Audio.native().len());
        assert_eq!(r.pdf_whitelist, vec!["pdf".to_owned()]);
        assert!(!r.convert_images);
        assert_eq!(r.image_format, ImageFormat::Webp);
        assert_eq!(r.image_quality, None);
        assert_eq!(r.folder_template, DEFAULT_FOLDER_TEMPLATE);
        assert_eq!(r.name_template, DEFAULT_NAME_TEMPLATE);
        assert_eq!(r.timeout_secs, 30);
        assert_eq!(r.max_bytes, 50 * 1024 * 1024);
    }

    #[test]
    fn remote_media_every_key_parses() {
        let r = parse(
            "[remote_media]\nimages = \"localize\"\nvideo = \"link\"\naudio = \"localize\"\n\
             pdf = \"localize\"\nimages_whitelist = [\"PNG\"]\naudio_whitelist = [\"mp3\"]\n\
             video_whitelist = [\"mp4\"]\npdf_whitelist = []\nconvert_images = true\n\
             image_format = \"jpeg\"\nimage_quality = 70\nfolder_template = \"att\"\n\
             name_template = \"{{md5}}.{{ext}}\"\ntimeout_secs = 5\nmax_bytes = 1000\n",
        )
        .unwrap()
        .remote_media;
        assert_eq!(r.video, VaultVideoMode::Link);
        assert_eq!(r.audio, MediaMode::Localize);
        assert_eq!(r.pdf, MediaMode::Localize);
        assert_eq!(r.images_whitelist, vec!["png".to_owned()]);
        assert!(r.pdf_whitelist.is_empty());
        assert!(r.convert_images);
        assert_eq!(r.image_format, ImageFormat::Jpeg);
        assert_eq!(r.image_quality, Some(70.0));
        assert_eq!(r.folder_template, "att");
        assert_eq!(r.timeout_secs, 5);
        assert_eq!(r.max_bytes, 1000);
        let f = parse("[remote_media]\nimage_quality = 72.5\n").unwrap();
        assert_eq!(f.remote_media.image_quality, Some(72.5));
    }

    #[test]
    fn remote_media_bad_values_rejected() {
        for bad in [
            "images = \"link\"",
            "video = \"localize\"",
            "video = true",
            "audio = \"thumbnail\"",
            "pdf = false",
            "image_format = \"png\"",
            "image_quality = \"high\"",
            "images_whitelist = \"png\"",
            "timeout_secs = 0",
            "max_bytes = -1",
            "name_template = \"{{name}}\"",
            "folder_template = \"{{datetime:YYYY}}\"",
            "convert_images = 1",
        ] {
            assert!(
                parse(&format!("[remote_media]\n{bad}\n")).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn remote_media_unknown_key_rejected() {
        let e = parse("[remote_media]\nthumbnails = true\n").unwrap_err();
        assert!(e.contains("thumbnails"), "{e}");
        assert!(parse("[remote]\nimages = \"localize\"\n").is_err());
    }
}
