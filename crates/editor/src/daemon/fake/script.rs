//! The fake's scripted replies and call recorders (views, search, tracker status, bool
//! properties, held replies, durable-action queue): what a test scripts, and what it asks the
//! fake afterwards that the editor sent. No id bookkeeping lives here (that is the real store's).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cv_protocol::{JobState, SearchResult, ViewResult};

use super::{DaemonError, FakeDaemon, Notification, Result, RunActionParams, emit, terminal};

/// The scripted-reply and recorder state held in the fake's `Inner`.
pub(super) struct Script {
    /// WP-7: scripted view results, and every `view_run` call `(view_id, this_path)` in order.
    pub(super) views: BTreeMap<String, ViewResult>,
    view_calls: Vec<(String, Option<String>)>,
    /// The UI sort sent with each `view_run`, parallel to `view_calls`.
    view_sorts: Vec<Option<cv_protocol::ViewSort>>,
    /// Plan round-13 WP-4: the selection sent with each `view_run`, parallel to `view_calls`.
    view_selects: Vec<Option<cv_protocol::ViewSelect>>,
    /// editor-usability-round WP-5: actions run as durable jobs (job id now, handler and
    /// terminal `JobUpdated` on [`FakeDaemon::complete_deferred`]), and the queued runs.
    pub(super) deferred_ids: BTreeSet<String>,
    pub(super) queued: Vec<(String, RunActionParams)>,
    /// A scripted `tracker_item_set_status` failure message.
    done_fail: Option<String>,
    /// Plan round-6-feedback-todos WP-6: every `tracker_item_set_status` call (fails with
    /// `done_fail` too).
    status_calls: Vec<cv_protocol::TrackerItemSetStatusParams>,
    /// Plan round-8-intake-views-anchor WP-3: every `note_set_bool` call.
    bool_calls: Vec<cv_protocol::NoteSetBoolParams>,
    /// Plan semsearch-crate WP-4: the canned `search` result, the "disconnected" mode, and
    /// every `search` call `(query, limit)`.
    search: SearchResult,
    search_disconnected: bool,
    search_calls: Vec<(String, u32)>,
    /// Plan round-18 WP-2: while `Some((path, label))`, `write_note` / `rename` / `delete` /
    /// `delete_note` / `note_set_bool` answer the daemon's `HELD` refusal.
    held_reply: Option<(String, String)>,
    /// Every `write_note` / `rename` / `delete` / `delete_note` / `note_set_bool` request that
    /// reached the fake, as `"<method> <path>"`, in order.
    mutation_calls: Vec<String>,
}

impl Script {
    pub(super) fn new() -> Self {
        Self {
            views: BTreeMap::new(),
            view_calls: Vec::new(),
            view_sorts: Vec::new(),
            view_selects: Vec::new(),
            deferred_ids: BTreeSet::new(),
            queued: Vec::new(),
            done_fail: None,
            status_calls: Vec::new(),
            bool_calls: Vec::new(),
            search: SearchResult {
                hits: Vec::new(),
                semantic: false,
            },
            search_disconnected: false,
            search_calls: Vec::new(),
            held_reply: None,
            mutation_calls: Vec::new(),
        }
    }

    /// Records one `view_run`, then answers the scripted result.
    pub(super) fn run_view(
        &mut self,
        view_id: &str,
        this_path: Option<&str>,
        sort: Option<&cv_protocol::ViewSort>,
        select: Option<&cv_protocol::ViewSelect>,
    ) -> Result<ViewResult> {
        self.view_calls
            .push((view_id.to_owned(), this_path.map(str::to_owned)));
        self.view_sorts.push(sort.cloned());
        self.view_selects.push(select.cloned());
        self.views
            .get(view_id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(view_id.to_owned()))
    }

    pub(super) fn run_search(&mut self, query: &str, limit: u32) -> Result<SearchResult> {
        self.search_calls.push((query.to_owned(), limit));
        if self.search_disconnected {
            return Err(DaemonError::Disconnected);
        }
        Ok(self.search.clone())
    }

    pub(super) fn set_status(&mut self, p: &cv_protocol::TrackerItemSetStatusParams) -> Result<()> {
        self.status_calls.push(p.clone());
        match &self.done_fail {
            Some(msg) => Err(DaemonError::Rpc(cv_protocol::RpcError {
                code: cv_protocol::error_codes::NOT_FOUND,
                message: msg.clone(),
                data: None,
            })),
            None => Ok(()),
        }
    }

    /// Logs one mutating request, then answers the scripted `HELD` refusal when set.
    pub(super) fn record_mutation(&mut self, method: &str, path: &str) -> Result<()> {
        self.mutation_calls.push(format!("{method} {path}"));
        match &self.held_reply {
            Some((held, label)) => Err(DaemonError::Rpc(cv_protocol::RpcError {
                code: cv_protocol::error_codes::HELD,
                message: format!("{label} is updating {held}"),
                data: Some(serde_json::json!({"path": held, "label": label})),
            })),
            None => Ok(()),
        }
    }

    pub(super) fn record_bool(&mut self, p: &cv_protocol::NoteSetBoolParams) {
        self.bool_calls.push(p.clone());
    }
}

