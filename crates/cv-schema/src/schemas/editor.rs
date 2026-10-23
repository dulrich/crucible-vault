//! Editor-owned file schemas: `bookmarks.toml`, `views/*.toml`, `editor.toml`.

use super::*;

// ---------------------------------------------------------------------------------------------
// bookmarks.toml — `crates/editor/src/bookmarks.rs` (`Bookmark`, `File`)
// ---------------------------------------------------------------------------------------------

/// One `[[bookmark]]`.
pub static BOOKMARK: Record = Record {
    name: "bookmark",
    fields: &[
        field("name", "Name", "Shown in the panel", Node::Str).req(),
        field("path", "Path", "Vault path", Node::Path).req(),
        // plan editor-ui-round-3 WP-9: chords for "Open bookmark: <name>" (an action's shape).
        field("keys", "Keys", "Key chords", Node::Chords),
    ],
    rules: &[],
    summary: None,
};

/// `bookmarks.toml`.
pub static BOOKMARKS_FILE: Record = Record {
    name: "bookmarks.toml",
    fields: &[field(
        "bookmark",
        "Bookmarks",
        "In panel order",
        Node::List(&Node::Record(&BOOKMARK)),
    )],
    rules: &[],
    summary: None,
};

// ---------------------------------------------------------------------------------------------
// views/*.toml — `crates/views/src/def.rs` (`BaseDef`, `ViewDef`). The snake_case aliases
// (`group_by`, `column_size`, `display_name`) parse but are not modelled (preserved, unchecked).
// ---------------------------------------------------------------------------------------------

/// `[properties."<id>"]`.
pub static PROPERTY_DEF: Record = Record {
    name: "property",
    fields: &[field(
        "displayName",
        "Display name",
        "Column header",
        Node::Str,
    )],
    rules: &[],
    summary: None,
};

const DIRECTION: Node = Node::Enum(&["ASC", "DESC", "asc", "desc"]);

/// `SortSpec`.
pub static SORT: Record = Record {
    name: "sort",
    fields: &[
        field("property", "Property", "Property id", Node::Str).req(),
        field("direction", "Direction", "Sort direction", DIRECTION).dflt(D::Str("ASC")),
    ],
    rules: &[],
    summary: None,
};

/// `PivotColumn`.
pub static PIVOT_COLUMN: Record = Record {
    name: "column",
    fields: &[
        field("label", "Label", "Column header", Node::Str).req(),
        field(
            "when",
            "When",
            "Row goes here when truthy; absent = catch-all",
            Node::Expr,
        ),
    ],
    rules: &[],
    summary: None,
};

/// `SummaryRowDef` (`[[views.summary_rows]]`, plan round-10 WP-4; the parser also accepts the
/// `summaryRows` key alias).
pub static SUMMARY_ROW: Record = Record {
    name: "summary row",
    fields: &[
        field("label", "Label", "Footer row label", Node::Str).req(),
        field(
            "filter",
            "Filter",
            "Row predicate; the row aggregates only matching rows (null cells when none match)",
            Node::Expr,
        )
        .req(),
        field(
            "summaries",
            "Summaries",
            "Property id → Sum | Average | Count | Min | Max",
            Node::Map(&Node::Str),
        ),
    ],
    rules: &[],
    summary: None,
};

