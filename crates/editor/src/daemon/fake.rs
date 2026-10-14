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
    ActionInfo, ContentHash, DaemonClient, DaemonError, HelloResult, JobStatus, LocalIndex, Mode,
    Notification, Result, RunActionParams, RunActionResult, StatusResult, Topic, ViewResult,
    ViewsListResult, vault_abs,
};

/// A scripted action: params in, `Ok(open_path)` or `Err(message)` out.
pub type ActionHandler =
    Arc<dyn Fn(&RunActionParams) -> std::result::Result<Option<String>, String> + Send + Sync>;

struct Inner {
    index: Index,
    mode: Mode,
    subscriber: Option<(Vec<Topic>, Sender<Notification>)>,
    actions: BTreeMap<String, (ActionInfo, ActionHandler)>,
    jobs: BTreeMap<String, JobStatus>,
    next_job: u64,
    /// WP-7: scripted view results, and every `view_run` call `(view_id, this_path)` in order.
    views: BTreeMap<String, ViewResult>,
    view_calls: Vec<(String, Option<String>)>,
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
    /// Serves `vault_root`, keeping `index.db`/`state.db` under `data_dir` (never in the vault).
    /// Starts in `live` mode so editing paths are exercisable.
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
            mode: Mode::Live,
            vault_root: vault_root.display().to_string(),
            index_db: base.display().to_string(),
            state_db: state_db.display().to_string(),
            generation,
        };
        Ok(Self {
            inner: Mutex::new(Inner {
                index,
                mode: Mode::Live,
                subscriber: None,
                actions: BTreeMap::new(),
                jobs: BTreeMap::new(),
                next_job: 1,
                views: BTreeMap::new(),
                view_calls: Vec::new(),
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

    /// Switches the reported mode and emits `mode_changed`.
    pub fn set_mode(&self, mode: Mode) -> Result<()> {
        let mut g = self.lock()?;
        g.mode = mode;
        emit(&mut g, Notification::ModeChanged { mode });
        Ok(())
    }

    /// Emits an arbitrary notification (tests of the UI's reaction).
    pub fn emit(&self, n: Notification) -> Result<()> {
        let mut g = self.lock()?;
        emit(&mut g, n);
        Ok(())
    }

    /// WP-7: scripts `view_run(id, _)` to return `result` (and lists `id` in `views_list`).
    pub fn set_view_result(&self, id: &str, result: ViewResult) -> Result<()> {
        self.lock()?.views.insert(id.to_owned(), result);
        Ok(())
    }

    /// WP-7: every `view_run` call so far, `(view_id, this_path)` in order.
    pub fn view_calls(&self) -> Result<Vec<(String, Option<String>)>> {
        Ok(self.lock()?.view_calls.clone())
    }

    /// The current index generation.
    pub fn generation(&self) -> Result<u64> {
        Ok(self.lock()?.index.generation()?)
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
            },
        );
        emit(g, Notification::IndexGeneration { n });
        Ok(())
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
        let abs = vault_abs(&self.vault_root, path)?;
        let mut g = self.lock()?;
        let found = current_hash(&abs)?;
        let matches = match &found {
            Some(h) => h == base_hash,
            None => base_hash.as_str().is_empty(),
        };
        if !matches {
            return Err(DaemonError::Conflict {
                current_hash: found.unwrap_or_else(|| ContentHash::from_hex("")),
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
        self.write_note(path, content, &ContentHash::from_hex(""))?;
        Ok(path.to_owned())
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
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
        let abs = vault_abs(&self.vault_root, path)?;
        let mut g = self.lock()?;
        std::fs::remove_file(&abs).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => DaemonError::NotFound(path.to_owned()),
            _ => DaemonError::Io(e),
        })?;
        self.after_write(&mut g, path, ChangeKind::Deleted, None)
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
            let g = self.lock()?;
            g.actions
                .get(&params.action_id)
                .map(|(_, h)| Arc::clone(h))
                .ok_or_else(|| DaemonError::NotFound(params.action_id.clone()))?
        };
        // The handler runs without the lock so it may call back into the fake.
        let outcome = handler(&params);
        let mut g = self.lock()?;
        let job_id = format!("fake-{}", g.next_job);
        g.next_job += 1;
        let (status, open_path, message) = match outcome {
            Ok(p) => (JobState::Done, p, None),
            Err(m) => (JobState::Failed, None, Some(m)),
        };
        let job = JobStatus {
            job_id: job_id.clone(),
            status,
            message,
            output_paths: open_path.iter().cloned().collect(),
        };
        g.jobs.insert(job_id.clone(), job.clone());
        emit(&mut g, Notification::JobUpdated(job));
        Ok(RunActionResult { job_id, open_path })
    }

    fn job(&self, job_id: &str) -> Result<JobStatus> {
        self.lock()?
            .jobs
            .get(job_id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(job_id.to_owned()))
    }

    fn cancel(&self, job_id: &str) -> Result<JobStatus> {
        let mut g = self.lock()?;
        let job = g
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| DaemonError::NotFound(job_id.to_owned()))?;
        if matches!(job.status, JobState::Pending | JobState::Running) {
            job.status = JobState::Cancelled;
        }
        Ok(job.clone())
    }

    fn status(&self) -> Result<StatusResult> {
        let g = self.lock()?;
        Ok(StatusResult {
            mode: g.mode,
            generation: g.index.generation()?,
            queue: BTreeMap::new(),
            config_errors: vec![],
            services: vec![],
        })
    }

    fn views_list(&self) -> Result<ViewsListResult> {
        Ok(ViewsListResult {
            view_ids: self.lock()?.views.keys().cloned().collect(),
        })
    }

    fn view_run(&self, view_id: &str, this_path: Option<&str>) -> Result<ViewResult> {
        let mut g = self.lock()?;
        g.view_calls
            .push((view_id.to_owned(), this_path.map(str::to_owned)));
        g.views
            .get(view_id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(view_id.to_owned()))
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
