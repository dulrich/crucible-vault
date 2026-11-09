//! `FakeDaemon`: an in-process, directory-backed stand-in for the daemon (plan Summary), so
//! editor WPs never wait on the daemon's action layer. It builds a real `index.db` with
//! `cv-index` over the directory, applies writes with CAS against the content hash, rebuilds the
//! index (bumping the generation) and emits `file_changed` + `index_generation`, and runs actions
//! from a scriptable registry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use cv_index::Index;
use cv_protocol::{ChangeKind, JobState, Origin};

use super::{
    ActionInfo, ContentHash, DaemonClient, DaemonError, DeleteMode, DeleteNoteResult,
    DeletePreviewItem, HelloResult, JobStatus, LocalIndex, Notification, Result, RunActionParams,
    RunActionResult, StatusResult, Topic, ViewResult, ViewsListResult, is_no_base, no_base,
    vault_abs,
};

/// A scripted action: params in, `Ok(open_path)` or `Err(message)` out.
pub type ActionHandler =
    Arc<dyn Fn(&RunActionParams) -> std::result::Result<Option<String>, String> + Send + Sync>;

/// Canned lookup data (plan round-5-reading-pickers-intake WP-5). `fail = Some(msg)` makes every
/// lookup return an RPC error with `msg`; a provider absent from `models` is `NotFound`.
#[derive(Debug, Clone)]
pub struct FakeLookups {
    pub currencies: Vec<cv_protocol::LookupCurrency>,
    pub places: Vec<cv_protocol::LookupGeoResult>,
    pub models: BTreeMap<String, cv_protocol::LookupModelsResult>,
    pub channels: BTreeMap<String, String>,
    pub fail: Option<String>,
}

impl Default for FakeLookups {
    fn default() -> Self {
        use cv_protocol::{LookupCurrency, LookupGeoResult, LookupModel, LookupModelsResult};
        let cur = |code: &str, name: &str| LookupCurrency {
            code: code.to_owned(),
            name: name.to_owned(),
        };
        let place = |label: &str, admin1: &str, cc: &str, lat: f64, lon: f64| LookupGeoResult {
            label: label.to_owned(),
            admin1: Some(admin1.to_owned()),
            country_code: cc.to_owned(),
            lat,
            lon,
        };
        let model = |id: &str| LookupModel {
            id: id.to_owned(),
            display_name: None,
        };
        Self {
            currencies: vec![
                cur("EUR", "Euro"),
                cur("MXN", "Mexican Peso"),
                cur("USD", "United States Dollar"),
            ],
            places: vec![
                place("Gotham, US", "Exampleland", "US", 40.000, -100.000),
                place("Paris, FR", "Ile-de-France", "FR", 48.853, 2.349),
            ],
            models: BTreeMap::from([(
                "local".to_owned(),
                LookupModelsResult {
                    models: vec![model("gemma-4"), model("bge-m3")],
                    live: true,
                },
            )]),
            channels: BTreeMap::from([("UCfake".to_owned(), "Fake Channel".to_owned())]),
            fail: None,
        }
    }
}

