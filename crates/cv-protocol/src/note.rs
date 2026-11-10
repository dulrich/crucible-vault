//! Note identity and file-change types: the note id, `bind_note` / `locate_note`, and one vault
//! event's `file_changed` notifications (plan note-identity D1, D2, D7).

use serde::{Deserialize, Serialize};

use crate::Notification;

/// A daemon-minted, never-reused note id: the identity of one vault file across renames (plan
/// note-identity D1). A bare integer on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NoteId(pub u64);

/// `bind_note` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindNoteParams {
    pub path: String,
}

/// `bind_note` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindNoteResult {
    pub id: NoteId,
}

/// `locate_note` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocateNoteParams {
    pub id: NoteId,
}

/// `locate_note` result: where a note id is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NoteLocation {
    /// Bound to this live path.
    Live { path: String },
    /// Retired: deleted, or absorbed by the note `replaced_by`.
    Gone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaced_by: Option<NoteId>,
    },
    /// Never minted, or its tombstone was pruned.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Daemon,
    External,
}

/// One vault event, in protocol types: the input of [`file_changed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    pub hash: Option<String>,
    pub origin: Origin,
    /// A rename's source path (`kind` is then [`ChangeKind::Renamed`]).
    pub from: Option<String>,
    pub replaced_by: Option<String>,
    pub id: Option<NoteId>,
}

/// One vault event's `file_changed` notifications (plan note-identity D7: the daemon bridge and
/// the editor's test daemon both build them here). A rename (`from` set) reports the destination
/// as `Renamed { from }` first, then its source as `Deleted`: a client that follows moves re-keys
/// on the first and finds nothing open for the second, while clients without a rename record
/// still drop the old path. Both halves carry the same `id`.
#[must_use]
pub fn file_changed(c: FileChange) -> Vec<Notification> {
    let mut out = Vec::with_capacity(2);
    out.push(Notification::FileChanged {
        path: c.path,
        kind: c.kind,
        hash: c.hash,
        origin: c.origin,
        from: c.from.clone(),
        replaced_by: c.replaced_by,
        id: c.id,
    });
    if let Some(from) = c.from {
        out.push(Notification::FileChanged {
            path: from,
            kind: ChangeKind::Deleted,
            hash: None,
            origin: c.origin,
            from: None,
            replaced_by: None,
            id: c.id,
        });
    }
    out
}
