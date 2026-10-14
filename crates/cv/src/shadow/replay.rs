//! `cv shadow replay <action> --corpus <dir>`: runs one configured action, once, against the
//! user's scratch copy of the vault, for review. This is the user's tool against their own copy,
//! so its output lists corpus-relative paths.
//!
//! Guards (all checked before anything starts):
//! - the corpus must exist and canonicalize outside `CV_PROTECTED_VAULT`, outside every repo
//!   root (`cv_vault::validate_scratch`'s check), and must not overlap the configured `CV_VAULT`;
//! - the action must exist in `CV_CONFIG_DIR` and contain no `search_upsert`/`search_delete`
//!   step — the replay daemon runs in **live** mode, so such a step would mutate the real
//!   companion's index;
//! - the in-process daemon gets a fresh `state.db`, index and socket under
//!   `$CV_SCRATCH/replay-<ts>/`, an **empty** config dir there, and `landlock: false`; the
//!   runner's config is then replaced by `CV_CONFIG_DIR`'s config reduced to the one action with
//!   only a command starter (no event or schedule starter can fire; `schedules.toml` is dropped).
//!
//! Agent steps call the real providers configured in `providers.toml` (cost, network).
//! In live mode the vault filesystem logs no `shadow_writes`, so the reported writes are the
//! corpus diff (content hash per file, before vs after), plus any rows the replay `state.db`
//! holds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use cv_actions::{CONFIG_FILES, ConfigSet, StepKind};
use cv_protocol::{ActionContext, Mode};
use cv_state::{JobStatus, Lane};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::daemon::{Daemon, DaemonOptions};
use crate::runner::{RunRequest, Trigger, apply_defaults};

/// How long a replayed job may run.
pub const REPLAY_TIMEOUT: Duration = Duration::from_secs(600);

/// One replay.
#[derive(Debug, Clone)]
pub struct ReplayRequest {
    /// Action id.
    pub action_id: String,
    /// The user's scratch copy.
    pub corpus: PathBuf,
    /// Run params.
    pub params: Map<String, Value>,
    /// Context `active_path`.
    pub active_path: Option<String>,
    /// Repo roots the corpus must be outside of.
    pub repo_roots: Vec<PathBuf>,
}

fn overlaps(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (canon(a), canon(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Validates the corpus (see the module docs); returns its canonical path.
pub fn check_corpus(config: &Config, req: &ReplayRequest) -> Result<PathBuf> {
    if !req.corpus.is_dir() {
        bail!(
            "corpus {} is not an existing directory",
            req.corpus.display()
        );
    }
    let canon = cv_vault::validate_scratch(&req.corpus, &config.protected_vault, &req.repo_roots)
        .map_err(|e| anyhow!("corpus refused: {e}"))?;
    if overlaps(&canon, &config.vault) {
        bail!(
            "corpus refused: it overlaps the configured CV_VAULT {}",
            config.vault.display()
        );
    }
    Ok(canon)
}

/// Reads `CV_CONFIG_DIR` and reduces it to `action_id` with only a command starter.
pub fn filtered_config(config_dir: &Path, action_id: &str) -> Result<ConfigSet> {
    let mut full = ConfigSet::new();
    crate::runner::load_config_dir(config_dir, &mut full);
    let action = full
        .action(action_id)
        .ok_or_else(|| anyhow!("no action {action_id:?} in {}", config_dir.display()))?;
    if action
        .steps
        .iter()
        .any(|s| matches!(s.kind(), StepKind::SearchUpsert | StepKind::SearchDelete))
    {
        bail!(
            "action {action_id:?} has a search_upsert/search_delete step; replay runs in live mode and would mutate the real search companion — refused"
        );
    }
    let mut files: BTreeMap<String, Option<String>> = BTreeMap::new();
    for name in CONFIG_FILES {
        let text = match std::fs::read_to_string(config_dir.join(name)) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(anyhow!("reading {name}: {e}")),
        };
        files.insert(name.to_owned(), text);
    }
    let actions_text = files
        .get("actions.toml")
        .cloned()
        .flatten()
        .ok_or_else(|| anyhow!("actions.toml missing"))?;
    let mut table: toml::Table = actions_text.parse().context("parsing actions.toml")?;
    let entries = table
        .remove("action")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let mut kept: Vec<toml::Value> = entries
        .into_iter()
        .filter(|e| e.get("id").and_then(toml::Value::as_str) == Some(action_id))
        .collect();
    for e in &mut kept {
        if let Some(t) = e.as_table_mut() {
            let mut cmd = toml::Table::new();
            cmd.insert("type".into(), toml::Value::String("command".into()));
            t.insert(
                "starters".into(),
                toml::Value::Array(vec![toml::Value::Table(cmd)]),
            );
        }
    }
    let mut out = toml::Table::new();
    out.insert("action".into(), toml::Value::Array(kept));
    files.insert(
        "actions.toml".into(),
        Some(toml::to_string(&out).context("serializing the filtered actions")?),
    );
    files.insert("schedules.toml".into(), None);
    let mut set = ConfigSet::new();
    set.reload(&files, &crate::steps::validate);
    if set.armed(action_id).is_none() {
        bail!(
            "action {action_id:?} is disarmed: {}",
            set.disarm_reason(action_id).unwrap_or("unknown")
        );
    }
    Ok(set)
}

/// `path -> sha256` of every regular file under `root` (dot-directories skipped).
fn snapshot(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))? {
            let e = e?;
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let ft = e.file_type()?;
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let bytes = std::fs::read(e.path())?;
                out.insert(rel, hex::encode(Sha256::digest(&bytes)));
            }
        }
    }
    Ok(out)
}

