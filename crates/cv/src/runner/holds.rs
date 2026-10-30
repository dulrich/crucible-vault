//! Note holds of running jobs (plan round-17 WP-2, round-18 Decisions 1-2): the hold record, the
//! daemon's read view over it, and the runner's hold-carrying status publishing.

use cv_protocol::NoteHold;

use super::*;
use crate::daemon::JobError;

/// The runner's claimed jobs: `job id -> its note hold` (`None` = claimed, holds no note).
pub(super) type Running = Arc<Mutex<HashMap<String, Option<Hold>>>>;

/// One running job's note hold: the vault path it holds and its action label.
///
/// Crucible reference: `src/orchestration/NoteLockManager.ts:47-112` (per-note lock +
/// `note-lock-changed` event). The Obsidian-induced part (plugin-side async lock bookkeeping
/// around Obsidian's shared writer) is dropped: claim-time resource keys already serialize jobs,
/// so the daemon only publishes the hold as a signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hold {
    path: String,
    label: String,
}

impl Hold {
    fn refusal(&self) -> JobError {
        JobError::Held {
            path: self.path.clone(),
            label: self.label.clone(),
        }
    }

    fn to_wire(&self) -> NoteHold {
        NoteHold {
            path: self.path.clone(),
            label: self.label.clone(),
        }
    }
}

/// A cheap shared read of the runner's note holds (plan round-18 Decision 2): the daemon's
/// client-write handlers consult it inside the serialized writer job, before mutating. It reads
/// the `Some` entries of the runner's claimed-jobs map only.
#[derive(Clone, Default)]
pub struct HoldsView(pub(super) Running);

impl HoldsView {
    /// Whether no note is held.
    pub fn is_empty(&self) -> bool {
        lock(&self.0).values().all(Option::is_none)
    }

    /// Refuses (`JobError::Held`) when any of `paths` is held; with no holds nothing is compared.
    pub(crate) fn refuse<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), JobError> {
        let running = lock(&self.0);
        let holds: Vec<&Hold> = running.values().flatten().collect();
        if holds.is_empty() {
            return Ok(());
        }
        for p in paths {
            if let Some(h) = holds.iter().find(|h| h.path == p) {
                return Err(h.refusal());
            }
        }
        Ok(())
    }

    /// Refuses when `path` itself or any note under it as a folder (`path/…`) is held.
    pub(crate) fn refuse_under(&self, path: &str) -> Result<(), JobError> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        match lock(&self.0)
            .values()
            .flatten()
            .find(|h| h.path == path || h.path.starts_with(&prefix))
        {
            Some(h) => Err(h.refusal()),
            None => Ok(()),
        }
    }
}

impl std::fmt::Debug for HoldsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HoldsView").finish_non_exhaustive()
    }
}

impl Inner {
    /// Publishes one `job_updated`, carrying `hold` when the job holds a note.
    pub(super) fn publish(
        &self,
        id: &str,
        status: JobState,
        message: Option<String>,
        output_paths: Vec<String>,
        open_path: Option<String>,
        hold: Option<&Hold>,
    ) {
        let _ = self.events.send(Notification::JobUpdated(JobStatus {
            job_id: id.to_owned(),
            status,
            message,
            output_paths,
            open_path,
            hold: hold.map(Hold::to_wire),
        }));
    }

    /// Starts a hold for `job_id` when the action holds its note, then publishes `running`.
    pub(super) fn start_running(
        &self,
        job_id: &str,
        action: &Action,
        context: &ActionContext,
        holds_note: bool,
    ) {
        let hold = context
            .active_path
            .as_ref()
            .filter(|p| holds_note && !p.is_empty())
            .map(|p| Hold {
                path: p.clone(),
                label: action.label.clone(),
            });
        if hold.is_some() {
            lock(&self.running).insert(job_id.to_owned(), hold.clone());
        }
        self.publish(
            job_id,
            JobState::Running,
            None,
            Vec::new(),
            None,
            hold.as_ref(),
        );
    }

    /// Re-targets a hold after a move of the active note and republishes `running` (holds follow
    /// renames).
    pub(super) fn retarget_hold(&self, job_id: &str, context: &ActionContext) {
        let Some(path) = context.active_path.as_ref() else {
            return;
        };
        let moved = {
            let mut running = lock(&self.running);
            match running.get_mut(job_id) {
                Some(Some(h)) if &h.path != path => {
                    h.path.clone_from(path);
                    Some(h.clone())
                }
                _ => None,
            }
        };
        if let Some(h) = moved {
            self.publish(job_id, JobState::Running, None, Vec::new(), None, Some(&h));
        }
    }
}

impl Runner {
    /// A shared read handle on the current note holds (the daemon's write refusal).
    pub fn holds_view(&self) -> HoldsView {
        HoldsView(Arc::clone(&self.inner.running))
    }

    /// The current note holds as `running` statuses (the subscribe snapshot, plan round-17 WP-2).
    pub fn held_notes(&self) -> Vec<JobStatus> {
        let running = lock(&self.inner.running);
        let mut v: Vec<JobStatus> = running
            .iter()
            .filter_map(|(id, h)| {
                h.as_ref().map(|h| JobStatus {
                    job_id: id.clone(),
                    status: JobState::Running,
                    message: None,
                    output_paths: Vec::new(),
                    open_path: None,
                    hold: Some(h.to_wire()),
                })
            })
            .collect();
        v.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        v
    }
}
