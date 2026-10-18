//! `cv search import`: brings the retired Node companion's chunks and embeddings into a
//! `semsearch` index (plan semsearch-crate WP-3, audit M1).
//!
//! The companion DB is opened read-only. Column mapping (mirrors `semsearch`'s
//! `examples/bench.rs` `load --companion`): `path` → source, `id` → chunk id, `content_hash` →
//! source hash, `title`/`heading`/`text`/`entities`/`metadata_json` → chunk fields, `embedding`
//! (LE f32 blob) + `embedding_space` → vector in `Space { embedding_space, blob_len / 4 }`; only
//! rows of vault `default` are read. The first embedded row fixes the import space; a vector in
//! another space is not imported (counted in `foreign`).
//!
//! **Idempotent and resumable.** Per source: skipped when the index already has the same hash,
//! the same chunk count and at least as many embedded chunks; embeddings filled in with
//! `set_embeddings` (text untouched) when only vectors are missing (a lexical-only index from an
//! early feed); otherwise upserted. A second run over a populated index writes nothing. Sources
//! the index holds but the companion lacks are left alone.
//!
//! **Import state.** The meta key [`IMPORT_STATE_KEY`] is removed at the start of a run and
//! written **last** on success: [`ImportState`] as JSON — the companion file's identity
//! (absolute path, byte size, mtime in ms; no content fingerprint, a re-snapshot changes the
//! mtime) plus the counts. Read it with [`import_state`].

use std::collections::HashMap;
use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use semsearch::{Chunk, Index, Space};
use serde::{Deserialize, Serialize};

use crate::error::SearchError;

/// The companion's vault id the import reads.
pub const COMPANION_VAULT: &str = "default";
/// `user_meta` key of the import state.
pub const IMPORT_STATE_KEY: &str = "cv.search_import";

/// Recorded on success (the WP-4 migration keys on its presence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportState {
    /// Absolute path of the companion DB.
    pub source_path: String,
    /// Its size in bytes.
    pub source_size: u64,
    /// Its mtime (ms since the epoch).
    pub source_mtime_ms: u64,
    /// Sources read.
    pub sources: u64,
    /// Chunks read.
    pub chunks: u64,
    /// Chunks read with an importable embedding.
    pub embedded: u64,
}

/// Aggregate counts of one run.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ImportCounts {
    /// Sources read.
    pub sources: u64,
    /// Chunks read.
    pub chunks: u64,
    /// Chunks with an importable embedding.
    pub embedded: u64,
    /// Embeddings in another space than the import's (not imported).
    pub foreign: u64,
    /// Sources already current.
    pub skipped: u64,
    /// Sources whose embeddings were filled in.
    pub filled: u64,
    /// Sources upserted.
    pub written: u64,
    /// Wall time.
    pub seconds: f64,
}

/// The recorded import state, if a run succeeded.
pub fn import_state(index: &Index) -> Result<Option<ImportState>, SearchError> {
    index
        .get_meta(IMPORT_STATE_KEY)?
        .map(|s| {
            serde_json::from_str(&s)
                .map_err(|e| SearchError::Malformed(format!("{IMPORT_STATE_KEY}: {e}")))
        })
        .transpose()
}

/// One companion `chunks` row for [`write_companion_fixture`] (synthetic test fixtures only).
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct CompanionRow {
    pub id: String,
    pub vault: String,
    pub path: String,
    pub content_hash: String,
    pub title: String,
    pub heading: String,
    pub text: String,
    pub ordinal: i64,
    pub metadata_json: String,
    pub embedding: Option<Vec<f32>>,
    pub embedding_space: Option<String>,
}

