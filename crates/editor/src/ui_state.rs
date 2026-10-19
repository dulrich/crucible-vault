//! Local UI state (plan WP-1): `$XDG_STATE_HOME/crucible-vault/editor.json`, falling back to
//! `~/.local/state/crucible-vault/editor.json`. Never stored in the vault. Missing or corrupt
//! files yield defaults; writes are atomic (temp file + rename).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::reading::ViewMode;
use crate::theme::Surround;

/// What a tab shows (WP-R15 B4). Persisted as tagged JSON (`{"tab":"note","path":…}`,
/// `{"tab":"settings"}`, `{"tab":"log"}`); a bare string (every `editor.json` before WP-R15)
/// still reads: a vault path, or one of the old reserved sentinel paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "TabWire", from = "TabIn")]
pub enum TabKind {
    /// A vault note at this path.
    Note(String),
    /// Plan editor-settings-tab WP-2: the Settings tab.
    Settings,
    /// Plan round-6-feedback-todos WP-7: the notice log.
    Log,
}

/// The pre-WP-R15 Settings / Log tab paths (read path only).
const LEGACY_SETTINGS_TAB: &str = "/:settings";
const LEGACY_LOG_TAB: &str = "/:log";

#[derive(Serialize, Deserialize)]
#[serde(tag = "tab", rename_all = "snake_case")]
enum TabWire {
    Note { path: String },
    Settings,
    Log,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TabIn {
    Legacy(String),
    Tagged(TabWire),
}

impl From<TabKind> for TabWire {
    fn from(k: TabKind) -> Self {
        match k {
            TabKind::Note(path) => Self::Note { path },
            TabKind::Settings => Self::Settings,
            TabKind::Log => Self::Log,
        }
    }
}

impl From<TabIn> for TabKind {
    fn from(w: TabIn) -> Self {
        match w {
            TabIn::Legacy(p) if p == LEGACY_SETTINGS_TAB => Self::Settings,
            TabIn::Legacy(p) if p == LEGACY_LOG_TAB => Self::Log,
            TabIn::Legacy(p) | TabIn::Tagged(TabWire::Note { path: p }) => Self::Note(p),
            TabIn::Tagged(TabWire::Settings) => Self::Settings,
            TabIn::Tagged(TabWire::Log) => Self::Log,
        }
    }
}

/// Persisted editor state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// A WP-1 file's open tabs, in order (read only: `panes` supersedes it and it is no
    /// longer written).
    #[serde(skip_serializing)]
    pub tabs: Vec<TabKind>,
    /// Index into `tabs` (read only, as `tabs`).
    #[serde(skip_serializing)]
    pub active_tab: Option<usize>,
    /// Per-tab display mode of the open tabs (vault-relative path → mode); a path absent here
    /// opens in the default (Reading). Replaces the old global `layout` field, which was saved
    /// but never read; old files carrying it still load (the key is ignored).
    pub modes: BTreeMap<String, ViewMode>,
    pub surround: Surround,
    /// Most recent first, capped at [`RECENT_CAP`].
    pub recent: Vec<String>,
    /// WP-4 pane layout (pane tree + per-pane tabs + focused pane). `None` (a WP-1 file) →
    /// one pane built from `tabs`/`active_tab`.
    pub panes: Option<PaneLayout>,
    /// WP-4 explorer: expanded folders (vault-relative).
    pub expanded: BTreeSet<String>,
    /// editor-usability-round WP-3: the side column is collapsed to its icon strip.
    pub side_collapsed: bool,
    /// editor-usability-round WP-3: debug pane borders (off by default).
    pub pane_borders: bool,
}

/// Split direction: `Right` = side by side, `Down` = stacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    Right,
    Down,
}

/// A persisted pane tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneNode {
    /// One pane: its tabs in order and the active index.
    Leaf {
        #[serde(default)]
        tabs: Vec<TabKind>,
        #[serde(default)]
        active: Option<usize>,
    },
    /// A split; `ratio_milli` is the first child's share in thousandths.
    Split {
        dir: SplitDir,
        ratio_milli: u16,
        a: Box<PaneNode>,
        b: Box<PaneNode>,
    },
}

/// The persisted layout: the tree and the focused leaf (depth-first index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub root: PaneNode,
    #[serde(default)]
    pub focused: usize,
}

