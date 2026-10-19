//! Image descriptions (plan `v0-core-daemon` WP-13, migration v3): the MD5-keyed store the
//! search feed folds into chunk content and `contentHash`.
//!
//! Ported from Crucible's `ImageDescriptionStore` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/search/
//! imageDescriptionStore.ts`): one record per described image, keyed by the image's content
//! MD5 — Crucible keys its index by the record file's basename (`<md5>.json`, `load()` at
//! `:290-311`). `descriptionHash` is
//! stored as written (never recomputed), because it is exactly the value Crucible folded into
//! every note's `contentHash` (`combinedDescriptionHash`, `:217-227`).
//!
//! Dropped (Obsidian-induced): the per-file JSON storage under the plugin data dir, the lazy
//! in-memory `md5 -> hash` index and the load-time corrupt-file purge — SQLite is the index. The
//! `pruneDegenerate`/`pruneFailed` hygiene belongs to A2's describe pipeline.

use crate::error::Result;
use crate::store::StateDb;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

/// One stored description record (Crucible `ImageDescriptionRecord`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDescription {
    /// Lowercase content MD5 (the key).
    pub md5: String,
    /// `vision` | `svg-text` | `imported` | `failed`.
    pub kind: String,
    /// One dense paragraph (may be empty).
    pub narrative: String,
    /// Structured transcription (may be empty).
    pub extraction: String,
    /// Crucible's `fnv1a(narrative + '\n' + extraction)`, stored as written.
    pub description_hash: String,
    /// ISO timestamp the record was written.
    pub described_at: String,
    /// The full record JSON (provenance: provider/model ids, failure fields, schema version).
    pub raw: String,
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ImageDescription> {
    Ok(ImageDescription {
        md5: r.get(0)?,
        kind: r.get(1)?,
        narrative: r.get(2)?,
        extraction: r.get(3)?,
        description_hash: r.get(4)?,
        described_at: r.get(5)?,
        raw: r.get(6)?,
    })
}

fn put_on(conn: &rusqlite::Connection, d: &ImageDescription) -> Result<bool> {
    let n = conn.execute(
            "INSERT INTO image_descriptions (md5, kind, narrative, extraction, description_hash, described_at, raw)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(md5) DO UPDATE SET kind = excluded.kind, narrative = excluded.narrative,
               extraction = excluded.extraction, description_hash = excluded.description_hash,
               described_at = excluded.described_at, raw = excluded.raw
             WHERE raw IS NOT excluded.raw OR kind IS NOT excluded.kind
               OR narrative IS NOT excluded.narrative OR extraction IS NOT excluded.extraction
               OR description_hash IS NOT excluded.description_hash
               OR described_at IS NOT excluded.described_at",
            params![
                d.md5,
                d.kind,
                d.narrative,
                d.extraction,
                d.description_hash,
                d.described_at,
                d.raw
            ],
        )?;
    Ok(n > 0)
}

const COLS: &str = "md5, kind, narrative, extraction, description_hash, described_at, raw";

/// Every stored record among `md5s`, ordered by md5 (shared by [`StateDb`] and
/// [`crate::StateReader`]).
pub(crate) fn descriptions_for(
    conn: &rusqlite::Connection,
    md5s: &[String],
) -> Result<Vec<ImageDescription>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM image_descriptions WHERE md5 = ?1"
    ))?;
    let mut keys: Vec<&String> = md5s.iter().collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for k in keys {
        if let Some(d) = stmt.query_row(params![k], row).optional()? {
            out.push(d);
        }
    }
    Ok(out)
}

impl StateDb {
    /// Upserts one record (keyed by `md5`). Returns `true` when a row was inserted or changed.
    pub fn put_image_description(&mut self, d: &ImageDescription) -> Result<bool> {
        put_on(self.conn(), d)
    }