/// One `[[views]]` (`ViewDef`).
pub static VIEW: Record = Record {
    name: "view",
    fields: &[
        field(
            "type",
            "Type",
            "View type",
            Node::Enum(&["table", "pivot", "tasks"]),
        )
        .req(),
        field("name", "Name", "View name", Node::Str).req(),
        field("filters", "Filters", "Per-view filter tree", Node::Filter),
        field("order", "Columns", "Property ids in column order", STR_LIST),
        field(
            "sort",
            "Sort",
            "Sort keys",
            Node::List(&Node::Record(&SORT)),
        ),
        field("groupBy", "Group by", "Grouping key", Node::Record(&SORT)),
        field(
            "summaries",
            "Summaries",
            "Property id → Sum | Average | Count | Min | Max",
            Node::Map(&Node::Str),
        ),
        field(
            "summary_rows",
            "Summary rows",
            "Extra footer rows over the rows matching a filter, after the plain summaries row",
            Node::List(&Node::Record(&SUMMARY_ROW)),
        ),
        field("limit", "Limit", "Row cap", UINT),
        field(
            "columnSize",
            "Column sizes",
            "Display hint only",
            Node::Map(&INT),
        ),
        field("rows", "Rows", "Pivot row key expression", Node::Expr),
        field(
            "rows_label",
            "Rows label",
            "Header of the row-key column",
            Node::Str,
        ),
        field("rows_direction", "Rows direction", "Row order", DIRECTION).dflt(D::Str("ASC")),
        field(
            "columns",
            "Pivot columns",
            "First match wins",
            Node::List(&Node::Record(&PIVOT_COLUMN)),
        ),
        field(
            "value",
            "Value",
            "Value expression aggregated per cell",
            Node::Expr,
        ),
        field(
            "aggregate",
            "Aggregate",
            "Cell aggregate",
            Node::Enum(&["sum", "count"]),
        )
        .dflt(D::Str("sum")),
        field("display", "Display", "Cell display expression", Node::Expr),
        field(
            "total_column",
            "Total column",
            "Label; omitted when unset",
            Node::Str,
        ),
        field(
            "total_row",
            "Total row",
            "Label; omitted when unset",
            Node::Str,
        ),
        field(
            "average_row",
            "Average row",
            "Label; omitted when unset",
            Node::Str,
        ),
        field(
            "status",
            "Task status",
            "Allowed status chars; empty = any",
            STR_LIST,
        ),
        field(
            "require_text",
            "Require text",
            "Drop blank tasks",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
        field(
            "task_filter",
            "Task filter",
            "Per-task predicate",
            Node::Expr,
        ),
        field(
            "task_group",
            "Task group",
            "Task grouping",
            Node::Enum(&["heading", "file", "none"]),
        )
        .dflt(D::Str("heading")),
    ],
    rules: &[],
    summary: None,
};

/// One `views/*.toml` (`BaseDef`).
pub static VIEW_FILE: Record = Record {
    name: "views/*.toml",
    fields: &[
        // Plan round-5-reading-pickers-intake WP-10: where rows come from (`views::Source`).
        field(
            "source",
            "Source",
            "Row source: index notes or state.db tracker items",
            Node::Enum(&["index", "tracker_items"]),
        )
        .dflt(D::Str("index")),
        field("filters", "Filters", "Base filter tree", Node::Filter),
        field(
            "formulas",
            "Formulas",
            "Formula name → expression",
            Node::Map(&Node::Expr),
        ),
        field(
            "properties",
            "Properties",
            "Property id → display settings",
            Node::Map(&Node::Record(&PROPERTY_DEF)),
        ),
        field(
            "views",
            "Views",
            "One [[views]] each",
            Node::List(&Node::Record(&VIEW)),
        ),
    ],
    rules: &[],
    summary: None,
};

// ---------------------------------------------------------------------------------------------
// editor.toml — editor-owned (plan editor-settings-tab WP-2); defaults mirror the current
// consts: `editor::theme::BASE_TEXT_SIZE`, the `editor::theme::Surround` default,
// `editor::panels::SEARCH_LIMIT`/`SEARCH_DEBOUNCE`.
// ---------------------------------------------------------------------------------------------

/// `[search]`.
pub static EDITOR_SEARCH: Record = Record {
    name: "search",
    fields: &[
        field(
            "limit",
            "Result limit",
            "Search panel result cap",
            Node::Int {
                min: Some(1),
                max: Some(1000),
            },
        )
        .dflt(D::Int(50))
        .step(10.0),
        field(
            "debounce_ms",
            "Debounce (ms)",
            "Search input debounce",
            Node::Int {
                min: Some(0),
                max: Some(5000),
            },
        )
        .dflt(D::Int(250))
        .step(50.0),
    ],
    rules: &[],
    summary: None,
};

/// `[palette]`.
pub static EDITOR_PALETTE: Record = Record {
    name: "palette",
    fields: &[field(
        "hidden",
        "Hidden commands",
        "Command/action ids omitted from the palette (chords still work)",
        STR_LIST,
    )],
    rules: &[],
    summary: None,
};

/// `editor.toml`.
pub static EDITOR_FILE: Record = Record {
    name: "editor.toml",
    fields: &[
        field(
            "font_size",
            "Font size",
            "Base text size (px)",
            Node::Float {
                min: Some(8.0),
                max: Some(64.0),
            },
        )
        .dflt(D::Float(22.0))
        .step(1.0),
        field(
            "ui_scale",
            "UI scale",
            "Window scale factor",
            Node::Float {
                min: Some(0.5),
                max: Some(3.0),
            },
        )
        .dflt(D::Float(1.0))
        .step(0.25),
        field(
            "surround",
            "Surround",
            "Colour surround",
            Node::Enum(&["dark", "med", "light"]),
        )
        .dflt(D::Str("med")),
        field(
            "tab_title",
            "Tab title",
            "Tab header text: frontmatter title, file stem, vault path, or shortest unambiguous path suffix",
            Node::Enum(&["title", "basename", "path", "unambiguous"]),
        )
        .dflt(D::Str("title")),
        field(
            "quick_switch_open",
            "Quick Switch opens",
            "Quick Switch on a note already open: switch to its tab, move the tab into the active pane, or duplicate it there",
            Node::Enum(&["switch", "move", "duplicate"]),
        )
        .dflt(D::Str("move")),
        field(
            "delete_mode",
            "Delete mode",
            "How Delete note removes files: move to .trash/ or delete permanently (the confirm dialog always asks)",
            Node::Enum(&["trash", "permanent"]),
        )
        .dflt(D::Str("trash")),
        field(
            "missing_link",
            "Missing-note links",
            "Clicking a link to a note that does not exist: warn with a notice, or create the note (New-note folder, or the link's folder path) and open it",
            Node::Enum(&["warn", "create"]),
        )
        .dflt(D::Str("warn")),
        field(
            "focus_follows_mouse",
            "Focus follows mouse",
            "The pointer entering a pane focuses it (never while a modal or menu is open, or during a drag)",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "auto_lint",
            "Auto-lint on autosave",
            "Lint the open note's frontmatter when it autosaves after your edit (the lint-note action's settings; never on open, while the cursor is in the frontmatter, in _custom/ or template folders, or for notes without frontmatter)",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
        field(
            "scroll_anchor",
            "Scroll anchor (%)",
            "Where a Reading ↔ Source switch keeps your place: the block/line this far down the viewport (0 = top, 50 = middle, 100 = bottom) stays under the same height",
            Node::Int {
                min: Some(0),
                max: Some(100),
            },
        )
        .dflt(D::Int(0))
        .step(5.0),
        field(
            "open_url_cmd",
            "Open URL command",
            "Program + args that open an external http/https/mailto link; the URL is appended as the last argument (no shell). Empty = default",
            STR_LIST,
        )
        .dflt(D::StrList(&["xdg-open"])),
        field(
            "search",
            "Search panel",
            "Search panel tuning",
            Node::Record(&EDITOR_SEARCH),
        ),
        field(
            "palette",
            "Palette",
            "Palette options",
            Node::Record(&EDITOR_PALETTE),
        ),
        field(
            "keys",
            "Key overrides",
            "Command id → chords; [] unbinds",
            Node::Map(&Node::Chords),
        ),
    ],
    rules: &[],
    summary: None,
};
