//! The daemon's one shared `semsearch` handle (plan semsearch-crate Decision 7): opened lazily on
//! first use by a live search step, then shared by the job runner and (WP-4) the request path,
//! so a completed write is visible to the next query without reopening.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use semsearch::{Index, IndexOptions};

use crate::error::SearchError;

/// A shared, lock-guarded index (`semsearch::Index` is `Send` but not `Sync`). Hold the lock
/// only around index calls, never across an embed call.
pub type SharedIndex = Arc<Mutex<Index>>;

/// `CV_SEARCH_DB` plus its lazily opened shared handle.
#[derive(Debug)]
pub struct SearchDb {
    path: PathBuf,
    cell: Mutex<Option<SharedIndex>>,
}

impl SearchDb {
    /// A handle for `path`; nothing is opened yet.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cell: Mutex::new(None),
        }
    }

    /// The index file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The shared writable handle, opening (and creating) the file on first call. An open
    /// error is returned and retried on the next call.
    pub fn shared(&self) -> Result<SharedIndex, SearchError> {
        let mut cell = self.cell.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = cell.as_ref() {
            return Ok(Arc::clone(i));
        }
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| SearchError::Index(format!("creating {}: {e}", parent.display())))?;
        }
        retire_legacy_space(&self.path)?;
        let index = Arc::new(Mutex::new(Index::open(
            &self.path,
            &IndexOptions::default(),
        )?));
        *cell = Some(Arc::clone(&index));
        Ok(index)
    }

    /// Whether the shared handle has been opened.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.cell
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
}

/// The pre-0.1.1 recorded space label of the `bge-m3-f16` embedder.
const LEGACY_SPACE_MODEL: &str = "bge-m3/f16";

/// One-time 0.1.1 transition; deleted in plan tn-review-residue-cleanup WP-X6.
///
/// An index recording the legacy space label `bge-m3/f16` is rebuilt from scratch: the derived
/// file and its `-wal`/`-shm` are deleted, so the next open has no recorded space and no
/// hash-current source, and the next reconcile re-embeds every note into `bge-m3-f16`. (A bare
/// `reset_space` would leave the source content hashes current, so nothing would be re-fed.)
/// Returns whether the file was removed. A missing file or any other recorded space is untouched.
fn retire_legacy_space(path: &Path) -> Result<bool, SearchError> {
    let legacy = match Index::open_read_only(path, &IndexOptions::default()) {
        Ok(idx) => idx.space().is_some_and(|s| s.model == LEGACY_SPACE_MODEL),
        Err(semsearch::Error::NotFound(_)) => false,
        Err(e) => return Err(e.into()),
    };
    if !legacy {
        return Ok(false);
    }
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        match std::fs::remove_file(PathBuf::from(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(SearchError::Index(format!(
                    "removing legacy search index {}: {e}",
                    path.display()
                )));
            }
        }
    }
    tracing::info!(
        path = %path.display(),
        "search index recorded the legacy space {LEGACY_SPACE_MODEL}; rebuilding from scratch (re-embeds every note)"
    );
    Ok(true)
}

/// Locks `index`, recovering from poison (the index's own transactions keep it consistent).
pub fn lock(index: &Mutex<Index>) -> std::sync::MutexGuard<'_, Index> {
    index.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use semsearch::{Chunk, Space};

    fn temp_db(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cv-search-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("search.db")
    }

    fn seed(path: &Path, model: &str) {
        let mut idx = Index::open(path, &IndexOptions::default()).unwrap();
        let chunk = Chunk::new("c0", "t", "", "body").with_embedding(vec![1.0, 0.0, 0.0]);
        idx.upsert_source_in(&Space::new(model, 3), "a.md", "h1", &[chunk])
            .unwrap();
    }

    #[test]
    fn legacy_space_is_rebuilt_empty_and_current_space_is_kept() {
        let legacy = temp_db("legacy");
        seed(&legacy, LEGACY_SPACE_MODEL);
        let db = SearchDb::new(legacy.clone());
        let shared = db.shared().unwrap();
        {
            let idx = lock(&shared);
            assert!(idx.space().is_none(), "no recorded space after rebuild");
            assert!(
                idx.source_states().unwrap().is_empty(),
                "no hash-current source"
            );
        }
        drop(shared);

        let current = temp_db("current");
        seed(&current, "bge-m3-f16");
        assert!(!retire_legacy_space(&current).unwrap());
        let idx = Index::open(&current, &IndexOptions::default()).unwrap();
        assert_eq!(idx.space().unwrap().model, "bge-m3-f16");
        assert_eq!(idx.source_states().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
        let _ = std::fs::remove_dir_all(current.parent().unwrap());
    }
}
