//! Review S5: the schema's vault-structure defaults are literals (`cv-schema` has no normal edge to
//! `cv-actions`); this test ties each one to the `cv-actions` constant that owns it.

use cv_actions::vault::{
    DEFAULT_APP_FOLDER, DEFAULT_RAW_TRANSCRIPT_FOLDER, DEFAULT_X_METADATA_ROOT,
    DEFAULT_YT_METADATA_ROOT,
};
use cv_schema::node::{Default as D, Record};
use cv_schema::schemas::{LINK_SCAN, PIPE_X, PIPE_YOUTUBE, VAULT_STRUCTURE};

fn default_of(r: &Record, key: &str) -> D {
    r.fields
        .iter()
        .find(|f| f.key == key)
        .and_then(|f| f.default)
        .unwrap_or_else(|| panic!("{}.{key} has a schema default", r.name))
}

fn str_default(r: &Record, key: &str) -> &'static str {
    match default_of(r, key) {
        D::Str(s) => s,
        _ => panic!("{}.{key} defaults to a string", r.name),
    }
}

#[test]
fn schema_folder_defaults_equal_their_cv_actions_owners() {
    assert_eq!(
        str_default(&VAULT_STRUCTURE, "app_folder"),
        DEFAULT_APP_FOLDER
    );
    assert_eq!(
        str_default(&VAULT_STRUCTURE, "raw_transcript_folder"),
        DEFAULT_RAW_TRANSCRIPT_FOLDER
    );
    assert_eq!(
        str_default(&PIPE_YOUTUBE, "metadata_root"),
        DEFAULT_YT_METADATA_ROOT
    );
    assert_eq!(
        str_default(&PIPE_X, "metadata_root"),
        DEFAULT_X_METADATA_ROOT
    );
    match default_of(&LINK_SCAN, "exclude") {
        D::StrList(l) => assert_eq!(l, [DEFAULT_APP_FOLDER]),
        _ => panic!("link_scan.exclude defaults to a list"),
    }
}