fn diff(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<Value> {
    let mut out = Vec::new();
    for (p, h) in after {
        match before.get(p) {
            None => out.push(json!({"op": "create", "path": p})),
            Some(b) if b != h => out.push(json!({"op": "modify", "path": p})),
            Some(_) => {}
        }
    }
    for p in before.keys().filter(|p| !after.contains_key(*p)) {
        out.push(json!({"op": "delete", "path": p}));
    }
    out
}

/// Runs the replay (inside a tokio runtime) and returns `{job_id, status, message, writes}`.
pub async fn run(config: &Config, req: &ReplayRequest) -> Result<Value> {
    let corpus = check_corpus(config, req)?;
    let filtered = filtered_config(&config.config_dir, &req.action_id)?;
    let action = filtered
        .armed(&req.action_id)
        .cloned()
        .ok_or_else(|| anyhow!("action {:?} not armed", req.action_id))?;
    let scratch = cv_vault::validate_scratch(&config.scratch, &config.vault, &req.repo_roots)
        .context("CV_SCRATCH refused")?;
    let base = scratch.join(format!("replay-{}", crate::now_ms()));
    let rcfg = Config {
        vault: corpus.clone(),
        mode: Mode::Live,
        protected_vault: config.protected_vault.clone(),
        state_db: base.join("state").join("state.db"),
        index_db: base.join("index").join("index.db"),
        scratch: base.join("scratch"),
        config_dir: base.join("config"),
        socket: base.join("d.sock"),
        open_cmd: None,
        clipboard_cmd: config.clipboard_cmd.clone(),
    };
    let before = snapshot(&corpus)?;
    let daemon = Daemon::start(
        rcfg.clone(),
        DaemonOptions {
            landlock: false,
            config_poll: Duration::from_secs(86_400),
            ..DaemonOptions::default()
        },
    )?;
    let running = daemon.spawn().await?;
    let result = drive(&running, &action, filtered, req).await;
    let shutdown = running.shutdown().await;
    let (job_id, status, message) = result?;
    shutdown?;
    let after = snapshot(&corpus)?;
    let mut writes = diff(&before, &after);
    if let Ok(r) = cv_state::StateReader::open(&rcfg.state_db) {
        for w in super::report::all_writes(&r, None)? {
            writes.push(json!({"op": w.op, "path": w.path, "source": "shadow_writes"}));
        }
    }
    Ok(json!({
        "job_id": job_id,
        "status": status,
        "message": message,
        "replay_dir": base.display().to_string(),
        "writes": writes,
    }))
}

async fn drive(
    running: &crate::daemon::RunningDaemon,
    action: &cv_actions::Action,
    filtered: ConfigSet,
    req: &ReplayRequest,
) -> Result<(String, String, Option<String>)> {
    let shared = running.shared();
    let runner = shared
        .runner()
        .ok_or_else(|| anyhow!("replay daemon has no runner"))?;
    runner.set_config(filtered);
    let mut params = req.params.clone();
    apply_defaults(action, &mut params);
    let job_id = runner
        .enqueue(
            action,
            RunRequest {
                params,
                context: ActionContext {
                    active_path: req.active_path.clone(),
                    ..ActionContext::default()
                },
                trigger: Trigger::Command,
                chain: Vec::new(),
                default_lane: Lane::User,
                dedupe_key: None,
            },
        )
        .map_err(|e| anyhow!("enqueue: {e:?}"))?;
    let t0 = Instant::now();
    loop {
        let job = {
            let db = shared
                .state()
                .lock()
                .map_err(|_| anyhow!("state lock poisoned"))?;
            db.job(&job_id)?
        };
        if let Some(j) = job
            && matches!(
                j.status,
                JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
            )
        {
            return Ok((job_id, j.status.as_str().to_owned(), j.error));
        }
        if t0.elapsed() > REPLAY_TIMEOUT {
            bail!("job {job_id} did not finish within {REPLAY_TIMEOUT:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
