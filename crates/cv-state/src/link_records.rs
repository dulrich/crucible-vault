//! Link records (migration v5, plan `v0-automation-pipelines` WP-3): the `link_records` store.
//!
//! Merge rules are a port of Crucible `mergeLinkRecord` / `unionSourceNotes` / `pickDate`
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/db/LinkRegistryStore.ts:150-247`) and its upsert's
//! no-op detection (`:137`): an upsert whose merged record equals the stored row performs no
//! write. Nothing Obsidian-induced was carried (Crucible's store already lived in SQLite).

use std::path::Path;

use rusqlite::{OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use crate::error::Result;
use crate::store::StateDb;

/// One `link_records` row.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRecord {
    /// Primary key: the canonical URL.
    pub canonical_url: String,
    /// The last observed raw URL.
    pub url: String,
    /// Host (may be empty).
    pub domain: String,
    /// `[[path-without-.md]]` wikilinks, stored order first.
    pub source_notes: Vec<String>,
    /// Earliest valid `YYYY-MM-DD…`.
    pub first_seen: Option<String>,
    /// Latest valid `YYYY-MM-DD…`.
    pub last_seen: Option<String>,
    /// Triage state (default `pending`).
    pub state: String,
    /// How it was found (default `scan`).
    pub discovery_method: String,
    /// JSON scalar: `false` (unflagged), `"candidate"`, or another stored value (kept verbatim).
    pub tracked_source: Value,
    pub tracked_source_type: Option<String>,
    pub tracked_source_note: Option<String>,
    pub referred_material: Option<String>,
    pub decision_reason: Option<String>,
    pub yt_video_id: Option<String>,
    pub x_status_id: Option<String>,
}

/// One observation to merge (Crucible `LinkRecordInput`). `None` = not observed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LinkRecordInput {
    pub canonical_url: String,
    pub url: String,
    pub domain: Option<String>,
    pub source_notes: Vec<String>,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub state: Option<String>,
    pub discovery_method: Option<String>,
    /// Anything other than `false`/`null`/absent escalates an unflagged stored value.
    pub tracked_source: Option<Value>,
    pub tracked_source_type: Option<String>,
    pub tracked_source_note: Option<String>,
    pub referred_material: Option<String>,
    pub decision_reason: Option<String>,
    pub yt_video_id: Option<String>,
    pub x_status_id: Option<String>,
}

impl From<LinkRecord> for LinkRecordInput {
    /// A stored record as an observation (the Crucible importer feeds rows this way).
    fn from(r: LinkRecord) -> Self {
        LinkRecordInput {
            canonical_url: r.canonical_url,
            url: r.url,
            domain: Some(r.domain),
            source_notes: r.source_notes,
            first_seen: r.first_seen,
            last_seen: r.last_seen,
            state: Some(r.state),
            discovery_method: Some(r.discovery_method),
            tracked_source: Some(r.tracked_source),
            tracked_source_type: r.tracked_source_type,
            tracked_source_note: r.tracked_source_note,
            referred_material: r.referred_material,
            decision_reason: r.decision_reason,
            yt_video_id: r.yt_video_id,
            x_status_id: r.x_status_id,
        }
    }
}

/// What [`merge`] did beyond producing the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeFlags {
    /// No stored record existed.
    pub created: bool,
    /// `tracked_source` escalated from unflagged to the incoming value.
    pub candidate_flagged: bool,
}

/// What [`StateDb::upsert_link_record`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpsertOutcome {
    /// A row was inserted.
    pub created: bool,
    /// A row was inserted or updated (`false` = no write happened).
    pub changed: bool,
    /// `tracked_source` escalated (only reported when a write happened).
    pub candidate_flagged: bool,
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.trim().is_empty())
}

fn valid_date(v: Option<&str>) -> Option<&str> {
    let s = v?;
    let b = s.as_bytes();
    let ok = b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit);
    ok.then_some(s)
}

fn pick_date(a: Option<&str>, b: Option<&str>, min: bool) -> Option<String> {
    match (valid_date(a), valid_date(b)) {
        (None, y) => y.map(str::to_owned),
        (x, None) => x.map(str::to_owned),
        (Some(x), Some(y)) => Some(if (x <= y) == min { x } else { y }.to_owned()),
    }
}

fn union_source_notes(existing: &[String], additions: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in existing.iter().chain(additions) {
        if !v.trim().is_empty() && !out.contains(v) {
            out.push(v.clone());
        }
    }
    out
}

fn is_unflagged(v: Option<&Value>) -> bool {
    matches!(v, None | Some(Value::Null | Value::Bool(false)))
}

fn keep(stored: Option<&str>, incoming: Option<&str>) -> Option<String> {
    non_empty(stored).or(non_empty(incoming)).map(str::to_owned)
}

/// Merges one observation into the stored record (Crucible `mergeLinkRecord`, pure).
#[must_use]
pub fn merge(existing: Option<&LinkRecord>, input: &LinkRecordInput) -> (LinkRecord, MergeFlags) {
    let fresh;
    let base = match existing {
        Some(r) => r,
        None => {
            fresh = LinkRecord {
                canonical_url: input.canonical_url.clone(),
                url: input.url.clone(),
                domain: input.domain.clone().unwrap_or_default(),
                source_notes: Vec::new(),
                first_seen: None,
                last_seen: None,
                state: String::new(),
                discovery_method: String::new(),
                tracked_source: Value::Bool(false),
                tracked_source_type: None,
                tracked_source_note: None,
                referred_material: None,
                decision_reason: None,
                yt_video_id: None,
                x_status_id: None,
            };
            &fresh
        }
    };
    let mut candidate_flagged = false;
    let mut tracked_source = base.tracked_source.clone();
    let mut tracked_source_type = base.tracked_source_type.clone();
    if !is_unflagged(input.tracked_source.as_ref()) && is_unflagged(Some(&base.tracked_source)) {
        tracked_source = input.tracked_source.clone().unwrap_or(Value::Bool(false));
        tracked_source_type = non_empty(input.tracked_source_type.as_deref()).map(str::to_owned);
        candidate_flagged = true;
    }
    if is_unflagged(Some(&tracked_source)) {
        tracked_source = Value::Bool(false);
    }
    let record = LinkRecord {
        canonical_url: base.canonical_url.clone(),
        url: if input.url.trim().is_empty() {
            base.url.clone()
        } else {
            input.url.clone()
        },
        domain: non_empty(input.domain.as_deref())
            .map_or_else(|| base.domain.clone(), str::to_owned),
        source_notes: union_source_notes(&base.source_notes, &input.source_notes),
        first_seen: pick_date(
            base.first_seen.as_deref(),
            input.first_seen.as_deref(),
            true,
        ),
        last_seen: pick_date(base.last_seen.as_deref(), input.last_seen.as_deref(), false),
        state: keep(Some(&base.state), input.state.as_deref()).unwrap_or_else(|| "pending".into()),
        discovery_method: keep(
            Some(&base.discovery_method),
            input.discovery_method.as_deref(),
        )
        .unwrap_or_else(|| "scan".into()),
        tracked_source,
        tracked_source_type,
        tracked_source_note: keep(
            base.tracked_source_note.as_deref(),
            input.tracked_source_note.as_deref(),
        ),
        referred_material: keep(
            base.referred_material.as_deref(),
            input.referred_material.as_deref(),
        ),
        decision_reason: keep(
            base.decision_reason.as_deref(),
            input.decision_reason.as_deref(),
        ),
        yt_video_id: keep(base.yt_video_id.as_deref(), input.yt_video_id.as_deref()),
        x_status_id: keep(base.x_status_id.as_deref(), input.x_status_id.as_deref()),
    };
    (
        record,
        MergeFlags {
            created: existing.is_none(),
            candidate_flagged,
        },
    )
}

const COLS: &str = "canonical_url, url, domain, source_notes, first_seen, last_seen, state, \
    discovery_method, tracked_source, tracked_source_type, tracked_source_note, \
    referred_material, decision_reason, yt_video_id, x_status_id";

/// Maps a row selected with [`COLS`] (Crucible `mapRow`: corrupt JSON falls back, NULL text
/// columns take their defaults).
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<LinkRecord> {
    let notes: Option<String> = r.get(3)?;
    let source_notes = notes
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| match v {
            Value::Array(a) => Some(
                a.into_iter()
                    .filter_map(|x| match x {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let tracked: Option<String> = r.get(8)?;
    Ok(LinkRecord {
        canonical_url: r.get(0)?,
        url: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        domain: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        source_notes,
        first_seen: r.get(4)?,
        last_seen: r.get(5)?,
        state: r
            .get::<_, Option<String>>(6)?
            .unwrap_or_else(|| "pending".into()),
        discovery_method: r
            .get::<_, Option<String>>(7)?
            .unwrap_or_else(|| "scan".into()),
        tracked_source: tracked
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Bool(false)),
        tracked_source_type: r.get(9)?,
        tracked_source_note: r.get(10)?,
        referred_material: r.get(11)?,
        decision_reason: r.get(12)?,
        yt_video_id: r.get(13)?,
        x_status_id: r.get(14)?,
    })
}

fn get(conn: &rusqlite::Connection, canonical: &str) -> Result<Option<LinkRecord>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM link_records WHERE canonical_url = ?1"),
            [canonical],
            row,
        )
        .optional()?)
}

fn list(conn: &rusqlite::Connection, sql: &str, arg: &str) -> Result<Vec<LinkRecord>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([arg], row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl StateDb {
    /// Create-or-merge one observation in one IMMEDIATE transaction; an upsert whose merged
    /// record equals the stored row performs no write (`changed: false`).
    pub fn upsert_link_record(&mut self, input: &LinkRecordInput) -> Result<UpsertOutcome> {
        let tx = Transaction::new_unchecked(self.conn(), TransactionBehavior::Immediate)?;
        let existing = get(&tx, &input.canonical_url)?;
        let (rec, flags) = merge(existing.as_ref(), input);
        if existing.as_ref() == Some(&rec) {
            tx.commit()?;
            return Ok(UpsertOutcome::default());
        }
        let notes = serde_json::to_string(&rec.source_notes)?;
        let tracked = serde_json::to_string(&rec.tracked_source)?;
        tx.execute(
            &format!(
                "INSERT INTO link_records ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(canonical_url) DO UPDATE SET url = excluded.url, domain = excluded.domain,
                 source_notes = excluded.source_notes, first_seen = excluded.first_seen,
                 last_seen = excluded.last_seen, state = excluded.state,
                 discovery_method = excluded.discovery_method, tracked_source = excluded.tracked_source,
                 tracked_source_type = excluded.tracked_source_type,
                 tracked_source_note = excluded.tracked_source_note,
                 referred_material = excluded.referred_material,
                 decision_reason = excluded.decision_reason, yt_video_id = excluded.yt_video_id,
                 x_status_id = excluded.x_status_id"
            ),
            params![
                rec.canonical_url,
                rec.url,
                rec.domain,
                notes,
                rec.first_seen,
                rec.last_seen,
                rec.state,
                rec.discovery_method,
                tracked,
                rec.tracked_source_type,
                rec.tracked_source_note,
                rec.referred_material,
                rec.decision_reason,
                rec.yt_video_id,
                rec.x_status_id,
            ],
        )?;
        tx.commit()?;
        Ok(UpsertOutcome {
            created: flags.created,
            changed: true,
            candidate_flagged: flags.candidate_flagged,
        })
    }

    /// The record for one canonical URL.
    pub fn link_record(&self, canonical_url: &str) -> Result<Option<LinkRecord>> {
        get(self.conn(), canonical_url)
    }

    /// Records carrying YouTube video id `id` (index-backed), by canonical URL.
    pub fn link_records_by_yt(&self, id: &str) -> Result<Vec<LinkRecord>> {
        list(
            self.conn(),
            &format!(
                "SELECT {COLS} FROM link_records WHERE yt_video_id = ?1 ORDER BY canonical_url"
            ),
            id,
        )
    }

    /// Records carrying X status id `id` (index-backed), by canonical URL.
    pub fn link_records_by_x(&self, id: &str) -> Result<Vec<LinkRecord>> {
        list(
            self.conn(),
            &format!(
                "SELECT {COLS} FROM link_records WHERE x_status_id = ?1 ORDER BY canonical_url"
            ),
            id,
        )
    }

    /// Every record with a non-empty X status id, by canonical URL (Crucible `listWithXStatus`).
    pub fn link_records_with_x(&self) -> Result<Vec<LinkRecord>> {
        let mut stmt = self.conn().prepare(&format!(
            "SELECT {COLS} FROM link_records WHERE x_status_id IS NOT NULL AND x_status_id != '' ORDER BY canonical_url"
        ))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Number of records.
    pub fn link_record_count(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT count(*) FROM link_records", [], |r| r.get(0))?)
    }
}

/// Reads every `link_records` row of a Crucible `jobs.sqlite`, opened read-only and immutable
/// (`file:<path>?mode=ro&immutable=1`) so the frozen file is never written or locked.
pub fn read_crucible_link_records(path: &Path) -> Result<Vec<LinkRecord>> {
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        path.to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3f")
            .replace('#', "%23")
    );
    let conn = rusqlite::Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM link_records ORDER BY canonical_url"
    ))?;
    let rows = stmt.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(canon: &str) -> LinkRecordInput {
        LinkRecordInput {
            canonical_url: canon.into(),
            url: format!("{canon}?x"),
            domain: Some("ex.com".into()),
            source_notes: vec!["[[a]]".into()],
            first_seen: Some("2026-05-02".into()),
            last_seen: Some("2026-05-02".into()),
            state: Some("pending".into()),
            discovery_method: Some("scan".into()),
            ..LinkRecordInput::default()
        }
    }

    #[test]
    fn merge_rule_table() {
        let (base, f) = merge(None, &input("https://ex.com/p"));
        assert!(f.created && !f.candidate_flagged);
        assert_eq!(base.state, "pending");
        assert_eq!(base.tracked_source, json!(false));

        let mut stored = base.clone();
        stored.state = "kept".into();
        stored.decision_reason = Some("why".into());
        stored.yt_video_id = Some("Y1".into());
        stored.first_seen = Some("2026-01-01".into());
        stored.last_seen = Some("garbage".into());
        stored.source_notes = vec!["[[z]]".into(), "[[a]]".into()];

        let mut inc = input("https://ex.com/p");
        inc.source_notes = vec!["[[b]]".into(), "[[a]]".into(), " ".into()];
        inc.first_seen = Some("2026-03-01".into());
        inc.last_seen = Some("2026-03-01".into());
        inc.state = Some("pending".into());
        inc.decision_reason = Some("other".into());
        inc.yt_video_id = Some("Y2".into());
        inc.x_status_id = Some("123".into());
        inc.url = String::new();
        inc.domain = Some(String::new());
        inc.tracked_source = Some(json!("candidate"));
        inc.tracked_source_type = Some("youtube-channel".into());

        let (m, f) = merge(Some(&stored), &inc);
        assert!(!f.created && f.candidate_flagged);
        assert_eq!(m.source_notes, vec!["[[z]]", "[[a]]", "[[b]]"]);
        assert_eq!(m.first_seen.as_deref(), Some("2026-01-01"));
        assert_eq!(m.last_seen.as_deref(), Some("2026-03-01"));
        assert_eq!(m.state, "kept");
        assert_eq!(m.decision_reason.as_deref(), Some("why"));
        assert_eq!(m.yt_video_id.as_deref(), Some("Y1"));
        assert_eq!(m.x_status_id.as_deref(), Some("123"));
        assert_eq!(m.url, stored.url, "empty incoming url keeps stored");
        assert_eq!(m.domain, "ex.com");
        assert_eq!(m.tracked_source, json!("candidate"));
        assert_eq!(m.tracked_source_type.as_deref(), Some("youtube-channel"));

        // Already flagged: never re-escalates or overwrites.
        let mut inc2 = inc.clone();
        inc2.tracked_source = Some(json!("other"));
        inc2.tracked_source_type = None;
        let (m2, f2) = merge(Some(&m), &inc2);
        assert!(!f2.candidate_flagged);
        assert_eq!(m2.tracked_source, json!("candidate"));
        assert_eq!(m2.tracked_source_type.as_deref(), Some("youtube-channel"));

        // Unflagged incoming never downgrades; null normalizes to false.
        let mut s3 = base.clone();
        s3.tracked_source = Value::Null;
        let mut inc3 = input("https://ex.com/p");
        inc3.tracked_source = Some(json!(false));
        let (m3, _) = merge(Some(&s3), &inc3);
        assert_eq!(m3.tracked_source, json!(false));

        // Non-empty incoming url/domain follow the observation.
        let mut inc4 = input("https://ex.com/p");
        inc4.url = "https://ex.com/p?new".into();
        inc4.domain = Some("www.ex.com".into());
        let (m4, _) = merge(Some(&base), &inc4);
        assert_eq!(m4.url, "https://ex.com/p?new");
        assert_eq!(m4.domain, "www.ex.com");
    }

    #[test]
    fn unchanged_upsert_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = StateDb::open(&dir.path().join("s.db"), 0, Default::default()).unwrap();
        let mut i = input("https://ex.com/p");
        i.tracked_source = Some(json!("candidate"));
        i.yt_video_id = Some("Y".into());
        let o = db.upsert_link_record(&i).unwrap();
        assert!(o.created && o.changed && o.candidate_flagged);
        let before = db.conn().total_changes();
        let o2 = db.upsert_link_record(&i).unwrap();
        assert_eq!(o2, UpsertOutcome::default());
        assert_eq!(db.conn().total_changes(), before);
        i.source_notes.push("[[b]]".into());
        let o3 = db.upsert_link_record(&i).unwrap();
        assert!(o3.changed && !o3.created && !o3.candidate_flagged);
        assert_eq!(db.link_record_count().unwrap(), 1);
        assert_eq!(db.link_records_by_yt("Y").unwrap().len(), 1);
        assert!(db.link_records_by_x("Y").unwrap().is_empty());
        let r = db.link_record("https://ex.com/p").unwrap().unwrap();
        assert_eq!(r.source_notes, vec!["[[a]]", "[[b]]"]);
    }

    #[test]
    fn crucible_import_is_idempotent_and_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let jobs = dir.path().join("jobs.sqlite");
        let c = rusqlite::Connection::open(&jobs).unwrap();
        c.execute_batch(
            "CREATE TABLE link_records (canonical_url TEXT PRIMARY KEY, url TEXT NOT NULL,
             domain TEXT NOT NULL DEFAULT '', source_notes TEXT NOT NULL DEFAULT '[]',
             first_seen TEXT, last_seen TEXT, state TEXT NOT NULL DEFAULT 'pending',
             discovery_method TEXT NOT NULL DEFAULT 'scan', tracked_source TEXT NOT NULL DEFAULT 'false',
             tracked_source_type TEXT, tracked_source_note TEXT, referred_material TEXT,
             decision_reason TEXT, yt_video_id TEXT, x_status_id TEXT);
             INSERT INTO link_records VALUES
             ('https://youtube.com/@c', 'https://www.youtube.com/@c', 'youtube.com', '[\"[[a]]\"]',
              '2026-01-01', '2026-02-01', 'pending', 'scan', '\"candidate\"', 'youtube-channel', NULL, NULL, NULL, NULL, NULL),
             ('https://youtube.com/watch?v=abc', 'https://youtu.be/abc', 'youtube.com', '[\"[[a]]\",\"[[b]]\"]',
              '2026-01-02', '2026-03-01', 'kept', 'scan', 'false', NULL, NULL, 'ref', 'why', 'abc', NULL),
             ('https://x.com/u/status/42', 'https://x.com/u/status/42', 'x.com', '[\"[[c]]\"]',
              '2026-01-03', '2026-01-03', 'pending', 'import', 'false', NULL, NULL, NULL, NULL, NULL, '42');",
        )
        .unwrap();
        drop(c);
        let before = std::fs::read(&jobs).unwrap();
        let mut db = StateDb::open(&dir.path().join("s.db"), 0, Default::default()).unwrap();
        let import = |db: &mut StateDb| {
            read_crucible_link_records(&jobs)
                .unwrap()
                .into_iter()
                .map(|r| db.upsert_link_record(&LinkRecordInput::from(r)).unwrap())
                .collect::<Vec<_>>()
        };
        let first = import(&mut db);
        assert_eq!(first.len(), 3);
        assert!(first.iter().all(|o| o.created && o.changed));
        let changes = db.conn().total_changes();
        let second = import(&mut db);
        assert!(second.iter().all(|o| !o.created && !o.changed));
        assert_eq!(db.conn().total_changes(), changes);
        assert_eq!(std::fs::read(&jobs).unwrap(), before, "source untouched");
        let r = db.link_record("https://youtube.com/@c").unwrap().unwrap();
        assert_eq!(r.tracked_source, json!("candidate"));
        let v = db.link_records_by_yt("abc").unwrap();
        assert_eq!(v[0].source_notes, vec!["[[a]]", "[[b]]"]);
        assert_eq!(
            (v[0].state.as_str(), v[0].decision_reason.as_deref()),
            ("kept", Some("why"))
        );
        assert_eq!(
            db.link_records_by_x("42").unwrap()[0].discovery_method,
            "import"
        );
        assert_eq!(db.link_records_with_x().unwrap().len(), 1);
    }
}