impl FakeDaemon {
    /// Makes `action_id` durable: `run_action` returns only a job id (emitting `pending`); the
    /// handler runs and the terminal status (with `open_path`) is emitted on
    /// [`FakeDaemon::complete_deferred`], like the real runner's durable path.
    pub fn defer_action(&self, action_id: &str) -> Result<()> {
        self.lock()?
            .script
            .deferred_ids
            .insert(action_id.to_owned());
        Ok(())
    }

    /// Runs every queued durable job and emits its terminal `JobUpdated`. Returns the job ids.
    pub fn complete_deferred(&self) -> Result<Vec<String>> {
        let queued = std::mem::take(&mut self.lock()?.script.queued);
        let mut done = Vec::new();
        for (job_id, params) in queued {
            let handler = {
                let g = self.lock()?;
                g.actions
                    .get(&params.action_id)
                    .map(|(_, h)| Arc::clone(h))
                    .ok_or_else(|| DaemonError::NotFound(params.action_id.clone()))?
            };
            let job = terminal(job_id.clone(), handler(&params));
            let mut g = self.lock()?;
            g.jobs.insert(job_id.clone(), job.clone());
            emit(&mut g, Notification::JobUpdated(job));
            done.push(job_id);
        }
        Ok(done)
    }

    /// Queues a durable run of `params` as a pending job and emits its `pending` status.
    pub(super) fn queue_deferred(&self, params: RunActionParams) -> Result<super::RunActionResult> {
        let mut g = self.lock()?;
        let job_id = format!("fake-{}", g.next_job);
        g.next_job += 1;
        let job = super::JobStatus {
            job_id: job_id.clone(),
            status: JobState::Pending,
            message: None,
            output_paths: Vec::new(),
            open_path: None,
            hold: None,
        };
        g.jobs.insert(job_id.clone(), job.clone());
        g.script.queued.push((job_id.clone(), params));
        emit(&mut g, Notification::JobUpdated(job));
        Ok(super::RunActionResult {
            job_id,
            open_path: None,
        })
    }

    /// WP-7: scripts `view_run(id, _)` to return `result` (and lists `id` in `views_list`).
    pub fn set_view_result(&self, id: &str, result: ViewResult) -> Result<()> {
        self.lock()?.script.views.insert(id.to_owned(), result);
        Ok(())
    }

    /// The sort each `view_run` carried, parallel to [`Self::view_calls`].
    pub fn view_sorts(&self) -> Result<Vec<Option<cv_protocol::ViewSort>>> {
        Ok(self.lock()?.script.view_sorts.clone())
    }

    /// Plan round-13 WP-4: the selection each `view_run` carried, parallel to [`Self::view_calls`].
    pub fn view_selects(&self) -> Result<Vec<Option<cv_protocol::ViewSelect>>> {
        Ok(self.lock()?.script.view_selects.clone())
    }

    /// WP-7: every `view_run` call so far, `(view_id, this_path)` in order.
    pub fn view_calls(&self) -> Result<Vec<(String, Option<String>)>> {
        Ok(self.lock()?.script.view_calls.clone())
    }

    /// Plan round-6-feedback-todos WP-6: every `tracker_item_set_status` call so far, in order.
    pub fn status_calls(&self) -> Result<Vec<cv_protocol::TrackerItemSetStatusParams>> {
        Ok(self.lock()?.script.status_calls.clone())
    }

    /// Plan round-18 WP-2: the mutating requests that reached the fake (`"<method> <path>"`).
    pub fn mutation_calls(&self) -> Result<Vec<String>> {
        Ok(self.lock()?.script.mutation_calls.clone())
    }

    /// Plan round-18 WP-2: makes the mutating requests answer `HELD` (`None` = succeed again).
    pub fn set_held_reply(&self, held: Option<(&str, &str)>) -> Result<()> {
        self.lock()?.script.held_reply = held.map(|(p, l)| (p.to_owned(), l.to_owned()));
        Ok(())
    }

    /// Logs one mutating request, then answers the scripted `HELD` refusal when set.
    pub(super) fn mutation(&self, method: &str, path: &str) -> Result<()> {
        self.lock()?.script.record_mutation(method, path)
    }

    /// Plan round-8-intake-views-anchor WP-3: every `note_set_bool` call so far, in order.
    pub fn bool_calls(&self) -> Result<Vec<cv_protocol::NoteSetBoolParams>> {
        Ok(self.lock()?.script.bool_calls.clone())
    }

    /// Plan semsearch-crate WP-4: the canned `search` answer.
    pub fn set_search_result(&self, result: SearchResult) -> Result<()> {
        self.lock()?.script.search = result;
        Ok(())
    }

    /// Plan semsearch-crate WP-4: `search` answers [`DaemonError::Disconnected`] while `on`.
    pub fn set_search_disconnected(&self, on: bool) -> Result<()> {
        self.lock()?.script.search_disconnected = on;
        Ok(())
    }

    /// Plan semsearch-crate WP-4: every `search` call `(query, limit)` so far.
    pub fn search_calls(&self) -> Result<Vec<(String, u32)>> {
        Ok(self.lock()?.script.search_calls.clone())
    }

    /// Round-5 WP-11: makes `tracker_item_set_status` fail with `msg` (`None` = succeed).
    pub fn set_done_failure(&self, msg: Option<&str>) -> Result<()> {
        self.lock()?.script.done_fail = msg.map(str::to_owned);
        Ok(())
    }
}
