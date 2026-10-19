//! WP-R18 (M5): the one whole-vault link resolver cache, shared (`Arc`) by Reading and the
//! panels. Built from one `file_paths` scan of the daemon index.
//!
//! Staleness rule (the stricter of the two former per-consumer rules): the cache is rebuilt
//! when it was invalidated (a file was created, deleted or renamed — incremental indexing keeps
//! the generation, so a generation key alone never sees new files), when the index generation
//! differs from the one it was built at, **or when the generation is unknown** (`None`: no
//! key can prove it fresh).

use std::collections::HashMap;
use std::sync::Mutex;

use cv_index::Resolver;

use crate::daemon::DaemonClient;

#[derive(Debug)]
struct Built {
    generation: Option<u64>,
    resolver: Resolver,
    ids: HashMap<String, i64>,
    paths: HashMap<i64, String>,
}

#[derive(Debug, Default)]
struct Inner {
    built: Option<Built>,
    /// Bumped by [`ResolverCache::invalidate`].
    epoch: u64,
    /// Bumped by every rebuild and every invalidation.
    version: u64,
}

/// The shared resolver cache (interior mutability: consumers hold an `Arc`).
#[derive(Debug, Default)]
pub struct ResolverCache {
    inner: Mutex<Inner>,
}

impl ResolverCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Drops the cache (a vault file was created, deleted or renamed).
    pub fn invalidate(&self) {
        let mut g = self.lock();
        g.built = None;
        g.epoch += 1;
        g.version += 1;
    }

    /// Invalidation count: Reading re-renders Docs resolved under an older epoch.
    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    /// Changes on every rebuild and invalidation: a consumer that derived data from the
    /// resolver recomputes when this moves.
    pub fn version(&self) -> u64 {
        self.lock().version
    }

    /// Whether a resolver is built (no daemon / failed scan = nothing known).
    pub fn is_built(&self) -> bool {
        self.lock().built.is_some()
    }

    /// Rebuilds from `d`'s index when stale (see the module doc).
    pub fn refresh(&self, d: &dyn DaemonClient) {
        let generation = d.index().generation();
        if self
            .lock()
            .built
            .as_ref()
            .is_some_and(|b| generation.is_some() && b.generation == generation)
        {
            return;
        }
        let t0 = std::time::Instant::now();
        // One scan (editor-usability-round WP-5): the old per-folder walk was quadratic over
        // folders (docs/spikes/panels-lag/FINDINGS.md).
        match d.index().with_query(|q| q.file_paths()) {
            Ok(rows) => {
                tracing::debug!(rows = rows.len(), "resolver rows");
                let mut resolver = Resolver::default();
                let mut ids = HashMap::new();
                let mut paths = HashMap::new();
                for (id, path) in rows {
                    resolver.add(id, &path);
                    ids.insert(path.clone(), id);
                    paths.insert(id, path);
                }
                let mut g = self.lock();
                g.built = Some(Built {
                    generation,
                    resolver,
                    ids,
                    paths,
                });
                g.version += 1;
            }
            Err(e) => tracing::warn!(error = %e, "resolver rebuild failed"),
        }
        tracing::debug!(
            elapsed_ms = t0.elapsed().as_secs_f64() * 1e3,
            "refresh_resolver"
        );
    }

    /// Resolves `target` written in `src` to a vault path.
    pub fn resolve(&self, src: &str, target: &str) -> Option<String> {
        let g = self.lock();
        let b = g.built.as_ref()?;
        let src_id = b.ids.get(src).copied().unwrap_or(-1);
        let id = b.resolver.resolve(src, src_id, target)?;
        b.paths.get(&id).cloned()
    }
}
