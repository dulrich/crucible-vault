//! The Settings section table: one
//! [`SectionInfo`] row per rail entry — label, glyph, group, file and intro — instead of a
//! match per property. File paths derive from the one config dir (`cv_schema::config_path!`).

use super::{editor_prefs, pages};
use crate::icons::Icon;

/// The vault config folder (the daemon's `config_dir`, `crates/cv/src/config.rs:100`), with
/// its trailing `/` — the one definition is `cv_schema::config_path!`.
pub use cv_schema::CONFIG_DIR;

/// One settings section (rail entry), in rail order; its data is one [`SectionInfo`] row of
/// [`SECTIONS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    Editor,
    Vault,
    RemoteContent,
    Hotkeys,
    Actions,
    Schedules,
    Pipelines,
    Sources,
    Captures,
    Kinds,
    Agents,
    Providers,
    Views,
    Bookmarks,
    Status,
}

/// One row of the section table (SM11): everything the rail and the page need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionInfo {
    pub section: Section,
    /// The rail label.
    pub label: &'static str,
    /// The rail glyph.
    pub icon: Icon,
    /// The rail group heading this section opens (Crucible's grouping), if any.
    pub group: Option<&'static str>,
    /// The vault-relative file this section edits (`None` for Hotkeys, Views — a folder — and
    /// Status); always under [`CONFIG_DIR`].
    pub file: Option<&'static str>,
    /// The page intro line.
    pub intro: &'static str,
    /// The top-level records of `file` this section's form shows (`None` = the whole file);
    /// lets two sections split one file (plan remote-media-sprint WP-2: `vault.toml`).
    pub keys: Option<&'static [&'static str]>,
}

const fn sec(
    section: Section,
    label: &'static str,
    icon: Icon,
    file: Option<&'static str>,
    intro: &'static str,
) -> SectionInfo {
    SectionInfo {
        section,
        label,
        icon,
        group: None,
        file,
        intro,
        keys: None,
    }
}

const fn grouped(mut i: SectionInfo, group: &'static str) -> SectionInfo {
    i.group = Some(group);
    i
}

const fn only(mut i: SectionInfo, keys: &'static [&'static str]) -> SectionInfo {
    i.keys = Some(keys);
    i
}

impl SectionInfo {
    /// Whether a form row at `path` belongs on this section's page.
    pub fn shows(&self, path: &cv_schema::Path) -> bool {
        match (self.keys, path.0.first()) {
            (None, _) | (_, None) => true,
            (Some(keys), Some(cv_schema::Seg::Key(k))) => keys.contains(&k.as_str()),
            (Some(_), Some(_)) => false,
        }
    }
}

/// The section table, in rail order (index = `Section as usize`).
pub static SECTIONS: [SectionInfo; 15] = [
    sec(
        Section::Editor,
        "Editor",
        Icon::Type,
        Some(editor_prefs::EDITOR_PATH),
        "",
    ),
    only(
        sec(
            Section::Vault,
            "Vault",
            Icon::Folder,
            Some(cv_schema::config_path!("vault.toml")),
            "Vault housekeeping: where recoverable deletes go (the vault .trash/ folder or the system trash) and how long trashed files are kept.",
        ),
        &["trash"],
    ),
    only(
        sec(
            Section::RemoteContent,
            "Remote content",
            Icon::Eye,
            Some(cv_schema::config_path!("vault.toml")),
            "How remote images, video, audio and PDFs are handled: localized into the vault, or shown as links or thumbnails.",
        ),
        &["remote_media"],
    ),
    sec(
        Section::Hotkeys,
        "Hotkeys",
        Icon::Keyboard,
        None,
        pages::hotkeys::INTRO,
    ),
    grouped(
        sec(
            Section::Actions,
            "Actions",
            Icon::Zap,
            Some(pages::hotkeys::ACTIONS_PATH),
            "Actions, each with params, starters, resources and its step list. Lint and search settings are the lint / search_* step forms; localize defaults live under Remote content, and a localize step form only overrides them.",
        ),
        "Automate",
    ),
    sec(
        Section::Schedules,
        "Schedules",
        Icon::Timer,
        Some(cv_schema::config_path!("schedules.toml")),
        "Scheduled runs of actions.",
    ),
    sec(
        Section::Pipelines,
        "Pipelines",
        Icon::TrendingUp,
        Some(cv_schema::config_path!("pipelines.toml")),
        "",
    ),
    sec(
        Section::Sources,
        "Sources",
        Icon::BookOpen,
        Some(cv_schema::config_path!("sources.toml")),
        "",
    ),
    sec(
        Section::Captures,
        "Captures & periods",
        Icon::CalendarDays,
        Some(crate::settings::CAPTURES_PATH),
        "Period notes (day / week / month folders, formats, templates), then named capture presets.",
    ),
    sec(
        Section::Kinds,
        "Kinds",
        Icon::Tags,
        Some(cv_schema::config_path!("kinds.toml")),
        "Note kinds: folder, template and defaults per kind.",
    ),
    grouped(
        sec(
            Section::Agents,
            "Agents",
            Icon::Bot,
            Some(cv_schema::config_path!("agents.toml")),
            "Agent definitions (one table per agent id).",
        ),
        "AI",
    ),
    sec(
        Section::Providers,
        "Providers",
        Icon::Server,
        Some(cv_schema::config_path!("providers.toml")),
        "Model providers. API keys are referenced by env var name or key file name only; the values are never read or shown here.",
    ),
    sec(
        Section::Views,
        "Views",
        Icon::Table,
        None,
        pages::views::INTRO,
    ),
    sec(
        Section::Bookmarks,
        "Bookmarks",
        Icon::Bookmark,
        Some(crate::bookmarks::BOOKMARKS_PATH),
        "Bookmarks shown in the side panel. The panel edits the same file; both save automatically, and while this page has an edit not yet saved the panel waits for it.",
    ),
    sec(Section::Status, "Status", Icon::Activity, None, ""),
];

impl Section {
    /// Rail order.
    pub const ALL: [Self; 15] = [
        Self::Editor,
        Self::Vault,
        Self::RemoteContent,
        Self::Hotkeys,
        Self::Actions,
        Self::Schedules,
        Self::Pipelines,
        Self::Sources,
        Self::Captures,
        Self::Kinds,
        Self::Agents,
        Self::Providers,
        Self::Views,
        Self::Bookmarks,
        Self::Status,
    ];

    /// This section's table row.
    pub fn info(self) -> &'static SectionInfo {
        &SECTIONS[self as usize]
    }

    /// The rail label.
    pub fn label(self) -> &'static str {
        self.info().label
    }

    /// The rail glyph.
    pub fn icon(self) -> Icon {
        self.info().icon
    }

    /// The rail group heading this section opens, if any.
    pub fn group(self) -> Option<&'static str> {
        self.info().group
    }

    /// The vault-relative path this section edits.
    pub fn file(self) -> Option<&'static str> {
        self.info().file
    }

    /// The file name inside [`CONFIG_DIR`] this section edits.
    pub fn file_name(self) -> Option<&'static str> {
        self.file().and_then(|p| p.strip_prefix(CONFIG_DIR))
    }

    /// The section whose file is `name` (a bare file name such as `actions.toml`).
    pub fn for_file_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.file_name() == Some(name))
    }
}
