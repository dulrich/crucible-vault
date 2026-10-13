//! The read-only query API (plan WP-5): backlinks, outgoing links, tags, props, folder listings,
//! tasks, unresolved links and FTS. Shared by the writer ([`crate::Index::query`]) and read-only
//! clients ([`Reader`]).

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::{generation, schema};

/// A link's syntactic kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkKind {
    /// `[[x]]`.
    Wikilink,
    /// `![[x]]`.
    Embed,
    /// `[l](x)` / `<x>` / bare autolink.
    MdLink,
    /// `![a](x)`.
    MdImage,
    /// A whole-value `[[x]]` frontmatter property.
    FmLink,
}

impl LinkKind {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wikilink => "wikilink",
            Self::Embed => "embed",
            Self::MdLink => "md-link",
            Self::MdImage => "md-image",
            Self::FmLink => "fm-link",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "wikilink" => Self::Wikilink,
            "embed" => Self::Embed,
            "md-link" => Self::MdLink,
            "md-image" => Self::MdImage,
            "fm-link" => Self::FmLink,
            _ => return None,
        })
    }
}

/// One `files` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub folder: String,
    pub name: String,
    pub ext: String,
    pub is_md: bool,
    pub size: i64,
    pub mtime: i64,
    pub ctime: i64,
    pub has_fm: bool,
}

/// One link, with both endpoints' paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRow {
    pub src_path: String,
    pub kind: LinkKind,
    /// The link's source text (fm-links: reconstructed `[[…]]`; md links: the destination).
    pub raw: String,
    pub target: String,
    pub fragment: Option<String>,
    pub alias: Option<String>,
    /// An md link with a URI scheme: never resolvable to a vault file.
    pub external: bool,
    pub resolved_path: Option<String>,
    /// 1-based; `None` for fm-links.
    pub line: Option<i64>,
    /// Byte span into the source file; `None` for fm-links.
    pub span: Option<(i64, i64)>,
}

/// One tag occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagRow {
    pub path: String,
    pub tag: String,
    /// `fm` or `inline`.
    pub source: String,
    pub line: Option<i64>,
}

/// A tag with its occurrence and file counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagCount {
    pub tag: String,
    pub occurrences: i64,
    pub files: i64,
}

/// One frontmatter property, JSON-typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropRow {
    pub path: String,
    pub key: String,
    pub value: serde_json::Value,
}

/// One block item (`callout|fndef|fnref|blockid`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRow {
    pub path: String,
    pub kind: String,
    pub label: String,
    pub line: i64,
}

/// One task list item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRow {
    pub path: String,
    pub line: i64,
    /// The checkbox char, any value (`' '`, `x`, `>`, …).
    pub status: char,
    pub text: String,
    pub heading: Option<String>,
    pub heading_level: Option<u8>,
}

/// One FTS hit (lower `rank` is better: FTS5 `bm25`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FtsHit {
    pub path: String,
    pub rank: f64,
}

const FILE_COLS: &str =
    "f.id, f.path, f.folder, f.name, f.ext, f.is_md, f.size, f.mtime, f.ctime, f.has_fm";

fn file_row(r: &Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        path: r.get(1)?,
        folder: r.get(2)?,
        name: r.get(3)?,
        ext: r.get(4)?,
        is_md: r.get::<_, i64>(5)? != 0,
        size: r.get(6)?,
        mtime: r.get(7)?,
        ctime: r.get(8)?,
        has_fm: r.get::<_, i64>(9)? != 0,
    })
}

const LINK_SELECT: &str = "SELECT s.path, l.kind, l.raw, l.target, l.fragment, l.alias, l.external, d.path, l.line, l.span_start, l.span_end FROM links l JOIN files s ON s.id = l.src_id LEFT JOIN files d ON d.id = l.resolved_id";

fn link_row(r: &Row<'_>) -> rusqlite::Result<LinkRow> {
    let kind: String = r.get(1)?;
    let kind = LinkKind::parse(&kind).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            format!("unknown link kind {kind:?}").into(),
        )
    })?;
    let start: Option<i64> = r.get(9)?;
    let end: Option<i64> = r.get(10)?;
    Ok(LinkRow {
        src_path: r.get(0)?,
        kind,
        raw: r.get(2)?,
        target: r.get(3)?,
        fragment: r.get(4)?,
        alias: r.get(5)?,
        external: r.get::<_, i64>(6)? != 0,
        resolved_path: r.get(7)?,
        line: r.get(8)?,
        span: start.zip(end),
    })
}

