//! Local UI state (plan WP-1): `$XDG_STATE_HOME/crucible-vault/editor.json`, falling back to
//! `~/.local/state/crucible-vault/editor.json`. Never stored in the vault. Missing or corrupt
//! files yield defaults; writes are atomic (temp file + rename).

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::Surround;

/// Source / reading / split (D9; WP-3 fills the reading side).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Source,
    Reading,
    Split,
}

/// Persisted editor state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Open tabs (vault-relative paths), in order.
    pub tabs: Vec<String>,
    /// Index into `tabs`.
    pub active_tab: Option<usize>,
    pub layout: Layout,
    pub surround: Surround,
    /// Most recent first, capped at [`RECENT_CAP`].
    pub recent: Vec<String>,
}

/// Recent-files cap.
pub const RECENT_CAP: usize = 50;

impl UiState {
    /// Records `path` as most recently opened.
    pub fn touch_recent(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_owned());
        self.recent.truncate(RECENT_CAP);
    }
}

/// Resolves the state file from `XDG_STATE_HOME` / `HOME` values (pure, for tests).
pub fn state_path_from(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let base = match xdg_state_home.filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(home.filter(|v| !v.is_empty())?)
            .join(".local")
            .join("state"),
    };
    Some(base.join("crucible-vault").join("editor.json"))
}

/// The state file for this process's environment.
pub fn default_path() -> Option<PathBuf> {
    state_path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// Loads `path`; a missing or unparseable file yields defaults.
pub fn load(path: &Path) -> UiState {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "editor.json unreadable; using defaults");
            UiState::default()
        }),
        Err(_) => UiState::default(),
    }
}

/// Atomically writes `state` to `path`, refusing any path inside `vault_root`.
pub fn save(path: &Path, state: &UiState, vault_root: Option<&Path>) -> std::io::Result<()> {
    if let Some(v) = vault_root
        && !v.as_os_str().is_empty()
        && path.starts_with(v)
    {
        return Err(std::io::Error::other(
            "refusing to store UI state in the vault",
        ));
    }
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("state path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".editor.json.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_xdg_then_home() {
        assert_eq!(
            state_path_from(Some("/x".into()), Some("/h".into())),
            Some(PathBuf::from("/x/crucible-vault/editor.json"))
        );
        assert_eq!(
            state_path_from(Some("".into()), Some("/h".into())),
            Some(PathBuf::from("/h/.local/state/crucible-vault/editor.json"))
        );
        assert_eq!(state_path_from(None, None), None);
    }

    #[test]
    fn round_trip_through_temp_xdg_state_home() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = state_path_from(Some(tmp.path().into()), None).expect("path");
        assert_eq!(load(&path), UiState::default(), "missing → defaults");
        let mut s = UiState {
            tabs: vec!["a.md".into(), "b/c.md".into()],
            active_tab: Some(1),
            layout: Layout::Split,
            surround: Surround::Light,
            recent: vec![],
        };
        s.touch_recent("a.md");
        s.touch_recent("b/c.md");
        s.touch_recent("a.md");
        assert_eq!(s.recent, vec!["a.md".to_owned(), "b/c.md".to_owned()]);
        save(&path, &s, None).expect("save");
        assert_eq!(load(&path), s);
        std::fs::write(&path, b"{not json").expect("corrupt");
        assert_eq!(load(&path), UiState::default(), "corrupt → defaults");
        std::fs::write(&path, br#"{"surround":"dark"}"#).expect("partial");
        assert_eq!(
            load(&path).surround,
            Surround::Dark,
            "partial → defaults for the rest"
        );
    }

    #[test]
    fn refuses_vault_paths() {
        let tmp = tempfile::tempdir().expect("tmp");
        let p = tmp.path().join("crucible-vault/editor.json");
        assert!(save(&p, &UiState::default(), Some(tmp.path())).is_err());
    }
}
