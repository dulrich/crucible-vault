//! The schema vocabulary: plain `&'static` data a form renderer can walk generically.
//!
//! A file's schema is a [`Node`] (usually [`Node::Record`]). Every node a TOML value can take is
//! one variant of the small, exhaustively matchable [`Node`] enum; records carry [`Field`]s and
//! cross-field [`Rule`]s; [`Node::Union`] is the `type`-tagged union used by action steps and
//! starters.

/// One schema node: the shape of a TOML value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Node {
    /// A string.
    Str,
    /// A boolean.
    Bool,
    /// A TOML integer within the inclusive bounds (`None` = unbounded).
    Int { min: Option<i64>, max: Option<i64> },
    /// A number within the inclusive bounds; accepts TOML integer or float input (as the parsers
    /// do, e.g. `image_quality`).
    Float { min: Option<f64>, max: Option<f64> },
    /// A string from a closed set (aliases the parser accepts are listed too).
    Enum(&'static [&'static str]),
    /// A list of strings. `scalar_ok`: the parser also accepts a single string (`cv` steps'
    /// `get_str_list`). `choices`: when non-empty, every item must be one of them.
    StrList {
        scalar_ok: bool,
        choices: &'static [&'static str],
    },
    /// A list of key chords (`"Ctrl+Shift+K"`); the grammar lives in the editor's `chord.rs`.
    Chords,
    /// A vault-relative path string (or path template).
    Path,
    /// A Bases expression string (edited as text, never evaluated here).
    Expr,
    /// A views filter tree: an expression string, or a table with one of `and`/`or`/`not` holding
    /// a list of filter trees.
    Filter,
    /// A table whose keys and values are free-form (`set_props.props`, `run_action.params`).
    /// Rendered read-only / as raw TOML text.
    FreeTable,
    /// A table with user-chosen keys, every value of the given node (`[kinds.NAME]`).
    Map(&'static Node),
    /// A table with fixed fields.
    Record(&'static Record),
    /// An array (array-of-tables or inline array) of the given node.
    List(&'static Node),
    /// A table tagged by a string key, whose remaining fields depend on the tag.
    Union(&'static Union),
}

/// A default value shown by a form (and documented by the schema). Never written implicitly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Default {
    Str(&'static str),
    Bool(bool),
    Int(i64),
    Float(f64),
    StrList(&'static [&'static str]),
}

/// One field of a [`Record`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Field {
    /// The TOML key.
    pub key: &'static str,
    /// Form label.
    pub label: &'static str,
    /// One help line.
    pub help: &'static str,
    /// The value's shape.
    pub node: Node,
    /// The parser's default when the key is absent (`None` = absent means "unset").
    pub default: Option<Default>,
    /// The key must be present.
    pub required: bool,
}

/// A cross-field rule over keys of one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Exactly one of the keys is present.
    ExactlyOneOf(&'static [&'static str]),
    /// At most one of the keys is present (mutually exclusive).
    AtMostOneOf(&'static [&'static str]),
    /// At least one of the keys is present.
    AtLeastOneOf(&'static [&'static str]),
}

impl Rule {
    /// The keys the rule names.
    #[must_use]
    pub fn keys(&self) -> &'static [&'static str] {
        match *self {
            Rule::ExactlyOneOf(k) | Rule::AtMostOneOf(k) | Rule::AtLeastOneOf(k) => k,
        }
    }
}

/// A table with fixed fields. Keys the record does not list are preserved untouched by edits and
/// ignored by validation (the real parser decides whether they are errors).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Record {
    /// Diagnostic name (e.g. `"action"`).
    pub name: &'static str,
    pub fields: &'static [Field],
    pub rules: &'static [Rule],
}

impl Record {
    /// The field with `key`.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&'static Field> {
        self.fields.iter().find(|f| f.key == key)
    }
}

/// A tagged union: the `tag` key selects the variant record (which does not list the tag).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Union {
    /// Diagnostic name (e.g. `"step"`).
    pub name: &'static str,
    /// The tag key (`"type"`).
    pub tag: &'static str,
    pub variants: &'static [Variant],
}

impl Union {
    /// The variant with tag value `tag`.
    #[must_use]
    pub fn variant(&self, tag: &str) -> Option<&'static Variant> {
        self.variants.iter().find(|v| v.tag == tag)
    }
}

/// One variant of a [`Union`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Variant {
    /// The tag value.
    pub tag: &'static str,
    /// Form label.
    pub label: &'static str,
    pub record: &'static Record,
}

/// Builds a [`Field`] (const-friendly).
#[must_use]
pub const fn field(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    node: Node,
) -> Field {
    Field {
        key,
        label,
        help,
        node,
        default: None,
        required: false,
    }
}

impl Field {
    /// Marks the field required.
    #[must_use]
    pub const fn req(mut self) -> Self {
        self.required = true;
        self
    }

    /// Sets the default.
    #[must_use]
    pub const fn dflt(mut self, d: Default) -> Self {
        self.default = Some(d);
        self
    }
}

/// An unbounded integer.
pub const INT: Node = Node::Int {
    min: None,
    max: None,
};
/// A non-negative integer.
pub const UINT: Node = Node::Int {
    min: Some(0),
    max: None,
};
/// A positive integer.
pub const POS_INT: Node = Node::Int {
    min: Some(1),
    max: None,
};
/// A positive `u32` (`max_tokens`).
pub const POS_U32: Node = Node::Int {
    min: Some(1),
    max: Some(u32::MAX as i64),
};
/// A non-negative `u32`.
pub const U32: Node = Node::Int {
    min: Some(0),
    max: Some(u32::MAX as i64),
};
/// A plain list of strings.
pub const STR_LIST: Node = Node::StrList {
    scalar_ok: false,
    choices: &[],
};
/// A string or list of strings (`cv` steps' `get_str_list`).
pub const STR_OR_LIST: Node = Node::StrList {
    scalar_ok: true,
    choices: &[],
};