fn task_row(r: &Row<'_>) -> rusqlite::Result<TaskRow> {
    let status: String = r.get(2)?;
    Ok(TaskRow {
        path: r.get(0)?,
        line: r.get(1)?,
        status: status.chars().next().unwrap_or(' '),
        text: r.get(3)?,
        heading: r.get(4)?,
        heading_level: r.get(5)?,
    })
}

fn collect<T>(
    conn: &Connection,
    sql: &str,
    p: impl rusqlite::Params,
    f: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let rows = stmt.query_map(p, f)?;
    Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
}

/// Read queries over one connection.
#[derive(Clone, Copy)]
pub struct Query<'a> {
    conn: &'a Connection,
}

impl<'a> Query<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// The rebuild generation of this DB.
    /// The connection's `PRAGMA journal_mode` (`wal` for every final generation).
    pub fn journal_mode(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))?)
    }

    pub fn generation(&self) -> Result<u64> {
        schema::generation(self.conn)
    }

    /// One file by vault path.
    pub fn file(&self, path: &str) -> Result<Option<FileRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {FILE_COLS} FROM files f WHERE f.path = ?1"),
                [path],
                file_row,
            )
            .optional()?)
    }

    /// Number of indexed files.
    pub fn file_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM files", [], |r| r.get(0))?)
    }

    /// Files directly in `folder` (`""` = vault root), by path.
    pub fn folder_files(&self, folder: &str) -> Result<Vec<FileRow>> {
        collect(
            self.conn,
            &format!("SELECT {FILE_COLS} FROM files f WHERE f.folder = ?1 ORDER BY f.path"),
            [folder],
            file_row,
        )
    }

    /// Immediate subfolders of `folder` (`""` = vault root), sorted. Folders exist only as
    /// prefixes of indexed files.
    pub fn subfolders(&self, folder: &str) -> Result<Vec<String>> {
        let folders: Vec<String> = collect(
            self.conn,
            "SELECT DISTINCT folder FROM files WHERE folder <> ''",
            [],
            |r| r.get(0),
        )?;
        let prefix = if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        };
        let mut out: Vec<String> = folders
            .iter()
            .filter_map(|f| f.strip_prefix(prefix.as_str()))
            .filter_map(|rest| rest.split('/').next())
            .filter(|s| !s.is_empty())
            .map(|s| format!("{prefix}{s}"))
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Links resolving to `path`, by source path then line.
    pub fn backlinks(&self, path: &str) -> Result<Vec<LinkRow>> {
        collect(
            self.conn,
            &format!("{LINK_SELECT} WHERE d.path = ?1 ORDER BY s.path, l.line, l.id"),
            [path],
            link_row,
        )
    }

    /// Links written in `path`, in source order (fm-links last).
    pub fn outgoing(&self, path: &str) -> Result<Vec<LinkRow>> {
        collect(
            self.conn,
            &format!("{LINK_SELECT} WHERE s.path = ?1 ORDER BY l.id"),
            [path],
            link_row,
        )
    }

    /// Unresolved links. `include_external` adds md links with a URI scheme (S2: ~30 % of links
    /// are unresolved by nature; the UI separates external-ish from missing notes).
    pub fn unresolved(&self, include_external: bool) -> Result<Vec<LinkRow>> {
        collect(
            self.conn,
            &format!(
                "{LINK_SELECT} WHERE l.resolved_id IS NULL AND (?1 OR l.external = 0) ORDER BY s.path, l.id"
            ),
            [include_external],
            link_row,
        )
    }

    /// Every tag with counts, by tag.
    pub fn tags(&self) -> Result<Vec<TagCount>> {
        collect(
            self.conn,
            "SELECT tag, count(*), count(DISTINCT file_id) FROM tags GROUP BY tag ORDER BY tag",
            [],
            |r| {
                Ok(TagCount {
                    tag: r.get(0)?,
                    occurrences: r.get(1)?,
                    files: r.get(2)?,
                })
            },
        )
    }

    fn tag_rows(&self, filter: &str, arg: &str) -> Result<Vec<TagRow>> {
        collect(
            self.conn,
            &format!(
                "SELECT f.path, t.tag, t.source, t.line FROM tags t JOIN files f ON f.id = t.file_id WHERE {filter} ORDER BY f.path, t.line, t.rowid"
            ),
            [arg],
            |r| {
                Ok(TagRow {
                    path: r.get(0)?,
                    tag: r.get(1)?,
                    source: r.get(2)?,
                    line: r.get(3)?,
                })
            },
        )
    }

    /// Tags of one file.
    pub fn tags_of(&self, path: &str) -> Result<Vec<TagRow>> {
        self.tag_rows("f.path = ?1", path)
    }

    /// Occurrences of `tag` (exact) or any nested `tag/…`.
    pub fn tagged(&self, tag: &str) -> Result<Vec<TagRow>> {
        self.tag_rows(
            "(t.tag = ?1 OR substr(t.tag, 1, length(?1) + 1) = ?1 || '/')",
            tag,
        )
    }

    fn prop_rows(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<PropRow>> {
        let raw: Vec<(String, String, String)> =
            collect(self.conn, sql, p, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        raw.into_iter()
            .map(|(path, key, v)| {
                Ok(PropRow {
                    path,
                    key,
                    value: serde_json::from_str(&v).map_err(Error::Json)?,
                })
            })
            .collect()
    }

    /// Properties of one file, in frontmatter order.
    pub fn props(&self, path: &str) -> Result<Vec<PropRow>> {
        self.prop_rows(
            "SELECT f.path, p.key, p.value FROM props p JOIN files f ON f.id = p.file_id WHERE f.path = ?1 ORDER BY p.rowid",
            [path],
        )
    }

    /// Files whose `key` equals `value` with its JSON type (`true` ≠ `"true"`).
    pub fn files_with_prop(&self, key: &str, value: &serde_json::Value) -> Result<Vec<PropRow>> {
        let all = self.prop_rows(
            "SELECT f.path, p.key, p.value FROM props p JOIN files f ON f.id = p.file_id WHERE p.key = ?1 ORDER BY f.path",
            [key],
        )?;
        Ok(all.into_iter().filter(|r| &r.value == value).collect())
    }

    /// Block items of one file.
    pub fn blocks(&self, path: &str) -> Result<Vec<BlockRow>> {
        collect(
            self.conn,
            "SELECT f.path, b.kind, b.label, b.line FROM blocks b JOIN files f ON f.id = b.file_id WHERE f.path = ?1 ORDER BY b.line, b.rowid",
            [path],
            |r| {
                Ok(BlockRow {
                    path: r.get(0)?,
                    kind: r.get(1)?,
                    label: r.get(2)?,
                    line: r.get(3)?,
                })
            },
        )
    }

    /// Tasks, optionally of one file and/or one status char; by path then line.
    pub fn tasks(&self, path: Option<&str>, status: Option<char>) -> Result<Vec<TaskRow>> {
        let status = status.map(String::from);
        collect(
            self.conn,
            "SELECT f.path, t.line, t.status, t.text, t.heading, t.heading_level FROM tasks t JOIN files f ON f.id = t.file_id WHERE (?1 IS NULL OR f.path = ?1) AND (?2 IS NULL OR t.status = ?2) ORDER BY f.path, t.line",
            params![path, status],
            task_row,
        )
    }

    /// Full-text search (FTS5 `MATCH` syntax), best first.
    pub fn fts(&self, query: &str, limit: usize) -> Result<Vec<FtsHit>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        collect(
            self.conn,
            "SELECT f.path, bm25(fts) AS rank FROM fts JOIN files f ON f.id = fts.rowid WHERE fts MATCH ?1 ORDER BY rank LIMIT ?2",
            params![query, limit],
            |r| {
                Ok(FtsHit {
                    path: r.get(0)?,
                    rank: r.get(1)?,
                })
            },
        )
    }
}

