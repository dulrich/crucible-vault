//! [`LineageSink`]: the path-identity transitions the writer (and the watcher, for External
//! changes) records, in mutation order, synchronously on the writer thread (plan
//! note-path-continuity D2). The daemon's implementation persists them to `state.db` and assigns
//! the sequence numbers; `cv-vault` only calls it.

/// Receives every identity transition before the mutating call returns. Plain content writes are
/// not identity transitions. Runs on the writer thread: implementations must not call back into
/// the writer.
pub trait LineageSink: Send {
    /// A new incarnation starts at `path` (create, or an External file appearing).
    fn created(&mut self, path: &str);
    /// The note at `from` now lives at `to`.
    fn renamed(&mut self, from: &str, to: &str);
    /// The note at `from` was deleted because `by` absorbed it.
    fn replaced(&mut self, from: &str, by: &str);
    /// The note at `path` is gone.
    fn deleted(&mut self, path: &str);
}

/// The default sink: records nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLineage;

impl LineageSink for NoLineage {
    fn created(&mut self, _: &str) {}
    fn renamed(&mut self, _: &str, _: &str) {}
    fn replaced(&mut self, _: &str, _: &str) {}
    fn deleted(&mut self, _: &str) {}
}