impl PaneNode {
    /// Leaves in depth-first order.
    pub fn leaves(&self) -> Vec<(&[TabKind], Option<usize>)> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(n) = stack.pop() {
            match n {
                Self::Leaf { tabs, active } => out.push((tabs.as_slice(), *active)),
                Self::Split { a, b, .. } => {
                    stack.push(b);
                    stack.push(a);
                }
            }
        }
        out
    }
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
            panes: Some(PaneLayout {
                root: PaneNode::Leaf {
                    tabs: vec![TabKind::Note("a.md".into()), TabKind::Note("b/c.md".into())],
                    active: Some(1),
                },
                focused: 0,
            }),
            modes: BTreeMap::from([("b/c.md".to_owned(), ViewMode::Split)]),
            surround: Surround::Light,
            recent: vec![],
            ..UiState::default()
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
    fn layout_flags_default_off_and_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("editor.json");
        std::fs::write(&path, br#"{"tabs":["a.md"],"surround":"dark"}"#).expect("old");
        let s = load(&path);
        assert!(!s.side_collapsed && !s.pane_borders, "pre-WP-3 file → off");
        let mut s2 = s.clone();
        s2.tabs.clear(); // read-only legacy field: never written
        s2.side_collapsed = true;
        s2.pane_borders = true;
        save(&path, &s2, None).expect("save");
        assert_eq!(load(&path), s2);
    }

    #[test]
    fn old_format_file_loads_and_modes_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("editor.json");
        // A pre-WP-2 file: global `layout`, no per-tab modes.
        std::fs::write(
            &path,
            br#"{"tabs":["a.md"],"active_tab":0,"layout":"source","surround":"dark","recent":[],"panes":{"root":{"kind":"leaf","tabs":["a.md"],"active":0},"focused":0},"expanded":[]}"#,
        )
        .expect("old");
        let s = load(&path);
        assert_eq!(s.tabs, vec![TabKind::Note("a.md".into())]);
        assert!(s.panes.is_some(), "old panes still restore");
        assert!(s.modes.is_empty(), "no per-tab modes → defaults");
        assert_eq!(
            s.modes.get("a.md").copied().unwrap_or_default(),
            ViewMode::Reading
        );
        let mut s2 = s.clone();
        s2.modes.insert("a.md".into(), ViewMode::Source);
        s2.modes.insert("b.md".into(), ViewMode::Split);
        save(&path, &s2, None).expect("save");
        assert_eq!(load(&path).modes, s2.modes);
    }

    /// WP-R16 (L2): `ViewMode` replaced `ui_state::Layout`; the persisted mode spellings are
    /// unchanged, so an existing `editor.json` reads and writes back the same strings.
    #[test]
    fn view_modes_keep_the_layout_spellings() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("editor.json");
        let old = br#"{"panes":{"root":{"kind":"leaf","tabs":[{"tab":"note","path":"a.md"}],"active":0},"focused":0},"modes":{"a.md":"source","b.md":"reading","c.md":"split"}}"#;
        std::fs::write(&path, old).expect("old");
        let s = load(&path);
        assert_eq!(
            s.modes.values().copied().collect::<Vec<_>>(),
            [ViewMode::Source, ViewMode::Reading, ViewMode::Split]
        );
        save(&path, &s, None).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        for (p, m) in [("a.md", "source"), ("b.md", "reading"), ("c.md", "split")] {
            assert!(text.contains(&format!(r#""{p}": "{m}""#)), "{text}");
        }
        assert_eq!(load(&path), s);
    }

    /// WP-R15 (B4): tabs persist as tagged JSON, the WP-1 flat fields are no longer written,
    /// and a file in the pre-WP-R15 shape (bare strings, sentinel paths) still reads.
    #[test]
    fn tabs_persist_tagged_and_legacy_sentinels_read() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("editor.json");
        let old = br#"{"tabs":["a.md","/:settings"],"active_tab":1,"panes":{"root":{"kind":"split","dir":"right","ratio_milli":500,"a":{"kind":"leaf","tabs":["a.md","/:log"],"active":1},"b":{"kind":"leaf","tabs":["/:settings"],"active":0}},"focused":1}}"#;
        std::fs::write(&path, old).expect("old");
        let s = load(&path);
        let note = TabKind::Note("a.md".into());
        assert_eq!(s.tabs, vec![note.clone(), TabKind::Settings]);
        let leaves: Vec<Vec<TabKind>> = s
            .panes
            .as_ref()
            .expect("panes")
            .root
            .leaves()
            .into_iter()
            .map(|(t, _)| t.to_vec())
            .collect();
        assert_eq!(leaves, [vec![note, TabKind::Log], vec![TabKind::Settings]]);
        let mut s2 = s.clone();
        s2.tabs.clear();
        s2.active_tab = None;
        save(&path, &s2, None).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains(r#""tab": "note""#), "{text}");
        assert!(text.contains(r#""tab": "settings""#), "{text}");
        assert!(text.contains(r#""tab": "log""#), "{text}");
        assert!(!text.contains("/:"), "no sentinel paths written: {text}");
        assert!(!text.contains("active_tab"), "no flat dual-write: {text}");
        assert_eq!(load(&path), s2, "the new format reads back");
    }

    #[test]
    fn refuses_vault_paths() {
        let tmp = tempfile::tempdir().expect("tmp");
        let p = tmp.path().join("crucible-vault/editor.json");
        assert!(save(&p, &UiState::default(), Some(tmp.path())).is_err());
    }
}
