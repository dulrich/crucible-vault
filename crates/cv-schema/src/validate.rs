//! Form-level validation of a [`ConfigDoc`] against a schema [`Node`], and schema coverage.

use std::collections::BTreeSet;

use toml_edit::{Item, Value};

use crate::doc::{ConfigDoc, Path};
use crate::node::{Node, Record, Rule, Union};

/// One validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// Where (the field, or the record for a cross-field rule).
    pub path: Path,
    pub message: String,
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.0.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Checks `doc` against `schema`: required keys, types, enum membership, ranges, exactly-one-of,
/// mutual exclusion, at-least-one-of. Keys the schema does not list are ignored.
#[must_use]
pub fn validate(doc: &ConfigDoc, schema: &Node) -> Vec<FieldError> {
    let mut out = Vec::new();
    check(doc.root(), schema, &Path::root(), &mut out);
    out
}

/// Validates one item against `node` (for a single form section).
#[must_use]
pub fn validate_item(item: &Item, node: &Node, at: &Path) -> Vec<FieldError> {
    let mut out = Vec::new();
    check(item, node, at, &mut out);
    out
}

fn err(out: &mut Vec<FieldError>, at: &Path, m: impl Into<String>) {
    out.push(FieldError {
        path: at.clone(),
        message: m.into(),
    });
}

fn value(it: &Item) -> Option<&Value> {
    it.as_value()
}

/// The elements of a list item (array of tables or inline array).
pub(crate) fn list_elems(it: &Item) -> Option<Vec<&Item>> {
    match it {
        Item::ArrayOfTables(a) => Some((0..a.len()).filter_map(|i| it.get(i)).collect()),
        Item::Value(Value::Array(a)) => Some((0..a.len()).filter_map(|i| it.get(i)).collect()),
        _ => None,
    }
}

#[allow(clippy::cast_precision_loss)]
fn check(it: &Item, node: &Node, at: &Path, out: &mut Vec<FieldError>) {
    match *node {
        Node::Str | Node::Path | Node::Expr => {
            if it.as_str().is_none() {
                err(out, at, "must be a string");
            }
        }
        Node::Bool => {
            if it.as_bool().is_none() {
                err(out, at, "must be true or false");
            }
        }
        Node::Int { min, max } => match it.as_integer() {
            None => err(out, at, "must be an integer"),
            Some(n) => {
                if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                    err(out, at, range_msg(min, max));
                }
            }
        },
        Node::Float { min, max } => {
            match it.as_float().or_else(|| it.as_integer().map(|n| n as f64)) {
                None => err(out, at, "must be a number"),
                Some(x) => {
                    if min.is_some_and(|m| x < m) || max.is_some_and(|m| x > m) || x.is_nan() {
                        err(out, at, range_msg(min, max));
                    }
                }
            }
        }
        Node::Enum(choices) => match it.as_str() {
            Some(s) if choices.contains(&s) => {}
            Some(s) => err(
                out,
                at,
                format!("{s:?} is not one of {}", choices.join(", ")),
            ),
            None => err(out, at, "must be a string"),
        },
        Node::StrList { scalar_ok, choices } => {
            let items: Option<Vec<&str>> = match value(it) {
                Some(Value::String(s)) if scalar_ok => Some(vec![s.value().as_str()]),
                Some(Value::Array(a)) => a.iter().map(Value::as_str).collect(),
                _ => None,
            };
            match items {
                None if scalar_ok => err(out, at, "must be a string or a list of strings"),
                None => err(out, at, "must be a list of strings"),
                Some(v) => {
                    for s in v {
                        if !choices.is_empty() && !choices.contains(&s) {
                            err(
                                out,
                                at,
                                format!("{s:?} is not one of {}", choices.join(", ")),
                            );
                        }
                    }
                }
            }
        }
        Node::Chords => match value(it) {
            Some(Value::Array(a))
                if a.iter()
                    .all(|v| v.as_str().is_some_and(|s| !s.trim().is_empty())) => {}
            _ => err(out, at, "must be a list of key chords"),
        },
        Node::Filter => check_filter(it, at, out),
        Node::FreeTable => {
            if it.as_table_like().is_none() {
                err(out, at, "must be a table");
            }
        }
        Node::Map(v) => match it.as_table_like() {
            None => err(out, at, "must be a table"),
            Some(t) => {
                for (k, child) in t.iter() {
                    check(child, v, &at.key(k), out);
                }
            }
        },
        Node::Record(r) => check_record(it, r, at, out),
        Node::List(el) => match list_elems(it) {
            None => err(out, at, "must be a list"),
            Some(items) => {
                for (i, child) in items.into_iter().enumerate() {
                    check(child, el, &at.index(i), out);
                }
            }
        },
        Node::Union(u) => check_union(it, u, at, out),
    }
}

fn range_msg<T: std::fmt::Display>(min: Option<T>, max: Option<T>) -> String {
    match (min, max) {
        (Some(a), Some(b)) => format!("must be between {a} and {b}"),
        (Some(a), None) => format!("must be at least {a}"),
        (None, Some(b)) => format!("must be at most {b}"),
        (None, None) => "out of range".into(),
    }
}

