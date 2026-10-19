//! [`ConfigDoc`]: a comment- and order-preserving TOML document with typed, path-addressed edits.

use std::fmt;

use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Key, Table, Value};

/// One step of a [`Path`]: a table key or a list index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// A path into a document (`action[0].steps[1].type`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Path(pub Vec<Seg>);

impl Path {
    /// The root.
    #[must_use]
    pub fn root() -> Self {
        Self::default()
    }

    /// Parses `a.b[0]."quoted.key"[2]` (keys bare or double-quoted; `\"` and `\\` escapes).
    ///
    /// # Errors
    /// A malformed path.
    pub fn parse(s: &str) -> Result<Self, DocError> {
        let bad = || DocError::BadPath(s.to_owned());
        let mut out = Vec::new();
        let mut chars = s.chars().peekable();
        let mut expect_key = true;
        while let Some(&c) = chars.peek() {
            match c {
                '[' => {
                    chars.next();
                    let mut n = String::new();
                    for d in chars.by_ref() {
                        if d == ']' {
                            break;
                        }
                        n.push(d);
                    }
                    out.push(Seg::Index(n.parse().map_err(|_| bad())?));
                    expect_key = false;
                }
                '.' if !expect_key => {
                    chars.next();
                    expect_key = true;
                }
                '"' if expect_key => {
                    chars.next();
                    let mut k = String::new();
                    let mut closed = false;
                    while let Some(d) = chars.next() {
                        match d {
                            '\\' => k.push(chars.next().ok_or_else(bad)?),
                            '"' => {
                                closed = true;
                                break;
                            }
                            _ => k.push(d),
                        }
                    }
                    if !closed {
                        return Err(bad());
                    }
                    out.push(Seg::Key(k));
                    expect_key = false;
                }
                _ if expect_key => {
                    let mut k = String::new();
                    while let Some(&d) = chars.peek() {
                        if d == '.' || d == '[' {
                            break;
                        }
                        k.push(d);
                        chars.next();
                    }
                    if k.is_empty() {
                        return Err(bad());
                    }
                    out.push(Seg::Key(k));
                    expect_key = false;
                }
                _ => return Err(bad()),
            }
        }
        if expect_key && !out.is_empty() {
            return Err(bad());
        }
        Ok(Self(out))
    }

    /// This path plus a key.
    #[must_use]
    pub fn key(&self, k: &str) -> Self {
        let mut p = self.clone();
        p.0.push(Seg::Key(k.to_owned()));
        p
    }

    /// This path plus an index.
    #[must_use]
    pub fn index(&self, i: usize) -> Self {
        let mut p = self.clone();
        p.0.push(Seg::Index(i));
        p
    }

    fn split_last(&self) -> Option<(&[Seg], &Seg)> {
        self.0.split_last().map(|(l, r)| (r, l))
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.0.iter().enumerate() {
            match s {
                Seg::Index(n) => write!(f, "[{n}]")?,
                Seg::Key(k) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    let bare = !k.is_empty()
                        && k.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                    if bare {
                        f.write_str(k)?;
                    } else {
                        write!(f, "\"{}\"", k.replace('\\', "\\\\").replace('"', "\\\""))?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// A failed document operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocError {
    /// The text is not valid TOML.
    Parse(String),
    /// A malformed path string.
    BadPath(String),
    /// Nothing at the path (or a parent is missing).
    NotFound(String),
    /// The node at the path has the wrong shape for the operation.
    NotContainer(String),
    /// The key already exists (insert / rename target).
    Exists(String),
    /// A list index is out of range.
    OutOfRange(String),
}

impl fmt::Display for DocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocError::Parse(m) => write!(f, "parse error: {m}"),
            DocError::BadPath(p) => write!(f, "bad path {p:?}"),
            DocError::NotFound(p) => write!(f, "nothing at {p}"),
            DocError::NotContainer(p) => write!(f, "{p} is not a table or list"),
            DocError::Exists(p) => write!(f, "{p} already exists"),
            DocError::OutOfRange(p) => write!(f, "{p}: index out of range"),
        }
    }
}

impl std::error::Error for DocError {}

/// A TOML config file held as a `toml_edit` document.
///
/// Every edit keeps comments, whitespace, key order and keys the schema does not know. Two
/// `toml_edit` limits apply (`toml_edit` 0.25.17 crate-level docs):
/// - the relative order of **dotted keys** (`a.b = 1` mixed with other keys) is not preserved
///   exactly when such a table is re-rendered;
/// - a file **missing its trailing newline** may gain one.
///
/// Reordering an array-of-tables reassigns the moved entries' document positions within the
/// span the list already occupied, so the rendered order follows the list order and the
/// comments above each `[[header]]` move with their entry.
#[derive(Debug, Clone)]
pub struct ConfigDoc {
    doc: DocumentMut,
}

impl ConfigDoc {
    /// Parses `text`.
    ///
    /// # Errors
    /// [`DocError::Parse`] when `text` is not TOML.
    pub fn parse(text: &str) -> Result<Self, DocError> {
        text.parse::<DocumentMut>()
            .map(|doc| Self { doc })
            .map_err(|e| DocError::Parse(e.to_string()))
    }

