//! Job bodies: `search_upsert` (one path), reconcile (the param-less `search-refresh` sweep) and
//! `search_delete`, writing the local `semsearch` index (plan semsearch-crate WP-3, D22). The
//! chunking and file preparation are the Crucible port (WP-13); the up-to-date rule is
//! Crucible's (`SearchManager.indexFiles` + `embeddingCoverageSatisfied`,
//! `src/search/SearchManager.ts:404-416,621-624`).
//!
//! **Writes.** A note is a `semsearch` source keyed by its vault-relative path; chunk ids are the
//! stable chunk ids (vault id [`DEFAULT_VAULT_ID`]) and the source hash is the prepared `contentHash`. A note with zero chunks
//! is deleted. With an embedder configured, chunks are embedded first and written with
//! [`semsearch::Index::upsert_source_in`] in [`space_for`]'s space; otherwise keyword-only.
//!
//! **Embeddings.** A path is unchanged when its hash matches and, with an embedder configured,
//! every chunk is embedded in the recorded space whose model is the configured model id.
//! Unchanged paths make no embed call. A recorded space with another model is
//! [`SearchError::SpaceMismatch`] before any embed call (a space change is a rebuild). An
//! embedder outage is [`SearchError::EmbedderUnavailable`] (→ deferred on the provider id), so a
//! vector gap never gets recorded as done.
//!
//! **Report only.** [`report_only`] opens the file read-only, makes no embed call and no write, and
//! returns aggregate counts only.
//!
//! **Locking.** The shared index lock is held only around index calls, never across an embed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use cv_providers::{ProviderConfig, ProviderError, ProviderManager};
use semsearch::{Index, IndexOptions, SourceState, Space};

use crate::chunker::SearchChunk;
use crate::error::SearchError;
use crate::index::lock;
use crate::prepare::{PreparedFile, build_prepared_chunks};

/// Texts per embedding request (Crucible's sub-batch bound, `SearchEmbeddingMismatchError` doc).
pub const EMBED_BATCH: usize = 96;
/// Notes prepared and embedded per write group in reconcile.
pub const NOTE_BATCH: usize = 64;
/// The vault id folded into stable chunk ids (Crucible's `searchVaultId` default).
pub const DEFAULT_VAULT_ID: &str = "default";

/// The write space for `model_id` at `dim` (the embedder's output width).
#[must_use]
pub fn space_for(model_id: &str, dim: usize) -> Space {
    Space::new(model_id, dim)
}

/// A configured embedder.
#[derive(Debug, Clone)]
pub struct EmbedTarget {
    /// Provider.
    pub provider: ProviderConfig,
    /// Serving model id (also the index space's model).
    pub model: String,
}

/// Feed settings for one run.
#[derive(Debug, Clone)]
pub struct FeedConfig {
    /// Embedder (absent = lexical-only).
    pub embed: Option<EmbedTarget>,
    /// `searchChunkMaxChars`.
    pub max_chars: usize,
    /// `searchChunkOverlapChars`.
    pub overlap_chars: usize,
}

/// One prepared local note.
#[derive(Debug, Clone)]
pub struct LocalNote {
    /// Prepared file (content + folded hash + facets).
    pub prepared: PreparedFile,
    /// Basename (title fallback).
    pub basename: String,
    /// mtime (ms).
    pub mtime: i64,
}

/// Classification counts — what a report-only call may report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedCounts {
    /// Hash equal (and embeddings current when checked).
    pub matched: usize,
    /// Hash differs.
    pub mismatched: usize,
    /// Local path the index does not hold.
    pub missing_index: usize,
    /// Indexed path the vault no longer has.
    pub missing_local: usize,
    /// Hash equal but vectors missing, partial or in another space.
    pub embedding_mismatch: usize,
}

/// What a run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedReport {
    /// Classification counts.
    pub counts: FeedCounts,
    /// Paths upserted (live).
    pub upserted: usize,
    /// Paths deleted (live).
    pub deleted: usize,
    /// Chunks written (live).
    pub chunks: usize,
    /// Chunks embedded (live; 0 lexical-only).
    pub embedded: usize,
}

/// Per-path classification.
#[derive(Debug, Default)]
struct Classified {
    counts: FeedCounts,
    to_upsert: Vec<String>,
    missing_local: Vec<String>,
}