/// Writes a companion-schema DB (the columns the import reads) holding `rows` — for synthetic
/// test fixtures in this crate and `cv`.
#[doc(hidden)]
pub fn write_companion_fixture(path: &Path, rows: &[CompanionRow]) -> Result<(), SearchError> {
    let db = Connection::open(path).map_err(sql)?;
    db.execute_batch(
        "CREATE TABLE chunks (id TEXT, vault_id TEXT, path TEXT, content_hash TEXT, \
         title TEXT, heading TEXT, text TEXT, mtime INTEGER, ordinal INTEGER, \
         metadata_json TEXT NOT NULL, embedding BLOB, embedding_dim INTEGER, \
         embedding_model TEXT, embedding_space TEXT, entities TEXT NOT NULL DEFAULT '');",
    )
    .map_err(sql)?;
    for r in rows {
        let blob: Option<Vec<u8>> = r
            .embedding
            .as_ref()
            .map(|v| v.iter().flat_map(|x| x.to_le_bytes()).collect());
        db.execute(
            "INSERT INTO chunks (id, vault_id, path, content_hash, title, heading, text, \
             ordinal, metadata_json, embedding, embedding_space) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                r.id,
                r.vault,
                r.path,
                r.content_hash,
                r.title,
                r.heading,
                r.text,
                r.ordinal,
                r.metadata_json,
                blob,
                r.embedding_space
            ],
        )
        .map_err(sql)?;
    }
    Ok(())
}

struct Group {
    source: String,
    hash: String,
    chunks: Vec<Chunk>,
    space: Option<Space>,
}

fn sql(e: rusqlite::Error) -> SearchError {
    SearchError::Index(format!("companion: {e}"))
}

