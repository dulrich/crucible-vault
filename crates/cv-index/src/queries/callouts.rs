//! Callout queries (plan `intake-removals-sources-rescan` WP-4).

use crate::Result;
use crate::query::Query;

/// Every indexed note with at least one callout of `kind` (case-insensitive, e.g. `removed`),
/// by path; `.trash/` is excluded. The callout line is indexed in `blocks`, so this is the
/// cheap candidate set the daemon confirms with a body parse.
pub fn notes_with_callout(q: &Query<'_>, kind: &str) -> Result<Vec<String>> {
    let mut stmt = q.conn().prepare_cached(
        "SELECT DISTINCT f.path FROM blocks b JOIN files f ON f.id = b.file_id \
         WHERE b.kind = 'callout' AND lower(b.label) = lower(?1) \
         AND f.path NOT LIKE '.trash/%' ORDER BY f.path",
    )?;
    let rows = stmt.query_map([kind], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
}
