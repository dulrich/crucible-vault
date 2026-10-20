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

/// Locks `index`, recovering from poison (the index's own transactions keep it consistent).
pub fn lock(index: &Mutex<Index>) -> std::sync::MutexGuard<'_, Index> {
    index.lock().unwrap_or_else(PoisonError::into_inner)
}