struct Inner {
    lookups: FakeLookups,
    index: Index,
    subscriber: Option<(Vec<Topic>, Sender<Notification>)>,
    actions: BTreeMap<String, (ActionInfo, ActionHandler)>,
    jobs: BTreeMap<String, JobStatus>,
    next_job: u64,
    /// WP-7: scripted view results, and every `view_run` call `(view_id, this_path)` in order.
    views: BTreeMap<String, ViewResult>,
    view_calls: Vec<(String, Option<String>)>,
    /// The UI sort sent with each `view_run`, parallel to `view_calls`.
    view_sorts: Vec<Option<cv_protocol::ViewSort>>,
    /// Plan round-13 WP-4: the selection sent with each `view_run`, parallel to `view_calls`.
    view_selects: Vec<Option<cv_protocol::ViewSelect>>,
    /// editor-usability-round WP-5: actions run as durable jobs (job id now, handler and
    /// terminal `JobUpdated` on [`FakeDaemon::complete_deferred`]), and the queued runs.
    deferred_ids: std::collections::BTreeSet<String>,
    queued: Vec<(String, RunActionParams)>,
    /// A scripted `tracker_item_set_status` failure message.
    done_fail: Option<String>,
    /// Plan round-6-feedback-todos WP-6: every `tracker_item_set_status` call (fails with
    /// `done_fail` too).
    status_calls: Vec<cv_protocol::TrackerItemSetStatusParams>,
    /// Plan round-8-intake-views-anchor WP-3: every `note_set_bool` call.
    bool_calls: Vec<cv_protocol::NoteSetBoolParams>,
    /// Plan semsearch-crate WP-4: the canned `search` result, the "disconnected" mode, and
    /// every `search` call `(query, limit)`.
    search: cv_protocol::SearchResult,
    search_disconnected: bool,
    search_calls: Vec<(String, u32)>,
    /// Plan round-18 WP-2: while `Some((path, label))`, `write_note` / `rename` / `delete` /
    /// `delete_note` / `note_set_bool` answer the daemon's `HELD` refusal.
    held_reply: Option<(String, String)>,
    /// Every `write_note` / `rename` / `delete` / `delete_note` / `note_set_bool` request that
    /// reached the fake, as `"<method> <path>"`, in order.
    mutation_calls: Vec<String>,
}

/// The fake daemon.
pub struct FakeDaemon {
    inner: Mutex<Inner>,
    hello: HelloResult,
    vault_root: PathBuf,
    state_db: PathBuf,
    local: LocalIndex,
}

impl std::fmt::Debug for FakeDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeDaemon")
            .field("vault_root", &self.vault_root)
            .finish_non_exhaustive()
    }
}

impl FakeDaemon {
    /// One job's status (the fake's job table; tests only).
    pub fn job(&self, job_id: &str) -> Result<JobStatus> {
        self.lock()?
            .jobs
            .get(job_id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(job_id.to_owned()))
    }

