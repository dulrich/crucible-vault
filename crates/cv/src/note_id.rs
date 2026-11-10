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
use cv_vault::{NoteId, NoteIdSink, NoteLocation, VaultWriter};

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
    ) -> Option<T> {
        let now = now_ms();
        let mut db = lock(&self.state);
        match f(&mut db, now) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, op = what, "note id: binding not recorded");
                None
            }
        }
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
    fn bind(&mut self, path: &str) -> Option<NoteId> {
        self.run("bind", |db, _| db.note_id_bind(path))
    }
    fn moved(&mut self, from: &str, to: &str) -> Option<NoteId> {
        self.run("move", |db, now| db.note_id_move(from, to, now))
    }
    fn retired(&mut self, path: &str, replaced_by: Option<&str>) -> Option<NoteId> {
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
    fn revive(&mut self, id: NoteId, path: &str) -> Option<NoteId> {
        self.run("revive", |db, _| db.note_id_revive(id, path))
            .and_then(|ok| ok.then_some(id))
    }
    fn locate(&mut self, id: NoteId) -> Option<NoteLocation> {
        self.run("locate", |db, _| db.note_id_locate(id))
    }
}

/// Installs the sink on `writer` (before the writer thread starts) and runs one retention pass.
pub fn install(writer: &mut VaultWriter, state: &Arc<Mutex<StateDb>>) {
    prune(&mut lock(state), now_ms());
    writer.set_note_id_sink(Box::new(StateNoteIds::new(Arc::clone(state))));
}

#[cfg(test)]
#[path = "note_id_tests.rs"]
mod tests;