    /// An empty document.
    #[must_use]
    pub fn new() -> Self {
        Self {
            doc: DocumentMut::new(),
        }
    }

    /// The underlying document.
    #[must_use]
    pub fn document(&self) -> &DocumentMut {
        &self.doc
    }

    /// The root item.
    #[must_use]
    pub fn root(&self) -> &Item {
        self.doc.as_item()
    }

    /// The item at `path` (`None` when absent).
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&Item> {
        let mut cur = self.doc.as_item();
        for s in &path.0 {
            cur = child(cur, s)?;
        }
        (!cur.is_none()).then_some(cur)
    }

    /// The string at `path`.
    #[must_use]
    pub fn get_str(&self, path: &Path) -> Option<&str> {
        self.get(path)?.as_str()
    }

    /// The bool at `path`.
    #[must_use]
    pub fn get_bool(&self, path: &Path) -> Option<bool> {
        self.get(path)?.as_bool()
    }

    /// The integer at `path`.
    #[must_use]
    pub fn get_int(&self, path: &Path) -> Option<i64> {
        self.get(path)?.as_integer()
    }

    /// The number at `path` (integer or float).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn get_float(&self, path: &Path) -> Option<f64> {
        let i = self.get(path)?;
        i.as_float().or_else(|| i.as_integer().map(|n| n as f64))
    }

    /// The string list at `path` (a single string reads as one item).
    #[must_use]
    pub fn get_str_list(&self, path: &Path) -> Option<Vec<String>> {
        let i = self.get(path)?;
        if let Some(s) = i.as_str() {
            return Some(vec![s.to_owned()]);
        }
        i.as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect()
    }

    /// The keys of the table at `path`, in document order.
    #[must_use]
    pub fn keys(&self, path: &Path) -> Vec<String> {
        self.get(path)
            .and_then(Item::as_table_like)
            .map(|t| t.iter().map(|(k, _)| k.to_owned()).collect())
            .unwrap_or_default()
    }

    /// The length of the list at `path` (0 when absent).
    #[must_use]
    pub fn len(&self, path: &Path) -> usize {
        match self.get(path) {
            Some(Item::ArrayOfTables(a)) => a.len(),
            Some(Item::Value(Value::Array(a))) => a.len(),
            _ => 0,
        }
    }

    /// Sets the value at `path` (replacing an existing value keeps its surrounding comments and
    /// position). Missing parent tables are created (`[a.b]` headers as standard tables, inside
    /// inline tables as inline tables); missing list items are not.
    ///
    /// # Errors
    /// An index segment out of range, or a parent that is not a table.
    pub fn set(&mut self, path: &Path, value: impl Into<Value>) -> Result<(), DocError> {
        self.set_item(path, Item::Value(value.into()))
    }

