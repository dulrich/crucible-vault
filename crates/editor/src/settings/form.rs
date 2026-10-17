//! The schema-driven form model (plan `editor-settings-tab` WP-3): a pure walk from a
//! `cv_schema` [`Node`] and a [`ConfigDoc`] to a flat list of [`Row`]s, which
//! [`super::widgets::render`] turns into iced elements.
//!
//! Flat rows (with a nesting `depth`) keep the page one cheap column; record lists and
//! compound map entries are collapsed to a summary row by default ([`FormState::expanded`]),
//! the main lever for the Actions page's open time. Rows are plain data, so tests inspect what a
//! page shows without a renderer.
//!
//! Edits go through [`DocEdit`](super::DocEdit) on the file's [`ConfigDoc`] (comments, key order
//! and unknown keys survive); typing-level UI state (number drafts, chip/new-key/rename drafts,
//! the path field showing suggestions) lives in [`FormState`] and never reaches the file.

use std::collections::{BTreeMap, BTreeSet};

use cv_schema::toml_edit::{Item, Value};
use cv_schema::{ConfigDoc, Default as Dflt, FieldError, Node, Path, Record, Seg, Union};

pub use super::pickers::Picker;
use crate::widget::typeahead::Typeahead;

/// Keys a summary row reads, in preference order.
const SUMMARY_KEYS: [&str; 10] = [
    "channel", "id", "name", "label", "key", "action", "agent", "path", "folder", "file",
];

/// Transient form UI state (never written to a file). Keyed by vault path of the file.
#[derive(Debug, Default, Clone)]
pub struct FormState {
    /// Expanded list items / map entries.
    pub expanded: BTreeSet<(String, Path)>,
    /// Typing drafts, by `(file, draft key)` (see [`draft_key`]).
    pub drafts: BTreeMap<(String, String), String>,
    /// The field whose typeahead is active (plan round-5 WP-4).
    pub typeahead: Option<ActiveTa>,
}

/// The active typeahead field: its file, path, picker, typed text and widget state.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveTa {
    pub file: String,
    pub path: Path,
    pub picker: Picker,
    /// A chips field (the typed text is the chip draft; commits add a chip).
    pub chips: bool,
    pub required: bool,
    /// The typed text (text fields; chips use their chip draft).
    pub draft: String,
    pub ta: Typeahead,
}

impl ActiveTa {
    /// Whether this is the field at `(file, path)`.
    pub fn is(&self, file: &str, path: &Path) -> bool {
        self.file == file && self.path == *path
    }
}

/// The draft key of a widget kind at a path (`num:action[0].x`).
pub fn draft_key(kind: &str, path: &Path) -> String {
    format!("{kind}:{path}")
}

impl FormState {
    /// Whether the item at `path` is expanded.
    pub fn is_expanded(&self, file: &str, path: &Path) -> bool {
        self.expanded.contains(&(file.to_owned(), path.clone()))
    }

    /// Expands or collapses the item at `path`.
    pub fn toggle(&mut self, file: &str, path: &Path) {
        let k = (file.to_owned(), path.clone());
        if !self.expanded.remove(&k) {
            self.expanded.insert(k);
        }
    }

    /// A draft.
    pub fn draft(&self, file: &str, key: &str) -> Option<&str> {
        self.drafts
            .get(&(file.to_owned(), key.to_owned()))
            .map(String::as_str)
    }

    /// Sets (or, when `None`, clears) a draft.
    pub fn set_draft(&mut self, file: &str, key: String, v: Option<String>) {
        let k = (file.to_owned(), key);
        match v {
            Some(v) => {
                self.drafts.insert(k, v);
            }
            None => {
                self.drafts.remove(&k);
            }
        }
    }

    /// Drops every draft of `file` (a reload, or a structural edit that shifts paths).
    pub fn clear_drafts(&mut self, file: &str) {
        self.drafts.retain(|(f, _), _| f != file);
        if self.typeahead.as_ref().is_some_and(|a| a.file == file) {
            self.typeahead = None;
        }
    }