/// Classifies `local` `(path, contentHash)` against the index's `states`. With `full` the states
/// are the whole index, so index-only paths are `missing_local`. `want` is the space model an
/// embedder would write (absent = hashes decide alone).
fn classify(
    local: &[(String, String)],
    states: &[SourceState],
    recorded: Option<&Space>,
    want: Option<&str>,
    full: bool,
) -> Classified {
    let by: BTreeMap<&str, &SourceState> = states.iter().map(|s| (s.source.as_str(), s)).collect();
    let local_paths: BTreeSet<&str> = local.iter().map(|(p, _)| p.as_str()).collect();
    let mut c = Classified::default();
    let mut sorted: Vec<&(String, String)> = local.iter().collect();
    sorted.sort();
    sorted.dedup_by(|a, b| a.0 == b.0);
    for (path, hash) in sorted {
        let Some(s) = by.get(path.as_str()) else {
            c.counts.missing_index += 1;
            c.to_upsert.push(path.clone());
            continue;
        };
        if s.content_hash != *hash {
            c.counts.mismatched += 1;
            c.to_upsert.push(path.clone());
            continue;
        }
        let embedded_ok = want.is_none_or(|m| {
            recorded.is_some_and(|r| r.model == m) && s.embedded_chunks == s.chunks
        });
        if embedded_ok {
            c.counts.matched += 1;
        } else {
            c.counts.embedding_mismatch += 1;
            c.to_upsert.push(path.clone());
        }
    }
    if full {
        c.missing_local = by
            .keys()
            .filter(|p| !local_paths.contains(*p))
            .map(|p| (*p).to_owned())
            .collect();
        c.counts.missing_local = c.missing_local.len();
    }
    c
}

/// Maps a provider failure: transport/5xx/429 → outage (defer); anything else → config error.
fn embed_error(provider: &str, e: ProviderError) -> SearchError {
    let outage = match &e {
        ProviderError::Transport(_) | ProviderError::Task(_) => true,
        ProviderError::Http { status, .. } => *status >= 500 || *status == 429,
        _ => false,
    };
    if outage {
        SearchError::EmbedderUnavailable {
            provider: provider.to_owned(),
            message: e.to_string(),
        }
    } else {
        SearchError::EmbedConfig(e.to_string())
    }
}

/// Embeds `chunks` in place (`SearchManager.attachEmbeddings`, `:852-870`); returns the width.
async fn embed_chunks(
    mgr: &ProviderManager,
    t: &EmbedTarget,
    chunks: &mut [SearchChunk],
    expected_dim: Option<usize>,
) -> Result<Option<usize>, SearchError> {
    let mut dim = expected_dim;
    for batch in chunks.chunks_mut(EMBED_BATCH) {
        let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
        let out = mgr
            .embed(&t.provider, &t.model, &texts)
            .await
            .map_err(|e| embed_error(&t.provider.id, e))?;
        if out.vectors.len() != batch.len() {
            return Err(SearchError::EmbedderUnavailable {
                provider: t.provider.id.clone(),
                message: format!(
                    "embedder produced {} vectors for {} chunks",
                    out.vectors.len(),
                    batch.len()
                ),
            });
        }
        for (c, v) in batch.iter_mut().zip(out.vectors) {
            match dim {
                Some(d) if d != v.len() => {
                    return Err(SearchError::EmbedConfig(format!(
                        "embedding width {} != {d}",
                        v.len()
                    )));
                }
                _ => dim = Some(v.len()),
            }
            c.embedding = Some(v);
            c.embedding_model = Some(t.model.clone());
            c.embedding_space = Some(t.model.clone());
        }
    }
    Ok(dim)
}

/// A built chunk as a `semsearch` chunk (entities one per line,
/// metadata [`SearchChunk::metadata_json`]).
fn to_chunk(c: SearchChunk) -> semsearch::Chunk {
    let entities = c
        .entities
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    semsearch::Chunk {
        metadata: Some(c.metadata_json()),
        id: c.id,
        ordinal: i64::try_from(c.ordinal).unwrap_or(i64::MAX),
        title: c.title,
        heading: c.heading,
        text: c.text,
        entities,
        embedding: c.embedding,
    }
}

