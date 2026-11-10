//! [`NoteIdSink`]: the durable note-id binding (plan note-identity D1), kept on the writer thread
//! at the identity-transition sites. The daemon's implementation stores
//! ids in `state.db`; `cv-vault` only calls it and stamps the returned id on the event it emits
//! ([`crate::VaultEvent::id`]). Every [`crate::VaultWriter`] has one; a store failure is a
//! [`NoteIdError`], never an absent id (plan tn-review-2026-11-10-remediation Decision 4).

pub use cv_protocol::{NoteId, NoteLocation};

/// The id store failed (the binding was not recorded or could not be read).
#[derive(Debug, thiserror::Error)]
#[error("note id store: {0}")]
pub struct NoteIdError(pub String);

/// `Result` with [`NoteIdError`].
pub type IdResult<T> = std::result::Result<T, NoteIdError>;

/// Keeps every id → path binding current. Called on the writer thread after the mutation
/// succeeded and before the mutating call returns; implementations must not call back into the
/// writer. Each method returns the id concerned, or the store's failure.
pub trait NoteIdSink: Send {
    /// The id of the file at `path`, minted if it has none.
    fn bind(&mut self, path: &str) -> IdResult<NoteId>;
    /// The file at `from` now lives at `to`; returns its (unchanged) id.
    fn moved(&mut self, from: &str, to: &str) -> IdResult<NoteId>;
    /// The file at `path` is gone (absorbed by the file at `replaced_by`, if given); returns the
    /// retired id.
    fn retired(&mut self, path: &str, replaced_by: Option<&str>) -> IdResult<NoteId>;
    /// Makes the retired `id` live again at `path` (D3); `false` if it cannot be (unknown, live
    /// elsewhere, or `path` holds another live id).
    fn revive(&mut self, id: NoteId, path: &str) -> IdResult<bool>;
    /// Where `id` is now (`Unknown` is a value, not an error).
    fn locate(&mut self, id: NoteId) -> IdResult<NoteLocation>;
}