    /// Sets the item at `path` (a `Table` under an inline parent becomes an inline table).
    ///
    /// # Errors
    /// As [`ConfigDoc::set`].
    pub fn set_item(&mut self, path: &Path, item: Item) -> Result<(), DocError> {
        let (parent, last) = path
            .split_last()
            .ok_or_else(|| DocError::NotContainer("root".into()))?;
        let p = self.walk_create(parent, path)?;
        match last {
            Seg::Key(k) => {
                let inline = p.is_value();
                let t = p
                    .as_table_like_mut()
                    .ok_or_else(|| DocError::NotContainer(path.to_string()))?;
                let item = fit(item, inline);
                match t.get_mut(k) {
                    Some(old) if !old.is_none() => replace_keep_decor(old, item),
                    _ => {
                        t.insert(k, item);
                    }
                }
                Ok(())
            }
            Seg::Index(i) => {
                let inline = p.is_value();
                let old = p
                    .get_mut(*i)
                    .ok_or_else(|| DocError::OutOfRange(path.to_string()))?;
                let item = fit(item, inline || old.is_value());
                replace_keep_decor(old, item);
                Ok(())
            }
        }
    }

    /// Inserts a table record into the list at `list` at `index` (`index == len` appends). An
    /// absent list is created as an array of tables (inline array under an inline parent).
    ///
    /// # Errors
    /// An index past the end, or `list` is not a list of tables.
    pub fn insert_record(
        &mut self,
        list: &Path,
        index: usize,
        record: Table,
    ) -> Result<(), DocError> {
        self.ensure_list(list, true)?;
        let it = self.walk_mut(list)?;
        match it {
            Item::ArrayOfTables(a) => {
                if index > a.len() {
                    return Err(DocError::OutOfRange(list.to_string()));
                }
                let mut record = record;
                record.set_implicit(false);
                a.insert(index, record);
                Ok(())
            }
            Item::Value(Value::Array(a)) => {
                if index > a.len() {
                    return Err(DocError::OutOfRange(list.to_string()));
                }
                insert_spaced(a, index, Value::InlineTable(record.into_inline_table()));
                Ok(())
            }
            _ => Err(DocError::NotContainer(list.to_string())),
        }
    }

    /// Inserts a value into the inline array at `list` at `index` (created when absent).
    ///
    /// # Errors
    /// An index past the end, or `list` is not an inline array.
    pub fn insert_value(
        &mut self,
        list: &Path,
        index: usize,
        value: impl Into<Value>,
    ) -> Result<(), DocError> {
        self.ensure_list(list, false)?;
        match self.walk_mut(list)? {
            Item::Value(Value::Array(a)) => {
                if index > a.len() {
                    return Err(DocError::OutOfRange(list.to_string()));
                }
                insert_spaced(a, index, value.into());
                Ok(())
            }
            _ => Err(DocError::NotContainer(list.to_string())),
        }
    }

    /// Removes the key or list item at `path`, returning it.
    ///
    /// # Errors
    /// Nothing at `path`.
    pub fn remove(&mut self, path: &Path) -> Result<Item, DocError> {
        let nf = || DocError::NotFound(path.to_string());
        let (parent, last) = path.split_last().ok_or_else(nf)?;
        let p = self.walk_mut_inner(parent).ok_or_else(nf)?;
        match last {
            Seg::Key(k) => p
                .as_table_like_mut()
                .and_then(|t| t.remove(k))
                .filter(|i| !i.is_none())
                .ok_or_else(nf),
            Seg::Index(i) => match p {
                Item::ArrayOfTables(a) if *i < a.len() => Ok(Item::Table(a.remove(*i))),
                Item::Value(Value::Array(a)) if *i < a.len() => Ok(Item::Value(a.remove(*i))),
                _ => Err(nf()),
            },
        }
    }

    /// Moves list item `from` to position `to` (both indexes into the list before the move).
    ///
    /// # Errors
    /// Either index out of range, or `list` is not a list.
    pub fn move_item(&mut self, list: &Path, from: usize, to: usize) -> Result<(), DocError> {
        let oor = || DocError::OutOfRange(list.to_string());
        match self.walk_mut(list)? {
            Item::ArrayOfTables(a) => {
                if from >= a.len() || to >= a.len() {
                    return Err(oor());
                }
                if from == to {
                    return Ok(());
                }
                let pool = aot_positions(a);
                let t = a.remove(from);
                a.insert(to, t);
                reassign_positions(a, pool);
                Ok(())
            }
            Item::Value(Value::Array(a)) => {
                if from >= a.len() || to >= a.len() {
                    return Err(oor());
                }
                if from == to {
                    return Ok(());
                }
                // Keep each slot's leading whitespace: the first item has no leading space.
                let decors: Vec<_> = a.iter().map(|v| v.decor().clone()).collect();
                let v = a.remove(from);
                a.insert_formatted(to, v);
                for (v, d) in a.iter_mut().zip(decors) {
                    *v.decor_mut() = d;
                }
                Ok(())
            }
            _ => Err(DocError::NotContainer(list.to_string())),
        }
    }