fn decode(blob: &[u8]) -> Vec<f32> {
    blob.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// Imports `from` (a companion SQLite file, opened read-only) into `index`.
pub fn import_companion(from: &Path, index: &mut Index) -> Result<ImportCounts, SearchError> {
    run(from, index, None)
}

/// The import body; `abort_after` simulates a mid-run failure after that many sources (tests).
fn run(
    from: &Path,
    index: &mut Index,
    abort_after: Option<u64>,
) -> Result<ImportCounts, SearchError> {
    let t0 = Instant::now();
    let abs =
        std::fs::canonicalize(from).map_err(|e| SearchError::Index(format!("companion: {e}")))?;
    let meta =
        std::fs::metadata(&abs).map_err(|e| SearchError::Index(format!("companion: {e}")))?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let src = Connection::open_with_flags(&abs, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql)?;
    index.set_meta(IMPORT_STATE_KEY, None)?;
    let states: HashMap<String, (String, u64, u64)> = index
        .source_states()?
        .into_iter()
        .map(|s| (s.source, (s.content_hash, s.chunks, s.embedded_chunks)))
        .collect();
    let mut stmt = src
        .prepare(
            "SELECT path, id, title, heading, text, entities, metadata_json, embedding, \
             embedding_space, content_hash, ordinal FROM chunks WHERE vault_id = ?1 \
             ORDER BY path, ordinal, rowid",
        )
        .map_err(sql)?;
    let mut rows = stmt.query([COMPANION_VAULT]).map_err(sql)?;
    let mut c = ImportCounts::default();
    let mut import_space: Option<Space> = index.space().cloned();
    let mut cur: Option<Group> = None;
    while let Some(r) = rows.next().map_err(sql)? {
        let path: String = r.get(0).map_err(sql)?;
        if cur.as_ref().is_none_or(|g| g.source != path) {
            if let Some(g) = cur.take() {
                apply(index, &states, g, &mut c)?;
            }
            if abort_after.is_some_and(|n| c.sources >= n) {
                return Err(SearchError::Index("import aborted".into()));
            }
            c.sources += 1;
            cur = Some(Group {
                source: path,
                hash: r.get(9).map_err(sql)?,
                chunks: Vec::new(),
                space: None,
            });
        }
        let Some(g) = cur.as_mut() else { continue };
        let blob: Option<Vec<u8>> = r.get(7).map_err(sql)?;
        let space_s: Option<String> = r.get(8).map_err(sql)?;
        let embedding = match (blob, space_s) {
            (Some(b), Some(s)) if !b.is_empty() && b.len() % 4 == 0 => {
                let space = Space::new(&s, b.len() / 4);
                let fixed = import_space.get_or_insert_with(|| space.clone());
                if *fixed == space {
                    g.space = Some(space);
                    c.embedded += 1;
                    Some(decode(&b))
                } else {
                    c.foreign += 1;
                    None
                }
            }
            _ => None,
        };
        c.chunks += 1;
        g.chunks.push(Chunk {
            id: r.get(1).map_err(sql)?,
            ordinal: r.get::<_, Option<i64>>(10).map_err(sql)?.unwrap_or(0),
            title: r
                .get::<_, Option<String>>(2)
                .map_err(sql)?
                .unwrap_or_default(),
            heading: r
                .get::<_, Option<String>>(3)
                .map_err(sql)?
                .unwrap_or_default(),
            text: r
                .get::<_, Option<String>>(4)
                .map_err(sql)?
                .unwrap_or_default(),
            entities: r
                .get::<_, Option<String>>(5)
                .map_err(sql)?
                .unwrap_or_default(),
            metadata: r.get(6).map_err(sql)?,
            embedding,
        });
    }
    if let Some(g) = cur.take() {
        apply(index, &states, g, &mut c)?;
    }
    let state = ImportState {
        source_path: abs.to_string_lossy().into_owned(),
        source_size: meta.len(),
        source_mtime_ms: mtime_ms,
        sources: c.sources,
        chunks: c.chunks,
        embedded: c.embedded,
    };
    let json = serde_json::to_string(&state).map_err(|e| SearchError::Malformed(e.to_string()))?;
    index.set_meta(IMPORT_STATE_KEY, Some(&json))?;
    c.seconds = t0.elapsed().as_secs_f64();
    Ok(c)
}

fn apply(
    index: &mut Index,
    states: &HashMap<String, (String, u64, u64)>,
    g: Group,
    c: &mut ImportCounts,
) -> Result<(), SearchError> {
    let n = g.chunks.len() as u64;
    let embedded = g.chunks.iter().filter(|ch| ch.embedding.is_some()).count() as u64;
    if let Some((hash, chunks, have)) = states.get(&g.source)
        && *hash == g.hash
        && *chunks == n
    {
        if *have >= embedded {
            c.skipped += 1;
            return Ok(());
        }
        if let Some(space) = &g.space {
            let vecs: Vec<(String, Vec<f32>)> = g
                .chunks
                .iter()
                .filter_map(|ch| ch.embedding.clone().map(|v| (ch.id.clone(), v)))
                .collect();
            match index.set_embeddings(space, &g.source, &vecs) {
                Ok(()) => {
                    c.filled += 1;
                    return Ok(());
                }
                // Chunk ids drifted under the same hash: fall through to a full upsert.
                Err(semsearch::Error::InvalidInput(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    match &g.space {
        Some(s) => index.upsert_source_in(s, &g.source, &g.hash, &g.chunks)?,
        None => index.upsert_source(&g.source, &g.hash, &g.chunks)?,
    }
    c.written += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use semsearch::{IndexOptions, Query};
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let base = if Path::new("/dev/shm").is_dir() {
                PathBuf::from("/dev/shm")
            } else {
                std::env::temp_dir()
            };
            let d = base.join(format!("cv-search-import-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A tiny companion DB: `a.md` (2 chunks, embedded), `b.md` (1 chunk, lexical-only),
    /// `c.md` (1 chunk embedded), plus a row of another vault.
    fn companion(path: &Path) {
        type Row<'a> = (&'a str, &'a str, &'a str, i64, &'a str, Option<[f32; 4]>);
        let rows: [Row<'_>; 5] = [
            (
                "a1",
                "a.md",
                "ha",
                0,
                "apples and pears",
                Some([1.0, 0.0, 0.0, 0.0]),
            ),
            (
                "a2",
                "a.md",
                "ha",
                1,
                "orchard harvest",
                Some([0.9, 0.1, 0.0, 0.0]),
            ),
            ("b1", "b.md", "hb", 0, "bicycle repair", None),
            (
                "c1",
                "c.md",
                "hc",
                0,
                "comet tails",
                Some([0.0, 0.0, 1.0, 0.0]),
            ),
            ("z1", "z.md", "hz", 0, "other vault", None),
        ];
        let rows: Vec<CompanionRow> = rows
            .iter()
            .map(|(id, p, h, ord, text, v)| CompanionRow {
                id: (*id).into(),
                vault: if *p == "z.md" {
                    "other"
                } else {
                    COMPANION_VAULT
                }
                .into(),
                path: (*p).into(),
                content_hash: (*h).into(),
                title: (*p).into(),
                text: (*text).into(),
                ordinal: *ord,
                metadata_json: "{}".into(),
                embedding: v.map(|v| v.to_vec()),
                embedding_space: v.map(|_| "bge-m3/f16".into()),
                ..CompanionRow::default()
            })
            .collect();
        write_companion_fixture(path, &rows).unwrap();
    }

    #[test]
    fn imports_then_rerun_writes_nothing_and_state_is_recorded() {
        let t = Tmp::new("rerun");
        let comp = t.0.join("companion.sqlite");
        companion(&comp);
        let mut idx = Index::open(t.0.join("s.db"), &IndexOptions::default()).unwrap();
        let c = import_companion(&comp, &mut idx).unwrap();
        assert_eq!((c.sources, c.chunks, c.embedded), (3, 4, 3));
        assert_eq!((c.written, c.skipped, c.filled), (3, 0, 0));
        assert_eq!(idx.space(), Some(&Space::new("bge-m3/f16", 4)));
        let st = import_state(&idx).unwrap().unwrap();
        assert_eq!((st.sources, st.chunks, st.embedded), (3, 4, 3));
        assert_eq!(st.source_size, std::fs::metadata(&comp).unwrap().len());
        assert!(Path::new(&st.source_path).is_absolute());
        let hits = idx
            .search(&Query::new("comet").with_vector(vec![0.0, 0.0, 1.0, 0.0]))
            .unwrap();
        assert_eq!(hits[0].source, "c.md");
        let again = import_companion(&comp, &mut idx).unwrap();
        assert_eq!((again.written, again.filled, again.skipped), (0, 0, 3));
    }

    #[test]
    fn lexical_only_index_gets_embeddings_filled() {
        let t = Tmp::new("fill");
        let comp = t.0.join("companion.sqlite");
        companion(&comp);
        let mut idx = Index::open(t.0.join("s.db"), &IndexOptions::default()).unwrap();
        // An early lexical-only feed: same ids and hashes, no vectors.
        for (src, h, ids) in [
            ("a.md", "ha", &["a1", "a2"][..]),
            ("c.md", "hc", &["c1"][..]),
        ] {
            let chunks: Vec<Chunk> = ids.iter().map(|i| Chunk::new(i, src, "", "x")).collect();
            idx.upsert_source(src, h, &chunks).unwrap();
        }
        let c = import_companion(&comp, &mut idx).unwrap();
        assert_eq!((c.filled, c.written, c.skipped), (2, 1, 0));
        assert_eq!(idx.embedded_count(), 3);
        let again = import_companion(&comp, &mut idx).unwrap();
        assert_eq!((again.written, again.filled), (0, 0));
    }

    #[test]
    fn mid_run_failure_leaves_no_import_state() {
        let t = Tmp::new("abort");
        let comp = t.0.join("companion.sqlite");
        companion(&comp);
        let mut idx = Index::open(t.0.join("s.db"), &IndexOptions::default()).unwrap();
        import_companion(&comp, &mut idx).unwrap();
        assert!(import_state(&idx).unwrap().is_some());
        assert!(run(&comp, &mut idx, Some(1)).is_err());
        assert!(import_state(&idx).unwrap().is_none());
    }
}
