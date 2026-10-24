//! [`ViewHost`]: the daemon-hosted service (`crates/cv/src/views.rs` runs it on its view thread).
//! It owns its own read-only connection on the reader's
//! generation file and reopens it when the generation changes. `source = "tracker_items"` views
//! (plan round-5-reading-pickers-intake WP-10) also read `state.db` through a lazily opened
//! read-only connection.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate};
use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

use crate::db::Db;
use crate::def::{Base, Source, load_dir};
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
    #[error("state.db: {0}")]
    State(rusqlite::Error),
    #[error("view `{0}` reads state.db, but no state.db path is configured")]
    NoStateDb(String),
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
    state_db: Option<PathBuf>,
    state_conn: Option<Connection>,
    today: Option<NaiveDate>,
    /// `source = "sources"` rows, as last set by [`Self::set_sources`].
    sources: Vec<engine::Record>,
    /// Note path → first matching `[[serial]]` row name (`file.serial`), from [`Self::set_sources`].
    serial: Option<std::collections::BTreeMap<String, String>>,
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
            state_db: None,
            state_conn: None,
            today: None,
            sources: Vec::new(),
            serial: None,
        }
    }

    /// Sets the `state.db` path (`CV_STATE_DB`) `tracker_items` views read from.
    #[must_use]
    pub fn with_state_db(mut self, path: &Path) -> Self {
        self.state_db = Some(path.to_path_buf());
        self.state_conn = None;
        self
    }

    /// The ids of the views whose effective `source` (the view's override, else its file's) is
    /// `source`.
    pub fn view_ids_for(&self, source: Source) -> Vec<String> {
        let mut v: Vec<String> = self
            .bases
            .iter()
            .flat_map(|b| {
                b.views
                    .iter()
                    .filter(move |v| v.source == source)
                    .map(|v| b.id(v))
            })
            .collect();
        v.sort();
        v
    }

    /// A `state.db` table changed (daemon-internal signal): the ids of the views reading it.
    pub fn on_state_changed(&mut self, table: &str) -> Vec<String> {
        match table {
            "tracker_items" => self.view_ids_for(Source::TrackerItems),
            _ => Vec::new(),
        }
    }

    /// Sets the `source = "sources"` rows and the `file.serial` match set (plan
    /// round-11-feedback-todos Decision 5). The daemon (`crates/cv/src/views.rs`) computes both
    /// from `sources.toml` and the index with `cv_actions`' serial matcher, so this crate never
    /// restates it. Records bind `name`, `publication`, `author`, `prefix`, `kind`, `status`,
    /// `row`, `entries`, `unread` and `last_update`.
    pub fn set_sources(
        &mut self,
        records: Vec<engine::Record>,
        serial: std::collections::BTreeMap<String, String>,
    ) {
        self.sources = records;
        self.serial = Some(serial);
    }

    /// Whether running `view_id` reads [`Self::set_sources`] data: its effective source is
    /// `sources`, or its definition file mentions `file.serial` ([`Base::uses_serial`]).
    pub fn needs_sources(&self, view_id: &str) -> bool {
        self.bases.iter().any(|b| {
            b.views
                .iter()
                .any(|v| b.id(v) == view_id && (v.source == Source::Sources || b.uses_serial))
        })
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

    /// A new index generation: every index- and sources-sourced view is invalidated (coarse;
    /// `sources` counts are over index notes; `tracker_items` views are not — they change on
    /// [`Self::on_state_changed`]); the connection reopens lazily.
    pub fn on_index_generation(&mut self, n: u64) -> Vec<String> {
        if self.conn.as_ref().is_some_and(|(g, _)| *g != n) {
            self.conn = None;
        }
        let mut v = self.view_ids_for(Source::Index);
        v.extend(self.view_ids_for(Source::Sources));
        v.sort();
        v
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
        let source = self
            .bases
            .get(bi)
            .and_then(|b| b.views.get(vi))
            .map(|v| v.source);
        let records = match source {
            Some(Source::TrackerItems) => {
                let path = self
                    .state_db
                    .clone()
                    .ok_or_else(|| ViewsError::NoStateDb(view_id.to_string()))?;
                let sc = open_state(&mut self.state_conn, &path)?;
                Some(tracker_records(sc).map_err(ViewsError::State)?)
            }
            Some(Source::Sources) => Some(self.sources.clone()),
            _ => None,
        };
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
            serial: self.serial.as_ref(),
        };
        let mut r = engine::run_on(&env, view, records.as_deref())?;
        r.notices
            .extend(load_notices.into_iter().map(|n| format!("load: {n}")));
        Ok(r)
    }

    /// The daemon's `view_run` body: evaluates on `reader`'s generation.
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