    /// Inserts a new map key at `map.key` (the map table is created when absent).
    ///
    /// # Errors
    /// The key exists, or `map` is not a table.
    pub fn insert_key(&mut self, map: &Path, key: &str, item: Item) -> Result<(), DocError> {
        let at = map.key(key);
        if self.get(&at).is_some() {
            return Err(DocError::Exists(at.to_string()));
        }
        self.set_item(&at, item)
    }

    /// Renames map key `from` to `to` in the table at `map`, keeping its position, value and
    /// comments.
    ///
    /// # Errors
    /// `from` is absent, `to` exists, or `map` is not a table.
    pub fn rename_key(&mut self, map: &Path, from: &str, to: &str) -> Result<(), DocError> {
        if from == to {
            return Ok(());
        }
        if self.get(&map.key(to)).is_some() {
            return Err(DocError::Exists(map.key(to).to_string()));
        }
        let nf = || DocError::NotFound(map.key(from).to_string());
        let it = if map.0.is_empty() {
            self.doc.as_item_mut()
        } else {
            self.walk_mut(map)?
        };
        match it {
            Item::Table(t) => {
                if !t.contains_key(from) {
                    return Err(nf());
                }
                let keys: Vec<String> = t.iter().map(|(k, _)| k.to_owned()).collect();
                let mut tail = Vec::new();
                for k in keys.iter().skip_while(|k| k.as_str() != from) {
                    if let Some(e) = t.remove_entry(k) {
                        tail.push(e);
                    }
                }
                for (k, v) in tail {
                    let k = if k.get() == from { renamed(&k, to) } else { k };
                    t.insert_formatted(&k, v);
                }
                Ok(())
            }
            Item::Value(Value::InlineTable(t)) => {
                if !t.contains_key(from) {
                    return Err(nf());
                }
                let keys: Vec<String> = t.iter().map(|(k, _)| k.to_owned()).collect();
                let mut tail = Vec::new();
                for k in keys.iter().skip_while(|k| k.as_str() != from) {
                    if let Some(e) = t.remove_entry(k) {
                        tail.push(e);
                    }
                }
                for (k, v) in tail {
                    let k = if k.get() == from { renamed(&k, to) } else { k };
                    t.insert_formatted(&k, v);
                }
                Ok(())
            }
            _ => Err(DocError::NotContainer(map.to_string())),
        }
    }

    // --- navigation ---------------------------------------------------------------------------

    fn walk_mut_inner(&mut self, segs: &[Seg]) -> Option<&mut Item> {
        let mut cur = self.doc.as_item_mut();
        for s in segs {
            cur = child_mut(cur, s)?;
        }
        Some(cur)
    }

    fn walk_mut(&mut self, path: &Path) -> Result<&mut Item, DocError> {
        self.walk_mut_inner(&path.0)
            .filter(|i| !i.is_none())
            .ok_or_else(|| DocError::NotFound(path.to_string()))
    }

    /// Walks `segs`, creating missing key segments as tables.
    fn walk_create(&mut self, segs: &[Seg], full: &Path) -> Result<&mut Item, DocError> {
        let mut cur = self.doc.as_item_mut();
        for s in segs {
            cur = match s {
                Seg::Index(i) => cur
                    .get_mut(*i)
                    .ok_or_else(|| DocError::OutOfRange(full.to_string()))?,
                Seg::Key(k) => {
                    let inline = cur.is_value();
                    let t = cur
                        .as_table_like_mut()
                        .ok_or_else(|| DocError::NotContainer(full.to_string()))?;
                    if t.get(k).is_none_or(Item::is_none) {
                        let new = if inline {
                            Item::Value(Value::InlineTable(InlineTable::new()))
                        } else {
                            let mut nt = Table::new();
                            nt.set_implicit(true);
                            Item::Table(nt)
                        };
                        t.insert(k, new);
                    }
                    t.get_mut(k)
                        .ok_or_else(|| DocError::NotFound(full.to_string()))?
                }
            };
        }
        Ok(cur)
    }

