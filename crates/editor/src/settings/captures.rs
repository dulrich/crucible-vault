//! The captures config path and the period folders it feeds the editor.

/// `captures.toml` (period folders, WP-4 audit E2).
pub const CAPTURES_PATH: &str = cv_schema::config_path!("captures.toml");

/// The day / month note folders from the loaded `captures.toml` (`[periods.day].folder`,
/// `[periods.month].folder`); constants when absent, unloaded, malformed or not a string.
/// `[periods.week]` does not feed the editor.
pub fn period_folders(
    f: Option<&crate::settings::ConfigFile>,
) -> editor_model::period::PeriodFolders {
    let doc = f.and_then(|f| f.doc());
    let get = |p: &str| {
        let path = cv_schema::Path::parse(p).ok()?;
        doc?.get_str(&path)
    };
    editor_model::period::PeriodFolders::from_config(
        get("periods.day.folder"),
        get("periods.month.folder"),
    )
}

/// `vault.toml` (`[structure] app_folder`, review S5: the default nav link's page lives there).
pub const VAULT_PATH: &str = cv_schema::config_path!("vault.toml");

/// The configured app folder from the loaded `vault.toml` (`[structure] app_folder`, surrounding
/// whitespace and `/` trimmed); the schema's default (tied to `cv_actions::vault::DEFAULT_APP_FOLDER`
/// by a `cv-schema` test) when absent, unloaded, blank or not a string.
pub fn app_folder(f: Option<&crate::settings::ConfigFile>) -> String {
    let configured = f.and_then(|f| f.doc()).and_then(|doc| {
        let path = cv_schema::Path::parse("structure.app_folder").ok()?;
        doc.get_str(&path)
    });
    configured
        .map(|s| s.trim().trim_matches('/'))
        .filter(|s| !s.is_empty())
        .map_or_else(|| default_app_folder().to_owned(), str::to_owned)
}

/// The schema default of `[structure] app_folder`.
fn default_app_folder() -> &'static str {
    cv_schema::schemas::VAULT_STRUCTURE
        .fields
        .iter()
        .find(|f| f.key == "app_folder")
        .and_then(|f| match f.default {
            Some(cv_schema::node::Default::Str(s)) => Some(s),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    /// Unloaded `vault.toml` → the schema default, so the default nav page stays put.
    #[test]
    fn app_folder_defaults_to_the_schema_default() {
        assert_eq!(super::app_folder(None), "_crucible");
    }
}
