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
        #[serde(default)]
        label: String,
    },
    List {
        v: Vec<Cell>,
    },
    /// An external URL (opened in the browser, never resolved in the vault).
    Url {
        url: String,
        #[serde(default)]
        label: String,
    },
    /// A cell tag this build does not know (a newer
    /// producer); decodes instead of failing the whole view and renders empty.
    #[serde(other)]
    Unknown,
}

/// A column header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub key: String,
    #[serde(default)]
    pub label: String,
    /// The column's interactive behaviour, set
    /// by the producer; `None` = a plain display column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ColumnRole>,
}

impl Column {
    /// A plain display column.
    #[must_use]
    pub fn plain(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            role: None,
        }
    }

    /// The same column with `role`.
    #[must_use]
    pub fn with_role(mut self, role: ColumnRole) -> Self {
        self.role = Some(role);
        self
    }
}

/// What a column's cells do in the editor.
///
/// JSON: `{"kind": "checkbox", "key": "read"}`, `{"kind": "status"}`, `{"kind": "task_text"}`,
/// `{"kind": "task_file"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColumnRole {
    /// Plan round-8 WP-3: a `checkbox = true` note-property column; a click sets frontmatter
    /// boolean `key` (the bare property name) of the row's note.
    Checkbox { key: String },
    /// A task-status cell: a record row's `tracker_items.status` (click/menu → set-status) or a
    /// Tasks row's status char.
    Status,
    /// A Tasks view's task text.
    TaskText,
    /// A Tasks view's file link.
    TaskFile,
}

/// One row; `path`/`line` locate the source (a task row carries its 1-based line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<i64>,
    #[serde(default)]
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
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub cells: Vec<Cell>,
}

/// A group of rows (ungrouped views have exactly one group without a key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<Cell>,
    pub rows: Vec<Row>,
    #[serde(default)]
    pub summaries: Vec<Summary>,
}

/// The result of one `view_run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewResult {
    pub kind: Kind,
    pub columns: Vec<Column>,
    pub groups: Vec<Group>,
    #[serde(default)]
    pub summaries: Vec<Summary>,
    #[serde(default)]
    pub notices: Vec<String>,
    /// Plan round-8 WP-2: the view's `maxHeight` (`None` = the editor default). A malformed value
    /// decodes as `None` rather than failing the view.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_max_height"
    )]
    pub max_height: Option<MaxHeight>,
}

fn lenient_max_height<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<MaxHeight>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

/// Plan round-8 WP-2: a view's height cap — `Px(n)` (n > 0) or uncapped (`"none"`).
///
/// TOML/JSON form: a positive number, or the string `"none"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MaxHeight {
    Px(f32),
    Uncapped,
}

impl Serialize for MaxHeight {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Px(v) => s.serialize_f32(*v),
            Self::Uncapped => s.serialize_str("none"),
        }
    }
}

impl<'de> Deserialize<'de> for MaxHeight {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = MaxHeight;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("maxHeight: a positive number of pixels or \"none\"")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<MaxHeight, E> {
                if v == "none" {
                    Ok(MaxHeight::Uncapped)
                } else {
                    Err(E::custom(format!(
                        "maxHeight: expected a positive number or \"none\", got {v:?}"
                    )))
                }
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<MaxHeight, E> {
                if v.is_finite() && v > 0.0 {
                    #[allow(clippy::cast_possible_truncation)]
                    Ok(MaxHeight::Px(v as f32))
                } else {
                    Err(E::custom(format!(
                        "maxHeight: expected a positive number or \"none\", got {v}"
                    )))
                }
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<MaxHeight, E> {
                #[allow(clippy::cast_precision_loss)]
                self.visit_f64(v as f64)
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<MaxHeight, E> {
                #[allow(clippy::cast_precision_loss)]
                self.visit_f64(v as f64)
            }
        }
        d.deserialize_any(V)
    }
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
            max_height: None,
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
    fn max_height_round_trips_and_defaults() {
        let mut r = ViewResult::empty(Kind::Table, Vec::new());
        let j = serde_json::to_value(&r).unwrap();
        assert!(j.get("max_height").is_none(), "absent field = default");
        assert_eq!(
            serde_json::from_value::<ViewResult>(j).unwrap().max_height,
            None
        );
        for m in [MaxHeight::Px(600.5), MaxHeight::Uncapped] {
            r.max_height = Some(m);
            let back: ViewResult =
                serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
            assert_eq!(back.max_height, Some(m));
        }
    }

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
