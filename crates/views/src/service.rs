//! [`ViewHost`]: the daemon-hosted service. `crates/cv` adapts it to its `ViewService` trait
//! (this crate cannot depend on `cv`). It owns its own read-only connection on the reader's
//! generation file and reopens it when the generation changes.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate};
use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

use crate::db::Db;
use crate::def::{Base, load_dir};
use crate::engine;
use crate::eval::Env;
use crate::result::ViewResult;

/// Why a view run failed (definition problems are notices, not errors).
#[derive(Debug, Error)]
pub enum ViewsError {
    #[error("unknown view `{0}`")]
    UnknownView(String),
    #[error("index: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("encoding result: {0}")]
    Json(#[from] serde_json::Error),
}

/// The view evaluator hosted by the daemon.
pub struct ViewHost {
    config_dir: PathBuf,
    index_base: PathBuf,
    bases: Vec<Base>,
    load_notices: Vec<String>,
    conn: Option<(u64, Connection)>,
    today: Option<NaiveDate>,
}

impl std::fmt::Debug for ViewHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewHost")
            .field("config_dir", &self.config_dir)
            .field("views", &self.view_ids().len())
            .finish_non_exhaustive()
    }
}

impl ViewHost {
    /// Loads `<config_dir>/views/*.toml`; `index_base` is `CV_INDEX_DB`.
    pub fn new(config_dir: &Path, index_base: &Path) -> Self {
        let (bases, load_notices) = load_dir(config_dir);
        Self {
            config_dir: config_dir.to_path_buf(),
            index_base: index_base.to_path_buf(),
            bases,
            load_notices,
            conn: None,
            today: None,
        }
    }

    /// Pins `today()` (tests); default is the local date at run time.
    #[must_use]
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }

    /// Definition-file problems from the last (re)load.
    pub fn load_notices(&self) -> &[String] {
        &self.load_notices
    }

    /// Every view id (`<file stem>#<view name>`), sorted.
    pub fn view_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .bases
            .iter()
            .flat_map(|b| b.views.iter().map(|v| b.id(v)))
            .collect();
        v.sort();
        v
    }

    /// A new index generation: every view is invalidated (coarse); the connection reopens lazily.
    pub fn on_index_generation(&mut self, n: u64) -> Vec<String> {
        if self.conn.as_ref().is_some_and(|(g, _)| *g != n) {
            self.conn = None;
        }
        self.view_ids()
    }

    /// Reloads definitions from `config_dir`; returns the old and new ids (deduplicated).
    pub fn on_config_changed(&mut self, config_dir: &Path) -> Vec<String> {
        let mut ids = self.view_ids();
        let (bases, notices) = load_dir(config_dir);
        self.config_dir = config_dir.to_path_buf();
        self.bases = bases;
        self.load_notices = notices;
        ids.extend(self.view_ids());
        ids.sort();
        ids.dedup();
        ids
    }

    /// Evaluates `view_id` against index generation `generation`.
    pub fn run_typed(
        &mut self,
        generation: u64,
        view_id: &str,
        this_path: Option<&str>,
    ) -> Result<ViewResult, ViewsError> {
        let (bi, vi) = self
            .bases
            .iter()
            .enumerate()
            .find_map(|(bi, b)| {
                b.views
                    .iter()
                    .position(|v| b.id(v) == view_id)
                    .map(|vi| (bi, vi))
            })
            .ok_or_else(|| ViewsError::UnknownView(view_id.to_string()))?;
        let today = self.today.unwrap_or_else(|| Local::now().date_naive());
        let load_notices = self.load_notices.clone();
        let conn = open_conn(&mut self.conn, &self.index_base, generation)?;
        let Some(base) = self.bases.get(bi) else {
            return Err(ViewsError::UnknownView(view_id.to_string()));
        };
        let Some(view) = base.views.get(vi) else {
            return Err(ViewsError::UnknownView(view_id.to_string()));
        };
        let db = Db::new(conn);
        let this = match this_path {
            Some(p) => db.file(p)?,
            None => None,
        };
        let env = Env {
            db: &db,
            formulas: &base.formulas,
            this,
            today,
            now: today.and_time(Local::now().time()),
            notices: RefCell::default(),
        };
        let mut r = engine::run(&env, view)?;
        r.notices
            .extend(load_notices.into_iter().map(|n| format!("load: {n}")));
        Ok(r)
    }

    /// The `ViewService::run` body: evaluates on `reader`'s generation.
    pub fn run(
        &mut self,
        reader: &cv_index::Reader,
        view_id: &str,
        this_path: Option<&str>,
    ) -> Result<cv_protocol::ViewResult, String> {
        self.run_typed(reader.generation(), view_id, this_path)
            .and_then(|r| r.into_protocol().map_err(ViewsError::from))
            .map_err(|e| e.to_string())
    }
}

fn open_conn<'c>(
    slot: &'c mut Option<(u64, Connection)>,
    index_base: &Path,
    generation: u64,
) -> Result<&'c Connection, ViewsError> {
    if slot.as_ref().is_none_or(|(g, _)| *g != generation) {
        *slot = None;
        let path = cv_index::generation_path(index_base, generation);
        let c = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        *slot = Some((generation, c));
    }
    match slot {
        Some((_, c)) => Ok(c),
        None => Err(ViewsError::Sql(rusqlite::Error::InvalidQuery)),
    }
}