    /// Drops all UI state of `file`.
    pub fn clear_file(&mut self, file: &str) {
        self.clear_drafts(file);
        self.expanded.retain(|(f, _)| f != file);
    }

    /// Rewrites the segment right below `prefix` in every expanded path of `file` (`None`
    /// drops the entry) — keeps expansion attached to its item across insert / remove / move /
    /// rename.
    pub fn remap(&mut self, file: &str, prefix: &Path, f: impl Fn(&Seg) -> Option<Seg>) {
        let n = prefix.0.len();
        let old = std::mem::take(&mut self.expanded);
        for (fl, p) in old {
            if fl != file || p.0.len() <= n || p.0[..n] != prefix.0[..] {
                self.expanded.insert((fl, p));
                continue;
            }
            if let Some(s) = f(&p.0[n]) {
                let mut q = p.clone();
                q.0[n] = s;
                self.expanded.insert((fl, q));
            }
        }
    }
}

/// A new map entry's initial value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewValue {
    Table,
    Bool,
    Int,
    Str,
    List,
}

impl NewValue {
    /// The initial value for a map whose entries are `node`.
    pub fn for_node(node: &Node) -> Self {
        match node {
            Node::Bool => Self::Bool,
            Node::Int { .. } | Node::Float { .. } => Self::Int,
            Node::StrList { .. } | Node::Chords | Node::List(_) => Self::List,
            Node::Str | Node::Path | Node::Folder | Node::Expr | Node::Enum(_) | Node::Filter => {
                Self::Str
            }
            Node::FreeTable | Node::Map(_) | Node::Record(_) | Node::Union(_) => Self::Table,
        }
    }

    /// The TOML item.
    pub fn item(self) -> Item {
        use cv_schema::toml_edit::{Array, Table};
        match self {
            Self::Table => Item::Table(Table::new()),
            Self::Bool => Item::Value(false.into()),
            Self::Int => Item::Value(0.into()),
            Self::Str => Item::Value("".into()),
            Self::List => Item::Value(Value::Array(Array::new())),
        }
    }
}

/// What a row shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A section heading (nested record / list / map).
    Heading,
    /// A single-line text field. `picker`: a typeahead source (WP-4); `mono`:
    /// expression (monospace, never evaluated).
    Text {
        value: String,
        placeholder: String,
        mono: bool,
        picker: Option<Picker>,
        required: bool,
    },
    Toggle {
        value: bool,
    },
    /// A number field; `text` is the draft or the TOML value.
    Number {
        text: String,
        placeholder: String,
        float: bool,
    },
    /// A closed choice: seg-group when ≤4 choices, else a pick list.
    Enum {
        choices: &'static [&'static str],
        value: Option<String>,
        required: bool,
    },
    /// String-list chips (add/remove); `choices` non-empty → pick from them.
    Chips {
        items: Vec<String>,
        choices: &'static [&'static str],
        draft: String,
        /// A typeahead source for the adder (WP-4; chips then show picker labels).
        picker: Option<Picker>,
    },
    /// A value the form does not render: its TOML text, read-only.
    ReadOnly {
        toml: String,
    },
    /// One list item's summary row.
    Item {
        list: Path,
        index: usize,
        len: usize,
        summary: String,
        expanded: bool,
    },
    /// "Add" under a list (`union`: pick the new item's kind).
    AddItem {
        list: Path,
        len: usize,
        union: Option<&'static Union>,
    },
    /// A tagged union's kind picker.
    Variant {
        union: &'static Union,
        tag: Option<String>,
    },
    /// One map entry's key row (rename / remove; `collapsible` for compound values).
    MapKey {
        map: Path,
        key: String,
        draft: String,
        collapsible: bool,
        expanded: bool,
    },
    /// "Add key" under a map.
    AddKey {
        map: Path,
        draft: String,
        value: NewValue,
    },
}

/// One form row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub depth: u16,
    /// The document path the row edits.
    pub path: Path,
    pub label: String,
    pub help: &'static str,
    pub kind: Kind,
    /// Inline errors at this path.
    pub errors: Vec<String>,
}

