//! [`NoteIdSink`]: the durable note-id binding (plan note-identity D1), kept on the writer thread
//! at the identity-transition sites. The daemon's implementation stores
//! ids in `state.db`; `cv-vault` only calls it and stamps the returned id on the event it emits
//! ([`crate::VaultEvent::id`]). Without an installed sink ([`NoNoteIds`]) events carry no id.

/// A daemon-minted, never-reused note id: the identity of one vault file across renames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NoteId(pub u64);

/// Where a note id is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteLocation {
    /// Bound to this live path.
    Live { path: String },
    /// Retired: deleted, or absorbed by the note `replaced_by`.
    Gone { replaced_by: Option<NoteId> },
    /// Never minted, or its tombstone was pruned.
    Unknown,
}

/// Keeps every id → path binding current. Called on the writer thread after the mutation
/// succeeded and before the mutating call returns; implementations must not call back into the
/// writer. Each method returns the id concerned (`None` = not recorded, e.g. a store failure).
pub trait NoteIdSink: Send {
    /// The id of the file at `path`, minted if it has none.
    fn bind(&mut self, path: &str) -> Option<NoteId>;
    /// The file at `from` now lives at `to`; returns its (unchanged) id.
    fn moved(&mut self, from: &str, to: &str) -> Option<NoteId>;
    /// The file at `path` is gone (absorbed by the file at `replaced_by`, if given); returns the
    /// retired id.
    fn retired(&mut self, path: &str, replaced_by: Option<&str>) -> Option<NoteId>;
    /// Makes the retired `id` live again at `path` (D3); `None` if it cannot be.
    fn revive(&mut self, id: NoteId, path: &str) -> Option<NoteId>;
    /// Where `id` is now (`None` = no store to ask).
    fn locate(&mut self, id: NoteId) -> Option<NoteLocation>;
}

/// The default sink: binds nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoNoteIds;

impl NoteIdSink for NoNoteIds {
    fn bind(&mut self, _: &str) -> Option<NoteId> {
        None
    }
    fn moved(&mut self, _: &str, _: &str) -> Option<NoteId> {
        None
    }
    fn retired(&mut self, _: &str, _: Option<&str>) -> Option<NoteId> {
        None
    }
    fn revive(&mut self, _: NoteId, _: &str) -> Option<NoteId> {
        None
    }
    fn locate(&mut self, _: NoteId) -> Option<NoteLocation> {
        None
    }
}
