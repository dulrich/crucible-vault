//! Path lineage in the daemon (plan note-path-continuity WP-2, D2): the `state.db` sink the writer
//! records every identity transition through, and the one resolution both `resolve_path` and the
//! job runner use.
//!
//! The sink persists **synchronously on the writer thread**, inside the mutation, so an edge is
//! durable before the mutating call returns. It locks the shared `state.db` mutex there: nothing
//! may hold that mutex while blocking on a writer call (the deadlock rule; see the WP-2 report).

use std::path::Path;
use std::sync::{Arc, Mutex};

use cv_protocol::{PathResolution, ResolvePathResult};
use cv_state::{LINEAGE_RETENTION_MS, LineageEnd, LineageKind, StateDb, StateError};
use cv_vault::{LineageSink, VaultWriter};

use crate::now_ms;
use crate::sync::lock;

/// Appends between two retention passes (one more runs at daemon start).
const PRUNE_EVERY: u32 = 4096;

/// The daemon's [`LineageSink`]: one `path_lineage` row per transition.
pub struct StateLineage {
    state: Arc<Mutex<StateDb>>,
    appends: u32,
}

impl StateLineage {
    /// A sink over `state`.
    pub fn new(state: Arc<Mutex<StateDb>>) -> Self {
        Self { state, appends: 0 }
    }

    fn append(&mut self, kind: LineageKind, from: &str, to: Option<&str>) -> Option<u64> {
        let now = now_ms();
        let mut db = lock(&self.state);
        let seq = match db.lineage_append(kind, from, to, now) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(error = %e, kind = kind.as_str(), "lineage: transition not recorded");
                None
            }
        };
        self.appends += 1;
        if self.appends >= PRUNE_EVERY {
            self.appends = 0;
            prune(&mut db, now);
        }
        seq
    }
}

/// One retention pass (rows older than [`LINEAGE_RETENTION_MS`]).
pub fn prune(db: &mut StateDb, now: i64) {
    match db.lineage_prune(now.saturating_sub(LINEAGE_RETENTION_MS)) {
        Ok(0) => {}
        Ok(n) => tracing::debug!(pruned = n, "lineage: retention pass"),
        Err(e) => tracing::warn!(error = %e, "lineage: retention pass failed"),
    }
}

impl LineageSink for StateLineage {
    fn created(&mut self, path: &str) -> Option<u64> {
        self.append(LineageKind::Created, path, None)
    }
    fn renamed(&mut self, from: &str, to: &str) -> Option<u64> {
        self.append(LineageKind::Renamed, from, Some(to))
    }
    fn replaced(&mut self, from: &str, by: &str) -> Option<u64> {
        self.append(LineageKind::Replaced, from, Some(by))
    }
    fn deleted(&mut self, path: &str) -> Option<u64> {
        self.append(LineageKind::Deleted, path, None)
    }
}

/// Installs the sink on `writer` (before the writer thread starts) and runs one retention pass.
pub fn install(writer: &mut VaultWriter, state: &Arc<Mutex<StateDb>>) {
    prune(&mut lock(state), now_ms());
    writer.set_lineage_sink(Box::new(StateLineage::new(Arc::clone(state))));
}

fn exists(root: &Path, rel: &str) -> bool {
    cv_vault::validate_rel_path(rel).is_ok() && root.join(rel).is_file()
}

/// Resolves the note bound at `path` as of `since` (`None` = no cursor: the path as it is now).
/// The chain end must exist on disk under `root` to be `Live`/`Moved`; otherwise `Unknown`.
/// Locks `state` briefly (never across a writer call).
pub fn resolve(
    state: &Mutex<StateDb>,
    root: &Path,
    path: &str,
    since: Option<u64>,
) -> Result<ResolvePathResult, StateError> {
    let (end, head) = {
        let db = lock(state);
        match since {
            Some(c) => {
                let r = db.lineage_resolve(path, c)?;
                (r.end, r.head)
            }
            None => (LineageEnd::Unchanged, db.lineage_head()?),
        }
    };
    let resolution = match end {
        LineageEnd::Unchanged if exists(root, path) => PathResolution::Live {
            path: path.to_owned(),
        },
        LineageEnd::Moved(to) if exists(root, &to) => PathResolution::Moved { to },
        LineageEnd::Deleted => PathResolution::Deleted,
        LineageEnd::Unchanged | LineageEnd::Moved(_) | LineageEnd::Stale => PathResolution::Unknown,
    };
    Ok(ResolvePathResult { resolution, head })
}
