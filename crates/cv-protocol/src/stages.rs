//! Intake stage states (plan `intake-foundations` WP-1): the wire form of one note's
//! `cv_ingest::stage::stage_states`. The type only — no request carries it yet (the consumer is
//! plan `intake-stage-jobs`). Stage and kind names are the stage model's `as_str` strings, so this
//! crate does not depend on `cv-ingest`.

use serde::{Deserialize, Serialize};

use crate::NoteId;

/// One stage's state (`{"state": "done" | "pending" | "skipped" | "unfinished" | "failed" |
/// "stuck", …}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StageStateWire {
    /// Stage-specific evidence says the stage is done.
    Done,
    /// Not done and nothing explains why: the stage should run.
    Pending,
    /// The stage does not apply to the note's current facts.
    Skipped { reason: String },
    /// A run happened and left work behind (an enrich miss, images left undescribed).
    Unfinished,
    /// Recorded failures.
    Failed { attempts: u32 },
    /// Blocked behind a failed or stuck prerequisite.
    Stuck,
}

/// One stage of a note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteStage {
    /// The stage name (`cv_ingest::stage::Stage::as_str`).
    pub stage: String,
    /// Its state.
    #[serde(flatten)]
    pub state: StageStateWire,
}

/// A note's stage states, in stage order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteStageStates {
    /// The note's durable id, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NoteId>,
    /// Intake kind (`blog` | `mail` | `youtube` | `clip`).
    pub kind: String,
    /// Every stage the kind requires.
    pub stages: Vec<NoteStage>,
}
