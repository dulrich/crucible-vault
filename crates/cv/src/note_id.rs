//! Note ids in the daemon (plan note-identity WP-1, D1): the `state.db` sink the writer keeps
//! every id → path binding current through, at the identity-transition sites.
//!
//! The sink persists **synchronously on the writer thread**, inside the mutation, so
//! a binding is durable before the mutating call returns, and it locks the shared `state.db` mutex
//! there: nothing may hold that mutex while blocking on a writer call. A store failure is logged
//! and the event carries no id (never a guessed one). WP-2 answers `bind_note` / `locate_note`
//! through [`cv_vault::WriterHandle::bind_note`] / [`cv_vault::WriterHandle::locate_note`], which
//! run on the writer thread and so observe every mutation that returned before them.

use std::sync::{Arc, Mutex};

use cv_state::{NOTE_ID_RETENTION_MS, StateDb, StateError};
use cv_vault::{IdResult, NoteId, NoteIdError, NoteIdSink, NoteLocation, VaultWriter};

use crate::now_ms;
use crate::sync::lock;

/// Retirements between two tombstone retention passes (one more runs at daemon start).
const PRUNE_EVERY: u32 = 4096;

/// The daemon's [`NoteIdSink`] over `state.db`'s `note_ids`.
pub struct StateNoteIds {
    state: Arc<Mutex<StateDb>>,
    retirements: u32,
}

impl StateNoteIds {
    /// A sink over `state`.
    pub fn new(state: Arc<Mutex<StateDb>>) -> Self {
        Self {
            state,
            retirements: 0,
        }
    }

    fn run<T>(
        &mut self,
        what: &'static str,
        f: impl FnOnce(&mut StateDb, i64) -> Result<T, StateError>,
    ) -> IdResult<T> {
        let now = now_ms();
        let mut db = lock(&self.state);
        f(&mut db, now).map_err(|e| {
            tracing::warn!(error = %e, op = what, "note id: binding not recorded");
            NoteIdError(format!("{what}: {e}"))
        })
    }
}

/// One tombstone retention pass (retired ids older than [`NOTE_ID_RETENTION_MS`]).
pub fn prune(db: &mut StateDb, now: i64) {
    match db.note_id_prune(now.saturating_sub(NOTE_ID_RETENTION_MS)) {
        Ok(0) => {}
        Ok(n) => tracing::debug!(pruned = n, "note id: retention pass"),
        Err(e) => tracing::warn!(error = %e, "note id: retention pass failed"),
    }
}

impl NoteIdSink for StateNoteIds {
    fn bind(&mut self, path: &str) -> IdResult<NoteId> {
        self.run("bind", |db, _| db.note_id_bind(path))
    }
    fn moved(&mut self, from: &str, to: &str) -> IdResult<NoteId> {
        self.run("move", |db, now| db.note_id_move(from, to, now))
    }
    fn retired(&mut self, path: &str, replaced_by: Option<&str>) -> IdResult<NoteId> {
        let id = self.run("retire", |db, now| {
            let by = replaced_by.map(|b| db.note_id_bind(b)).transpose()?;
            db.note_id_retire(path, by, now)
        });
        self.retirements += 1;
        if self.retirements >= PRUNE_EVERY {
            self.retirements = 0;
            prune(&mut lock(&self.state), now_ms());
        }
        id
    }
    fn revive(&mut self, id: NoteId, path: &str) -> IdResult<bool> {
        self.run("revive", |db, _| db.note_id_revive(id, path))
    }
    fn locate(&mut self, id: NoteId) -> IdResult<NoteLocation> {
        self.run("locate", |db, _| db.note_id_locate(id))
    }
}

/// The daemon's sink over `state`, for [`VaultWriter::open`].
pub fn sink(state: &Arc<Mutex<StateDb>>) -> Box<dyn NoteIdSink> {
    Box::new(StateNoteIds::new(Arc::clone(state)))
}

/// Startup pass on the opened (reconciled) writer, before its thread starts: one retention pass,
/// then the repair (plan tn-review-2026-11-10-remediation Decision 3).
pub fn install(writer: &VaultWriter, state: &Arc<Mutex<StateDb>>) {
    let now = now_ms();
    let mut db = lock(state);
    prune(&mut db, now);
    repair(&mut db, writer, now);
}

/// Retires each live id row whose file is absent, exactly as a delete seen while running
/// (`note_id_retire`; the tombstone then ages out under the normal retention). The index's file
/// set is only a fast positive check — it omits `.gitignore`d files that exist and can be bound
/// (audit F1) — so every other row is confirmed `NotFound` on the filesystem first; an inspection
/// error is not absence and keeps the row.
fn repair(db: &mut StateDb, writer: &VaultWriter, now: i64) {
    let live = match db.note_id_live_paths() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "note id repair: listing live rows failed; skipped");
            return;
        }
    };
    let indexed: std::collections::HashSet<String> = match writer.index().query().file_paths() {
        Ok(v) => v.into_iter().map(|(_, p)| p).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "note id repair: index file list failed; checking every row");
            std::collections::HashSet::new()
        }
    };
    let root = writer.fs().root();
    let mut retired = 0usize;
    for path in live {
        if indexed.contains(&path) {
            continue;
        }
        match std::fs::symlink_metadata(root.join(&path)) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match db.note_id_retire(&path, None, now) {
                    Ok(_) => retired += 1,
                    Err(e) => tracing::warn!(error = %e, "note id repair: retire failed"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "note id repair: cannot inspect a file; row kept"),
        }
    }
    if retired > 0 {
        tracing::info!(retired, "note id repair: retired rows whose file is absent");
    }
}

#[cfg(test)]
#[path = "note_id_tests.rs"]
mod tests;
