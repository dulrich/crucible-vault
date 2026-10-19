//! The daemon's `search` request (plan semsearch-crate WP-4, D22): hybrid search over the one
//! shared `semsearch` handle (Decision 7), so a completed feed write is visible to the next query.
//!
//! **Query embedding (Decision 4).** The target is the `embed` of the first `search_upsert`
//! step ([`crate::steps::search::query_embed_target`]). It is used only when
//! its model id equals the index's recorded space model and the
//! index holds embedded chunks. Otherwise — no `embed`, no recorded space, an empty index, a
//! space mismatch, any embedder failure, or a width mismatch — the query is keyword-only and
//! `semantic` is false; an embedder failure is a `warn` log, never an error.
//!
//! The embed call is async and never holds the index lock; every index call runs on a blocking
//! thread.

use cv_actions::ConfigSet;
use cv_protocol::{SearchHit, SearchParams, SearchResult};
use cv_providers::ProviderManager;
use cv_search::SearchDb;
use cv_search::semsearch::Query;

/// Upper bound for `limit`.
pub const MAX_LIMIT: u32 = 1000;

/// Runs one `search` request. Errors are index open/SQL failures only.
pub async fn search(
    db: &SearchDb,
    config: &ConfigSet,
    mgr: &ProviderManager,
    params: &SearchParams,
) -> Result<SearchResult, String> {
    let text = params.query.trim().to_owned();
    if text.is_empty() {
        return Ok(SearchResult {
            hits: Vec::new(),
            semantic: false,
        });
    }
    let limit = params.limit.clamp(1, MAX_LIMIT) as usize;
    let shared = db.shared().map_err(|e| e.to_string())?;

    let probe = std::sync::Arc::clone(&shared);
    let space = tokio::task::spawn_blocking(move || {
        let idx = cv_search::index::lock(&probe);
        idx.space().cloned().filter(|_| idx.embedded_count() > 0)
    })
    .await
    .map_err(|e| e.to_string())?;

    let mut vector = None;
    if let (Some(space), Some(t)) = (space, crate::steps::search::query_embed_target(config))
        && t.model == space.model
    {
        match mgr
            .embed(&t.provider, &t.model, std::slice::from_ref(&text))
            .await
        {
            Ok(out) => match out.vectors.into_iter().next() {
                Some(v) if v.len() == space.dim => vector = Some(v),
                other => tracing::warn!(
                    width = other.map(|v| v.len()),
                    dim = space.dim,
                    "search: query embedding width mismatch; keyword-only"
                ),
            },
            Err(e) => {
                tracing::warn!(provider = %t.provider.id, error = %e, "search: embedder failed; keyword-only");
            }
        }
    }

    let semantic = vector.is_some();
    let hits = tokio::task::spawn_blocking(move || {
        let mut q = Query::new(&text).with_limit(limit);
        if let Some(v) = vector {
            q = q.with_vector(v);
        }
        cv_search::index::lock(&shared).search(&q)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    Ok(SearchResult {
        hits: hits
            .into_iter()
            .map(|h| SearchHit {
                path: h.source,
                title: h.title,
                heading: h.heading,
                snippet: h.snippet,
                score: h.score,
            })
            .collect(),
        semantic,
    })
}
