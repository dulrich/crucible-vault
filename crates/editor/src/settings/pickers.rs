//! Client-side typeahead pickers (plan round-5-reading-pickers-intake WP-4): data adapters that
//! turn the settings model and the vault path list into [`Candidate`]s for
//! [`crate::widget::typeahead`]. Stored values never change shape — a picker shows names but
//! writes the provider id, model id, channel id or vault path exactly as typed today.
//!
//! The network-backed pickers (currencies, geocode, live models, channel titles) are WP-6; the
//! model picker's declared list is built so WP-6 can append live models after it.

use cv_schema::{ConfigDoc, Node, Path, Seg};

use super::Settings;
use crate::widget::typeahead::{self, Candidate, LIMIT};

/// Which candidate source a field uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    /// Vault folders (`Node::Folder`).
    Folder,
    /// Vault files (`Node::Path`).
    Path,
    /// providers.toml providers (stores the id).
    Provider,
    /// providers.toml declared models (stores the model id).
    Model,
    /// sources.toml `[[youtube]]` channels (stores the id; free text allowed).
    Channel,
}

/// The picker of a field of `file` (vault path) at `p` with schema `node`.
pub fn for_field(file: &str, p: &Path, node: &Node) -> Option<Picker> {
    match node {
        Node::Folder => return Some(Picker::Folder),
        Node::Path => return Some(Picker::Path),
        _ => {}
    }
    let name = file.rsplit('/').next().unwrap_or(file);
    if name != "pipelines.toml" {
        return None;
    }
    let keys: Vec<&str> =
        p.0.iter()
            .map(|s| match s {
                Seg::Key(k) => k.as_str(),
                Seg::Index(_) => "",
            })
            .collect();
    match (keys.as_slice(), node) {
        (["image_describe", "provider"], Node::Str) => Some(Picker::Provider),
        (["image_describe", "model"], Node::Str) => Some(Picker::Model),
        (["ignore_shorts", "channels"], Node::StrList { .. }) => Some(Picker::Channel),
        _ => None,
    }
}

fn doc_named<'a>(s: &'a Settings, name: &str) -> Option<&'a ConfigDoc> {
    s.files
        .values()
        .find(|f| f.path.rsplit('/').next() == Some(name))
        .and_then(|f| f.doc.as_ref())
}

fn list_root(name: &str) -> Path {
    Path::root().key(name)
}

/// providers.toml providers: label = `name` (fallback `id`), secondary = kind, value = id.
pub fn providers(s: &Settings) -> Vec<Candidate> {
    let Some(doc) = doc_named(s, "providers.toml") else {
        return Vec::new();
    };
    let root = list_root("provider");
    (0..doc.len(&root))
        .filter_map(|i| {
            let ip = root.index(i);
            let id = doc.get_str(&ip.key("id")).filter(|v| !v.is_empty())?;
            let name = doc.get_str(&ip.key("name")).filter(|v| !v.is_empty());
            Some(Candidate {
                label: name.unwrap_or(id).to_owned(),
                secondary: doc.get_str(&ip.key("kind")).unwrap_or_default().to_owned(),
                value: id.to_owned(),
            })
        })
        .collect()
}

/// providers.toml declared models (of `provider` when given and declared, else every
/// provider), labelled "declared". WP-6 appends the live list after these.
pub fn declared_models(s: &Settings, provider: Option<&str>) -> Vec<Candidate> {
    let Some(doc) = doc_named(s, "providers.toml") else {
        return Vec::new();
    };
    let root = list_root("provider");
    let n = doc.len(&root);
    let known =
        provider.filter(|p| (0..n).any(|i| doc.get_str(&root.index(i).key("id")) == Some(*p)));
    let mut out = Vec::new();
    for i in 0..n {
        let ip = root.index(i);
        let id = doc.get_str(&ip.key("id")).unwrap_or_default();
        if known.is_some_and(|k| k != id) {
            continue;
        }
        let pname = doc
            .get_str(&ip.key("name"))
            .filter(|v| !v.is_empty())
            .unwrap_or(id);
        let mp = ip.key("models");
        for j in 0..doc.len(&mp) {
            if let Some(m) = doc
                .get_str(&mp.index(j).key("id"))
                .filter(|v| !v.is_empty())
            {
                out.push(Candidate {
                    label: m.to_owned(),
                    secondary: if known.is_some() {
                        "declared".to_owned()
                    } else {
                        format!("declared · {pname}")
                    },
                    value: m.to_owned(),
                });
            }
        }
    }
    out
}