/// The recorded space, after checking an embedder would write the same model.
fn check_space(index: &Mutex<Index>, cfg: &FeedConfig) -> Result<Option<Space>, SearchError> {
    let recorded = lock(index).space().cloned();
    if let (Some(r), Some(t)) = (&recorded, &cfg.embed) {
        let want = &t.model;
        if r.model != *want {
            return Err(SearchError::SpaceMismatch {
                recorded: format!("{}/{}d", r.model, r.dim),
                configured: format!("{want}/?d"),
            });
        }
    }
    Ok(recorded)
}

/// Builds, embeds and writes `notes`; a note with zero chunks is deleted instead.
async fn write_notes(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    index: &Mutex<Index>,
    notes: Vec<LocalNote>,
    report: &mut FeedReport,
) -> Result<(), SearchError> {
    if notes.is_empty() {
        return Ok(());
    }
    let recorded = check_space(index, cfg)?;
    let mut keys: Vec<(String, String, usize)> = Vec::new();
    let mut all: Vec<SearchChunk> = Vec::new();
    for n in notes {
        let chunks = build_prepared_chunks(
            &n.prepared,
            DEFAULT_VAULT_ID,
            &n.basename,
            n.mtime,
            cfg.max_chars,
            cfg.overlap_chars,
        );
        keys.push((n.prepared.path, n.prepared.content_hash, chunks.len()));
        all.extend(chunks);
    }
    let mut space = None;
    if let Some(t) = &cfg.embed {
        if let Some(d) = embed_chunks(mgr, t, &mut all, recorded.map(|s| s.dim)).await? {
            space = Some(space_for(&t.model, d));
        }
        report.embedded += all.len();
    }
    let mut it = all.into_iter();
    let mut idx = lock(index);
    for (path, hash, n) in keys {
        if n == 0 {
            if idx.delete_source(&path)? {
                report.deleted += 1;
            }
            continue;
        }
        let chunks: Vec<semsearch::Chunk> = it.by_ref().take(n).map(to_chunk).collect();
        match &space {
            Some(s) => idx.upsert_source_in(s, &path, &hash, &chunks)?,
            None => idx.upsert_source(&path, &hash, &chunks)?,
        }
        report.upserted += 1;
        report.chunks += n;
    }
    Ok(())
}

fn want_model(cfg: &FeedConfig) -> Option<String> {
    cfg.embed.as_ref().map(|t| t.model.clone())
}

/// `search_upsert` for one path. `note = None` means the path is gone or not indexable: the
/// path is deleted from the index.
pub async fn upsert_path(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    index: &Mutex<Index>,
    path: &str,
    note: Option<LocalNote>,
) -> Result<FeedReport, SearchError> {
    let mut report = FeedReport::default();
    let Some(note) = note else {
        return delete_path(index, path);
    };
    let local = [(path.to_owned(), note.prepared.content_hash.clone())];
    let c = {
        let idx = lock(index);
        let states: Vec<SourceState> = idx
            .source_states()?
            .into_iter()
            .filter(|s| s.source == path)
            .collect();
        classify(
            &local,
            &states,
            idx.space(),
            want_model(cfg).as_deref(),
            false,
        )
    };
    report.counts = c.counts;
    if !c.to_upsert.is_empty() {
        write_notes(mgr, cfg, index, vec![note], &mut report).await?;
    }
    Ok(report)
}

/// The reconcile sweep: `local` are every indexable note's `(path, contentHash)`; `load`
/// re-prepares one note for upsert (called only for paths that need it). Gone paths are deleted;
/// with an embedder configured, partially embedded sources are re-embedded (full upsert).
pub async fn reconcile(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    index: &Mutex<Index>,
    local: &[(String, String)],
    load: &dyn Fn(&str) -> Result<Option<LocalNote>, SearchError>,
) -> Result<FeedReport, SearchError> {
    let c = {
        let idx = lock(index);
        let states = idx.source_states()?;
        classify(
            local,
            &states,
            idx.space(),
            want_model(cfg).as_deref(),
            true,
        )
    };
    let mut report = FeedReport {
        counts: c.counts,
        ..FeedReport::default()
    };
    let mut batch = Vec::new();
    for path in &c.to_upsert {
        if let Some(n) = load(path)? {
            batch.push(n);
        }
        if batch.len() >= NOTE_BATCH {
            write_notes(mgr, cfg, index, std::mem::take(&mut batch), &mut report).await?;
        }
    }
    write_notes(mgr, cfg, index, batch, &mut report).await?;
    if !c.missing_local.is_empty() {
        let mut idx = lock(index);
        for p in &c.missing_local {
            if idx.delete_source(p)? {
                report.deleted += 1;
            }
        }
    }
    Ok(report)
}

