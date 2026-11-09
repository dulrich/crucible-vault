//! The one frontmatter property model,
//! built straight from the parsed `JsValue` and rendered by both Reading's properties block
//! (`render::RenderModel::properties`) and the Properties sidebar.
//!
//! [`TYPES`] is the neutral core map (plan public-release WP-2, C#13): Obsidian's own keys
//! (`aliases`, `tags`, `cssclasses`), `created`, and the keys the daemon itself writes (`word-count`,
//! `read`, `trim-label`). A vault adds its own through `editor.toml` `[property_types]`
//! ([`set_extra_types`]; an extra entry overrides a core one). A value that does not fit its
//! declared type is [`Property::flagged`].

use std::collections::BTreeMap;

use ofm::frontmatter::{JsValue, js_number_to_string, json_stringify, parse_frontmatter};

/// One frontmatter value: plain text, a boolean, an external URL or a `[[wikilink]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    Text(String),
    /// plan usability-sprint-captures-links WP-4: a YAML boolean (a Reading checkbox).
    Bool(bool),
    /// plan usability-sprint-captures-links WP-4: a bare remote URL or one `[label](url)`
    /// md-link with a remote url (opened by the external opener).
    Url {
        url: String,
        label: String,
    },
    Link {
        target: String,
        fragment: Option<String>,
        alias: Option<String>,
    },
}

/// One frontmatter key: its declared type, its values (a scalar has one value, a list one per
/// element, null none), whether it was a YAML list, and whether it misfits its declared type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub key: String,
    pub ty: Option<PropType>,
    pub values: Vec<PropValue>,
    /// The value was a YAML sequence (drawn as chips by the sidebar).
    pub list: bool,
    /// The value does not fit [`Self::ty`] (a non-boolean checkbox, a non-date date, a
    /// non-numeric number).
    pub flagged: bool,
}

impl Property {
    /// `key: value`, typed by [`TYPES`].
    pub fn new(key: &str, value: &JsValue) -> Self {
        let ty = type_of(key);
        let flagged = match (ty, value) {
            (_, JsValue::Null) => false,
            (Some(PropType::Checkbox), v) => !matches!(v, JsValue::Bool(_)),
            (Some(PropType::Date), v) => !matches!(v, JsValue::Str(s) if is_date(s)),
            (Some(PropType::Number), v) => {
                !matches!(v, JsValue::Num(_))
                    && !matches!(v, JsValue::Str(s) if s.trim().parse::<f64>().is_ok())
            }
            _ => false,
        };
        Self {
            key: key.to_owned(),
            ty,
            values: match value {
                JsValue::Arr(a) => a.iter().map(prop_value).collect(),
                JsValue::Null => Vec::new(),
                v => vec![prop_value(v)],
            },
            list: matches!(value, JsValue::Arr(_)),
            flagged,
        }
    }
}

/// An Obsidian property type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
    Aliases,
    Tags,
    Date,
    Number,
    Checkbox,
    Text,
    Multitext,
}

/// The built-in (core) property type map.
pub const TYPES: [(&str, PropType); 7] = [
    ("aliases", PropType::Aliases),
    ("tags", PropType::Tags),
    ("cssclasses", PropType::Multitext),
    ("created", PropType::Date),
    ("word-count", PropType::Number),
    ("read", PropType::Checkbox),
    ("trim-label", PropType::Checkbox),
];

/// The configured extension (`editor.toml` `[property_types]`), set by the editor on every prefs
/// change; read by every [`Property::new`], on whichever thread renders.
static EXTRA: std::sync::RwLock<BTreeMap<String, PropType>> =
    std::sync::RwLock::new(BTreeMap::new());

/// Replaces the configured extension of [`TYPES`] (key → type).
pub fn set_extra_types(types: BTreeMap<String, PropType>) {
    *EXTRA
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = types;
}

impl PropType {
    /// The `editor.toml` `[property_types]` value name (Obsidian's `types.json` names).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "aliases" => Self::Aliases,
            "tags" => Self::Tags,
            "date" => Self::Date,
            "number" => Self::Number,
            "checkbox" => Self::Checkbox,
            "text" => Self::Text,
            "multitext" => Self::Multitext,
            _ => return None,
        })
    }
}

