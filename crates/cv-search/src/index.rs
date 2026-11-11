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

/// `CV_SEARCH_DB` plus its lazily opened shared handle. Clones share one handle cell.
#[derive(Debug, Clone)]
pub struct SearchDb {
    path: PathBuf,
    cell: Arc<Mutex<Option<SharedIndex>>>,
}

impl SearchDb {
    /// A handle for `path`; nothing is opened yet.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cell: Arc::new(Mutex::new(None)),
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
        let index = Arc::new(Mutex::new(Index::open(
            &self.path,
            &IndexOptions::default(),
        )?));
        *cell = Some(Arc::clone(&index));
        Ok(index)
    }

    /// The shared handle if it is already open; never opens it (memory gauges, plan
    /// memory-instrumentation-and-reduction WP-M1).
    #[must_use]
    pub fn if_open(&self) -> Option<SharedIndex> {
        self.cell
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(Arc::clone)
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

/// Locks `index`, recovering from poison (the index's own transactions keep it consistent).
pub fn lock(index: &Mutex<Index>) -> std::sync::MutexGuard<'_, Index> {
    index.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_shares_the_opened_index() {
        let dir = std::env::temp_dir().join(format!("cv-search-clone-{}", std::process::id()));
        let db = SearchDb::new(dir.join("search.db"));
        let other = db.clone();
        assert!(!other.is_open());
        let a = db.shared().expect("open");
        let b = other.if_open().expect("opened through the original");
        assert!(Arc::ptr_eq(&a, &b));
        assert!(Arc::ptr_eq(&a, &other.shared().expect("shared")));
        drop((a, b, db, other));
        let _ = std::fs::remove_dir_all(dir);
    }
}
