//! The sources-only `intake-reconcile` re-match a changed `sources.toml` enqueues, and its
//! follow-up policy ([`cv_state::StateDb::enqueue_with_successor`]).

use cv_actions::Action;
use cv_protocol::{ActionContext, Lane};
use serde_json::Map;

use super::{EnqueueError, Inner, RunRequest, Runner, Trigger, apply_defaults};
use crate::sync::lock;

/// The action a changed `sources.toml` re-runs (sources-only).
const REMATCH_ACTION: &str = "intake-reconcile";

/// The job `dedupe_key` of the sources-only re-match; the action's resource key only serializes.
const SOURCES_REMATCH_DEDUPE: &str = "intake-reconcile:sources";

/// The dedupe key of the follow-up sources re-match enqueued while the first is already running
/// (it read the config at its start): at most one such follow-up is queued at a time.
const SOURCES_REMATCH_NEXT_DEDUPE: &str = "intake-reconcile:sources-next";

impl RunRequest {
    /// The sources-only re-match after a changed `sources.toml`: a background run with the
    /// sources-only params under [`SOURCES_REMATCH_DEDUPE`], so repeated saves leave at most one
    /// queued or running re-match.
    fn sources_rematch(action: &Action) -> Self {
        let mut params = Map::new();
        apply_defaults(action, &mut params);
        crate::steps::reconcile::insert_only_sources(&mut params);
        Self {
            params,
            context: ActionContext::default(),
            trigger: Trigger::Command,
            chain: Vec::new(),
            default_lane: Lane::Background,
            dedupe_key: Some(SOURCES_REMATCH_DEDUPE.to_owned()),
        }
    }
}

impl Runner {
    /// Enqueues the sources-only re-match; skipped (one `debug` line) when the config has no
    /// armed such action. A duplicate of a queued or running run is coalesced.
    pub(super) fn enqueue_sources_rematch(&self) {
        let cfg = self.config();
        let Some(action) = cfg.armed(REMATCH_ACTION) else {
            tracing::debug!("sources changed; no `{REMATCH_ACTION}` action, re-match not enqueued");
            return;
        };
        match self.inner.enqueue_rematch(action) {
            Ok(id) => tracing::info!(job = %id, "sources changed; re-match enqueued"),
            Err(e) => tracing::warn!(error = %e, "sources changed; re-match not enqueued"),
        }
    }
}

impl Inner {
    /// A still-queued re-match absorbs the change (it has not read the config yet); a running one
    /// read the config at its start, so the change enqueues exactly one follow-up under
    /// [`SOURCES_REMATCH_NEXT_DEDUPE`], into which further changes coalesce. Both share the
    /// step's resource key, so they never overlap.
    fn enqueue_rematch(&self, action: &Action) -> Result<String, EnqueueError> {
        let job = self.new_job(action, RunRequest::sources_rematch(action))?;
        let out = lock(&self.state)
            .enqueue_with_successor(job, SOURCES_REMATCH_NEXT_DEDUPE, self.clock.now_ms())
            .map_err(|e| EnqueueError::State(e.to_string()))?;
        self.enqueued(out)
    }
}