/// The declared type of `key`: the configured extension first, then [`TYPES`].
pub fn type_of(key: &str) -> Option<PropType> {
    if let Some(t) = EXTRA
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(key)
    {
        return Some(*t);
    }
    TYPES.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// The properties of a parsed frontmatter object, in key order (none for a non-object).
pub fn from_value(v: &JsValue) -> Vec<Property> {
    match v {
        JsValue::Obj(o) => o
            .entries()
            .into_iter()
            .map(|(k, v)| Property::new(k, v))
            .collect(),
        _ => Vec::new(),
    }
}

/// The frontmatter YAML as properties; a parse failure yields none.
pub fn from_yaml(meta: &str) -> Vec<Property> {
    parse_frontmatter(meta)
        .map(|v| from_value(&v))
        .unwrap_or_default()
}

/// One frontmatter value element: a boolean, a whole-value `[[wikilink]]`, a remote URL /
/// `[label](url)`, else text.
pub fn prop_value(v: &ofm::frontmatter::JsValue) -> PropValue {
    let s = match v {
        JsValue::Str(s) => s.clone(),
        JsValue::Num(n) => js_number_to_string(*n),
        JsValue::Bool(b) => return PropValue::Bool(*b),
        JsValue::Null => String::new(),
        other => json_stringify(other),
    };
    let t = s.trim();
    if let Some((len, w)) = ofm::wikilink::match_wikilink(t)
        && len == t.len()
        && !w.embed
    {
        return PropValue::Link {
            target: w.slug,
            fragment: w.fragment,
            alias: w.alias,
        };
    }
    if let Some((url, label)) = prop_url(t) {
        return PropValue::Url { url, label };
    }
    PropValue::Text(s)
}

/// A bare `http(s)://…` / `mailto:` value, or exactly one `[label](url)` md-link whose url is
/// remote: `(url, label)` (a bare URL is its own label).
fn prop_url(t: &str) -> Option<(String, String)> {
    let bare = |u: &str| {
        !u.contains(char::is_whitespace)
            && (u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:"))
    };
    if bare(t) {
        return Some((t.to_owned(), t.to_owned()));
    }
    let inner = t.strip_prefix('[')?.strip_suffix(')')?;
    let (label, url) = inner.split_once("](")?;
    if label.contains(['[', ']']) || url.contains(['(', ')']) {
        return None;
    }
    let url = url.trim();
    (!url.is_empty() && !url.contains(char::is_whitespace) && crate::render::is_remote(url))
        .then(|| (url.to_owned(), label.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's property extras as `editor.toml` `[property_types]` states them (plan
    /// public-release WP-2, C#13; WP-6 writes them): the pre-WP-2 built-in map minus the core.
    const OWNER_EXTRAS: &[(&str, &str)] = &[
        ("review-after-commits", "number"),
        ("review-after-lines", "number"),
        ("model", "text"),
        ("TQ_extra_instructions", "text"),
        ("pending-plans", "multitext"),
        ("source-prefix-filter", "multitext"),
        ("topics", "multitext"),
        ("TQ_explain", "checkbox"),
        ("TQ_short_mode", "checkbox"),
        ("TQ_show_backlink", "checkbox"),
        ("TQ_show_cancelled_date", "checkbox"),
        ("TQ_show_created_date", "checkbox"),
        ("TQ_show_depends_on", "checkbox"),
        ("TQ_show_done_date", "checkbox"),
        ("TQ_show_due_date", "checkbox"),
        ("TQ_show_edit_button", "checkbox"),
        ("TQ_show_id", "checkbox"),
        ("TQ_show_on_completion", "checkbox"),
        ("TQ_show_postpone_button", "checkbox"),
        ("TQ_show_priority", "checkbox"),
        ("TQ_show_recurrence_rule", "checkbox"),
        ("TQ_show_scheduled_date", "checkbox"),
        ("TQ_show_start_date", "checkbox"),
        ("TQ_show_tags", "checkbox"),
        ("TQ_show_task_count", "checkbox"),
        ("TQ_show_toolbar", "checkbox"),
        ("TQ_show_tree", "checkbox"),
        ("TQ_show_urgency", "checkbox"),
    ];

    /// Empty config = the neutral core only; the owner's extras restore every old type (35 keys).
    #[test]
    fn core_types_plus_configured_extension() {
        assert_eq!(type_of("tags"), Some(PropType::Tags));
        assert_eq!(type_of("TQ_show_tree"), None);
        assert_eq!(type_of("model"), None);
        set_extra_types(
            OWNER_EXTRAS
                .iter()
                .map(|(k, t)| ((*k).to_owned(), PropType::parse(t).unwrap()))
                .collect(),
        );
        assert_eq!(TYPES.len() + OWNER_EXTRAS.len(), 35);
        assert_eq!(type_of("TQ_show_tree"), Some(PropType::Checkbox));
        assert_eq!(type_of("review-after-lines"), Some(PropType::Number));
        assert_eq!(type_of("pending-plans"), Some(PropType::Multitext));
        assert!(Property::new("TQ_explain", &JsValue::Str("yes".into())).flagged);
        assert_eq!(type_of("created"), Some(PropType::Date));
        set_extra_types(BTreeMap::new());
        assert_eq!(type_of("model"), None);
    }
}
