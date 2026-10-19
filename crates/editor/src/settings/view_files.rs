//! The Views page's files: every `views/*.toml` gets a [`ConfigFile`] with the views carve-out
//! check ([`views_check`]); "New view file" adds an absent one.

use super::{ConfigFile, Settings};

impl Settings {
    /// Registers a [`ConfigFile`] for every `views/*.toml` among `paths` (the explorer's path
    /// list — no new daemon call), not yet known.
    pub fn sync_views(&mut self, paths: &[String]) {
        for p in paths {
            if is_view_path(p) && !self.files.contains_key(p.as_str()) {
                self.files.insert(p.clone(), view_file(p));
            }
        }
        if self.view_selected.is_none() {
            self.view_selected = self.view_paths().into_iter().next();
        }
    }

    /// Every known view file, sorted.
    pub fn view_paths(&self) -> Vec<String> {
        self.files
            .keys()
            .filter(|k| is_view_path(k))
            .cloned()
            .collect()
    }

    /// "New view file": an absent [`ConfigFile`] at `views/<name>.toml` (dirty, so Save
    /// creates it at the exact name). Returns the path, or why not.
    pub fn new_view_file(&mut self, name: &str) -> Result<String, String> {
        let name = name.trim();
        let stem = name.strip_suffix(".toml").unwrap_or(name);
        if stem.is_empty()
            || !stem
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("a view file name is letters, digits, '-' or '_'".into());
        }
        let path = format!("{VIEWS_DIR}/{stem}.toml");
        if self.files.contains_key(&path) {
            return Err(format!("{path} already exists"));
        }
        let f = ConfigFile::new_absent(&path, Some(cv_schema::view_schema()), Some(views_check));
        self.files.insert(path.clone(), f);
        self.view_selected = Some(path.clone());
        Ok(path)
    }
}

/// The views folder.
pub const VIEWS_DIR: &str = cv_schema::config_path!("views");

/// A direct `views/*.toml` child.
pub fn is_view_path(p: &str) -> bool {
    p.strip_prefix(VIEWS_DIR)
        .and_then(|r| r.strip_prefix('/'))
        .is_some_and(|n| !n.contains('/') && n.ends_with(".toml") && n.len() > 5)
}

fn view_file(path: &str) -> ConfigFile {
    let mut f = ConfigFile::new(path, Some(cv_schema::view_schema()));
    f.set_local_check(Some(views_check));
    f
}

/// The Views carve-out's local diagnostics: `views::def::check_text`, the same parse (blocking)
/// and compile (notices → warnings) the daemon's `views::def::load_dir` runs; per-view
/// definition errors are warnings too.
pub fn views_check(path: &str, text: &str) -> (Vec<String>, Vec<String>) {
    let stem = path
        .rsplit('/')
        .next()
        .and_then(|n| n.strip_suffix(".toml"))
        .unwrap_or("view");
    let mut notices = Vec::new();
    match ::views::def::check_text(stem, text, &mut notices) {
        Err(e) => (vec![e], Vec::new()),
        Ok(base) => {
            for v in &base.views {
                notices.extend(v.errors.iter().map(|e| format!("view `{}`: {e}", v.name)));
            }
            (Vec::new(), notices)
        }
    }
}