    /// The record for `md5`, if any (Crucible `has` + `get`).
    pub fn image_description(&self, md5: &str) -> Result<Option<ImageDescription>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLS} FROM image_descriptions WHERE md5 = ?1"),
                params![md5],
                row,
            )
            .optional()?)
    }

    /// Every stored record among `md5s`, ordered by md5 (unknown keys are skipped).
    pub fn image_descriptions_for(&self, md5s: &[String]) -> Result<Vec<ImageDescription>> {
        descriptions_for(self.conn(), md5s)
    }

    /// Crucible `pruneFailed('transient')` (`imageDescriptionStore.ts:253-277`): deletes `failed`
    /// records whose `raw.failureClass` is `transient` (a record without one classifies lazily
    /// from its `failure` text, [`legacy_transient`]). Returns the pruned count.
    pub fn prune_transient_failed_image_descriptions(&mut self) -> Result<usize> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let rows: Vec<(String, String)> = {
            let mut stmt =
                tx.prepare("SELECT md5, raw FROM image_descriptions WHERE kind = 'failed'")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut n = 0;
        for (md5, raw) in rows {
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let transient = match v.get("failureClass").and_then(Value::as_str) {
                Some(c) => c == "transient",
                None => legacy_transient(v.get("failure").and_then(Value::as_str).unwrap_or("")),
            };
            if transient {
                n += tx.execute(
                    "DELETE FROM image_descriptions WHERE md5 = ?1",
                    params![md5],
                )?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Crucible `pruneDegenerate` (`imageDescriptionStore.ts:239-251`): deletes `vision` records
    /// whose extraction exceeds `max_chars` characters. Returns the pruned count.
    pub fn prune_degenerate_image_descriptions(&mut self, max_chars: usize) -> Result<usize> {
        let max = i64::try_from(max_chars).unwrap_or(i64::MAX);
        Ok(self.conn().execute(
            "DELETE FROM image_descriptions WHERE kind = 'vision' AND length(extraction) > ?1",
            params![max],
        )?)
    }
}

/// Lazy classification of a pre-`failureClass` failure message — Crucible `classifyFailure`
/// (`imageDescriptionStore.ts:101-106`): `/timed out after \d+ms|net::ERR_/`.
fn legacy_transient(failure: &str) -> bool {
    if failure.contains("net::ERR_") {
        return true;
    }
    failure.match_indices("timed out after ").any(|(i, m)| {
        let rest = &failure[i + m.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        digits > 0 && rest[digits..].starts_with("ms")
    })
}

#[cfg(test)]
mod tests {
    use crate::*;

    fn rec(md5: &str, narrative: &str) -> ImageDescription {
        ImageDescription {
            md5: md5.into(),
            kind: "vision".into(),
            narrative: narrative.into(),
            extraction: String::new(),
            description_hash: "0000abcd".into(),
            described_at: "2026-01-01T00:00:00.000Z".into(),
            raw: "{}".into(),
        }
    }

    #[test]
    fn put_get_and_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = StateDb::open(&dir.path().join("s.db"), 1, QueueConfig::default()).unwrap();
        assert!(s.put_image_description(&rec("bb", "two")).unwrap());
        assert!(s.put_image_description(&rec("aa", "one")).unwrap());
        assert!(!s.put_image_description(&rec("aa", "one")).unwrap());
        assert!(s.put_image_description(&rec("aa", "uno")).unwrap());
        assert_eq!(s.image_description("aa").unwrap().unwrap().narrative, "uno");
        assert_eq!(s.image_description("zz").unwrap(), None);
        let got = s
            .image_descriptions_for(&["bb".into(), "zz".into(), "aa".into(), "bb".into()])
            .unwrap();
        assert_eq!(
            got.iter().map(|d| d.md5.as_str()).collect::<Vec<_>>(),
            ["aa", "bb"]
        );
        drop(s);
        let r = StateReader::open(&dir.path().join("s.db")).unwrap();
        let via_reader = r
            .image_descriptions_for(&["bb".into(), "zz".into(), "aa".into()])
            .unwrap();
        assert_eq!(via_reader, got);
    }
}
