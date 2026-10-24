//! The properties panel: the buffer's frontmatter as the shared [`Property`] model
//! (`editor_model::props`, the same rows Reading's properties block draws), typed by the vault's
//! property type map. Keys not in the map render by shape: a list as chips, anything else as
//! text. A text / list value with a wikilink or URL element renders as links. Read-only except
//! for a checkbox, which toggles through Reading's frontmatter path.

use editor_model::props::{PropType, PropValue, Property, from_value};

/// Plan round-6-feedback-todos WP-6: the checkbox state of a checkbox-typed property that fits
/// its type (`None` otherwise); null / missing draws unchecked.
pub fn checked(p: &Property) -> Option<bool> {
    (p.ty == Some(PropType::Checkbox) && !p.flagged)
        .then_some(matches!(p.values.as_slice(), [PropValue::Bool(true)]))
}

/// Plan round-12-feedback-todos WP-7: the frontmatter key whose link entries get the duplicate
/// verdict buttons (the daemon's `dupes_scan` writes it).
pub const POSSIBLE_DUPLICATE: &str = "possible-duplicate";

/// Plan round-12-feedback-todos WP-7: the `dupes-resolve` call one verdict button sends —
/// `keep` = the panel's note, `other` = the entry's resolved path.
pub fn dup_call(keep: &str, other: &str, verdict: &str) -> crate::palette::Call {
    let s = |v: &str| serde_json::Value::String(v.to_owned());
    crate::palette::Call {
        action_id: "dupes-resolve".into(),
        title: "Resolve duplicate".into(),
        params: serde_json::Map::from_iter([
            ("keep".to_owned(), s(keep)),
            ("other".to_owned(), s(other)),
            ("verdict".to_owned(), s(verdict)),
        ]),
        needs: Vec::new(),
    }
}

/// Plan round-6 WP-5: the values of a property drawn as links — a text / multitext / untyped
/// value with at least one wikilink or URL element (tags and aliases stay chips).
pub fn links(p: &Property) -> Option<&[PropValue]> {
    let typed = matches!(
        p.ty,
        Some(
            PropType::Checkbox
                | PropType::Date
                | PropType::Number
                | PropType::Tags
                | PropType::Aliases
        )
    );
    (!typed
        && !p.flagged
        && p.values
            .iter()
            .any(|v| matches!(v, PropValue::Link { .. } | PropValue::Url { .. })))
    .then_some(p.values.as_slice())
}

fn value_text(v: &PropValue) -> String {
    match v {
        PropValue::Text(s) => s.clone(),
        PropValue::Bool(b) => b.to_string(),
        PropValue::Url { label, .. } => label.clone(),
        PropValue::Link { target, alias, .. } => alias.clone().unwrap_or_else(|| target.clone()),
    }
}

/// Whether `p` draws as chips: a list-typed key, or an untyped YAML list.
fn chips(p: &Property) -> bool {
    match p.ty {
        Some(PropType::Tags | PropType::Aliases | PropType::Multitext) => true,
        None => p.list,
        _ => false,
    }
}

/// The one-line display form: ☑ / ☐, `⚠ value` (flagged), `[a] [b]` chips, `a, b` links, text.
pub fn display(p: &Property) -> String {
    if let Some(b) = checked(p) {
        return if b { "☑" } else { "☐" }.into();
    }
    let parts: Vec<String> = p.values.iter().map(value_text).collect();
    if p.flagged {
        format!("⚠ {}", parts.join(", "))
    } else if links(p).is_some() {
        parts.join(", ")
    } else if chips(p) {
        parts
            .iter()
            .filter(|s| !s.is_empty())
            .map(|c| format!("[{c}]"))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        parts.join(", ")
    }
}

/// The typed properties of a parsed note (empty without frontmatter; a YAML error is one
/// flagged row).
pub fn from_note(note: &ofm::Note) -> Vec<Property> {
    let Some(fm) = &note.frontmatter else {
        return Vec::new();
    };
    match &fm.value {
        Ok(v) => from_value(v),
        Err(_) => vec![Property {
            key: "frontmatter".into(),
            ty: None,
            values: vec![PropValue::Text("invalid YAML".into())],
            list: false,
            flagged: true,
        }],
    }
}