/// Walk context.
pub struct Ctx<'a> {
    pub file: &'a str,
    pub doc: &'a ConfigDoc,
    pub errors: &'a [FieldError],
    pub form: &'a FormState,
}

/// The rows of a whole file under `schema`.
pub fn rows(cx: &Ctx<'_>, schema: &'static Node) -> Vec<Row> {
    let mut out = Vec::new();
    match schema {
        Node::Record(r) => record_rows(cx, &Path::root(), r, None, 0, &mut out),
        Node::Map(inner) => map_rows(cx, &Path::root(), inner, 0, &mut out),
        n => node_rows(cx, &Path::root(), n, &Leaf::bare("File"), 0, &mut out),
    }
    out
}

/// The rows of the field `key` of the root record only (a page section).
pub fn field_rows(cx: &Ctx<'_>, schema: &'static Node, key: &str) -> Vec<Row> {
    let mut out = Vec::new();
    if let Node::Record(r) = schema
        && let Some(f) = r.field(key)
    {
        let leaf = Leaf {
            label: f.label,
            help: f.help,
            default: f.default,
            required: f.required,
        };
        node_rows(cx, &Path::root().key(key), &f.node, &leaf, 0, &mut out);
    }
    out
}

struct Leaf {
    label: &'static str,
    help: &'static str,
    default: Option<Dflt>,
    required: bool,
}

impl Leaf {
    fn bare(label: &'static str) -> Self {
        Self {
            label,
            help: "",
            default: None,
            required: false,
        }
    }
}

fn errors_at(cx: &Ctx<'_>, p: &Path) -> Vec<String> {
    cx.errors
        .iter()
        .filter(|e| &e.path == p)
        .map(|e| e.message.clone())
        .collect()
}

fn row(cx: &Ctx<'_>, depth: u16, p: &Path, label: String, help: &'static str, kind: Kind) -> Row {
    Row {
        depth,
        path: p.clone(),
        label,
        help,
        kind,
        errors: errors_at(cx, p),
    }
}

/// The TOML text of an item, trimmed (read-only display).
pub fn toml_text(item: &Item) -> String {
    match item {
        Item::Value(v) => {
            let mut v = v.clone();
            v.decor_mut().clear();
            v.to_string().trim().to_owned()
        }
        Item::Table(t) => {
            let s = t.to_string();
            let s = s.trim();
            if s.is_empty() {
                "{}".to_owned()
            } else {
                s.to_owned()
            }
        }
        Item::ArrayOfTables(a) => {
            let parts: Vec<String> = a.iter().map(|t| t.to_string().trim().to_owned()).collect();
            parts.join("\n---\n")
        }
        Item::None => String::new(),
    }
}

fn read_only(cx: &Ctx<'_>, p: &Path, leaf: &Leaf, depth: u16, out: &mut Vec<Row>) {
    let toml = cx
        .doc
        .get(p)
        .map_or_else(|| "(not set)".to_owned(), toml_text);
    out.push(row(
        cx,
        depth,
        p,
        leaf.label.to_owned(),
        leaf.help,
        Kind::ReadOnly { toml },
    ));
}

fn dflt_text(d: Option<Dflt>) -> String {
    match d {
        Some(Dflt::Str(s)) => s.to_owned(),
        Some(Dflt::Int(i)) => i.to_string(),
        Some(Dflt::Float(f)) => f.to_string(),
        Some(Dflt::Bool(b)) => b.to_string(),
        Some(Dflt::StrList(l)) => l.join(", "),
        None => String::new(),
    }
}

fn is_table(item: &Item) -> bool {
    item.is_table_like()
}

fn is_list(item: &Item) -> bool {
    matches!(item, Item::ArrayOfTables(_) | Item::Value(Value::Array(_)))
}

