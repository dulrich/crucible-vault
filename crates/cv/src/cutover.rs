//! The cut-over marker (plan `cut-over-now` WP-1): the state.db-backed [`CutoverMarker`] the live
//! daemon consults before writing the protected vault. The `cv cutover` CLI that created it was
//! retired (plan tn-review-baseline-remediation WP-R5); the marker row is seeded directly.

use std::path::Path;
use std::sync::{Arc, Mutex};

use cv_state::StateDb;
use cv_vault::{CutoverMarker, VaultError};

/// The live daemon's marker: present iff state.db holds a marker whose vault is the canonical
/// protected root.
#[derive(Debug, Clone)]
pub struct StateCutover {
    state: Arc<Mutex<StateDb>>,
    protected: String,
}

impl StateCutover {
    /// Marker over the daemon's shared `state` for `protected_root`.
    #[must_use]
    pub fn new(state: Arc<Mutex<StateDb>>, protected_root: &Path) -> Self {
        Self {
            state,
            protected: canonical(protected_root),
        }
    }
}

impl CutoverMarker for StateCutover {
    fn present(&self) -> cv_vault::Result<bool> {
        let db = crate::sync::lock(&self.state);
        let rec = db.cutover().map_err(|e| VaultError::Io {
            path: "state.db (cut-over marker)".to_owned(),
            source: std::io::Error::other(e),
        })?;
        Ok(rec.is_some_and(|r| r.vault == self.protected))
    }
}

fn canonical(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}
