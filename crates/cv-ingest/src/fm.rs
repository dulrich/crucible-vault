//! Intake frontmatter value quoting: the one JSON-string formatter every intake writer uses.

/// The `publication: <json string>` frontmatter line (plan intake-round-2-clean-dedupe-reading WP-3):
/// the one formatter every intake writer uses.
#[must_use]
pub fn publication_line(p: &str) -> String {
    format!("publication: {}", json_string(p))
}

/// Crucible's `yamlString` = `JSON.stringify` of a string.
#[must_use]
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