/// Rows for one value of shape `node` at `p`.
fn node_rows(
    cx: &Ctx<'_>,
    p: &Path,
    node: &'static Node,
    leaf: &Leaf,
    depth: u16,
    out: &mut Vec<Row>,
) {
    let item = cx.doc.get(p);
    let label = || {
        if leaf.required {
            format!("{} *", leaf.label)
        } else {
            leaf.label.to_owned()
        }
    };
    match node {
        Node::Record(r) => {
            if item.is_some_and(|i| !is_table(i)) {
                return read_only(cx, p, leaf, depth, out);
            }
            out.push(row(cx, depth, p, label(), leaf.help, Kind::Heading));
            record_rows(cx, p, r, None, depth + 1, out);
        }
        Node::Union(u) => {
            if item.is_some_and(|i| !is_table(i)) {
                return read_only(cx, p, leaf, depth, out);
            }
            out.push(row(cx, depth, p, label(), leaf.help, Kind::Heading));
            union_rows(cx, p, u, depth + 1, out);
        }
        Node::List(inner) => {
            if item.is_some_and(|i| !is_list(i)) {
                return read_only(cx, p, leaf, depth, out);
            }
            out.push(row(cx, depth, p, label(), leaf.help, Kind::Heading));
            list_rows(cx, p, inner, depth + 1, out);
        }
        Node::Map(inner) => {
            if item.is_some_and(|i| !is_table(i)) {
                return read_only(cx, p, leaf, depth, out);
            }
            out.push(row(cx, depth, p, label(), leaf.help, Kind::Heading));
            map_rows(cx, p, inner, depth + 1, out);
        }
        Node::FreeTable => read_only(cx, p, leaf, depth, out),
        Node::Str | Node::Path | Node::Folder | Node::Expr | Node::Filter => {
            let value = match item {
                None => String::new(),
                Some(i) => match i.as_str() {
                    Some(s) => s.to_owned(),
                    None => return read_only(cx, p, leaf, depth, out),
                },
            };
            out.push(row(
                cx,
                depth,
                p,
                label(),
                leaf.help,
                Kind::Text {
                    value,
                    placeholder: dflt_text(leaf.default),
                    mono: matches!(node, Node::Expr | Node::Filter),
                    picker: super::pickers::for_field(cx.file, p, node),
                    required: leaf.required,
                },
            ));
        }
        Node::Bool => {
            let value = match item {
                None => matches!(leaf.default, Some(Dflt::Bool(true))),
                Some(i) => match i.as_bool() {
                    Some(b) => b,
                    None => return read_only(cx, p, leaf, depth, out),
                },
            };
            out.push(row(
                cx,
                depth,
                p,
                label(),
                leaf.help,
                Kind::Toggle { value },
            ));
        }
        Node::Int { .. } | Node::Float { .. } => {
            let float = matches!(node, Node::Float { .. });
            let draft = cx.form.draft(cx.file, &draft_key("num", p));
            let text = match (draft, item) {
                (Some(d), _) => d.to_owned(),
                (None, None) => String::new(),
                (None, Some(i)) => {
                    let ok = i.as_integer().is_some() || (float && i.as_float().is_some());
                    if !ok {
                        return read_only(cx, p, leaf, depth, out);
                    }
                    toml_text(i)
                }
            };
            let mut r = row(
                cx,
                depth,
                p,
                label(),
                leaf.help,
                Kind::Number {
                    placeholder: dflt_text(leaf.default),
                    float,
                    text: text.clone(),
                },
            );
            if draft.is_some() && !text.trim().is_empty() && parse_number(&text, float).is_none() {
                r.errors.push("not a number (not applied)".into());
            }
            out.push(r);
        }
        Node::Enum(choices) => {
            let value = match item {
                None => None,
                Some(i) => match i.as_str() {
                    Some(s) => Some(s.to_owned()),
                    None => return read_only(cx, p, leaf, depth, out),
                },
            };
            out.push(row(
                cx,
                depth,
                p,
                label(),
                leaf.help,
                Kind::Enum {
                    choices,
                    value,
                    required: leaf.required,
                },
            ));
        }
        Node::StrList { .. } | Node::Chords => {
            let items = match item {
                None => Vec::new(),
                Some(_) => match cx.doc.get_str_list(p) {
                    Some(l) => l,
                    None => return read_only(cx, p, leaf, depth, out),
                },
            };
            let choices = match node {
                Node::StrList { choices, .. } => *choices,
                _ => &[],
            };
            let draft = cx
                .form
                .draft(cx.file, &draft_key("chip", p))
                .unwrap_or_default()
                .to_owned();
            out.push(row(
                cx,
                depth,
                p,
                label(),
                leaf.help,
                Kind::Chips {
                    items,
                    choices,
                    draft,
                    picker: super::pickers::for_field(cx.file, p, node),
                },
            ));
        }
    }
}