fn open_state<'c>(
    slot: &'c mut Option<Connection>,
    path: &Path,
) -> Result<&'c Connection, ViewsError> {
    if slot.is_none() {
        let c = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(ViewsError::State)?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(ViewsError::State)?;
        *slot = Some(c);
    }
    match slot {
        Some(c) => Ok(c),
        None => Err(ViewsError::State(rusqlite::Error::InvalidQuery)),
    }
}

/// Every `tracker_items` row as a record, newest first (`first_seen_ms DESC, item_id`). Fields:
/// `title`, `url`, `source_kind`, `source_key`, `item_id`, `day`, `outcome`, `score`,
/// `full_body`, `first_seen`, `status` (single-char task status, null = open; state.db v8), `done`
/// (bool, `status` is `x`/`X` — the legacy alias column), `note_path` (text or null) and — from
/// state.db v7 — `source_name` (text or null), `duration` (seconds or null) and `word_count` (or
/// null) — and from state.db v12 `access` (`free`/`paid`/`founding`/`unknown` or null), `complete`
/// (bool or null = unknown) and `full_word_count` (num or null).
pub fn tracker_records(conn: &Connection) -> rusqlite::Result<Vec<engine::Record>> {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use crate::value::Value;
    let mut stmt = conn.prepare_cached(
        "SELECT title, url, source_kind, source_key, item_id, day, outcome, score, full_body,
                first_seen_ms, status IN ('x', 'X'), note_path, source_name, duration_s, word_count,
                status, access, complete, full_word_count
         FROM tracker_items ORDER BY first_seen_ms DESC, item_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let mut m = BTreeMap::new();
        for (i, k) in [
            "title",
            "url",
            "source_kind",
            "source_key",
            "item_id",
            "day",
            "outcome",
        ]
        .into_iter()
        .enumerate()
        {
            m.insert(k.to_string(), Value::Str(r.get(i)?));
        }
        let score: Option<f64> = r.get(7)?;
        m.insert("score".into(), score.map_or(Value::Null, Value::Num));
        let full_body: Option<bool> = r.get(8)?;
        m.insert(
            "full_body".into(),
            full_body.map_or(Value::Null, Value::Bool),
        );
        m.insert("first_seen".into(), crate::eval::ms_date(r.get(9)?));
        m.insert(
            "done".into(),
            Value::Bool(r.get::<_, Option<bool>>(10)?.unwrap_or(false)),
        );
        let status: Option<String> = r.get(15)?;
        m.insert("status".into(), status.map_or(Value::Null, Value::Str));
        let note_path: Option<String> = r.get(11)?;
        m.insert(
            "note_path".into(),
            note_path.map_or(Value::Null, Value::Str),
        );
        let source_name: Option<String> = r.get(12)?;
        m.insert(
            "source_name".into(),
            source_name.map_or(Value::Null, Value::Str),
        );
        #[allow(clippy::cast_precision_loss)]
        let num = |n: Option<i64>| n.map_or(Value::Null, |n| Value::Num(n as f64));
        m.insert("duration".into(), num(r.get(13)?));
        m.insert("word_count".into(), num(r.get(14)?));
        let access: Option<String> = r.get(16)?;
        m.insert("access".into(), access.map_or(Value::Null, Value::Str));
        let complete: Option<bool> = r.get(17)?;
        m.insert("complete".into(), complete.map_or(Value::Null, Value::Bool));
        m.insert("full_word_count".into(), num(r.get(18)?));
        Ok(Arc::new(m))
    })?;
    rows.collect()
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
