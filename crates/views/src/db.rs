//! Read-only index access for one evaluation, with per-run caches (props, tags, links, tasks,
//! folder listings, path lookups). Never writes: the connection is opened
//! `SQLITE_OPEN_READ_ONLY` by the caller.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::value::Value;

/// One `files` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub id: i64,
    pub path: String,
    pub folder: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
    pub ctime: i64,
}

/// One task item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub line: i64,
    pub status: String,
    pub text: String,
    pub heading: Option<String>,
}

const COLS: &str = "f.id, f.path, f.folder, f.name, f.ext, f.size, f.mtime, f.ctime";

fn file_row(r: &Row<'_>) -> rusqlite::Result<FileInfo> {
    Ok(FileInfo {
        id: r.get(0)?,
        path: r.get(1)?,
        folder: r.get(2)?,
        name: r.get(3)?,
        ext: r.get(4)?,
        size: r.get(5)?,
        mtime: r.get(6)?,
        ctime: r.get(7)?,
    })
}

type Cache<K, V> = RefCell<HashMap<K, V>>;

/// The per-run data source.
pub struct Db<'c> {
    conn: &'c Connection,
    by_path: Cache<String, Option<Arc<FileInfo>>>,
    props: Cache<i64, Arc<BTreeMap<String, Value>>>,
    tags: Cache<i64, Arc<Vec<String>>>,
    links: Cache<i64, Arc<Vec<Value>>>,
    backlinks: Cache<i64, Arc<Vec<Value>>>,
    tasks: Cache<i64, Arc<Vec<TaskInfo>>>,
    folders: Cache<String, Arc<Vec<Value>>>,
}

