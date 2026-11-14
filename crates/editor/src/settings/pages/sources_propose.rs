//! "Create Source from note" (plan sources-publication-model WP-7): the editor half. The daemon's
//! `propose_source` answer is mapped here (pure, no `iced`): a proposal becomes an unsaved new
//! `[[publication]]` record, an existing match expands that record and raises an offer of the
//! note's missing matcher values, which the user applies explicitly (one [`DocEdit::SetMany`]).
//! Nothing is written until the normal settings save.

use cv_protocol::{ProposeSourceResult, ProposedSource, SourceMatchers};
use cv_schema::Path;

use super::sources::SOURCES_PATH;
use crate::daemon::DaemonError;
use crate::settings::{ConfigFile, DocEdit, ScalarValue, Settings};

/// What the page shows after the command: the plain-language notes and, for an existing match,
/// the offer of the values the record lacks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FromNote {
    /// The daemon's notes.
    pub notes: Vec<String>,
    /// `(record name, values to add)`; `None` when nothing is missing.
    pub offer: Option<(String, SourceMatchers)>,
}

/// The user-facing text of a failed `propose_source` call. An older daemon that does not know
/// the request answers method-not-found.
#[must_use]
pub fn propose_error(e: &DaemonError) -> String {
    match e {
        DaemonError::Rpc(r) if r.code == cv_protocol::error_codes::METHOD_NOT_FOUND => {
            "Create Source from note: the running daemon does not support this — restart it after installing".to_owned()
        }
        e => format!("Create Source from note: {e}"),
    }
}

/// The matcher lists as `(key, value)` pairs, empty lists skipped.
fn matcher_lists(m: &SourceMatchers) -> Vec<(&'static str, &Vec<String>)> {
    [
        ("from", &m.from),
        ("list_id", &m.list_id),
        ("hosts", &m.hosts),
        ("prefix", &m.prefix),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect()
}

/// A proposal as the new record's initial fields (`name` first; empty values skipped).
#[must_use]
pub fn proposal_fields(r: &ProposedSource) -> Vec<(String, ScalarValue)> {
    let mut out = vec![("name".to_owned(), ScalarValue::Str(r.name.clone()))];
    if let Some(k) = r.kind.as_deref().filter(|k| !k.trim().is_empty()) {
        out.push(("kind".to_owned(), ScalarValue::Str(k.to_owned())));
    }
    for (k, v) in matcher_lists(&r.matchers) {
        out.push((k.to_owned(), ScalarValue::StrList(v.clone())));
    }
    out
}

/// One line naming the offered values (`from: a@b; hosts: c`).
#[must_use]
pub fn offer_summary(m: &SourceMatchers) -> String {
    matcher_lists(m)
        .into_iter()
        .map(|(k, v)| format!("{k}: {}", v.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

fn record_index(s: &Settings, name: &str) -> Option<usize> {
    let doc = s.file(SOURCES_PATH).and_then(ConfigFile::doc)?;
    let list = Path::root().key("publication");
    (0..doc.len(&list)).find(|&i| doc.get_str(&list.index(i).key("name")) == Some(name))
}

impl Settings {
    /// Applies a `propose_source` answer: a proposal is appended as an unsaved record, an
    /// existing match is expanded (never appended: a name the file does not hold is an error).
    /// Returns the record's index. The caller dispatches `SettingsMsg::Section(Sources)`.
    pub fn apply_proposal(&mut self, r: ProposeSourceResult) -> Result<usize, String> {
        match r {
            ProposeSourceResult::Proposed { record, notes } => {
                let i = self.open_publication(&record.name, proposal_fields(&record))?;
                self.sources.from_note = Some(FromNote { notes, offer: None });
                Ok(i)
            }
            ProposeSourceResult::Existing {
                name,
                missing,
                notes,
            } => {
                if self.file(SOURCES_PATH).and_then(ConfigFile::doc).is_none() {
                    return Err("sources.toml is not loaded".to_owned());
                }
                if record_index(self, &name).is_none() {
                    return Err(format!(
                        "the note matches {name:?}, which is not a [[publication]] record in sources.toml"
                    ));
                }
                let i = self.open_publication(&name, Vec::new())?;
                let offer = (!missing.is_empty()).then_some((name, missing));
                self.sources.from_note = Some(FromNote { notes, offer });
                Ok(i)
            }
        }
    }

    /// The offer's edit: each offered list merged into the record's current list (existing
    /// entries first, duplicates dropped), as one edit.
    #[must_use]
    pub fn offer_edit(&self) -> Option<DocEdit> {
        let (name, m) = self.sources.from_note.as_ref()?.offer.as_ref()?;
        let i = record_index(self, name)?;
        let doc = self.file(SOURCES_PATH).and_then(ConfigFile::doc)?;
        let rec = Path::root().key("publication").index(i);
        let sets = matcher_lists(m)
            .into_iter()
            .map(|(k, add)| {
                let mut v = doc.get_str_list(&rec.key(k)).unwrap_or_default();
                for a in add {
                    if !v.contains(a) {
                        v.push(a.clone());
                    }
                }
                (rec.key(k), ScalarValue::StrList(v))
            })
            .collect();
        Some(DocEdit::SetMany(sets))
    }

    /// Applies the offer (the user's explicit accept) and clears it.
    pub fn apply_offer(&mut self) {
        if let Some(e) = self.offer_edit() {
            let _ = self.edit(SOURCES_PATH, &e);
        }
        if let Some(f) = &mut self.sources.from_note {
            f.offer = None;
        }
    }
}
