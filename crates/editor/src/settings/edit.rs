//! Document edits ([`DocEdit`], [`ScalarValue`]) and [`Settings::edit`], which keeps the form
//! UI state attached to the items an edit moves.

use cv_schema::{ConfigDoc, DocError, Node, Seg};

use super::{ConfigFile, Settings};

impl Settings {
    /// Applies a document edit to `file` (an error becomes its notice) and keeps the form UI
    /// state attached to the items it names.
    pub fn edit(&mut self, file: &str, edit: &DocEdit) -> Result<(), String> {
        let Some(f) = self.files.get_mut(file) else {
            return Err(format!("{file}: no such settings file"));
        };
        // WP-R3 (review E9): the display order is snapshotted here, before the edit, so the
        // render pass never has to write it.
        let snapshots = sorted_snapshots(f, file, edit);
        if let Err(e) = f.edit(|doc| edit.apply(doc)) {
            f.set_notice(Some(e.clone()));
            return Err(e);
        }
        let fs = &mut self.form;
        let idx = |s: &Seg| match s {
            Seg::Index(i) => Some(*i),
            Seg::Key(_) => None,
        };
        match edit {
            DocEdit::Set(..) | DocEdit::SetMany(_) => {
                for (list, order) in snapshots {
                    fs.pin_order(file, &list, order);
                }
            }
            DocEdit::Remove(p) => {
                fs.clear_drafts(file);
                if let Some((Seg::Index(gone), parent)) = p.0.split_last() {
                    let parent = cv_schema::Path(parent.to_vec());
                    let gone = *gone;
                    fs.remap(file, &parent, |s| match idx(s) {
                        Some(i) if i == gone => None,
                        Some(i) if i > gone => Some(Seg::Index(i - 1)),
                        _ => Some(s.clone()),
                    });
                } else {
                    let p = p.clone();
                    fs.expanded.retain(|(fl, q)| {
                        fl != file || q.0.len() < p.0.len() || q.0[..p.0.len()] != p.0[..]
                    });
                }
            }
            DocEdit::InsertRecord(list, at, _) => {
                fs.clear_drafts(file);
                let at = *at;
                fs.remap(file, list, |s| match idx(s) {
                    Some(i) if i >= at => Some(Seg::Index(i + 1)),
                    _ => Some(s.clone()),
                });
                fs.expanded.insert((file.to_owned(), list.index(at)));
            }
            DocEdit::Move(list, from, to) => {
                fs.clear_drafts(file);
                let (from, to) = (*from, *to);
                fs.remap(file, list, |s| match idx(s) {
                    Some(i) if i == from => Some(Seg::Index(to)),
                    Some(i) if from < to && i > from && i <= to => Some(Seg::Index(i - 1)),
                    Some(i) if to < from && i >= to && i < from => Some(Seg::Index(i + 1)),
                    _ => Some(s.clone()),
                });
            }
            DocEdit::InsertKey(map, key, _) => {
                fs.clear_drafts(file);
                fs.expanded.insert((file.to_owned(), map.key(key)));
            }
            DocEdit::RenameKey(map, from, to) => {
                fs.clear_drafts(file);
                let (from, to) = (from.clone(), to.clone());
                fs.remap(file, map, |s| match s {
                    Seg::Key(k) if *k == from => Some(Seg::Key(to.clone())),
                    _ => Some(s.clone()),
                });
            }
            DocEdit::SwitchVariant(..) => fs.clear_drafts(file),
        }
        Ok(())
    }

    /// The stored text value at `(file, path)` (`""` when absent).
    pub(super) fn stored_str(&self, file: &str, path: &cv_schema::Path) -> String {
        self.file(file)
            .and_then(ConfigFile::doc)
            .and_then(|d| d.get_str(path))
            .unwrap_or_default()
            .to_owned()
    }

    pub(super) fn str_list(&self, file: &str, path: &cv_schema::Path) -> Vec<String> {
        self.file(file)
            .and_then(ConfigFile::doc)
            .and_then(|d| d.get_str_list(path))
            .unwrap_or_default()
    }

    /// Whether `(file, path)` holds a value.
    pub(super) fn present(&self, file: &str, path: &cv_schema::Path) -> bool {
        self.file(file)
            .and_then(ConfigFile::doc)
            .is_some_and(|d| d.get(path).is_some())
    }

    /// A text field's edit: empty + optional + present removes the key, else sets it.
    pub(super) fn text_edit(
        &self,
        file: &str,
        path: cv_schema::Path,
        text: String,
        required: bool,
    ) -> DocEdit {
        if text.is_empty() && !required && self.present(file, &path) {
            DocEdit::Remove(path)
        } else {
            DocEdit::Set(path, ScalarValue::Str(text))
        }
    }
}

