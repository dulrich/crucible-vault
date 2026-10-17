//! The typed `ViewResult` (plan `v0-editor.md` Public Interfaces), serialized into the untyped
//! `cv_protocol::ViewResult` wrapper for `view_run`.
//!
//! JSON shape:
//! `{"kind": "table"|"pivot"|"tasks", "columns": [{"key", "label"}], "groups": [{"key"?: Cell,
//! "rows": [{"path"?, "line"?, "cells": [Cell]}], "summaries": [Summary]}], "summaries":
//! [Summary], "notices": [String]}`.
//!
//! A [`Summary`] is one footer row aligned with `columns`: `{"label": "Sum", "cells": [Cell]}`
//! (a `null` cell where that column has no summary).
//!
//! [`Cell`] is tagged by `t`:
//! `{"t":"null"}`, `{"t":"bool","v":true}`, `{"t":"number","v":1.5}`, `{"t":"text","v":"…"}`,
//! `{"t":"date","v":"2026-10-12"}` (date only) or `{"t":"date","v":"2026-10-12T08:30:00"}`
//! (local, no zone), `{"t":"link","path":"vault/relative.md","label":"…"}`,
//! `{"t":"list","v":[Cell…]}`, `{"t":"url","url":"https://…","label":"…"}` (an external link,
//! round-5 WP-11; `link` stays a vault note).
//!
//! A record-sourced row (`tracker_items`) carries an opaque `"key"` (a [`TrackerKey`]); index rows
//! never do, so their JSON is unchanged.

use serde::{Deserialize, Serialize};

/// The view kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Table,
    Pivot,
    Tasks,
}

/// One displayed value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Cell {
    Null,
    Bool {
        v: bool,
    },
    Number {
        v: f64,
    },
    Text {
        v: String,
    },
    Date {
        v: String,
    },
    Link {
        path: String,
        label: String,
    },
    List {
        v: Vec<Cell>,
    },
    /// An external URL (opened in the browser, never resolved in the vault).
    Url {
        url: String,
        label: String,
    },
}

/// A column header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub key: String,
    pub label: String,
}

/// One row; `path`/`line` locate the source (a task row carries its 1-based line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<i64>,
    pub cells: Vec<Cell>,
    /// Opaque row identity for record-sourced rows ([`TrackerKey::encode`]); `None` for index rows.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<String>,
}

/// The identity of one `tracker_items` row, carried in [`Row::key`] as a JSON array
/// `["<source_kind>","<source_key>","<item_id>"]` (unambiguous for any string content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackerKey {
    pub source_kind: String,
    pub source_key: String,
    pub item_id: String,
}

impl TrackerKey {
    /// The opaque key string.
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(&[&self.source_kind, &self.source_key, &self.item_id])
            .unwrap_or_default()
    }

    /// Parses a key made by [`TrackerKey::encode`]; `None` for anything else.
    #[must_use]
    pub fn decode(key: &str) -> Option<Self> {
        let [source_kind, source_key, item_id]: [String; 3] = serde_json::from_str(key).ok()?;
        Some(Self {
            source_kind,
            source_key,
            item_id,
        })
    }
}

/// A footer row aligned with the columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub label: String,
    pub cells: Vec<Cell>,
}

/// A group of rows (ungrouped views have exactly one group without a key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<Cell>,
    pub rows: Vec<Row>,
    pub summaries: Vec<Summary>,
}

/// The result of one `view_run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewResult {
    pub kind: Kind,
    pub columns: Vec<Column>,
    pub groups: Vec<Group>,
    pub summaries: Vec<Summary>,
    pub notices: Vec<String>,
}

impl ViewResult {
    /// An empty result of `kind` carrying only `notices`.
    pub fn empty(kind: Kind, notices: Vec<String>) -> Self {
        Self {
            kind,
            columns: Vec::new(),
            groups: Vec::new(),
            summaries: Vec::new(),
            notices,
        }
    }

    /// Converts into the protocol's untyped wrapper.
    pub fn into_protocol(self) -> Result<cv_protocol::ViewResult, serde_json::Error> {
        serde_json::to_value(self).map(cv_protocol::ViewResult)
    }

    /// Every row across groups, in order.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.groups.iter().flat_map(|g| g.rows.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_encoding_is_tagged() {
        let j = serde_json::to_value(vec![
            Cell::Null,
            Cell::Bool { v: true },
            Cell::Number { v: 1.5 },
            Cell::Text { v: "x".into() },
            Cell::Date {
                v: "2026-10-12".into(),
            },
            Cell::Link {
                path: "a.md".into(),
                label: "a".into(),
            },
            Cell::List {
                v: vec![Cell::Null],
            },
        ])
        .unwrap();
        assert_eq!(
            j,
            serde_json::json!([
                {"t":"null"},{"t":"bool","v":true},{"t":"number","v":1.5},{"t":"text","v":"x"},
                {"t":"date","v":"2026-10-12"},{"t":"link","path":"a.md","label":"a"},
                {"t":"list","v":[{"t":"null"}]}
            ])
        );
        let r = ViewResult::empty(Kind::Pivot, vec!["n".into()]);
        let p = r.clone().into_protocol().unwrap();
        assert_eq!(p.0["kind"], "pivot");
        let back: ViewResult = serde_json::from_value(p.0).unwrap();
        assert_eq!(back, r);
    }
}
