//! Step-side unit tests of `localize` (the pure vectors live in `cv_text::attachments`).

use cv_text::attachments::{
    DEFAULT_FOLDER_TEMPLATE, DEFAULT_NAME_TEMPLATE, resolve_attachment_path,
};

use super::params::parse_params;
use super::repair_counts;

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
    assert!(parse_params(&toml::Table::new()).is_ok());
    let d = parse_params(&toml::Table::new()).unwrap();
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
    assert!(parse_params(&bad).is_err());
    let unknown: toml::Table = toml::from_str("bogus = 1").unwrap();
    assert!(parse_params(&unknown).is_err());
}
