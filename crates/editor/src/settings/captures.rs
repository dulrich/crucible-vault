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