/// A generic document edit (WP-3 grows the form vocabulary on top of these).
#[derive(Debug, Clone)]
pub enum DocEdit {
    /// Set a scalar at a schema path.
    Set(cv_schema::Path, ScalarValue),
    /// Set several scalars as one edit (one undo / dirty step; plan round-5 WP-6 weather pick).
    SetMany(Vec<(cv_schema::Path, ScalarValue)>),
    /// Remove the item at a path.
    Remove(cv_schema::Path),
    /// Insert a table record into a list at an index, with initial scalar fields (a union's
    /// tag among them).
    InsertRecord(cv_schema::Path, usize, Vec<(String, ScalarValue)>),
    /// Move a list item `from` → `to`.
    Move(cv_schema::Path, usize, usize),
    /// Add a map key; its value starts as the entry node's `empty_item`.
    InsertKey(cv_schema::Path, String, &'static Node),
    /// Rename a map key (position, value and comments kept).
    RenameKey(cv_schema::Path, String, String),
    /// Switch a tagged union's kind at a path: keys the old kind declares and the new one does
    /// not are dropped; shared and unknown keys stay.
    SwitchVariant(cv_schema::Path, &'static cv_schema::Union, String),
}

/// A scalar value for [`DocEdit::Set`].
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarValue {
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    StrList(Vec<String>),
}

impl ScalarValue {
    fn value(&self) -> cv_schema::toml_edit::Value {
        use cv_schema::toml_edit::{Array, Value};
        match self {
            ScalarValue::Str(s) => s.as_str().into(),
            ScalarValue::Bool(b) => (*b).into(),
            ScalarValue::Int(i) => (*i).into(),
            ScalarValue::Float(f) => (*f).into(),
            ScalarValue::StrList(l) => {
                let mut a = Array::new();
                for s in l {
                    a.push(s.as_str());
                }
                Value::Array(a)
            }
        }
    }
}

impl DocEdit {
    /// Applies the edit to `doc`.
    pub fn apply(&self, doc: &mut ConfigDoc) -> Result<(), DocError> {
        match self {
            DocEdit::Set(p, v) => doc.set(p, v.value()),
            DocEdit::SetMany(kv) => kv.iter().try_for_each(|(p, v)| doc.set(p, v.value())),
            DocEdit::Remove(p) => doc.remove(p).map(|_| ()),
            DocEdit::InsertRecord(list, at, fields) => {
                let mut t = cv_schema::toml_edit::Table::new();
                for (k, v) in fields {
                    t.insert(k, cv_schema::toml_edit::Item::Value(v.value()));
                }
                doc.insert_record(list, *at, t)
            }
            DocEdit::Move(list, from, to) => doc.move_item(list, *from, *to),
            DocEdit::InsertKey(map, key, node) => doc.insert_key(map, key, node.empty_item()),
            DocEdit::RenameKey(map, from, to) => doc.rename_key(map, from, to),
            DocEdit::SwitchVariant(p, u, tag) => doc.switch_variant(p, u, tag),
        }
    }
}

/// The shown order of every display-sorted list that holds an item `edit` sets (round-13 WP-1):
/// pinned when the edit applies, so the edited row stays where the user is typing until the
/// edit ends.
fn sorted_snapshots(
    f: &ConfigFile,
    file: &str,
    edit: &DocEdit,
) -> Vec<(cv_schema::Path, Vec<usize>)> {
    let paths: Vec<&cv_schema::Path> = match edit {
        DocEdit::Set(p, _) => vec![p],
        DocEdit::SetMany(items) => items.iter().map(|(p, _)| p).collect(),
        _ => return Vec::new(),
    };
    let (Some(doc), Some(root)) = (f.doc(), f.schema()) else {
        return Vec::new();
    };
    let mut out: Vec<(cv_schema::Path, Vec<usize>)> = Vec::new();
    for p in paths {
        // Every list the edited path passes through (`provider[1].models[0].id` → `provider`,
        // `provider[1].models`), nested lists included.
        for (n, seg) in p.0.iter().enumerate() {
            if !matches!(seg, Seg::Index(_)) || n == 0 {
                continue;
            }
            let list = cv_schema::Path(p.0[..n].to_vec());
            if !super::form::sorted_list(file, &list) || out.iter().any(|(l, _)| *l == list) {
                continue;
            }
            if let Some(Node::List(inner)) = node_at(root, &list) {
                out.push((list.clone(), super::form::current_order(doc, &list, inner)));
            }
        }
    }
    out
}

/// The schema node at `path` under `root` (records by field, lists by item).
fn node_at<'a>(root: &'a Node, path: &cv_schema::Path) -> Option<&'a Node> {
    let mut node = root;
    for seg in &path.0 {
        node = match (node, seg) {
            (Node::Record(r), Seg::Key(k)) => &r.field(k)?.node,
            (Node::List(inner), Seg::Index(_)) => inner,
            _ => return None,
        };
    }
    Some(node)
}