/// sources.toml `[[youtube]]` channels: label `channel (UC…)`, value = id.
pub fn channels(s: &Settings) -> Vec<Candidate> {
    let Some(doc) = doc_named(s, "sources.toml") else {
        return Vec::new();
    };
    let root = list_root("youtube");
    (0..doc.len(&root))
        .filter_map(|i| {
            let ip = root.index(i);
            let id = doc.get_str(&ip.key("id")).filter(|v| !v.is_empty())?;
            let label = match doc.get_str(&ip.key("channel")).filter(|v| !v.is_empty()) {
                Some(n) => format!("{n} ({id})"),
                None => id.to_owned(),
            };
            Some(Candidate {
                label,
                secondary: String::new(),
                value: id.to_owned(),
            })
        })
        .collect()
}

/// The display label of a stored channel chip (`name (UC…)` when declared, else as stored).
pub fn channel_label(chans: &[Candidate], stored: &str) -> String {
    chans
        .iter()
        .find(|c| c.value == stored)
        .map_or_else(|| stored.to_owned(), |c| c.label.clone())
}

/// The model picker's provider: `image_describe.provider` of the same file.
fn sibling_provider<'a>(s: &'a Settings, file: &str, at: &Path) -> Option<&'a str> {
    let doc = s.file(file)?.doc.as_ref()?;
    let mut p = Path(at.0[..at.0.len().saturating_sub(1)].to_vec());
    p = p.key("provider");
    doc.get_str(&p).filter(|v| !v.is_empty())
}

/// Every candidate of a non-path picker (`Folder`/`Path` rank the raw path list instead, see
/// [`visible`]).
fn small_set(s: &Settings, picker: Picker, file: &str, at: &Path) -> Vec<Candidate> {
    match picker {
        Picker::Provider => providers(s),
        Picker::Model => declared_models(s, sibling_provider(s, file, at)),
        Picker::Channel => channels(s),
        Picker::Folder | Picker::Path => Vec::new(),
    }
}

/// The folder strings (`switcher::folder_candidates`, vault root dropped).
fn folders(paths: &[String]) -> Vec<String> {
    crate::switcher::folder_candidates(paths, "")
}

/// The ranked, visible candidates for `query` (at most [`LIMIT`]).
pub fn visible(
    s: &Settings,
    picker: Picker,
    file: &str,
    at: &Path,
    paths: &[String],
    query: &str,
) -> Vec<Candidate> {
    let raw = |list: &[String]| -> Vec<Candidate> {
        editor_model::rank::rank(query, list, &[], |_| false, LIMIT)
            .into_iter()
            .map(|r| Candidate::plain(r.path))
            .collect()
    };
    match picker {
        Picker::Folder => raw(&folders(paths)),
        Picker::Path => raw(paths),
        _ => typeahead::filter(query, &small_set(s, picker, file, at), LIMIT),
    }
}

/// Whether `text` is a valid committed value of the picker (a candidate value).
pub fn is_valid(
    s: &Settings,
    picker: Picker,
    file: &str,
    at: &Path,
    paths: &[String],
    text: &str,
) -> bool {
    match picker {
        Picker::Folder => folders(paths).iter().any(|f| f == text),
        Picker::Path => paths.iter().any(|p| p == text),
        _ => typeahead::is_value(text, &small_set(s, picker, file, at)),
    }
}

/// The display label of a stored value (Provider: its name; others: as stored).
pub fn label_of(s: &Settings, picker: Picker, stored: &str) -> Option<String> {
    match picker {
        Picker::Provider => providers(s)
            .into_iter()
            .find(|c| c.value == stored)
            .map(|c| c.label),
        Picker::Channel => Some(channel_label(&channels(s), stored)),
        _ => None,
    }
}
