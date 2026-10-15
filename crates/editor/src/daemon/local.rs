//! Read-only `index.db` handle, reopened on `index_generation` (plan Public Interfaces, D1).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cv_index::{Query, Reader};

use super::{DaemonError, Result};

/// One read-only [`Reader`] behind a mutex, lazily (re)opened.
pub struct LocalIndex {
    base: PathBuf,
    reader: Mutex<Option<Reader>>,
}

impl std::fmt::Debug for LocalIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalIndex")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl LocalIndex {
    /// A handle on the index whose base path is `base` (`hello.index_db`). Opens lazily.
    pub fn new(base: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
            reader: Mutex::new(None),
        }
    }

    /// The generation currently open, if any.
    pub fn generation(&self) -> Option<u64> {
        self.reader
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(Reader::generation))
    }

    /// Called on `index_generation {n}`: reopens generation `n` (falls back to the current one).
    pub fn on_generation(&self, n: u64) -> Result<()> {
        let fresh = Reader::open(&self.base, n).or_else(|_| Reader::open_current(&self.base))?;
        let mut g = self.reader.lock().map_err(|_| poisoned())?;
        *g = Some(fresh);
        Ok(())
    }

    /// Runs `f` against the open generation in one read snapshot, opening the current generation
    /// first if needed and retrying once on a fresh handle if the read fails (a retired file).
    pub fn with_query<T>(&self, f: impl Fn(Query<'_>) -> cv_index::Result<T>) -> Result<T> {
        let mut g = self.reader.lock().map_err(|_| poisoned())?;
        if let Some(r) = g.as_mut()
            && let Ok(v) = r.snapshot(&f)
        {
            return Ok(v);
        }
        let mut fresh = Reader::open_current(&self.base)?;
        let v = fresh.snapshot(&f)?;
        *g = Some(fresh);
        Ok(v)
    }
}

impl LocalIndex {
    /// Whether the index holds `path` (plan editor-ui-round-3 WP-2, audit B1). A miss on the
    /// open generation is re-checked on the current one without switching to it: a rebuild may
    /// have committed before its `index_generation` arrived.
    pub fn holds(&self, path: &str) -> bool {
        if self.with_query(|q| q.file(path)).is_ok_and(|r| r.is_some()) {
            return true;
        }
        Reader::open_current(&self.base)
            .and_then(|r| r.query().file(path))
            .is_ok_and(|r| r.is_some())
    }
}

fn poisoned() -> DaemonError {
    DaemonError::Protocol("index handle lock poisoned".into())
}