/// `search_delete`: drops the path's chunks.
pub fn delete_path(index: &Mutex<Index>, path: &str) -> Result<FeedReport, SearchError> {
    let mut report = FeedReport::default();
    if lock(index).delete_source(path)? {
        report.counts.missing_local = 1;
        report.deleted = 1;
    }
    Ok(report)
}

/// What a report-only call classifies.
#[derive(Debug, Clone, Copy)]
pub enum ReportScope<'a> {
    /// One path (`search_upsert`/`search_delete` with a target); `None` hash = gone locally.
    Path(&'a str, Option<&'a str>),
    /// The reconcile sweep over every indexable note.
    All(&'a [(String, String)]),
}

/// Report-only mode: a read-only open of `search_db` (a missing file is an empty index — never an
/// error, never a create), no embed call, no write; aggregate counts only.
pub fn report_only(
    search_db: &Path,
    cfg: &FeedConfig,
    scope: ReportScope<'_>,
) -> Result<FeedReport, SearchError> {
    let (states, recorded) = match Index::open_read_only(search_db, &IndexOptions::default()) {
        Ok(idx) => (idx.source_states()?, idx.space().cloned()),
        Err(semsearch::Error::NotFound(_)) => (Vec::new(), None),
        Err(e) => return Err(e.into()),
    };
    let want = want_model(cfg);
    let counts = match scope {
        ReportScope::Path(path, Some(hash)) => {
            let states: Vec<SourceState> =
                states.into_iter().filter(|s| s.source == path).collect();
            let local = [(path.to_owned(), hash.to_owned())];
            classify(&local, &states, recorded.as_ref(), want.as_deref(), false).counts
        }
        ReportScope::Path(path, None) => FeedCounts {
            missing_local: usize::from(states.iter().any(|s| s.source == path)),
            ..FeedCounts::default()
        },
        ReportScope::All(local) => {
            classify(local, &states, recorded.as_ref(), want.as_deref(), true).counts
        }
    };
    Ok(FeedReport {
        counts,
        ..FeedReport::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(source: &str, hash: &str, chunks: u64, embedded: u64) -> SourceState {
        SourceState {
            source: source.into(),
            content_hash: hash.into(),
            chunks,
            embedded_chunks: embedded,
        }
    }

    #[test]
    fn space_for_uses_the_configured_id() {
        assert_eq!(
            space_for("bge-m3-f16", 1024),
            Space::new("bge-m3-f16", 1024)
        );
    }

    #[test]
    fn classifies_every_case() {
        let local: Vec<(String, String)> = [
            ("a.md", "h1"),
            ("b.md", "h2"),
            ("c.md", "h3"),
            ("d.md", "h4"),
        ]
        .iter()
        .map(|(p, h)| ((*p).to_owned(), (*h).to_owned()))
        .collect();
        let states = vec![
            st("a.md", "h1", 2, 2),
            st("b.md", "XX", 1, 1),
            st("d.md", "h4", 3, 1),
            st("gone.md", "h9", 1, 1),
        ];
        let space = Space::new("m", 4);
        let c = classify(&local, &states, Some(&space), Some("m"), true);
        assert_eq!(c.counts.matched, 1);
        assert_eq!(c.counts.mismatched, 1);
        assert_eq!(c.counts.missing_index, 1);
        assert_eq!(c.counts.embedding_mismatch, 1);
        assert_eq!(c.missing_local, ["gone.md"]);
        assert_eq!(c.to_upsert, ["b.md", "c.md", "d.md"]);
        // Lexical-only: hashes decide alone.
        let lex = classify(&local, &states, None, None, false);
        assert_eq!(lex.counts.matched, 2);
        assert!(lex.missing_local.is_empty());
        // Another recorded model demotes every hash match.
        let other = Space::new("x", 4);
        assert_eq!(
            classify(&local, &states, Some(&other), Some("m"), true)
                .counts
                .embedding_mismatch,
            2
        );
    }
}
