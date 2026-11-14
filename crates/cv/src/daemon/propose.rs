//! `propose_source` (plan sources-publication-model WP-7): which `[[publication]]` record a note
//! belongs to, or a proposed one, for the editor's "Create Source from note".
//!
//! Read-only: the note is read through the writer (one short job), everything else runs off the
//! executor. Nothing is written to the vault, `state.db` or config, and no job is enqueued. The
//! existing match is the one resolver's answer ([`SourcesConfig::by_note`], then
//! [`SourcesConfig::by_mail`] over the retained message); a `via: email` note's sender and
//! List-Id come only from the message its `message-id` names in the processed Maildir
//! directories ([`crate::steps::message_map`]), never from a tracker row's label. A value that
//! cannot be read is left empty and the reason is stated in `notes`.

use cv_actions::ConfigSet;
use cv_actions::pipelines::{Publication, SourcesConfig};
use cv_ingest::mail::MailPost;
use cv_note::FrontmatterDoc;
use cv_protocol::{ProposeSourceParams, ProposeSourceResult, ProposedSource, SourceMatchers};
use cv_text::url_canon::{bare_host_name, http_host};

use super::*;

impl Shared {
    /// `propose_source`: reads the note in one writer job, then computes the answer off the
    /// executor against the current config snapshot.
    pub(super) async fn propose_source(
        &self,
        p: ProposeSourceParams,
    ) -> Result<ProposeSourceResult, RpcError> {
        cv_vault::validate_rel_path(&p.note)
            .map_err(|e| rpc_err(error_codes::INVALID_PARAMS, e.to_string()))?;
        let config = self.runner_or_err()?.config();
        let note = p.note;
        let bytes = self
            .writer_job(move |vw| {
                Ok(vw
                    .read(&note)?
                    .ok_or_else(|| VaultError::NotFound(note.clone()))?)
            })
            .await?;
        tokio::task::spawn_blocking(move || {
            let mail = config.pipelines().mail.clone();
            let lookup = |id: &str| -> Option<MailPost> {
                let root = mail.maildir.trim();
                if root.is_empty() {
                    return None;
                }
                let map = crate::steps::message_map(Path::new(root), &mail.processed);
                let file = map.get(id)?;
                std::fs::read(file)
                    .ok()
                    .and_then(|b| cv_ingest::mail::parse_eml(&b))
            };
            propose(&bytes, &config, &lookup)
        })
        .await
        .map_err(internal)
    }
}

/// The answer for the note `bytes` (pure but for `message`, the retained-message lookup by
/// Message-ID).
pub(crate) fn propose(
    bytes: &[u8],
    config: &ConfigSet,
    message: &dyn Fn(&str) -> Option<MailPost>,
) -> ProposeSourceResult {
    let mut notes = Vec::new();
    let Ok(text) = std::str::from_utf8(bytes) else {
        notes.push("The note is not UTF-8 text, so nothing could be read from it.".to_owned());
        return proposed(ProposedSource::default(), notes);
    };
    let doc = FrontmatterDoc::new(text);
    if doc.get("source").is_err() {
        notes.push(
            "The note's frontmatter could not be parsed, so nothing could be read from it."
                .to_owned(),
        );
        return proposed(ProposedSource::default(), notes);
    }
    let source = doc.get_str_trimmed("source").unwrap_or_default();
    let host = http_host(&source).and_then(|h| bare_host_name(&h));
    if host.is_none() {
        notes.push("The note has no web `source` URL, so no host or prefix was proposed.".into());
    }
    let authors = doc.get_texts("author");
    let authors: Vec<&str> = authors.iter().map(String::as_str).collect();
    let post = if doc.get_str_trimmed("via").as_deref() == Some("email") {
        match doc.get_str_trimmed("message-id") {
            None => {
                notes.push(
                    "The note has no `message-id`, so sender and list id were left empty.".into(),
                );
                None
            }
            Some(id) => {
                let post = message(&id);
                if post.is_none() {
                    notes.push(
                        "The note's message is no longer in the mail directory, so sender and list id were left empty."
                            .into(),
                    );
                }
                post
            }
        }
    } else {
        None
    };
    let sources = config.sources();
    let by_note = (!source.is_empty() || !authors.is_empty())
        .then(|| sources.by_note(&authors, &source))
        .flatten();
    let found = by_note.or_else(|| {
        post.as_ref()
            .and_then(|m| sources.by_mail(&m.from_addr, m.list_id.as_deref()))
    });
    if let Some(p) = found {
        let missing = missing(p, sources, host.as_deref(), post.as_ref(), &mut notes);
        return ProposeSourceResult::Existing {
            name: p.name.clone(),
            missing,
            notes,
        };
    }
    let name = match doc.get_str_trimmed("publication") {
        Some(p) => p,
        None => {
            match &host {
                Some(h) => {
                    notes.push(format!(
                        "The note has no `publication`, so the name is the source host {h}."
                    ));
                    h.clone()
                }
                None => {
                    notes.push("The note has no `publication` and no source host, so the name was left empty.".into());
                    String::new()
                }
            }
        }
    };
    let mut record = ProposedSource {
        name,
        kind: note_kind(&doc, config),
        matchers: SourceMatchers::default(),
    };
    if record.kind.is_none() {
        notes.push("No tag of the note names a kind in kinds.toml, so kind was left empty.".into());
    }
    if let Some(h) = &host {
        record.matchers.hosts.push(h.clone());
    }
    if let Some(pre) = url_prefix(&source) {
        notes.push(format!(
            "The prefix {pre} is the source URL's parent path; narrow or clear it before saving."
        ));
        record.matchers.prefix.push(pre);
    }
    if let Some(m) = &post {
        if !m.from_addr.trim().is_empty() {
            record.matchers.from.push(m.from_addr.trim().to_lowercase());
        }
        if let Some(l) = m
            .list_id
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            record.matchers.list_id.push(l.to_owned());
        }
    }
    proposed(record, notes)
}

