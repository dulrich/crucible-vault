//! Removal review (plan `intake-removals-sources-rescan` WP-4): resolving `[!removed]` fold
//! groups, trimming a body line range as removed, and finding the next note with a fold.

use serde::{Deserialize, Serialize};

use crate::NoteId;

/// `resolve_removals` params: one operation on the note `id`, CAS against `base_hash` (the
/// content hash of the whole file the client saw).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRemovalsParams {
    /// The note (resolved to its current path on the daemon's writer thread).
    pub id: NoteId,
    /// Hash of the file the client saw; a mismatch is [`RemovalRefusal::StaleHash`].
    pub base_hash: String,
    /// What to do.
    pub op: RemovalOp,
}

/// The three `resolve_removals` forms (`{"form": "group" | "all" | "trim", …}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemovalOp {
    /// One fold group, by the identity `cv_ingest::enrich::fold_groups` reports.
    Group {
        group: String,
        verdict: RemovalVerdict,
    },
    /// Every fold group of the note (a note without a fold applies with zero counts).
    All { verdict: RemovalVerdict },
    /// Remove the **body** lines `first..=last`: 0-based, inclusive, counted over the body with
    /// the frontmatter block removed, split at `'\n'`. A range touching a generated
    /// (`%% cv-enrich %%`) block is [`RemovalRefusal::Generated`]. `kind` labels the rows
    /// (absent = blank kind).
    Trim {
        first: usize,
        last: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<RemovalKind>,
    },
}

/// A fold-group verdict: `delete` removes the group for good, `keep` restores its lines to the
/// body (at its anchor, or at the end for an old-layout group) and records a false positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalVerdict {
    Delete,
    Keep,
}

impl RemovalVerdict {
    /// The wire / label string (`delete` | `keep`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Keep => "keep",
        }
    }
}

/// Where a removal label came from: a resolved `[!removed]` fold group or a trimmed selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalOrigin {
    Fold,
    Selection,
}

impl RemovalOrigin {
    /// The label string (`fold` | `selection`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fold => "fold",
            Self::Selection => "selection",
        }
    }
}

/// The span kinds a trim can be labelled with (the four the enrich model emits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
    Sponsor,
    Cta,
    Chrome,
    Footer,
}

impl RemovalKind {
    /// The wire / label string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sponsor => "sponsor",
            Self::Cta => "cta",
            Self::Chrome => "chrome",
            Self::Footer => "footer",
        }
    }
}

/// `resolve_removals` result: applied with counts, or a refusal that changed nothing (no note
/// write, no label row, no trash move).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ResolveRemovalsResult {
    Applied(RemovalsApplied),
    Refused {
        reason: RemovalRefusal,
        /// The file's current hash (stale-hash refusals only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_hash: Option<String>,
    },
}

/// Why a `resolve_removals` request changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalRefusal {
    /// The file's hash is not `base_hash`.
    StaleHash,
    /// No fold group has the requested identity.
    UnknownGroup,
    /// The trim range is empty or past the body's end.
    BadRange,
    /// The trim range touches a generated block.
    Generated,
    /// The note id resolves to no live note.
    UnknownNote,
}

/// An applied `resolve_removals`.
///
/// Media: every managed media file (`…_MD5.<media ext>`) linked from a deleted line (Delete
/// verdicts and trims; never Keep) is a cleanup candidate (`media_queued`). They are not
/// touched in the request: the daemon enqueues one job of the built-in `removals-media-cleanup`
/// action (`cleanup_job`, its id), which holds the attachments queue key
/// (`cv_actions::ATTACHMENTS_KEY`, shared with `localize`, `tidy`, `track_*`) and re-checks each
/// file with the `tidy` orphan sweep's writer-side check. Its outcome vars carry the counts:
/// `media_trashed` (moved to the trash, `reason = orphan`), `media_too_young` (under `tidy`'s
/// minimum age, left to the daily sweep), `media_referenced` (still linked, kept) and
/// `media_skipped` (gone, not a file, or refused by the re-check). `cleanup_failed`: the
/// enqueue failed after the note write (logged; the write stands, the daily sweep remains).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalsApplied {
    /// The note's hash after the write (unchanged when nothing changed).
    pub hash: String,
    /// The note's current vault path.
    pub path: String,
    /// Fold groups resolved (0 for a trim).
    pub groups: u32,
    /// Label rows recorded (non-blank lines).
    pub lines_labelled: u32,
    /// Kept lines restored at their group's anchor.
    pub restored_at_anchor: u32,
    /// Kept lines restored at the end of the body (old-layout groups).
    pub restored_at_end: u32,
    /// Cleanup candidates handed to the queued job (0 = none queued).
    pub media_queued: u32,
    /// The queued cleanup job's id (omitted when none was queued).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_job: Option<String>,
    /// The cleanup could not be enqueued after the note write (logged; the write stands).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cleanup_failed: bool,
    /// Label rows could not be stored after the note write succeeded (logged; the write
    /// stands).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub labels_failed: bool,
}

/// `next_fold_note` params: the note the client is on. Absent, gone or unknown starts from the
/// first candidate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NextFoldNoteParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<NoteId>,
}

/// `next_fold_note` result: the first note after the current one in vault-path order (wrapping
/// to the first) whose body still has a `[!removed]` fold group; never the current note itself,
/// so `null` when no other note has one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextFoldNoteResult {
    #[serde(default)]
    pub path: Option<String>,
}
