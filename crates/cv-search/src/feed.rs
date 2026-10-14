//! Job bodies: `search_upsert` (one path), reconcile (the param-less `search-refresh` sweep) and
//! `search_delete` (port of `SearchManager.indexFiles` / `deletePath` and the WP-SA2 reconcile
//! command, `src/search/SearchManager.ts:322-445`; deferral per
//! `src/orchestration/workflows/SearchIndexWorkflow.ts:179-195`).
//!
//! **Modes.** [`Mode::Shadow`] runs only the read half of the client (`/health`, `/v1/paths`,
//! `/v1/files/state`) against Crucible's own `vaultId` and makes no embedding call; its result
//! is aggregate counts only. [`Mode::Live`] carries the [`LiveProof`] that unlocks
//! `/v1/chunks/*`.
//!
//! **Embeddings (E3).** With an embedder configured, a live run probes it once (one synthetic
//! input) for its dimension, and treats a path as unchanged only when its hash matches, the
//! companion reports every chunk embedded in the configured space, and `/health`'s
//! `embeddingModel`/`embeddingDim` equal the configured model and the probed dimension. A shadow
//! run compares the model id only. An embedder outage is
//! [`SearchError::EmbedderUnavailable`] (→ deferred on the provider id) — stricter than
//! Crucible's lenient FTS-only fallback, so a vector gap never gets recorded as done.
//!
//! **Idempotence (E2).** Upsert is a full per-path replace keyed by `vaultId + path` (ids are
//! `vaultId`-scoped, `stableChunkId`), and delete is per `vaultId + path`. A run that crashes
//! after some requests re-sends, on retry, at most the same chunks for the same paths — the
//! companion ends in the same state.

use std::collections::HashMap;

use cv_providers::{ProviderConfig, ProviderError, ProviderManager};

use crate::chunker::SearchChunk;
use crate::client::{CompanionReader, CompanionWriter, Health, LiveProof};
use crate::compare::{CompareCounts, EmbedCheck, RemoteEntry, compare};
use crate::error::SearchError;
use crate::js::lossy;
use crate::prepare::{PreparedFile, build_prepared_chunks};

/// Texts per embedding request (Crucible's sub-batch bound, `SearchEmbeddingMismatchError` doc).
pub const EMBED_BATCH: usize = 96;

/// A configured embedder.
#[derive(Debug, Clone)]
pub struct EmbedTarget {
    /// Provider.
    pub provider: ProviderConfig,
    /// Model id (also the chunk's `embeddingModel` and `embeddingSpace`).
    pub model: String,
}

/// Feed settings for one run.
#[derive(Debug, Clone)]
pub struct FeedConfig {
    /// Companion base URL.
    pub companion_url: String,
    /// Crucible's `vaultId`.
    pub vault_id: String,
    /// Embedder (absent = lexical-only).
    pub embed: Option<EmbedTarget>,
    /// `searchChunkMaxChars`.
    pub max_chars: usize,
    /// `searchChunkOverlapChars`.
    pub overlap_chars: usize,
}

