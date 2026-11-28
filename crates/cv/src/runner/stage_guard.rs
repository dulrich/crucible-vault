//! The stage model's guard (plan `intake-stage-jobs` WP-2): the production [`StageGuard`]. A
//! fresh seam job runs only when its stage is admissible for its note now
//! ([`cv_ingest::stage::admissible`] over [`crate::note_facts`]); a done, skipped, exhausted or
//! stuck stage, a stage whose hard prerequisites are not settled, an Index that other progressing
//! stages still precede, or a note that is gone or no longer an intake note settles the job
//! without running. The guard fails open: when the facts cannot be read, the job runs (its steps
//! judge the note themselves).
//!
//! Index's "can make progress now" fact: a stage whose stage action resolves to a breaker key
//! whose breaker is not closed is held; the other stages progress when the model admits them
//! (per-note serialization means no other seam job of the note runs while this one is claimed).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cv_actions::ConfigSet;
use cv_ingest::stage::{Progress, Stage, admissible};
use cv_protocol::{ActionContext, BreakerState, NoteLocation};
use cv_search::IndexedHashes;
use cv_state::StateDb;

use super::StageGuard;
use crate::note_facts::{ATTEMPT_CAP, FactEnv, note_view};
use crate::sync::lock;

/// The stage model's guard over the daemon's `StateDb`, vault, index and search database.
pub struct ModelGuard {
    state: Arc<Mutex<StateDb>>,
    vault: PathBuf,
    index_db: PathBuf,
    search_db: PathBuf,
}

impl ModelGuard {
    /// A guard reading `state`, the vault at `vault`, the index at `index_db` and the search
    /// database at `search_db`.
    #[must_use]
    pub fn new(
        state: Arc<Mutex<StateDb>>,
        vault: PathBuf,
        index_db: PathBuf,
        search_db: PathBuf,
    ) -> Self {
        Self {
            state,
            vault,
            index_db,
            search_db,
        }
    }

    /// The note's current path: by id when bound (`None` when the id is retired), else the path
    /// at enqueue.
    fn note_path(&self, context: &ActionContext) -> Option<Option<String>> {
        if let Some(id) = context.active_id {
            match lock(&self.state).note_id_locate(id) {
                Ok(NoteLocation::Live { path }) => return Some(Some(path)),
                Ok(NoteLocation::Gone { .. }) => return Some(None),
                Ok(NoteLocation::Unknown) => {}
                Err(e) => tracing::warn!(error = %e, "stage guard: locating the note failed"),
            }
        }
        context.active_path.clone().map(Some)
    }

    fn judge(&self, stage: Stage, path: &str, config: &ConfigSet) -> anyhow::Result<bool> {
        let reader = cv_index::Reader::open_current(&self.index_db)?;
        let indexed = if stage == Stage::Index {
            Some(IndexedHashes::open_read_only(&self.search_db)?)
        } else {
            None
        };
        let db = lock(&self.state);
        let env = FactEnv {
            vault: &self.vault,
            config,
            reader: &reader,
            state: Some(&*db),
            indexed: indexed.as_ref(),
        };
        let Some(view) = note_view(&env, path)? else {
            return Ok(false);
        };
        let states = view.states(ATTEMPT_CAP);
        let mut progress = Progress::default();
        if stage == Stage::Index {
            for (s, _) in &states {
                let key = config
                    .armed(s.action_id())
                    .and_then(|a| crate::steps::action_service_key(a, config).ok().flatten());
                if let Some(key) = key
                    && db.service_health(&key)?.state != BreakerState::Closed
                {
                    progress.held.push(*s);
                }
            }
        }
        Ok(admissible(&states, stage, &progress))
    }
}

impl StageGuard for ModelGuard {
    fn runnable(&self, stage: &str, context: &ActionContext, config: &ConfigSet) -> bool {
        let Some(stage) = Stage::parse(stage) else {
            return true;
        };
        let path = match self.note_path(context) {
            Some(Some(p)) => p,
            Some(None) => return false,
            None => return true,
        };
        match self.judge(stage, &path, config) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, stage = stage.as_str(), "stage guard: facts unreadable; the job runs");
                true
            }
        }
    }
}
