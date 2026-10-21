//! Schemas of every vault config file (`_custom/crucible-vault/`) and of all 15 step kinds.
//!
//! Each schema describes its real parser (cited per item); the drift test in
//! `crates/cv/tests/schema_drift.rs` holds them together. Where a parser accepts something the
//! vocabulary cannot model (conditional requirements, placeholder syntax, key aliases), the
//! schema stays looser than the parser rather than inventing constraints — the daemon's reload
//! errors are the second validation layer.

use crate::node::{
    Default as D, Field, INT, Node, POS_INT, POS_U32, Record, RefKind, Rule, STR_LIST, STR_OR_LIST,
    Summary, U32, UINT, Union, Variant, field,
};

mod actions;
mod captures;
mod editor;
mod pipelines;
mod providers;
mod step_notes;
mod step_services;

pub use actions::*;
pub use captures::*;
pub use editor::*;
pub use pipelines::*;
pub use providers::*;
pub use step_notes::*;
pub use step_services::*;

// ---------------------------------------------------------------------------------------------
// Shared field helpers
// ---------------------------------------------------------------------------------------------

const PATH_FIELD: Field = field(
    "path",
    "Target note",
    "Vault path (placeholders ok); default: the active/triggering note",
    Node::Str,
);
const KIND_FIELD: Field = field(
    "kind",
    "Kind",
    "A `kinds.toml` kind name, usually \"{param.kind}\"",
    Node::Str,
);

/// The `[[action.steps]]` union, one variant per `StepKind::ALL` member (same order).
pub static STEP: Union = Union {
    name: "step",
    tag: "type",
    variants: &[
        Variant::of(&GUARD, "Guard"),
        Variant::of(&CAPTURE, "Capture"),
        Variant::of(&MATERIALIZE, "Materialize"),
        Variant::of(&MOVE, "Move"),
        Variant::of(&COPY, "Copy"),
        Variant::of(&SET_PROPS, "Set properties"),
        Variant::of(&REMOVE_PROPS, "Remove properties"),
        Variant::of(&UPSERT_TAGS, "Upsert tags"),
        Variant::of(&LINT, "Lint"),
        Variant::of(&LOCALIZE, "Localize attachments"),
        Variant::of(&AGENT, "Agent"),
        Variant::of(&MARK_FORWARDED, "Mark forwarded"),
        Variant::of(&RUN_ACTION, "Run action"),
        Variant::of(&SEARCH_UPSERT, "Search: upsert"),
        Variant::of(&SEARCH_DELETE, "Search: delete"),
        Variant::of(&LINK_SCAN, "Link scan"),
        Variant::of(&YT_METADATA, "YouTube metadata"),
        Variant::of(&YT_CHANNEL_ENRICH, "YouTube channel enrich"),
        Variant::of(&X_DISCOVER, "X: discover"),
        Variant::of(&X_METADATA, "X: metadata"),
        Variant::of(&X_BACKFILL, "X: backfill"),
        Variant::of(&IMAGE_DESCRIBE, "Describe images"),
        Variant::of(&TRACK_YOUTUBE, "Track YouTube"),
        Variant::of(&TRACK_BLOGS, "Track blogs"),
        Variant::of(&TRACK_MAIL, "Track mail"),
        Variant::of(&AUTO_FILE, "Auto-file clip"),
        Variant::of(&INTAKE_RECONCILE, "Reconcile intake"),
        Variant::of(&IGNORE_VIDEO, "Ignore video"),
        Variant::of(&DAILY_BRIEF, "Daily brief"),
        Variant::of(&CAPTURE_REPAIR, "Capture repair"),
    ],
};

// ---------------------------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------------------------

static ACTIONS_NODE: Node = Node::Record(&ACTIONS_FILE);
static SCHEDULES_NODE: Node = Node::Record(&SCHEDULES_FILE);
static KINDS_NODE: Node = Node::Record(&KINDS_FILE);
static CAPTURES_NODE: Node = Node::Record(&CAPTURES_FILE);
static PROVIDERS_NODE: Node = Node::Record(&PROVIDERS_FILE);
static BOOKMARKS_NODE: Node = Node::Record(&BOOKMARKS_FILE);
static EDITOR_NODE: Node = Node::Record(&EDITOR_FILE);
static VIEW_NODE: Node = Node::Record(&VIEW_FILE);
static PIPELINES_NODE: Node = Node::Record(&PIPELINES_FILE);
static SOURCES_NODE: Node = Node::Record(&SOURCES_FILE);

/// A vault-relative path inside the config folder, as a `&'static str` literal
/// (`config_path!("actions.toml")` is the folder + `actions.toml`). The one
/// definition of the folder name.
#[macro_export]
macro_rules! config_path {
    ($name:literal) => {
        concat!("_custom/crucible-vault/", $name)
    };
}

/// The vault config folder, with its trailing `/` (the daemon's `config_dir`).
pub const CONFIG_DIR: &str = config_path!("");

/// Every config file name with a schema (the daemon's `CONFIG_FILES`, then `bookmarks.toml`,
/// `editor.toml`). Views are per-file: see [`view_schema`].
pub const FILES: [&str; 10] = [
    "actions.toml",
    "schedules.toml",
    "kinds.toml",
    "captures.toml",
    "agents.toml",
    "providers.toml",
    "pipelines.toml",
    "sources.toml",
    "bookmarks.toml",
    "editor.toml",
];

/// The root schema of a config file by name (`"actions.toml"`, …; `"views/NAME.toml"` too).
#[must_use]
pub fn schema_for(file: &str) -> Option<&'static Node> {
    Some(match file {
        "actions.toml" => &ACTIONS_NODE,
        "schedules.toml" => &SCHEDULES_NODE,
        "kinds.toml" => &KINDS_NODE,
        "captures.toml" => &CAPTURES_NODE,
        "agents.toml" => &AGENTS_FILE,
        "providers.toml" => &PROVIDERS_NODE,
        "pipelines.toml" => &PIPELINES_NODE,
        "sources.toml" => &SOURCES_NODE,
        "bookmarks.toml" => &BOOKMARKS_NODE,
        "editor.toml" => &EDITOR_NODE,
        f if f.starts_with("views/") && f.ends_with(".toml") => &VIEW_NODE,
        _ => return None,
    })
}

/// The schema of one `views/*.toml` file.
#[must_use]
pub fn view_schema() -> &'static Node {
    &VIEW_NODE
}

/// The schema of `editor.toml`.
#[must_use]
pub fn editor_schema() -> &'static Node {
    &EDITOR_NODE
}

/// The params record of a step kind by its TOML `type` name.
#[must_use]
pub fn step_schema(kind: &str) -> Option<&'static Record> {
    STEP.variant(kind).map(|v| v.record)
}