fn proposed(record: ProposedSource, notes: Vec<String>) -> ProposeSourceResult {
    ProposeSourceResult::Proposed { record, notes }
}

/// The note's values that `p` lacks: the source host (unless `p` matches by prefix, where a host
/// would widen it), the message's sender and List-Id. A value another record already holds is
/// not offered (the config refuses it) and is named in `notes`.
fn missing(
    p: &Publication,
    sources: &SourcesConfig,
    host: Option<&str>,
    post: Option<&MailPost>,
    notes: &mut Vec<String>,
) -> SourceMatchers {
    let mut out = SourceMatchers::default();
    let held_by_other = |hit: Option<&Publication>| hit.is_some_and(|o| o.name != p.name);
    if let Some(h) = host {
        let hosts: Vec<String> = crate::supersede::publication_host_list(p)
            .iter()
            .filter_map(|x| bare_host_name(x))
            .collect();
        let has_prefix = p.prefix.iter().any(|x| !x.trim().is_empty());
        if has_prefix {
            // A prefix record matches by prefix alone; a host would widen it.
        } else if !hosts.iter().any(|x| x == h) {
            if sources.publications().iter().any(|o| {
                o.name != p.name
                    && crate::supersede::publication_host_list(o)
                        .iter()
                        .filter_map(|x| bare_host_name(x))
                        .any(|x| x == h)
            }) {
                notes.push(format!(
                    "The host {h} belongs to another record, so it is not offered."
                ));
            } else {
                out.hosts.push(h.to_owned());
            }
        }
    }
    if let Some(m) = post {
        let addr = m.from_addr.trim().to_lowercase();
        if !addr.is_empty() && !sender_listed(p, &addr) {
            if held_by_other(sources.by_mail(&addr, None)) {
                notes.push(format!(
                    "The sender {addr} belongs to another record, so it is not offered."
                ));
            } else {
                out.from.push(addr);
            }
        }
        if let Some(l) = m
            .list_id
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            let want = l.to_lowercase();
            if !p.list_id.iter().any(|x| x.trim().to_lowercase() == want) {
                if held_by_other(sources.by_mail("", Some(l))) {
                    notes.push(format!(
                        "The list id {l} belongs to another record, so it is not offered."
                    ));
                } else {
                    out.list_id.push(l.to_owned());
                }
            }
        }
    }
    out
}

/// Whether `addr` (lowercase) is one of `p`'s `from` entries (an exact address or an `@domain`).
fn sender_listed(p: &Publication, addr: &str) -> bool {
    p.from.iter().any(|f| {
        let f = f.trim().to_lowercase();
        !f.is_empty() && (addr == f || (f.starts_with('@') && addr.ends_with(&f)))
    })
}

/// The first tag of the note that names a `kinds.toml` kind (by name, or among the kind's
/// `tags`).
fn note_kind(doc: &FrontmatterDoc, config: &ConfigSet) -> Option<String> {
    let kinds = config
        .table("kinds.toml")
        .get("kinds")
        .and_then(toml::Value::as_table)?;
    doc.get_strings("tags").iter().find_map(|t| {
        let t = t.trim().trim_start_matches('#');
        kinds
            .iter()
            .find(|(name, def)| {
                name.as_str() == t
                    || def
                        .get("tags")
                        .and_then(toml::Value::as_array)
                        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(t)))
            })
            .map(|(name, _)| name.clone())
    })
}

/// The parent path of an http(s) `source` URL (query and fragment dropped), when it is deeper
/// than the site root: `https://a.example/x/post` → `https://a.example/x/`.
fn url_prefix(source: &str) -> Option<String> {
    http_host(source)?;
    let base = source.split(['?', '#']).next().unwrap_or(source);
    let after_scheme = base.find("://")? + 3;
    let path_start = after_scheme + base[after_scheme..].find('/')?;
    let last = base.rfind('/')?;
    (last > path_start).then(|| base[..=last].to_owned())
}

#[cfg(test)]
#[path = "propose_tests.rs"]
mod tests;
