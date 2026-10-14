//! `cv shadow report`: aggregates `state.db` `shadow_writes` — totals by action, op and path
//! class, first/last time, and the P6 count of rows whose path would resolve outside the
//! overlay. Aggregates only: a path class is the first path segment when it is in
//! [`CLASS_ALLOWLIST`] (else `other`) plus a short alphanumeric extension.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use cv_state::{ShadowWrite, StateReader};
use serde_json::{Value, json};

/// First path segments reported by name; everything else is `other`.
pub const CLASS_ALLOWLIST: [&str; 4] = ["_custom", "daily", "core", "attachments"];
/// `shadow_writes` page size.
pub const PAGE: u32 = 1000;

/// Every `shadow_writes` row after `since_ms` (paged).
pub fn all_writes(reader: &StateReader, since_ms: Option<i64>) -> Result<Vec<ShadowWrite>> {
    let mut out = Vec::new();
    let mut seq = 0;
    loop {
        let page = reader.shadow_writes(seq, PAGE)?;
        let Some(last) = page.last() else { break };
        seq = last.seq;
        let n = page.len();
        out.extend(
            page.into_iter()
                .filter(|w| since_ms.is_none_or(|s| w.at_ms >= s)),
        );
        if n < PAGE as usize {
            break;
        }
    }
    Ok(out)
}

/// `<first segment or other>:<extension or none>`.
pub fn path_class(path: &str) -> String {
    let mut segs = path.split('/').filter(|s| !s.is_empty());
    let first = segs.next().unwrap_or("");
    let has_more = path.trim_matches('/').contains('/');
    let seg = if has_more && CLASS_ALLOWLIST.contains(&first) {
        first
    } else {
        "other"
    };
    let name = path.rsplit('/').next().unwrap_or("");
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 5 && e.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "none".to_owned());
    format!("{seg}:{ext}")
}

/// Whether a logged path would land outside the overlay (absolute, `..`, empty, …).
pub fn outside_overlay(path: &str) -> bool {
    cv_vault::validate_rel_path(path).is_err()
}

/// `job_id` → action id (`action:<id>` job type), `direct` without a job, `unknown-job` when the
/// job row is gone (pruned).
pub struct ActionResolver<'r> {
    reader: &'r StateReader,
    cache: HashMap<String, String>,
}

impl<'r> ActionResolver<'r> {
    /// A resolver over `reader`.
    pub fn new(reader: &'r StateReader) -> Self {
        Self {
            reader,
            cache: HashMap::new(),
        }
    }

    /// The action id of `job_id`.
    pub fn action_of(&mut self, job_id: Option<&str>) -> Result<String> {
        let Some(id) = job_id else {
            return Ok("direct".to_owned());
        };
        if let Some(a) = self.cache.get(id) {
            return Ok(a.clone());
        }
        let a = match self.reader.job(id)? {
            Some(j) => j
                .job_type
                .strip_prefix("action:")
                .unwrap_or(&j.job_type)
                .to_owned(),
            None => "unknown-job".to_owned(),
        };
        self.cache.insert(id.to_owned(), a.clone());
        Ok(a)
    }
}

/// The aggregate report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Rows counted.
    pub total: usize,
    /// By action id.
    pub by_action: BTreeMap<String, usize>,
    /// By op.
    pub by_op: BTreeMap<String, usize>,
    /// By path class.
    pub by_class: BTreeMap<String, usize>,
    /// First `at_ms`.
    pub first_at_ms: Option<i64>,
    /// Last `at_ms`.
    pub last_at_ms: Option<i64>,
    /// Rows whose path resolves outside the overlay (P6).
    pub outside_overlay: usize,
}

impl Report {
    /// JSON form (aggregates only).
    pub fn to_json(&self) -> Value {
        json!({
            "total": self.total,
            "by_action": self.by_action,
            "by_op": self.by_op,
            "by_path_class": self.by_class,
            "first_at_ms": self.first_at_ms,
            "last_at_ms": self.last_at_ms,
            "outside_overlay": self.outside_overlay,
        })
    }
}

/// Builds the report from `reader` (read-only).
pub fn build(reader: &StateReader, since_ms: Option<i64>) -> Result<Report> {
    let mut r = Report::default();
    let mut actions = ActionResolver::new(reader);
    for w in all_writes(reader, since_ms)? {
        r.total += 1;
        *r.by_action
            .entry(actions.action_of(w.job_id.as_deref())?)
            .or_default() += 1;
        *r.by_op.entry(w.op.clone()).or_default() += 1;
        *r.by_class.entry(path_class(&w.path)).or_default() += 1;
        r.first_at_ms = Some(r.first_at_ms.map_or(w.at_ms, |f| f.min(w.at_ms)));
        r.last_at_ms = Some(r.last_at_ms.map_or(w.at_ms, |l| l.max(w.at_ms)));
        if outside_overlay(&w.path) {
            r.outside_overlay += 1;
        }
    }
    Ok(r)
}
