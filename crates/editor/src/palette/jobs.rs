//! Job tracking: the job toast, the invoking action's title per job, and the origin
//! bookkeeping a terminal `open_path` follows.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cv_protocol::{JobState, JobStatus};

/// A terminal job's toast stays this long.
pub const TOAST_TTL: Duration = Duration::from_secs(5);

/// The job toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub title: String,
    pub state: JobState,
    /// When the job reached a terminal state (expiry clock).
    pub ended: Option<Instant>,
    /// plan round-5 WP-3: the job's failure reason on a non-Done terminal state.
    pub message: Option<String>,
}

impl Toast {
    /// The status-bar text.
    pub fn line(&self) -> String {
        let s = state_label(self.state);
        match &self.message {
            Some(m) => format!("{}: {s} — {m}", self.title),
            None => format!("{}: {s}", self.title),
        }
    }
}

/// A job state's wire label (`failed`, `done`, …).
pub fn state_label(s: JobState) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Whether a job state is final.
pub fn terminal(s: JobState) -> bool {
    matches!(
        s,
        JobState::Done | JobState::Failed | JobState::Cancelled | JobState::Deferred
    )
}

/// The palette's job state.
#[derive(Debug, Default)]
pub struct Jobs {
    pub toast: Option<Toast>,
    /// job id → title of the action that started it.
    pub job_titles: HashMap<String, String>,
    /// editor-usability-round WP-5 (audit B1/M2): job id → the invoking tab's
    /// `(path, edit_seq)` at dispatch, so a terminal `open_path` can follow the note.
    pub job_origins: HashMap<String, (String, u64)>,
    /// Terminal `open_path`s that arrived before the `run_action` reply (job id → path).
    pub early_open: HashMap<String, String>,
}

impl Jobs {
    /// A `job_updated`: refreshes the toast (title from the invoking action when known).
    pub fn job_updated(&mut self, j: &JobStatus, now: Instant) {
        let title = self
            .job_titles
            .get(&j.job_id)
            .cloned()
            .unwrap_or_else(|| j.job_id.clone());
        self.toast = Some(Toast {
            title,
            state: j.status,
            ended: terminal(j.status).then_some(now),
            message: (terminal(j.status) && j.status != JobState::Done)
                .then(|| j.message.clone())
                .flatten(),
        });
    }

    /// Clears a terminal toast older than [`TOAST_TTL`].
    pub fn expire_toast(&mut self, now: Instant) {
        if self
            .toast
            .as_ref()
            .and_then(|t| t.ended)
            .is_some_and(|e| now.duration_since(e) >= TOAST_TTL)
        {
            self.toast = None;
        }
    }
}