    /// Serves `vault_root`, keeping `index.db`/`state.db` under `data_dir` (never in the vault).
    pub fn open(vault_root: &Path, data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let base = data_dir.join("index.db");
        let index = Index::create(vault_root, &base)?;
        let generation = index.generation()?;
        let state_db = data_dir.join("state.db");
        drop(cv_state::StateDb::open(
            &state_db,
            0,
            cv_state::QueueConfig::default(),
        )?);
        let hello = HelloResult {
            protocol_version: cv_protocol::PROTOCOL_VERSION,
            index_schema: cv_index::SCHEMA_VERSION,
            vault_root: vault_root.display().to_string(),
            index_db: base.display().to_string(),
            state_db: state_db.display().to_string(),
            generation,
        };
        Ok(Self {
            inner: Mutex::new(Inner {
                lookups: FakeLookups::default(),
                index,
                subscriber: None,
                actions: BTreeMap::new(),
                jobs: BTreeMap::new(),
                next_job: 1,
                views: BTreeMap::new(),
                view_calls: Vec::new(),
                view_sorts: Vec::new(),
                view_selects: Vec::new(),
                deferred_ids: std::collections::BTreeSet::new(),
                queued: Vec::new(),
                status_calls: Vec::new(),
                bool_calls: Vec::new(),
                done_fail: None,
                search: cv_protocol::SearchResult {
                    hits: Vec::new(),
                    semantic: false,
                },
                search_disconnected: false,
                search_calls: Vec::new(),
                held_reply: None,
                mutation_calls: Vec::new(),
            }),
            hello,
            vault_root: vault_root.to_path_buf(),
            state_db,
            local: LocalIndex::new(&base),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| DaemonError::Protocol("fake daemon lock poisoned".into()))
    }

    /// Registers a scripted action.
    pub fn register_action(&self, info: ActionInfo, handler: ActionHandler) -> Result<()> {
        self.lock()?
            .actions
            .insert(info.id.clone(), (info, handler));
        Ok(())
    }

    /// Makes `action_id` durable: `run_action` returns only a job id (emitting `pending`); the
    /// handler runs and the terminal status (with `open_path`) is emitted on
    /// [`FakeDaemon::complete_deferred`], like the real runner's durable path.
    pub fn defer_action(&self, action_id: &str) -> Result<()> {
        self.lock()?.deferred_ids.insert(action_id.to_owned());
        Ok(())
    }

    /// Runs every queued durable job and emits its terminal `JobUpdated`. Returns the job ids.
    pub fn complete_deferred(&self) -> Result<Vec<String>> {
        let queued = std::mem::take(&mut self.lock()?.queued);
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

    /// Emits an arbitrary notification (tests of the UI's reaction).
    pub fn emit(&self, n: Notification) -> Result<()> {
        let mut g = self.lock()?;
        emit(&mut g, n);
        Ok(())
    }

    /// Round 9 WP-3: a daemon-side move (an auto-file action), not initiated by the editor:
    /// renames on disk, rebuilds the index, and reports the bridge's pair (`Renamed { from }`,
    /// then the source `Deleted`) to the subscriber and in the return value (tests feed it to
    /// the app).
    pub fn daemon_move(&self, from: &str, to: &str) -> Result<Vec<Notification>> {
        let (a, b) = (
            vault_abs(&self.vault_root, from)?,
            vault_abs(&self.vault_root, to)?,
        );
        let mut g = self.lock()?;
        if !a.exists() {
            return Err(DaemonError::NotFound(from.to_owned()));
        }
        if let Some(parent) = b.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&a, &b)?;
        let n = g.index.rebuild(&self.vault_root)?;
        let pair = vec![
            Notification::FileChanged {
                path: to.to_owned(),
                kind: ChangeKind::Renamed,
                hash: None,
                origin: Origin::Daemon,
                from: Some(from.to_owned()),
                replaced_by: None,
                seq: None,
            },
            Notification::FileChanged {
                path: from.to_owned(),
                kind: ChangeKind::Deleted,
                hash: None,
                origin: Origin::Daemon,
                from: None,
                replaced_by: None,
                seq: None,
            },
            Notification::IndexGeneration { n },
        ];
        for p in &pair {
            emit(&mut g, p.clone());
        }
        Ok(pair)
    }

    /// WP-7: scripts `view_run(id, _)` to return `result` (and lists `id` in `views_list`).
    pub fn set_view_result(&self, id: &str, result: ViewResult) -> Result<()> {
        self.lock()?.views.insert(id.to_owned(), result);
        Ok(())
    }

    /// WP-7: every `view_run` call so far, `(view_id, this_path)` in order.
    /// The sort each `view_run` carried, parallel to [`Self::view_calls`].
    pub fn view_sorts(&self) -> Result<Vec<Option<cv_protocol::ViewSort>>> {
        Ok(self.lock()?.view_sorts.clone())
    }

    /// Plan round-13 WP-4: the selection each `view_run` carried, parallel to [`Self::view_calls`].
    pub fn view_selects(&self) -> Result<Vec<Option<cv_protocol::ViewSelect>>> {
        Ok(self.lock()?.view_selects.clone())
    }

    pub fn view_calls(&self) -> Result<Vec<(String, Option<String>)>> {
        Ok(self.lock()?.view_calls.clone())
    }

    /// Plan round-6-feedback-todos WP-6: every `tracker_item_set_status` call so far, in order.
    pub fn status_calls(&self) -> Result<Vec<cv_protocol::TrackerItemSetStatusParams>> {
        Ok(self.lock()?.status_calls.clone())
    }

    /// Plan round-18 WP-2: the mutating requests that reached the fake (`"<method> <path>"`).
    pub fn mutation_calls(&self) -> Result<Vec<String>> {
        Ok(self.lock()?.mutation_calls.clone())
    }

    /// Plan round-18 WP-2: makes the mutating requests answer `HELD` (`None` = succeed again).
    pub fn set_held_reply(&self, held: Option<(&str, &str)>) -> Result<()> {
        self.lock()?.held_reply = held.map(|(p, l)| (p.to_owned(), l.to_owned()));
        Ok(())
    }

    /// Logs one mutating request, then answers the scripted `HELD` refusal when set.
    fn mutation(&self, method: &str, path: &str) -> Result<()> {
        let mut g = self.lock()?;
        g.mutation_calls.push(format!("{method} {path}"));
        match &g.held_reply {
            Some((held, label)) => Err(DaemonError::Rpc(cv_protocol::RpcError {
                code: cv_protocol::error_codes::HELD,
                message: format!("{label} is updating {held}"),
                data: Some(serde_json::json!({"path": held, "label": label})),
            })),
            None => Ok(()),
        }
    }

    /// Plan round-8-intake-views-anchor WP-3: every `note_set_bool` call so far, in order.
    pub fn bool_calls(&self) -> Result<Vec<cv_protocol::NoteSetBoolParams>> {
        Ok(self.lock()?.bool_calls.clone())
    }

    /// Plan semsearch-crate WP-4: the canned `search` answer.
    pub fn set_search_result(&self, result: cv_protocol::SearchResult) -> Result<()> {
        self.lock()?.search = result;
        Ok(())
    }

    /// Plan semsearch-crate WP-4: `search` answers [`DaemonError::Disconnected`] while `on`.
    pub fn set_search_disconnected(&self, on: bool) -> Result<()> {
        self.lock()?.search_disconnected = on;
        Ok(())
    }

    /// Plan semsearch-crate WP-4: every `search` call `(query, limit)` so far.
    pub fn search_calls(&self) -> Result<Vec<(String, u32)>> {
        Ok(self.lock()?.search_calls.clone())
    }

    /// Round-5 WP-11: makes `tracker_item_set_status` fail with `msg` (`None` = succeed).
    pub fn set_done_failure(&self, msg: Option<&str>) -> Result<()> {
        self.lock()?.done_fail = msg.map(str::to_owned);
        Ok(())
    }

    /// WP-5: replaces the canned lookup data.
    pub fn set_lookups(&self, lookups: FakeLookups) -> Result<()> {
        self.lock()?.lookups = lookups;
        Ok(())
    }

    /// The canned lookups, or the scripted failure.
    fn lookups(&self) -> Result<FakeLookups> {
        let l = self.lock()?.lookups.clone();
        match &l.fail {
            Some(msg) => Err(DaemonError::Rpc(cv_protocol::RpcError {
                code: 1005,
                message: msg.clone(),
                data: None,
            })),
            None => Ok(l),
        }
    }

    /// The current index generation.
    pub fn generation(&self) -> Result<u64> {
        Ok(self.lock()?.index.generation()?)
    }

    /// Removes `path` per `mode` (trash: `.trash/<path>` with a ` (n)` suffix on collision,
    /// like the daemon) and rebuilds the index.
    fn remove_mode(&self, g: &mut Inner, path: &str, mode: DeleteMode) -> Result<()> {
        let abs = vault_abs(&self.vault_root, path)?;
        if !abs.is_file() {
            return Err(DaemonError::NotFound(path.to_owned()));
        }
        match mode {
            DeleteMode::Permanent => std::fs::remove_file(&abs)?,
            DeleteMode::Trash => {
                let (stem, ext) = match path.rsplit_once('.') {
                    Some((s, e)) if !s.is_empty() && !e.contains('/') => (s, format!(".{e}")),
                    _ => (path, String::new()),
                };
                let mut n = 0u32;
                let dest = loop {
                    let cand = if n == 0 {
                        format!(".trash/{path}")
                    } else {
                        format!(".trash/{stem} ({n}){ext}")
                    };
                    let d = self.vault_root.join(cand);
                    if !d.exists() {
                        break d;
                    }
                    n += 1;
                };
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(&abs, &dest)?;
            }
        }
        self.after_write(g, path, ChangeKind::Deleted, None)
    }

    fn after_write(
        &self,
        g: &mut Inner,
        path: &str,
        kind: ChangeKind,
        hash: Option<String>,
    ) -> Result<()> {
        let n = g.index.rebuild(&self.vault_root)?;
        emit(
            g,
            Notification::FileChanged {
                path: path.to_owned(),
                kind,
                hash,
                origin: Origin::Daemon,
                from: None,
                replaced_by: None,
                seq: None,
            },
        );
        emit(g, Notification::IndexGeneration { n });
        Ok(())
    }
}

/// A terminal job status from a handler outcome.
fn terminal(job_id: String, outcome: std::result::Result<Option<String>, String>) -> JobStatus {
    let (status, open_path, message) = match outcome {
        Ok(p) => (JobState::Done, p, None),
        Err(m) => (JobState::Failed, None, Some(m)),
    };
    JobStatus {
        job_id,
        status,
        message,
        output_paths: open_path.iter().cloned().collect(),
        open_path,
        hold: None,
    }
}

fn emit(g: &mut Inner, n: Notification) {
    if let Some((topics, tx)) = &g.subscriber
        && topics.contains(&n.topic())
        && tx.send(n).is_err()
    {
        g.subscriber = None;
    }
}

fn current_hash(abs: &Path) -> Result<Option<ContentHash>> {
    match std::fs::read(abs) {
        Ok(b) => Ok(Some(ContentHash::of(&b))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl DaemonClient for FakeDaemon {
    fn hello(&self) -> &HelloResult {
        &self.hello
    }

    fn write_note(
        &self,
        path: &str,
        content: &str,
        base_hash: &ContentHash,
    ) -> Result<ContentHash> {
        self.mutation("write_note", path)?;
        let abs = vault_abs(&self.vault_root, path)?;
        let mut g = self.lock()?;
        let found = current_hash(&abs)?;
        let matches = match &found {
            Some(h) => h == base_hash,
            None => is_no_base(base_hash),
        };
        if !matches {
            return Err(DaemonError::Conflict {
                current_hash: found.unwrap_or_else(no_base),
            });
        }
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, content.as_bytes())?;
        let hash = ContentHash::of(content.as_bytes());
        let kind = if found.is_some() {
            ChangeKind::Modified
        } else {
            ChangeKind::Created
        };
        self.after_write(&mut g, path, kind, Some(hash.as_str().to_owned()))?;
        Ok(hash)
    }

    fn create_note(&self, path: &str, content: &str) -> Result<String> {
        let abs = vault_abs(&self.vault_root, path)?;
        if abs.exists() {
            return Err(DaemonError::Protocol(format!("{path} exists")));
        }
        self.write_note(path, content, &no_base())?;
        Ok(path.to_owned())
    }

    /// Stand-in path (not the daemon's MD5 template): `<folder>/_attachments/<basename>/
    /// <content-hash prefix>_FAKE.<ext>` — deterministic, so identical bytes reuse one file.
    fn write_attachment(&self, note_path: &str, ext: &str, bytes: &[u8]) -> Result<String> {
        let (folder, file) = note_path.rsplit_once('/').unwrap_or(("", note_path));
        let base = file.strip_suffix(".md").unwrap_or(file);
        let h = ContentHash::of(bytes);
        let name = format!("{}_FAKE.{ext}", &h.as_str()[..16.min(h.as_str().len())]);
        let path = if folder.is_empty() {
            format!("_attachments/{base}/{name}")
        } else {
            format!("{folder}/_attachments/{base}/{name}")
        };
        let abs = vault_abs(&self.vault_root, &path)?;
        let mut g = self.lock()?;
        if !abs.exists() {
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&abs, bytes)?;
            self.after_write(
                &mut g,
                &path,
                ChangeKind::Created,
                Some(h.as_str().to_owned()),
            )?;
        }
        Ok(path)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.mutation("rename", from)?;
        let (a, b) = (
            vault_abs(&self.vault_root, from)?,
            vault_abs(&self.vault_root, to)?,
        );
        let mut g = self.lock()?;
        if !a.exists() {
            return Err(DaemonError::NotFound(from.to_owned()));
        }
        if let Some(parent) = b.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&a, &b)?;
        self.after_write(&mut g, to, ChangeKind::Renamed, None)
    }

    fn delete(&self, path: &str) -> Result<()> {
        self.mutation("delete", path)?;
        let abs = vault_abs(&self.vault_root, path)?;
        let mut g = self.lock()?;
        std::fs::remove_file(&abs).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => DaemonError::NotFound(path.to_owned()),
            _ => DaemonError::Io(e),
        })?;
        self.after_write(&mut g, path, ChangeKind::Deleted, None)
    }

    fn delete_preview(&self, path: &str) -> Result<Vec<DeletePreviewItem>> {
        let g = self.lock()?;
        let items = cv_index::delete_preview(&g.index.query(), path)?;
        Ok(items
            .into_iter()
            .map(|(path, referenced_elsewhere)| DeletePreviewItem {
                path,
                referenced_elsewhere,
            })
            .collect())
    }

    fn delete_note(
        &self,
        path: &str,
        attachments: &[String],
        mode: DeleteMode,
    ) -> Result<DeleteNoteResult> {
        self.mutation("delete_note", path)?;
        let mut g = self.lock()?;
        self.remove_mode(&mut g, path, mode)?;
        let mut out = DeleteNoteResult {
            removed: vec![path.to_owned()],
            skipped: Vec::new(),
        };
        for a in attachments {
            let shared = a == path || cv_index::referenced_elsewhere(&g.index.query(), a, path)?;
            if shared || self.remove_mode(&mut g, a, mode).is_err() {
                out.skipped.push(a.clone());
            } else {
                out.removed.push(a.clone());
            }
        }
        Ok(out)
    }

    fn actions_list(&self) -> Result<Vec<ActionInfo>> {
        Ok(self
            .lock()?
            .actions
            .values()
            .map(|(i, _)| i.clone())
            .collect())
    }

    fn run_action(&self, params: RunActionParams) -> Result<RunActionResult> {
        let handler = {
            let mut g = self.lock()?;
            let handler = g
                .actions
                .get(&params.action_id)
                .map(|(_, h)| Arc::clone(h))
                .ok_or_else(|| DaemonError::NotFound(params.action_id.clone()))?;
            if g.deferred_ids.contains(&params.action_id) {
                let job_id = format!("fake-{}", g.next_job);
                g.next_job += 1;
                let job = JobStatus {
                    job_id: job_id.clone(),
                    status: JobState::Pending,
                    message: None,
                    output_paths: Vec::new(),
                    open_path: None,
                    hold: None,
                };
                g.jobs.insert(job_id.clone(), job.clone());
                g.queued.push((job_id.clone(), params));
                emit(&mut g, Notification::JobUpdated(job));
                return Ok(RunActionResult {
                    job_id,
                    open_path: None,
                });
            }
            handler
        };
        // The handler runs without the lock so it may call back into the fake.
        let outcome = handler(&params);
        let mut g = self.lock()?;
        let job_id = format!("fake-{}", g.next_job);
        g.next_job += 1;
        let job = terminal(job_id.clone(), outcome);
        let open_path = job.open_path.clone();
        g.jobs.insert(job_id.clone(), job.clone());
        emit(&mut g, Notification::JobUpdated(job));
        Ok(RunActionResult { job_id, open_path })
    }

    fn status(&self) -> Result<StatusResult> {
        let g = self.lock()?;
        Ok(StatusResult {
            generation: g.index.generation()?,
            queue: BTreeMap::new(),
            config_errors: vec![],
            services: vec![],
            tracker_items: None,
            link_records: None,
            memory: None,
        })
    }

    fn views_list(&self) -> Result<ViewsListResult> {
        Ok(ViewsListResult {
            view_ids: self.lock()?.views.keys().cloned().collect(),
        })
    }

    fn view_run(
        &self,
        view_id: &str,
        this_path: Option<&str>,
        sort: Option<&cv_protocol::ViewSort>,
        select: Option<&cv_protocol::ViewSelect>,
    ) -> Result<ViewResult> {
        let mut g = self.lock()?;
        g.view_calls
            .push((view_id.to_owned(), this_path.map(str::to_owned)));
        g.view_sorts.push(sort.cloned());
        g.view_selects.push(select.cloned());
        g.views
            .get(view_id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(view_id.to_owned()))
    }

    fn lookup_currencies(&self) -> Result<Vec<cv_protocol::LookupCurrency>> {
        Ok(self.lookups()?.currencies)
    }

    fn lookup_geocode(&self, query: &str) -> Result<Vec<cv_protocol::LookupGeoResult>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .lookups()?
            .places
            .into_iter()
            .filter(|p| p.label.to_lowercase().contains(&q))
            .collect())
    }

    fn lookup_models(&self, provider: &str) -> Result<cv_protocol::LookupModelsResult> {
        self.lookups()?
            .models
            .remove(provider)
            .ok_or_else(|| DaemonError::NotFound(provider.to_owned()))
    }

    fn search(&self, query: &str, limit: u32) -> Result<cv_protocol::SearchResult> {
        let mut g = self.lock()?;
        g.search_calls.push((query.to_owned(), limit));
        if g.search_disconnected {
            return Err(DaemonError::Disconnected);
        }
        Ok(g.search.clone())
    }

    fn tracker_item_set_status(&self, p: &cv_protocol::TrackerItemSetStatusParams) -> Result<()> {
        let mut g = self.lock()?;
        g.status_calls.push(p.clone());
        match &g.done_fail {
            Some(msg) => Err(DaemonError::Rpc(cv_protocol::RpcError {
                code: cv_protocol::error_codes::NOT_FOUND,
                message: msg.clone(),
                data: None,
            })),
            None => Ok(()),
        }
    }

    fn note_set_bool(&self, p: &cv_protocol::NoteSetBoolParams) -> Result<()> {
        self.mutation("note_set_bool", &p.path)?;
        self.lock()?.bool_calls.push(p.clone());
        Ok(())
    }

    fn memory_report(&self, _report: &cv_protocol::MemoryReport) -> Result<()> {
        Ok(())
    }

    /// Minimal (plan note-path-continuity WP-2): no lineage yet (WP-3 adds cursor semantics);
    /// a present file is `Live`, anything else `Unknown`, at head 0.
    fn resolve_path(
        &self,
        path: &str,
        _since: Option<u64>,
    ) -> Result<cv_protocol::ResolvePathResult> {
        let abs = vault_abs(&self.vault_root, path)?;
        let resolution = if abs.is_file() {
            cv_protocol::PathResolution::Live {
                path: path.to_owned(),
            }
        } else {
            cv_protocol::PathResolution::Unknown
        };
        Ok(cv_protocol::ResolvePathResult {
            resolution,
            head: 0,
        })
    }

    fn lookup_channels(&self, ids: &[String]) -> Result<Vec<cv_protocol::LookupChannel>> {
        let l = self.lookups()?;
        Ok(ids
            .iter()
            .filter_map(|id| {
                l.channels.get(id).map(|t| cv_protocol::LookupChannel {
                    id: id.clone(),
                    title: t.clone(),
                })
            })
            .collect())
    }

    fn subscribe(&self, topics: &[Topic]) -> Result<Receiver<Notification>> {
        let (tx, rx) = mpsc::channel();
        self.lock()?.subscriber = Some((topics.to_vec(), tx));
        Ok(rx)
    }

    fn vault_root(&self) -> &Path {
        &self.vault_root
    }

    fn index(&self) -> &LocalIndex {
        &self.local
    }

    fn state_db(&self) -> &Path {
        &self.state_db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_are_canned_and_scriptable() {
        let tmp = tempfile::tempdir().expect("tmp");
        let vault = tmp.path().join("v");
        std::fs::create_dir_all(&vault).expect("mkdir");
        let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
        assert_eq!(d.lookup_currencies().expect("cur")[0].code, "EUR");
        assert_eq!(d.lookup_geocode("got").expect("geo")[0].label, "Gotham, US");
        assert!(d.lookup_models("local").expect("models").live);
        assert!(matches!(
            d.lookup_models("nope"),
            Err(DaemonError::NotFound(_))
        ));
        let ch = d
            .lookup_channels(&["UCfake".to_owned(), "UCx".to_owned()])
            .expect("ch");
        assert_eq!(ch.len(), 1);
        d.set_lookups(FakeLookups {
            fail: Some("model list unavailable".into()),
            ..FakeLookups::default()
        })
        .expect("set");
        assert!(
            matches!(d.lookup_currencies(), Err(DaemonError::Rpc(e)) if e.message == "model list unavailable")
        );
    }
}
