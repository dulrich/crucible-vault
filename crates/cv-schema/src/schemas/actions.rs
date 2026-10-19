//! `actions.toml` and `schedules.toml` schemas.

use super::*;

// ---------------------------------------------------------------------------------------------
// actions.toml — `cv_actions::config::ActionDef`
// ---------------------------------------------------------------------------------------------

/// `cv_protocol::ParamSpec` (not `deny_unknown_fields`).
pub static PARAM: Record = Record {
    name: "param",
    fields: &[
        field("name", "Name", "Param name ({param.NAME})", Node::Str).req(),
        field("label", "Label", "Prompt label", Node::Str).req(),
        field(
            "kind",
            "Kind",
            "Input kind",
            Node::Enum(&["text", "multiline", "path", "enum", "date", "bool"]),
        )
        .req(),
        field(
            "required",
            "Required",
            "The client must supply a value",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "choices",
            "Choices",
            "Enum choices (required for kind = enum)",
            STR_LIST,
        ),
        field("default", "Default", "Default value", Node::Str),
    ],
    rules: &[],
    summary: None,
};

/// `PropSelector` (`crates/cv-actions/src/model.rs`).
pub static PROP_SELECTOR: Record = Record {
    name: "prop",
    fields: &[
        field("key", "Key", "Frontmatter property", Node::Str).req(),
        field(
            "value",
            "Value",
            "Required value; absent = key present",
            Node::Str,
        ),
    ],
    rules: &[],
    summary: None,
};

/// `Selector` (`crates/cv-actions/src/model.rs`); an all-empty selector disarms (I5).
pub static SELECTOR: Record = Record {
    name: "selector",
    fields: &[
        field(
            "folder",
            "Folder",
            "Folder prefix (no trailing slash)",
            Node::Folder,
        ),
        field("tag", "Tag", "Tag without #", Node::Str),
        field(
            "prop",
            "Property",
            "Frontmatter property",
            Node::Record(&PROP_SELECTOR),
        ),
    ],
    rules: &[Rule::AtLeastOneOf(&["folder", "tag", "prop"])],
    summary: None,
};

const DAILY: Field = field("daily", "Daily at", "HH:MM local time", Node::Str);
const EVERY: Field = field("every", "Every", "<n>m or <n>h", Node::Str);

/// `cv_actions::config::StarterDef::Command`.
pub static STARTER_COMMAND: Record = Record {
    name: "command",
    fields: &[],
    rules: &[],
    summary: None,
};

/// `StarterDef::Event`.
pub static STARTER_EVENT: Record = Record {
    name: "event",
    fields: &[
        field(
            "selector",
            "Selector",
            "Which notes",
            Node::Record(&SELECTOR),
        )
        .req(),
        field(
            "kinds",
            "Events",
            "Change kinds listened to",
            Node::StrList {
                scalar_ok: false,
                choices: &["created", "modified", "deleted", "renamed"],
            },
        )
        .req(),
        field(
            "origin",
            "Origin",
            "Whose edits",
            Node::Enum(&["external", "daemon", "any"]),
        )
        .dflt(D::Str("external")),
    ],
    rules: &[],
    summary: None,
};

/// `StarterDef::Schedule`.
pub static STARTER_SCHEDULE: Record = Record {
    name: "schedule",
    fields: &[DAILY, EVERY],
    rules: &[Rule::ExactlyOneOf(&["daily", "every"])],
    summary: None,
};

/// `[[action.starters]]` (`cv_actions::config::StarterDef`).
pub static STARTER: Union = Union {
    name: "starter",
    tag: "type",
    variants: &[
        Variant::of(&STARTER_COMMAND, "Command"),
        Variant::of(&STARTER_EVENT, "Event"),
        Variant::of(&STARTER_SCHEDULE, "Schedule"),
    ],
};

/// `ResourceSpec` (`crates/cv-actions/src/model.rs`).
pub static RESOURCES: Record = Record {
    name: "resources",
    fields: &[
        field(
            "note",
            "Note",
            "Include the note path as a resource key",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
        field(
            "keys",
            "Extra keys",
            "kind::id templates ({path}, {param.NAME})",
            STR_LIST,
        ),
    ],
    rules: &[],
    summary: None,
};

/// One `[[action]]` (`cv_actions::config::ActionDef`).
pub static ACTION: Record = Record {
    name: "action",
    fields: &[
        field("id", "Id", "Stable action id", Node::Str).req(),
        field(
            "label",
            "Label",
            "Palette label; default: the id",
            Node::Str,
        ),
        field("group", "Group", "Palette group", Node::Str),
        field("keys", "Keys", "Key chords", Node::Chords),
        field(
            "params",
            "Params",
            "Prompted parameters",
            Node::List(&Node::Record(&PARAM)),
        ),
        field(
            "needs",
            "Needs",
            "Context the client supplies",
            Node::StrList {
                scalar_ok: false,
                choices: &["active_path", "selection", "value", "line"],
            },
        ),
        field(
            "lane",
            "Lane",
            "Queue lane",
            Node::Enum(&["auto", "user", "background"]),
        )
        .dflt(D::Str("auto")),
        field(
            "durable",
            "Durable",
            "false: the palette awaits completion",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
        field(
            "steps",
            "Steps",
            "Executed in order",
            Node::List(&Node::Union(&STEP)),
        )
        .req(),
        field(
            "starters",
            "Starters",
            "Default: one command starter",
            Node::List(&Node::Union(&STARTER)),
        ),
        field(
            "resources",
            "Resources",
            "Resource-key derivation",
            Node::Record(&RESOURCES),
        ),
    ],
    rules: &[],
    summary: None,
};

/// `actions.toml`.
pub static ACTIONS_FILE: Record = Record {
    name: "actions.toml",
    fields: &[field(
        "action",
        "Actions",
        "One [[action]] each",
        Node::List(&Node::Record(&ACTION)),
    )],
    rules: &[],
    summary: None,
};

// ---------------------------------------------------------------------------------------------
// schedules.toml — `cv_actions::config::ScheduleDef`
// ---------------------------------------------------------------------------------------------

/// One `[[schedule]]`.
pub static SCHEDULE: Record = Record {
    name: "schedule",
    fields: &[
        field("action", "Action", "Action id to run", Node::Str).req(),
        DAILY,
        EVERY,
    ],
    rules: &[Rule::ExactlyOneOf(&["daily", "every"])],
    summary: None,
};

/// `schedules.toml`.
pub static SCHEDULES_FILE: Record = Record {
    name: "schedules.toml",
    fields: &[field(
        "schedule",
        "Schedules",
        "One [[schedule]] each",
        Node::List(&Node::Record(&SCHEDULE)),
    )],
    rules: &[],
    summary: None,
};