    fn ensure_list(&mut self, list: &Path, of_tables: bool) -> Result<(), DocError> {
        if self.get(list).is_some() {
            return Ok(());
        }
        let (parent, _) = list
            .split_last()
            .ok_or_else(|| DocError::NotContainer("root".into()))?;
        let inline = self.walk_mut_inner(parent).is_some_and(|p| p.is_value());
        let item = if of_tables && !inline {
            Item::ArrayOfTables(ArrayOfTables::new())
        } else {
            Item::Value(Value::Array(toml_edit::Array::new()))
        };
        self.set_item(list, item)
    }
}

impl Default for ConfigDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ConfigDoc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.doc)
    }
}

fn child<'a>(it: &'a Item, s: &Seg) -> Option<&'a Item> {
    match s {
        Seg::Key(k) => it.as_table_like()?.get(k),
        Seg::Index(i) => it.get(*i),
    }
}

fn child_mut<'a>(it: &'a mut Item, s: &Seg) -> Option<&'a mut Item> {
    match s {
        Seg::Key(k) => it.as_table_like_mut()?.get_mut(k),
        Seg::Index(i) => it.get_mut(*i),
    }
}

/// Converts `item` to the inline form when the parent is inline.
fn fit(item: Item, inline: bool) -> Item {
    if !inline {
        return item;
    }
    match item {
        Item::Table(t) => Item::Value(Value::InlineTable(t.into_inline_table())),
        Item::ArrayOfTables(a) => Item::Value(Value::Array(a.into_array())),
        other => other,
    }
}

/// Replaces `old` with `new`, keeping a value's surrounding whitespace/comments.
fn replace_keep_decor(old: &mut Item, mut new: Item) {
    if let (Item::Value(o), Item::Value(n)) = (&*old, &mut new) {
        *n.decor_mut() = o.decor().clone();
    }
    if let (Item::Table(o), Item::Table(n)) = (&*old, &mut new) {
        *n.decor_mut() = o.decor().clone();
        n.set_position(o.position());
    }
    *old = new;
}

/// Inserts into an inline array keeping `[a, b]` spacing when inserting at the front.
fn insert_spaced(a: &mut toml_edit::Array, index: usize, v: Value) {
    let front = index == 0 && !a.is_empty();
    let first_decor = a.get(0).map(|f| f.decor().clone());
    a.insert(index, v);
    if front {
        if let (Some(d), Some(new)) = (first_decor, a.get_mut(0)) {
            *new.decor_mut() = d;
        }
        if let Some(old) = a.get_mut(1) {
            old.decor_mut().set_prefix(" ");
        }
    }
}

fn renamed(k: &Key, to: &str) -> Key {
    let mut n = Key::new(to);
    *n.leaf_decor_mut() = k.leaf_decor().clone();
    n
}

fn positions_of(t: &Table, out: &mut Vec<isize>) {
    if let Some(p) = t.position() {
        out.push(p);
    }
    for (_, v) in t.iter() {
        match v {
            Item::Table(c) => positions_of(c, out),
            Item::ArrayOfTables(a) => a.iter().for_each(|c| positions_of(c, out)),
            _ => {}
        }
    }
}

fn aot_positions(a: &ArrayOfTables) -> Vec<isize> {
    let mut out = Vec::new();
    for t in a.iter() {
        positions_of(t, &mut out);
    }
    out.sort_unstable();
    out
}

/// Assigns the sorted position pool to the list's tables in their new visit order.
fn reassign_positions(a: &mut ArrayOfTables, pool: Vec<isize>) {
    let mut pool = pool.into_iter();
    for t in a.iter_mut() {
        assign(t, &mut pool);
    }
}

fn assign(t: &mut Table, pool: &mut impl Iterator<Item = isize>) {
    if t.position().is_some() {
        t.set_position(pool.next());
    }
    let mut kids = Vec::new();
    for (_, v) in t.iter_mut() {
        kids.push(v);
    }
    for k in kids {
        match k {
            Item::Table(c) => assign(c, pool),
            Item::ArrayOfTables(a) => {
                for c in a.iter_mut() {
                    assign(c, pool);
                }
            }
            _ => {}
        }
    }
}