/// Shadow or live.
#[derive(Debug)]
pub enum Mode {
    /// Read-only.
    Shadow,
    /// Mutating.
    Live(LiveProof),
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

/// What a run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedReport {
    /// Classification counts.
    pub counts: CompareCounts,
    /// Paths upserted (live).
    pub upserted: usize,
    /// Paths deleted (live).
    pub deleted: usize,
    /// Chunks sent (live).
    pub chunks: usize,
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

/// Probes the embedder's dimension with one synthetic input.
async fn probe_dimension(mgr: &ProviderManager, t: &EmbedTarget) -> Result<usize, SearchError> {
    let out = mgr
        .embed(&t.provider, &t.model, &["dimension probe".to_owned()])
        .await
        .map_err(|e| embed_error(&t.provider.id, e))?;
    out.vectors
        .first()
        .map(Vec::len)
        .filter(|&n| n > 0)
        .ok_or_else(|| SearchError::EmbedConfig("embedder returned no vector".into()))
}

/// Attaches embeddings to `chunks` (`SearchManager.attachEmbeddings`, `:852-870`).
async fn embed_chunks(
    mgr: &ProviderManager,
    t: &EmbedTarget,
    chunks: &mut [SearchChunk],
    expected_dim: Option<usize>,
) -> Result<(), SearchError> {
    let mut dim = expected_dim;
    for batch in chunks.chunks_mut(EMBED_BATCH) {
        let texts: Vec<String> = batch.iter().map(|c| lossy(&c.text)).collect();
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
    Ok(())
}

/// The run-level embedding check (E3 b), plus the probed dimension (live only).
async fn embed_check(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    health: &Health,
    live: bool,
) -> Result<(Option<EmbedCheck>, Option<usize>), SearchError> {
    let Some(t) = &cfg.embed else {
        return Ok((None, None));
    };
    let model_ok = health.embedding_model.as_deref() == Some(t.model.as_str());
    if !live {
        return Ok((
            Some(EmbedCheck {
                model: t.model.clone(),
                health_matches: model_ok,
            }),
            None,
        ));
    }
    let dim = probe_dimension(mgr, t).await?;
    let dim_ok = health.embedding_dim == u64::try_from(dim).ok();
    Ok((
        Some(EmbedCheck {
            model: t.model.clone(),
            health_matches: model_ok && dim_ok,
        }),
        Some(dim),
    ))
}

fn remote_from_states(
    states: &HashMap<String, crate::client::FileState>,
    paths: &[String],
) -> Vec<RemoteEntry> {
    paths
        .iter()
        .filter_map(|p| states.get(p))
        .map(|s| RemoteEntry {
            path: s.path.clone(),
            content_hash: s.content_hash.clone(),
            fully_embedded: s.has_embeddings == Some(true),
            embedding_space: s.embedding_space.clone(),
        })
        .collect()
}

fn writer_for(
    cfg: &FeedConfig,
    mode: Mode,
) -> Result<(CompanionReader, Option<CompanionWriter>), SearchError> {
    let reader = CompanionReader::new(&cfg.companion_url, &cfg.vault_id)?;
    Ok(match mode {
        Mode::Shadow => (reader, None),
        Mode::Live(proof) => (reader.clone(), Some(reader.into_writer(proof))),
    })
}

/// Builds, embeds and upserts `notes` (live only); a note with zero chunks is deleted instead
/// (the companion's upsert of nothing is a no-op, `client.ts:134-135`).
async fn push_notes(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    w: &CompanionWriter,
    notes: Vec<LocalNote>,
    dim: Option<usize>,
    report: &mut FeedReport,
) -> Result<(), SearchError> {
    let mut buffer: Vec<SearchChunk> = Vec::new();
    let mut empty: Vec<String> = Vec::new();
    for n in notes {
        let chunks = build_prepared_chunks(
            &n.prepared,
            &cfg.vault_id,
            &n.basename,
            n.mtime,
            cfg.max_chars,
            cfg.overlap_chars,
        );
        if chunks.is_empty() {
            empty.push(n.prepared.path.clone());
            continue;
        }
        report.upserted += 1;
        buffer.extend(chunks);
        if buffer.len() >= crate::client::UPSERT_BATCH_CHUNKS {
            flush(mgr, cfg, w, &mut buffer, dim, report).await?;
        }
    }
    flush(mgr, cfg, w, &mut buffer, dim, report).await?;
    if !empty.is_empty() {
        report.deleted += empty.len();
        w.delete_paths(&empty).await?;
    }
    Ok(())
}

async fn flush(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    w: &CompanionWriter,
    buffer: &mut Vec<SearchChunk>,
    dim: Option<usize>,
    report: &mut FeedReport,
) -> Result<(), SearchError> {
    if buffer.is_empty() {
        return Ok(());
    }
    if let Some(t) = &cfg.embed {
        embed_chunks(mgr, t, buffer, dim).await?;
    }
    w.upsert_chunks(buffer).await?;
    report.chunks += buffer.len();
    buffer.clear();
    Ok(())
}

/// `search_upsert` for one path. `note = None` means the path is gone or not indexable: a live
/// run deletes it from the companion; a shadow run counts it as `missing_local` if held.
pub async fn upsert_path(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    mode: Mode,
    path: &str,
    note: Option<LocalNote>,
) -> Result<FeedReport, SearchError> {
    let live = matches!(mode, Mode::Live(_));
    let (reader, writer) = writer_for(cfg, mode)?;
    let health = reader.health().await?;
    let (check, dim) = embed_check(mgr, cfg, &health, live).await?;
    let paths = [path.to_owned()];
    let states = reader.file_states(&paths).await?;
    let remote = remote_from_states(&states, &paths);
    let mut report = FeedReport::default();
    let Some(note) = note else {
        report.counts.missing_local = remote.len();
        if let (Some(w), false) = (&writer, remote.is_empty()) {
            w.delete_paths(&paths).await?;
            report.deleted = 1;
        }
        return Ok(report);
    };
    let local = [(path.to_owned(), note.prepared.content_hash.clone())];
    let cmp = compare(&local, &remote, check.as_ref(), false);
    report.counts = cmp.counts();
    if let Some(w) = &writer
        && cmp.matched.is_empty()
    {
        push_notes(mgr, cfg, w, vec![note], dim, &mut report).await?;
    }
    Ok(report)
}

/// The reconcile sweep: `local` are every indexable note's `(path, contentHash)`; `load`
/// re-prepares one note for upsert (called only for paths that need it, live only).
pub async fn reconcile(
    mgr: &ProviderManager,
    cfg: &FeedConfig,
    mode: Mode,
    local: &[(String, String)],
    load: &dyn Fn(&str) -> Result<Option<LocalNote>, SearchError>,
) -> Result<FeedReport, SearchError> {
    let live = matches!(mode, Mode::Live(_));
    let (reader, writer) = writer_for(cfg, mode)?;
    let health = reader.health().await?;
    let (check, dim) = embed_check(mgr, cfg, &health, live).await?;
    let listed = reader.list_paths().await?;
    let mut remote: Vec<RemoteEntry> = listed
        .iter()
        .map(|p| RemoteEntry {
            path: p.path.clone(),
            content_hash: p.content_hash.clone(),
            fully_embedded: p.chunk_count > 0 && p.embedded_count == p.chunk_count,
            embedding_space: None,
        })
        .collect();
    if check.is_some() {
        // E3 (a): refine hash-matched paths with `/v1/files/state` (it reports the space).
        let local_hash: HashMap<&str, &str> = local
            .iter()
            .map(|(p, h)| (p.as_str(), h.as_str()))
            .collect();
        let candidates: Vec<String> = remote
            .iter()
            .filter(|r| {
                r.content_hash.as_deref().is_some()
                    && local_hash.get(r.path.as_str()).copied() == r.content_hash.as_deref()
            })
            .map(|r| r.path.clone())
            .collect();
        let states = reader.file_states(&candidates).await?;
        for r in &mut remote {
            if let Some(s) = states.get(&r.path) {
                r.fully_embedded = s.has_embeddings == Some(true);
                r.embedding_space = s.embedding_space.clone();
            }
        }
    }
    let cmp = compare(local, &remote, check.as_ref(), true);
    let mut report = FeedReport {
        counts: cmp.counts(),
        ..FeedReport::default()
    };
    let Some(w) = writer else {
        return Ok(report);
    };
    let mut batch = Vec::new();
    for path in cmp.to_upsert() {
        if let Some(n) = load(&path)? {
            batch.push(n);
        }
        if batch.len() >= 64 {
            push_notes(mgr, cfg, &w, std::mem::take(&mut batch), dim, &mut report).await?;
        }
    }
    push_notes(mgr, cfg, &w, batch, dim, &mut report).await?;
    if !cmp.missing_local.is_empty() {
        w.delete_paths(&cmp.missing_local).await?;
        report.deleted += cmp.missing_local.len();
    }
    Ok(report)
}

/// `search_delete`: live drops the path's chunks; shadow only reports whether the companion
/// holds it (`missing_local`).
pub async fn delete_path(
    cfg: &FeedConfig,
    mode: Mode,
    path: &str,
) -> Result<FeedReport, SearchError> {
    let (reader, writer) = writer_for(cfg, mode)?;
    reader.health().await?;
    let mut report = FeedReport::default();
    let paths = [path.to_owned()];
    if let Some(w) = writer {
        w.delete_paths(&paths).await?;
        report.deleted = 1;
    } else {
        report.counts.missing_local = reader.file_states(&paths).await?.len();
    }
    Ok(report)
}