impl<'c> Db<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self {
            conn,
            by_path: RefCell::default(),
            props: RefCell::default(),
            tags: RefCell::default(),
            links: RefCell::default(),
            backlinks: RefCell::default(),
            tasks: RefCell::default(),
            folders: RefCell::default(),
        }
    }

    /// Markdown files matching `where_sql` (over alias `f`, `?` placeholders), by path.
    pub fn candidates(
        &self,
        where_sql: &str,
        args: &[rusqlite::types::Value],
    ) -> rusqlite::Result<Vec<Arc<FileInfo>>> {
        let sql =
            format!("SELECT {COLS} FROM files f WHERE f.is_md = 1{where_sql} ORDER BY f.path");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), file_row)?;
        let out: Vec<Arc<FileInfo>> = rows
            .map(|r| r.map(Arc::new))
            .collect::<rusqlite::Result<_>>()?;
        let mut cache = self.by_path.borrow_mut();
        for f in &out {
            cache.insert(f.path.clone(), Some(Arc::clone(f)));
        }
        Ok(out)
    }

    /// Bulk-loads props and tags for `files` (one query each instead of one per file). With
    /// `keys`, only props whose key is in the set are loaded (an empty set loads none — SQLite accepts an empty `IN ()`); the
    /// caller guarantees the set covers every key the run reads (`engine::reachable_keys`), so
    /// the projected cache entries answer every `prop(f, key)` read. `None` loads every key.
    pub fn preload(
        &self,
        files: &[Arc<FileInfo>],
        keys: Option<&BTreeSet<String>>,
    ) -> rusqlite::Result<()> {
        let ids: HashSet<i64> = files.iter().map(|f| f.id).collect();
        let mut props: HashMap<i64, BTreeMap<String, Value>> = HashMap::new();
        let sql = match keys {
            None => "SELECT file_id, key, value FROM props".to_string(),
            Some(k) => format!(
                "SELECT file_id, key, value FROM props WHERE key IN ({})",
                vec!["?"; k.len()].join(",")
            ),
        };
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(params_from_iter(keys.into_iter().flatten()))?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            if ids.contains(&id) {
                let v = json_value(text_ref(r, 2)?);
                props.entry(id).or_default().insert(r.get(1)?, v);
            }
        }
        let mut tags: HashMap<i64, Vec<String>> = HashMap::new();
        let mut stmt = self.conn.prepare_cached("SELECT file_id, tag FROM tags")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            if ids.contains(&id) {
                let t: String = r.get(1)?;
                let v = tags.entry(id).or_default();
                if !v.contains(&t) {
                    v.push(t);
                }
            }
        }
        let mut pc = self.props.borrow_mut();
        let mut tc = self.tags.borrow_mut();
        for id in ids {
            pc.insert(id, Arc::new(props.remove(&id).unwrap_or_default()));
            tc.insert(id, Arc::new(tags.remove(&id).unwrap_or_default()));
        }
        Ok(())
    }

    /// The cached props map of file `id`, if loaded (tests inspect the preload projection).
    #[cfg(test)]
    pub(crate) fn cached_props(&self, id: i64) -> Option<Arc<BTreeMap<String, Value>>> {
        self.props.borrow().get(&id).cloned()
    }

    /// The file at vault path `path` (any extension), if indexed.
    pub fn file(&self, path: &str) -> rusqlite::Result<Option<Arc<FileInfo>>> {
        if let Some(f) = self.by_path.borrow().get(path) {
            return Ok(f.clone());
        }
        let f = self
            .conn
            .prepare_cached(&format!("SELECT {COLS} FROM files f WHERE f.path = ?1"))?
            .query_row([path], file_row)
            .optional()?
            .map(Arc::new);
        self.by_path
            .borrow_mut()
            .insert(path.to_string(), f.clone());
        Ok(f)
    }

    /// Frontmatter of `f`.
    pub fn props(&self, f: &FileInfo) -> rusqlite::Result<Arc<BTreeMap<String, Value>>> {
        if let Some(p) = self.props.borrow().get(&f.id) {
            return Ok(Arc::clone(p));
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value FROM props WHERE file_id = ?1")?;
        let rows = stmt.query_map([f.id], |r| {
            let v = json_value(text_ref(r, 1)?);
            Ok((r.get::<_, String>(0)?, v))
        })?;
        let map: BTreeMap<String, Value> = rows.collect::<rusqlite::Result<_>>()?;
        let map = Arc::new(map);
        self.props.borrow_mut().insert(f.id, Arc::clone(&map));
        Ok(map)
    }

    /// Distinct tags of `f` (frontmatter + inline), without `#`.
    pub fn tags(&self, f: &FileInfo) -> rusqlite::Result<Arc<Vec<String>>> {
        if let Some(p) = self.tags.borrow().get(&f.id) {
            return Ok(Arc::clone(p));
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT tag FROM tags WHERE file_id = ?1 ORDER BY rowid")?;
        let v: Vec<String> = stmt
            .query_map([f.id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let v = Arc::new(v);
        self.tags.borrow_mut().insert(f.id, Arc::clone(&v));
        Ok(v)
    }

    /// Outgoing internal links of `f` as link values (resolved path when resolved).
    pub fn links(&self, f: &FileInfo) -> rusqlite::Result<Arc<Vec<Value>>> {
        if let Some(p) = self.links.borrow().get(&f.id) {
            return Ok(Arc::clone(p));
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT l.target, d.path FROM links l LEFT JOIN files d ON d.id = l.resolved_id WHERE l.src_id = ?1 AND l.external = 0 ORDER BY l.id",
        )?;
        let v: Vec<Value> = stmt
            .query_map([f.id], |r| {
                let target: String = r.get(0)?;
                let path: Option<String> = r.get(1)?;
                Ok(Value::Link {
                    path: path.unwrap_or_else(|| target.clone()),
                    label: target,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let v = Arc::new(v);
        self.links.borrow_mut().insert(f.id, Arc::clone(&v));
        Ok(v)
    }

    /// Distinct files linking to `f`, as links.
    pub fn backlinks(&self, f: &FileInfo) -> rusqlite::Result<Arc<Vec<Value>>> {
        if let Some(p) = self.backlinks.borrow().get(&f.id) {
            return Ok(Arc::clone(p));
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT s.path, s.name FROM links l JOIN files s ON s.id = l.src_id WHERE l.resolved_id = ?1 ORDER BY s.path",
        )?;
        let v: Vec<Value> = stmt
            .query_map([f.id], |r| {
                Ok(Value::Link {
                    path: r.get(0)?,
                    label: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let v = Arc::new(v);
        self.backlinks.borrow_mut().insert(f.id, Arc::clone(&v));
        Ok(v)
    }

    /// Tasks of `f` in line order.
    pub fn tasks(&self, f: &FileInfo) -> rusqlite::Result<Arc<Vec<TaskInfo>>> {
        if let Some(p) = self.tasks.borrow().get(&f.id) {
            return Ok(Arc::clone(p));
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT line, status, text, heading FROM tasks WHERE file_id = ?1 ORDER BY line",
        )?;
        let v: Vec<TaskInfo> = stmt
            .query_map(params![f.id], |r| {
                Ok(TaskInfo {
                    line: r.get(0)?,
                    status: r.get(1)?,
                    text: r.get(2)?,
                    heading: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let v = Arc::new(v);
        self.tasks.borrow_mut().insert(f.id, Arc::clone(&v));
        Ok(v)
    }

    /// Markdown files in `folder` or below it, by path, as file handles (`files(folder)`).
    pub fn folder_files(&self, folder: &str) -> rusqlite::Result<Arc<Vec<Value>>> {
        let folder = folder.trim_matches('/').to_string();
        if let Some(p) = self.folders.borrow().get(&folder) {
            return Ok(Arc::clone(p));
        }
        let files = if folder.is_empty() {
            self.candidates("", &[])?
        } else {
            self.candidates(
                " AND (f.folder = ? OR f.folder LIKE ? ESCAPE '\\')",
                &[
                    folder.clone().into(),
                    format!("{}/%", like_escape(&folder)).into(),
                ],
            )?
        };
        let v = Arc::new(files.into_iter().map(Value::File).collect::<Vec<_>>());
        self.folders.borrow_mut().insert(folder, Arc::clone(&v));
        Ok(v)
    }
}

/// Column `idx` as borrowed text, with no `String` copy. Errors are exactly those of
/// `r.get::<_, String>(idx)`, mirroring rusqlite 0.41.1 `Row::get`'s `FromSqlError` mapping
/// for the two errors `ValueRef::as_str` can raise (`InvalidType`, `Utf8Error`).
fn text_ref<'r>(r: &'r Row<'_>, idx: usize) -> rusqlite::Result<&'r str> {
    let value = r.get_ref(idx)?;
    value.as_str().map_err(|err| match err {
        rusqlite::types::FromSqlError::InvalidType => rusqlite::Error::InvalidColumnType(
            idx,
            r.as_ref()
                .column_name(idx)
                .map_or_else(|_| String::new(), str::to_string),
            value.data_type(),
        ),
        rusqlite::types::FromSqlError::Utf8Error(e) => rusqlite::Error::Utf8Error(idx, e),
        rusqlite::types::FromSqlError::OutOfRange(i) => {
            rusqlite::Error::IntegralValueOutOfRange(idx, i)
        }
        rusqlite::types::FromSqlError::Other(e) => {
            rusqlite::Error::FromSqlConversionFailure(idx, value.data_type(), e)
        }
        e => rusqlite::Error::FromSqlConversionFailure(idx, value.data_type(), Box::new(e)),
    })
}

fn json_value(text: &str) -> Value {
    Value::from_json_text(text).unwrap_or_else(|_| Value::Str(text.to_string()))
}

/// Escapes `%`, `_` and `\` for `LIKE … ESCAPE '\'`.
pub fn like_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_value_falls_back_to_raw_text() {
        for text in ["", "not json", "[1, 2,]", "1 2", "{} x", "\"open"] {
            match json_value(text) {
                Value::Str(s) => assert_eq!(s, text),
                other => panic!("{text:?} decoded to {other:?}"),
            }
        }
        assert!(matches!(json_value("2.5"), Value::Num(n) if n == 2.5));
    }

    #[test]
    fn text_ref_errors_match_get_string() {
        let conn = Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT 'k', NULL AS value UNION ALL SELECT 'k', 7 UNION ALL SELECT 'k', '[1]'",
            )
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        for _ in 0..2 {
            let r = rows.next().unwrap().unwrap();
            let want = format!("{:?}", r.get::<_, String>(1).unwrap_err());
            let got = format!("{:?}", text_ref(r, 1).unwrap_err());
            assert!(want.starts_with("InvalidColumnType(1, \"value\""), "{want}");
            assert_eq!(got, want);
        }
        let r = rows.next().unwrap().unwrap();
        assert_eq!(text_ref(r, 1).unwrap(), "[1]");
        assert!(matches!(json_value(text_ref(r, 1).unwrap()), Value::List(l) if l.len() == 1));
    }
}
