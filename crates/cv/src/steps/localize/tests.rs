//! Step-side unit tests of `localize` (the pure vectors live in `cv_text::attachments`).

use cv_actions::{ImageFormat, MediaMode, RemoteMediaConfig, VaultVideoMode};
use cv_text::attachments::{
    DEFAULT_FOLDER_TEMPLATE, DEFAULT_NAME_TEMPLATE, Media, resolve_attachment_path,
};

use super::params::{Params, VideoMode, parse_params};
use super::repair_counts;

fn p(step: &str, rm: &RemoteMediaConfig) -> Result<Params, String> {
    parse_params(&toml::from_str(step).unwrap(), rm)
}

fn vault_rm() -> RemoteMediaConfig {
    RemoteMediaConfig {
        video: VaultVideoMode::Link,
        audio: MediaMode::Localize,
        pdf: MediaMode::Localize,
        image_format: ImageFormat::Jpeg,
        timeout_secs: 7,
        name_template: "v-{{md5}}".into(),
        audio_whitelist: vec!["mp3".into()],
        ..RemoteMediaConfig::default()
    }
}

#[test]
fn builtin_defaults_keep_todays_enabled_set() {
    let d = p("", &RemoteMediaConfig::default()).unwrap();
    assert_eq!(d.enabled, vec![Media::Images]);
    assert_eq!(d.video, VideoMode::Thumbnail);
    assert_eq!(d.audio, MediaMode::Link);
    assert_eq!(d.pdf, MediaMode::Link);
    assert_eq!(d.image_format, ImageFormat::Webp);
    assert_eq!(d.timeout.as_secs(), 30);
}

#[test]
fn vault_config_overrides_builtin_defaults() {
    let d = p("", &vault_rm()).unwrap();
    assert_eq!(d.video, VideoMode::Link);
    assert_eq!(d.audio, MediaMode::Localize);
    assert_eq!(d.pdf, MediaMode::Localize);
    assert_eq!(d.enabled, vec![Media::Images, Media::Audio, Media::Pdf]);
    assert_eq!(d.image_format, ImageFormat::Jpeg);
    assert_eq!(d.timeout.as_secs(), 7);
    assert_eq!(d.name_template, "v-{{md5}}");
    assert_eq!(d.whitelists["audio"], vec!["mp3".to_owned()]);
    let n = Params::note("a.md".into(), &vault_rm());
    assert_eq!(n.audio, MediaMode::Localize);
    assert_eq!(n.path.as_deref(), Some("a.md"));
}

#[test]
fn step_param_overrides_vault_config() {
    let d = p(
        "video = \"thumbnail\"\naudio = \"link\"\npdf = \"link\"\nimage_format = \"webp\"\n\
         timeout_secs = 3\nname_template = \"s-{{md5}}\"\naudio_whitelist = [\"OGG\"]\n",
        &vault_rm(),
    )
    .unwrap();
    assert_eq!(d.video, VideoMode::Thumbnail);
    assert_eq!(d.audio, MediaMode::Link);
    assert_eq!(d.pdf, MediaMode::Link);
    assert_eq!(d.enabled, vec![Media::Images]);
    assert_eq!(d.image_format, ImageFormat::Webp);
    assert_eq!(d.timeout.as_secs(), 3);
    assert_eq!(d.name_template, "s-{{md5}}");
    assert_eq!(d.whitelists["audio"], vec!["ogg".to_owned()]);
}

#[test]
fn legacy_bools_map_to_modes() {
    let rm = vault_rm();
    let t = p(
        "audio = true\npdf = true\nvideo = true\n",
        &RemoteMediaConfig::default(),
    )
    .unwrap();
    assert_eq!(t.audio, MediaMode::Localize);
    assert_eq!(t.pdf, MediaMode::Localize);
    assert_eq!(t.video, VideoMode::Localize);
    assert_eq!(
        t.enabled,
        vec![Media::Images, Media::Audio, Media::Video, Media::Pdf]
    );
    let f = p("audio = false\npdf = false\nvideo = false\n", &rm).unwrap();
    assert_eq!(f.audio, MediaMode::Link);
    assert_eq!(f.pdf, MediaMode::Link);
    // `video = false` = the vault default.
    assert_eq!(f.video, VideoMode::Link);
    assert_eq!(f.enabled, vec![Media::Images]);
    let no_img = p("images = false\n", &rm).unwrap();
    assert!(!no_img.enabled.contains(&Media::Images));
    assert!(p("images = \"localize\"\n", &rm).is_ok());
}

#[test]
fn bad_mode_values_rejected() {
    let rm = RemoteMediaConfig::default();
    for bad in [
        "video = \"localize\"",
        "audio = \"thumbnail\"",
        "pdf = 1",
        "images = \"link\"",
        "image_format = \"png\"",
    ] {
        assert!(p(bad, &rm).is_err(), "accepted {bad}");
    }
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
fn validate_accepts_empty_and_rejects_bad_params() {
    assert!(parse_params(&toml::Table::new(), &RemoteMediaConfig::default()).is_ok());
    let d = parse_params(&toml::Table::new(), &RemoteMediaConfig::default()).unwrap();
    assert_eq!(d.folder_template, DEFAULT_FOLDER_TEMPLATE);
    assert_eq!(d.name_template, DEFAULT_NAME_TEMPLATE);
    assert_eq!(
        resolve_attachment_path(
            DEFAULT_FOLDER_TEMPLATE,
            DEFAULT_NAME_TEMPLATE,
            "notes/My Note.md",
            "",
            "png",
            "abc",
            chrono::Local::now().naive_local()
        ),
        format!(
            "notes/_attachments/{}/abc_MD5.png",
            cv_text::slugify("My Note")
        )
    );
    let bad: toml::Table = toml::from_str("mode = \"nope\"").unwrap();
    assert!(parse_params(&bad, &RemoteMediaConfig::default()).is_err());
    let unknown: toml::Table = toml::from_str("bogus = 1").unwrap();
    assert!(parse_params(&unknown, &RemoteMediaConfig::default()).is_err());
}