/// A number typed into a field: an integer when it parses as one (a float field accepts int
/// input), else a float when `float`.
pub fn parse_number(s: &str, float: bool) -> Option<Value> {
    let s = s.trim();
    if let Ok(i) = s.parse::<i64>() {
        return Some(i.into());
    }
    if float && let Ok(f) = s.parse::<f64>() {
        return f.is_finite().then(|| f.into());
    }
    None
}

/// The fields of record `r` at `p` (`skip`: a union tag key), then any keys the schema does not
/// know, read-only.
fn record_rows(
    cx: &Ctx<'_>,
    p: &Path,
    r: &'static Record,
    skip: Option<&str>,
    depth: u16,
    out: &mut Vec<Row>,
) {
    for f in r.fields {
        let leaf = Leaf {
            label: f.label,
            help: f.help,
            default: f.default,
            required: f.required,
        };
        node_rows(cx, &p.key(f.key), &f.node, &leaf, depth, out);
    }
    for k in cx.doc.keys(p) {
        if r.field(&k).is_some() || skip == Some(k.as_str()) {
            continue;
        }
        let kp = p.key(&k);
        let toml = cx.doc.get(&kp).map(toml_text).unwrap_or_default();
        out.push(row(
            cx,
            depth,
            &kp,
            k,
            "not in the schema; kept as is",
            Kind::ReadOnly { toml },
        ));
    }
}

fn union_rows(cx: &Ctx<'_>, p: &Path, u: &'static Union, depth: u16, out: &mut Vec<Row>) {
    let tag = cx.doc.get_str(&p.key(u.tag)).map(str::to_owned);
    out.push(row(
        cx,
        depth,
        &p.key(u.tag),
        "Kind *".to_owned(),
        "Switching keeps the fields both kinds declare",
        Kind::Variant {
            union: u,
            tag: tag.clone(),
        },
    ));
    match tag.as_deref().and_then(|t| u.variant(t)) {
        Some(v) => record_rows(cx, p, v.record, Some(u.tag), depth, out),
        None => {
            // Unknown or missing kind: every other key is shown read-only.
            for k in cx.doc.keys(p) {
                if k == u.tag {
                    continue;
                }
                let kp = p.key(&k);
                let toml = cx.doc.get(&kp).map(toml_text).unwrap_or_default();
                out.push(row(cx, depth, &kp, k, "", Kind::ReadOnly { toml }));
            }
        }
    }
}

/// The summary text of a list item / map entry.
pub fn summary(doc: &ConfigDoc, p: &Path, node: &Node, index: usize) -> String {
    let mut parts = Vec::new();
    if let Node::Union(u) = node
        && let Some(t) = doc.get_str(&p.key(u.tag))
    {
        parts.push(u.variant(t).map_or(t, |v| v.label).to_owned());
    }
    for k in SUMMARY_KEYS {
        if let Some(s) = doc.get_str(&p.key(k))
            && !s.is_empty()
        {
            parts.push(s.to_owned());
            break;
        }
    }
    if parts.is_empty() {
        if let Some(i) = doc.get(p)
            && !i.is_table_like()
        {
            return toml_text(i);
        }
        parts.push(format!("#{}", index + 1));
    }
    parts.join(" · ")
}