fn check_filter(it: &Item, at: &Path, out: &mut Vec<FieldError>) {
    if it.as_str().is_some() {
        return;
    }
    let Some(t) = it.as_table_like() else {
        return err(out, at, "must be an expression or an and/or/not table");
    };
    let keys: Vec<&str> = t.iter().map(|(k, _)| k).collect();
    match keys.as_slice() {
        [k @ ("and" | "or" | "not")] => {
            let child = t.get(k).and_then(list_elems);
            match child {
                None => err(out, &at.key(k), "must be a list of filters"),
                Some(items) => {
                    for (i, c) in items.into_iter().enumerate() {
                        check_filter(c, &at.key(k).index(i), out);
                    }
                }
            }
        }
        _ => err(out, at, "a filter table needs exactly one of and, or, not"),
    }
}

fn check_union(it: &Item, u: &Union, at: &Path, out: &mut Vec<FieldError>) {
    let Some(t) = it.as_table_like() else {
        return err(out, at, "must be a table");
    };
    match t.get(u.tag).and_then(Item::as_str) {
        None => err(out, &at.key(u.tag), format!("{} needs `{}`", u.name, u.tag)),
        Some(tag) => match u.variant(tag) {
            None => err(
                out,
                &at.key(u.tag),
                format!("unknown {} {} {tag:?}", u.name, u.tag),
            ),
            Some(v) => check_record(it, v.record, at, out),
        },
    }
}

fn check_record(it: &Item, r: &Record, at: &Path, out: &mut Vec<FieldError>) {
    let Some(t) = it.as_table_like() else {
        return err(out, at, "must be a table");
    };
    let present = |k: &str| t.get(k).is_some_and(|i| !i.is_none());
    for f in r.fields {
        match t.get(f.key).filter(|i| !i.is_none()) {
            Some(child) => check(child, &f.node, &at.key(f.key), out),
            None if f.required => err(out, &at.key(f.key), format!("{} is required", f.label)),
            None => {}
        }
    }
    for rule in r.rules {
        let n = rule.keys().iter().filter(|k| present(k)).count();
        let list = rule
            .keys()
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ");
        match rule {
            Rule::ExactlyOneOf(_) if n != 1 => err(out, at, format!("set exactly one of {list}")),
            Rule::AtMostOneOf(_) if n > 1 => err(out, at, format!("set at most one of {list}")),
            Rule::AtLeastOneOf(_) if n == 0 => err(out, at, format!("set at least one of {list}")),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------------------------

/// Every field a schema declares, as coverage keys: record fields `a.b`, list elements `a[]`,
/// map values `a.*`, union variants `a<type=agent>` (and their fields `a<type=agent>.b`).
#[must_use]
pub fn schema_paths(node: &Node) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk_schema(node, "", &mut out, 0);
    out
}

fn walk_schema(node: &Node, at: &str, out: &mut BTreeSet<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    match *node {
        Node::Map(v) => walk_schema(v, &format!("{at}.*"), out, depth + 1),
        Node::List(el) => walk_schema(el, &format!("{at}[]"), out, depth + 1),
        Node::Record(r) => {
            for f in r.fields {
                let p = join(at, f.key);
                out.insert(p.clone());
                walk_schema(&f.node, &p, out, depth + 1);
            }
        }
        Node::Union(u) => {
            for v in u.variants {
                let p = format!("{at}<{}={}>", u.tag, v.tag);
                out.insert(p.clone());
                walk_schema(&Node::Record(v.record), &p, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn join(at: &str, k: &str) -> String {
    if at.is_empty() {
        k.to_owned()
    } else {
        format!("{at}.{k}")
    }
}

/// The coverage keys (see [`schema_paths`]) that `doc` actually exercises.
#[must_use]
pub fn covered_paths(doc: &ConfigDoc, node: &Node) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk_doc(doc.root(), node, "", &mut out);
    out
}

fn walk_doc(it: &Item, node: &Node, at: &str, out: &mut BTreeSet<String>) {
    match *node {
        Node::Map(v) => {
            if let Some(t) = it.as_table_like() {
                for (_, c) in t.iter() {
                    walk_doc(c, v, &format!("{at}.*"), out);
                }
            }
        }
        Node::List(el) => {
            for c in list_elems(it).unwrap_or_default() {
                walk_doc(c, el, &format!("{at}[]"), out);
            }
        }
        Node::Record(r) => {
            if let Some(t) = it.as_table_like() {
                for f in r.fields {
                    if let Some(c) = t.get(f.key).filter(|i| !i.is_none()) {
                        let p = join(at, f.key);
                        out.insert(p.clone());
                        walk_doc(c, &f.node, &p, out);
                    }
                }
            }
        }
        Node::Union(u) => {
            if let Some(v) = it
                .as_table_like()
                .and_then(|t| t.get(u.tag))
                .and_then(Item::as_str)
                .and_then(|tag| u.variant(tag))
            {
                let p = format!("{at}<{}={}>", u.tag, v.tag);
                out.insert(p.clone());
                walk_doc(it, &Node::Record(v.record), &p, out);
            }
        }
        _ => {}
    }
}
