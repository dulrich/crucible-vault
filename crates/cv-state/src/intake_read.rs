//! [`IntakeRead`] (plan `intake-stage-jobs` WP-2): the read-only lookups the intake stage facts
//! need, over the daemon's [`StateDb`] and the daemon-free [`StateReader`] alike, so one facts
//! builder serves the `cv intake pending` report and the runner's stage guard. Nothing here
//! writes.

use crate::error::Result;
use crate::image_descriptions::ImageDescription;
use crate::stage_outcomes::{LearnedHost, StageOutcomeRow};
use crate::store::{StateDb, StateReader};
use crate::tracker::{TrackerItem, rows_where, status_of};
use rusqlite::params;

/// Read-only intake lookups (see each method's [`StateDb`] namesake).
pub trait IntakeRead {
    /// Rows whose `note_path` is `path`, oldest first.
    fn tracker_items_for_note(&self, path: &str) -> Result<Vec<TrackerItem>>;
    /// Rows whose `url` is exactly `url`, oldest first.
    fn tracker_items_for_url(&self, url: &str) -> Result<Vec<TrackerItem>>;
    /// Rows whose `url` contains `needle` (empty needle: none), oldest first.
    fn tracker_items_url_containing(&self, needle: &str) -> Result<Vec<TrackerItem>>;
    /// Rows of `source_kind` whose `item_id` is `item_id`, oldest first.
    fn tracker_items_for_item(&self, source_kind: &str, item_id: &str) -> Result<Vec<TrackerItem>>;
    /// One item's status (`Ok(None)`: no such row; `Some(None)`: open).
    fn tracker_item_status(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
    ) -> Result<Option<Option<char>>>;
    /// Every learned publication host.
    fn learned_hosts(&self) -> Result<Vec<LearnedHost>>;
    /// Stored image descriptions among `md5s`.
    fn image_descriptions_for(&self, md5s: &[String]) -> Result<Vec<ImageDescription>>;
    /// The stage outcome rows of `ids`.
    fn stage_outcomes_for(&self, ids: &[i64]) -> Result<Vec<StageOutcomeRow>>;
    /// The live note id at `path`.
    fn live_note_id(&self, path: &str) -> Result<Option<i64>>;
}

impl IntakeRead for StateDb {
    fn tracker_items_for_note(&self, path: &str) -> Result<Vec<TrackerItem>> {
        StateDb::tracker_items_for_note(self, path)
    }
    fn tracker_items_for_url(&self, url: &str) -> Result<Vec<TrackerItem>> {
        StateDb::tracker_items_for_url(self, url)
    }
    fn tracker_items_url_containing(&self, needle: &str) -> Result<Vec<TrackerItem>> {
        StateDb::tracker_items_url_containing(self, needle)
    }
    fn tracker_items_for_item(&self, source_kind: &str, item_id: &str) -> Result<Vec<TrackerItem>> {
        StateDb::tracker_items_for_item(self, source_kind, item_id)
    }
    fn tracker_item_status(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
    ) -> Result<Option<Option<char>>> {
        StateDb::tracker_item_status(self, source_kind, source_key, item_id)
    }
    fn learned_hosts(&self) -> Result<Vec<LearnedHost>> {
        StateDb::learned_hosts(self)
    }
    fn image_descriptions_for(&self, md5s: &[String]) -> Result<Vec<ImageDescription>> {
        StateDb::image_descriptions_for(self, md5s)
    }
    fn stage_outcomes_for(&self, ids: &[i64]) -> Result<Vec<StageOutcomeRow>> {
        StateDb::stage_outcomes_for(self, ids)
    }
    fn live_note_id(&self, path: &str) -> Result<Option<i64>> {
        Ok(StateDb::note_id_at(self, path)?.map(|id| i64::try_from(id.0).unwrap_or(i64::MAX)))
    }
}

impl IntakeRead for StateReader {
    fn tracker_items_for_note(&self, path: &str) -> Result<Vec<TrackerItem>> {
        rows_where(&self.conn, "note_path = ?1", params![path])
    }
    fn tracker_items_for_url(&self, url: &str) -> Result<Vec<TrackerItem>> {
        rows_where(&self.conn, "url = ?1", params![url])
    }
    fn tracker_items_url_containing(&self, needle: &str) -> Result<Vec<TrackerItem>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        rows_where(&self.conn, "instr(url, ?1) > 0", params![needle])
    }
    fn tracker_items_for_item(&self, source_kind: &str, item_id: &str) -> Result<Vec<TrackerItem>> {
        rows_where(
            &self.conn,
            "source_kind = ?1 AND item_id = ?2",
            params![source_kind, item_id],
        )
    }
    fn tracker_item_status(
        &self,
        source_kind: &str,
        source_key: &str,
        item_id: &str,
    ) -> Result<Option<Option<char>>> {
        status_of(&self.conn, source_kind, source_key, item_id)
    }
    fn learned_hosts(&self) -> Result<Vec<LearnedHost>> {
        StateReader::learned_hosts(self)
    }
    fn image_descriptions_for(&self, md5s: &[String]) -> Result<Vec<ImageDescription>> {
        StateReader::image_descriptions_for(self, md5s)
    }
    fn stage_outcomes_for(&self, ids: &[i64]) -> Result<Vec<StageOutcomeRow>> {
        StateReader::stage_outcomes_for(self, ids)
    }
    fn live_note_id(&self, path: &str) -> Result<Option<i64>> {
        StateReader::note_id_at(self, path)
    }
}