fn children(
    cx: &Ctx<'_>,
    p: &Path,
    node: &'static Node,
    label: &'static str,
    depth: u16,
    out: &mut Vec<Row>,
) {
    match node {
        Node::Record(r) => {
            if cx.doc.get(p).is_some_and(|i| !is_table(i)) {
                return read_only(cx, p, &Leaf::bare(label), depth, out);
            }
            record_rows(cx, p, r, None, depth, out);
        }
        Node::Union(u) => {
            if cx.doc.get(p).is_some_and(|i| !is_table(i)) {
                return read_only(cx, p, &Leaf::bare(label), depth, out);
            }
            union_rows(cx, p, u, depth, out);
        }
        n => node_rows(cx, p, n, &Leaf::bare(label), depth, out),
    }
}

fn compound(node: &Node) -> bool {
    matches!(
        node,
        Node::Record(_) | Node::Union(_) | Node::List(_) | Node::Map(_)
    )
}

fn list_rows(cx: &Ctx<'_>, p: &Path, inner: &'static Node, depth: u16, out: &mut Vec<Row>) {
    let len = cx.doc.len(p);
    for i in 0..len {
        let ip = p.index(i);
        let expanded = cx.form.is_expanded(cx.file, &ip);
        out.push(row(
            cx,
            depth,
            &ip,
            String::new(),
            "",
            Kind::Item {
                list: p.clone(),
                index: i,
                len,
                summary: summary(cx.doc, &ip, inner, i),
                expanded,
            },
        ));
        if expanded {
            children(cx, &ip, inner, "Item", depth + 1, out);
        }
    }
    let union = match inner {
        Node::Union(u) => Some(*u),
        _ => None,
    };
    out.push(row(
        cx,
        depth,
        p,
        String::new(),
        "",
        Kind::AddItem {
            list: p.clone(),
            len,
            union,
        },
    ));
}

fn map_rows(cx: &Ctx<'_>, p: &Path, inner: &'static Node, depth: u16, out: &mut Vec<Row>) {
    let collapsible = compound(inner);
    for k in cx.doc.keys(p) {
        let kp = p.key(&k);
        let expanded = !collapsible || cx.form.is_expanded(cx.file, &kp);
        let draft = cx
            .form
            .draft(cx.file, &draft_key("rename", &kp))
            .unwrap_or(&k)
            .to_owned();
        out.push(row(
            cx,
            depth,
            &kp,
            String::new(),
            "",
            Kind::MapKey {
                map: p.clone(),
                key: k.clone(),
                draft,
                collapsible,
                expanded,
            },
        ));
        if expanded {
            children(cx, &kp, inner, "Value", depth + 1, out);
        }
    }
    let draft = cx
        .form
        .draft(cx.file, &draft_key("newkey", p))
        .unwrap_or_default()
        .to_owned();
    out.push(row(
        cx,
        depth,
        p,
        String::new(),
        "",
        Kind::AddKey {
            map: p.clone(),
            draft,
            value: NewValue::for_node(inner),
        },
    ));
}

/// Every display string of `rows` (labels, values, summaries, read-only TOML) — what a page
/// shows, for tests.
pub fn shown_text(rows: &[Row]) -> Vec<String> {
    let mut v = Vec::new();
    for r in rows {
        v.push(r.label.clone());
        match &r.kind {
            Kind::Text { value, .. } => v.push(value.clone()),
            Kind::Number { text, .. } => v.push(text.clone()),
            Kind::Enum { value, .. } => v.extend(value.clone()),
            Kind::Chips { items, .. } => v.extend(items.iter().cloned()),
            Kind::ReadOnly { toml } => v.push(toml.clone()),
            Kind::Item { summary, .. } => v.push(summary.clone()),
            Kind::Variant { tag, .. } => v.extend(tag.clone()),
            Kind::MapKey { key, .. } => v.push(key.clone()),
            Kind::Heading | Kind::Toggle { .. } | Kind::AddItem { .. } | Kind::AddKey { .. } => {}
        }
    }
    v
}