/// A read-only client handle on one `index.db` generation. Reopen on `index_generation` (plan
/// Public Interfaces): a rebuild builds a new generation file, and this handle keeps reading its
/// own (even after retirement unlinks it). Readers never write and never checkpoint.
pub struct Reader {
    conn: Connection,
    generation: u64,
}

impl Reader {
    /// Opens generation `generation` beside `base` with `SQLITE_OPEN_READ_ONLY` (never creates
    /// it). Fails with [`Error::GenerationMissing`] when the file is absent, and on a schema or
    /// stored-generation mismatch.
    pub fn open(base: &Path, generation: u64) -> Result<Self> {
        let conn = generation::open_read_only(base, generation)?;
        Ok(Self { conn, generation })
    }

    /// Opens [`crate::current_generation`]; none → [`Error::NoIndex`].
    pub fn open_current(base: &Path) -> Result<Self> {
        let generation = generation::current_generation(base)?
            .ok_or_else(|| Error::NoIndex(base.display().to_string()))?;
        Self::open(base, generation)
    }

    /// The generation this handle reads.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The query API.
    pub fn query(&self) -> Query<'_> {
        Query::new(&self.conn)
    }

    /// Runs `f` inside one read transaction, so every query sees one consistent snapshot.
    pub fn snapshot<T>(&mut self, f: impl FnOnce(Query<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let out = f(Query::new(&tx))?;
        tx.finish()?;
        Ok(out)
    }
}
