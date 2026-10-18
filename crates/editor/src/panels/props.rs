//! The properties panel: the buffer's frontmatter, typed by the vault's property type map.
//!
//! [`TYPES`] is transcribed from the live vault's `.obsidian/types.json` (34 keys) by the
//! orchestrator (WP-5 brief pin 8); agents never read that file (I1). Keys not in the map render
//! by shape: a list as chips, anything else as text. Read-only: editing stays in source mode.

use editor_model::render::{PropValue, prop_value};
use ofm::frontmatter::{JsValue, js_number_to_string, json_stringify};

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

/// The vault's property type map.
pub const TYPES: [(&str, PropType); 34] = [
    ("aliases", PropType::Aliases),
    ("tags", PropType::Tags),
    ("created", PropType::Date),
    ("word-count", PropType::Number),
    ("review-after-commits", PropType::Number),
    ("review-after-lines", PropType::Number),
    ("read", PropType::Checkbox),
    ("model", PropType::Text),
    ("TQ_extra_instructions", PropType::Text),
    ("cssclasses", PropType::Multitext),
    ("pending-plans", PropType::Multitext),
    ("source-prefix-filter", PropType::Multitext),
    ("topics", PropType::Multitext),
    ("TQ_explain", PropType::Checkbox),
    ("TQ_short_mode", PropType::Checkbox),
    ("TQ_show_backlink", PropType::Checkbox),
    ("TQ_show_cancelled_date", PropType::Checkbox),
    ("TQ_show_created_date", PropType::Checkbox),
    ("TQ_show_depends_on", PropType::Checkbox),
    ("TQ_show_done_date", PropType::Checkbox),
    ("TQ_show_due_date", PropType::Checkbox),
    ("TQ_show_edit_button", PropType::Checkbox),
    ("TQ_show_id", PropType::Checkbox),
    ("TQ_show_on_completion", PropType::Checkbox),
    ("TQ_show_postpone_button", PropType::Checkbox),
    ("TQ_show_priority", PropType::Checkbox),
    ("TQ_show_recurrence_rule", PropType::Checkbox),
    ("TQ_show_scheduled_date", PropType::Checkbox),
    ("TQ_show_start_date", PropType::Checkbox),
    ("TQ_show_tags", PropType::Checkbox),
    ("TQ_show_task_count", PropType::Checkbox),
    ("TQ_show_toolbar", PropType::Checkbox),
    ("TQ_show_tree", PropType::Checkbox),
    ("TQ_show_urgency", PropType::Checkbox),
];

/// The declared type of `key`, if the map has it.
pub fn type_of(key: &str) -> Option<PropType> {
    TYPES.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

/// How one value renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rendered {
    /// ☑ / ☐.
    Check(bool),
    Text(String),
    Chips(Vec<String>),
    /// The value does not fit its declared type (shown as-is, marked).
    Flagged(String),
    /// Plan round-6 WP-5: a text / list value with at least one wikilink or URL element, in the
    /// Reading render model's frontmatter parse (non-link elements are `PropValue::Text`).
    Links(Vec<PropValue>),
}

impl Rendered {
    /// One-line display form.
    pub fn display(&self) -> String {
        match self {
            Rendered::Check(true) => "☑".into(),
            Rendered::Check(false) => "☐".into(),
            Rendered::Text(s) => s.clone(),
            Rendered::Chips(v) => v
                .iter()
                .map(|c| format!("[{c}]"))
                .collect::<Vec<_>>()
                .join(" "),
            Rendered::Flagged(s) => format!("⚠ {s}"),
            Rendered::Links(v) => v
                .iter()
                .map(|p| match p {
                    PropValue::Text(s) => s.clone(),
                    PropValue::Bool(b) => b.to_string(),
                    PropValue::Url { label, .. } => label.clone(),
                    PropValue::Link { target, alias, .. } => {
                        alias.clone().unwrap_or_else(|| target.clone())
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// One property row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prop {
    pub key: String,
    pub ty: Option<PropType>,
    pub value: Rendered,
}

fn scalar(v: &JsValue) -> String {
    match v {
        JsValue::Null => String::new(),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Num(n) => js_number_to_string(*n),
        JsValue::Str(s) => s.clone(),
        other => json_stringify(other),
    }
}

fn list(v: &JsValue) -> Vec<String> {
    match v {
        JsValue::Arr(a) => a.iter().map(scalar).filter(|s| !s.is_empty()).collect(),
        JsValue::Null => Vec::new(),
        other => vec![scalar(other)],
    }
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

/// Plan round-6 WP-5: a text / chips value whose elements include a wikilink or URL becomes
/// [`Rendered::Links`] (parsed by the render model's `prop_value`, not a second link parser).
fn linked(r: Rendered) -> Rendered {
    let elems: Vec<String> = match &r {
        Rendered::Text(s) => vec![s.clone()],
        Rendered::Chips(v) => v.clone(),
        _ => return r,
    };
    let parsed: Vec<PropValue> = elems
        .into_iter()
        .map(|e| match prop_value(&JsValue::Str(e.clone())) {
            v @ (PropValue::Link { .. } | PropValue::Url { .. }) => v,
            _ => PropValue::Text(e),
        })
        .collect();
    if parsed.iter().any(|v| !matches!(v, PropValue::Text(_))) {
        Rendered::Links(parsed)
    } else {
        r
    }
}

/// Renders `value` for `key` per the type map.
pub fn render(key: &str, value: &JsValue) -> Prop {
    let ty = type_of(key);
    let r = match ty {
        Some(PropType::Checkbox) => match value {
            JsValue::Bool(b) => Rendered::Check(*b),
            JsValue::Null => Rendered::Check(false),
            other => Rendered::Flagged(scalar(other)),
        },
        Some(PropType::Date) => match value {
            JsValue::Str(s) if is_date(s) => Rendered::Text(s.clone()),
            JsValue::Null => Rendered::Text(String::new()),
            other => Rendered::Flagged(scalar(other)),
        },
        Some(PropType::Number) => match value {
            JsValue::Num(n) => Rendered::Text(js_number_to_string(*n)),
            JsValue::Str(s) if s.trim().parse::<f64>().is_ok() => Rendered::Text(s.clone()),
            JsValue::Null => Rendered::Text(String::new()),
            other => Rendered::Flagged(scalar(other)),
        },
        Some(PropType::Tags | PropType::Aliases) => Rendered::Chips(list(value)),
        Some(PropType::Multitext) => linked(Rendered::Chips(list(value))),
        Some(PropType::Text) => linked(Rendered::Text(scalar(value))),
        None => linked(match value {
            JsValue::Arr(_) => Rendered::Chips(list(value)),
            other => Rendered::Text(scalar(other)),
        }),
    };
    Prop {
        key: key.to_owned(),
        ty,
        value: r,
    }
}

/// The typed properties of a parsed note (empty without frontmatter; a YAML error is one
/// flagged row).
pub fn from_note(note: &ofm::Note) -> Vec<Prop> {
    let Some(fm) = &note.frontmatter else {
        return Vec::new();
    };
    match &fm.value {
        Ok(JsValue::Obj(o)) => o.entries().into_iter().map(|(k, v)| render(k, v)).collect(),
        Ok(_) => Vec::new(),
        Err(_) => vec![Prop {
            key: "frontmatter".into(),
            ty: None,
            value: Rendered::Flagged("invalid YAML".into()),
        }],
    }
}
