//! Client-side typeahead pickers (plan round-5-reading-pickers-intake WP-4): data adapters that
//! turn the settings model and the vault path list into [`Candidate`]s for
//! [`crate::widget::typeahead`]. Stored values never change shape — a picker shows names but
//! writes the provider id, model id, channel id or vault path exactly as typed today.
//!
//! WP-6 adds the network-backed pickers (currencies, geocode, live models, channel titles) over
//! the [`super::lookups`] cache.

use cv_schema::{ConfigDoc, Node, Path, RefKind, Seg};

use super::Settings;
use super::lookups::Fetch;
use crate::widget::typeahead::{self, Candidate, LIMIT, Status};

/// Which candidate source a field uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    /// Vault folders (`Node::Folder`).
    Folder,
    /// Vault files (`Node::Path`).
    Path,
    /// providers.toml providers (stores the id).
    Provider,
    /// providers.toml declared models + the live list (stores the model id).
    Model,
    /// sources.toml `[[youtube]]` channels (stores the id; free text allowed).
    Channel,
    /// WP-6: ISO 4217 currencies (`CODE — Name`, stores the code).
    Currency,
    /// WP-6: a geocoded place (`City, CC`); a pick writes `label` + sibling `lat`/`lon`.
    Place,
    /// WP-6: a `provider:model` reference (the list edits the part after the first `:`).
    ModelRef,
}

/// A committed typeahead value: a stored string, or a geocoded place whose pick
/// writes `label` + sibling `lat`/`lon` in one edit.
#[derive(Debug, Clone, PartialEq)]
pub enum PickValue {
    Str(String),
    Place { label: String, lat: f64, lon: f64 },
}

/// The picker of a field: its schema reference hint (B13), else its node's vault-path kind.
pub fn for_field(node: &Node, hint: Option<RefKind>) -> Option<Picker> {
    match hint {
        Some(RefKind::Provider) => Some(Picker::Provider),
        Some(RefKind::Model) => Some(Picker::Model),
        Some(RefKind::ModelRef) => Some(Picker::ModelRef),
        Some(RefKind::Channel) => Some(Picker::Channel),
        Some(RefKind::Currency) => Some(Picker::Currency),
        Some(RefKind::Place) => Some(Picker::Place),
        None => match node {
            Node::Folder => Some(Picker::Folder),
            Node::Path => Some(Picker::Path),
            _ => None,
        },
    }
}

pub(super) fn doc_named<'a>(s: &'a Settings, name: &str) -> Option<&'a ConfigDoc> {
    s.files
        .values()
        .find(|f| f.path().rsplit('/').next() == Some(name))
        .and_then(|f| f.doc())
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
/// provider), labelled "declared". [`merged_models`] puts the live list ahead of these.
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
            let mj = mp.index(j);
            if let Some(m) = doc.get_str(&mj.key("id")).filter(|v| !v.is_empty()) {
                let tag = if known.is_some() {
                    "declared".to_owned()
                } else {
                    format!("declared · {pname}")
                };
                let label = doc.get_str(&mj.key("label")).filter(|v| !v.is_empty());
                out.push(model_candidate(m, label, &tag));
            }
        }
    }
    out
}

/// A model candidate (plan round-6-feedback-todos WP-8): shown by its label (providers.toml
/// `label` / the live display name) with `id · tag` beneath, or by its id with `tag` beneath
/// when it has no label. The stored value is always the id.
fn model_candidate(id: &str, label: Option<&str>, tag: &str) -> Candidate {
    match label.filter(|l| !l.is_empty() && *l != id) {
        Some(l) => Candidate {
            label: l.to_owned(),
            secondary: format!("{id} · {tag}"),
            value: id.to_owned(),
        },
        None => Candidate {
            label: id.to_owned(),
            secondary: tag.to_owned(),
            value: id.to_owned(),
        },
    }
}

/// Live models first (shown by display name, see [`model_candidate`]), then the declared models not
/// already listed (secondary "declared"), deduped by id.
pub fn merged_models(s: &Settings, provider: Option<&str>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = match provider.and_then(|p| s.lookups.models.get(p)) {
        Some(Fetch::Ok(ms)) => ms
            .iter()
            .map(|m| model_candidate(&m.id, m.display_name.as_deref(), "live"))
            .collect(),
        _ => Vec::new(),
    };
    for d in declared_models(s, provider) {
        if !out.iter().any(|c| c.value == d.value) {
            out.push(d);
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

/// [`channel_label`], falling back to a fetched title (`title (UC…)`, WP-6) for ids sources.toml
/// does not name; a missing title shows the id.
pub fn chip_label(s: &Settings, chans: &[Candidate], stored: &str) -> String {
    if chans
        .iter()
        .any(|c| c.value == stored && c.label != c.value)
    {
        return channel_label(chans, stored);
    }
    match s.lookups.channel_title(stored) {
        Some(t) => format!("{t} ({stored})"),
        None => stored.to_owned(),
    }
}

/// Channel candidates: sources.toml channels (a fetched title for unnamed ids), then other
/// ids with a fetched title.
fn all_channels(s: &Settings) -> Vec<Candidate> {
    let mut out = channels(s);
    for c in &mut out {
        if c.label == c.value
            && let Some(t) = s.lookups.channel_title(&c.value)
        {
            c.label = format!("{t} ({})", c.value);
        }
    }
    for (id, t) in s.lookups.channels.oks() {
        if let Some(t) = t
            && !out.iter().any(|c| c.value == *id)
        {
            out.push(Candidate {
                label: format!("{t} ({id})"),
                secondary: String::new(),
                value: id.clone(),
            });
        }
    }
    out
}

/// The `[[youtube]]` row's fetched title, the placeholder of an empty `channel` field (never
/// written to the doc).
pub fn source_channel_placeholder(s: &Settings, file: &str, at: &Path) -> Option<String> {
    if file.rsplit('/').next() != Some("sources.toml") {
        return None;
    }
    let n = at.0.len();
    if n < 2 || at.0[n - 1] != Seg::Key("channel".to_owned()) {
        return None;
    }
    if !matches!(at.0[n - 2], Seg::Index(_)) {
        return None;
    }
    let doc = s.file(file)?.doc()?;
    let id = doc.get_str(&Path(at.0[..n - 1].to_vec()).key("id"))?;
    s.lookups.channel_title(id).map(str::to_owned)
}

/// The model picker's provider: `image_describe.provider` of the same file.
fn sibling_provider<'a>(s: &'a Settings, file: &str, at: &Path) -> Option<&'a str> {
    let doc = s.file(file)?.doc()?;
    let mut p = Path(at.0[..at.0.len().saturating_sub(1)].to_vec());
    p = p.key("provider");
    doc.get_str(&p).filter(|v| !v.is_empty())
}

/// Splits a `provider:model` reference at the first `:` (`crates/cv-providers/src/refs.rs:13`).
pub fn split_ref(v: &str) -> Option<(&str, &str)> {
    v.split_once(':')
}

/// The provider whose models the picker lists: image_describe's sibling `provider`, or the
/// prefix of a `provider:model` reference (typed, else stored; `{param.…}` has none).
pub fn model_provider(
    s: &Settings,
    picker: Picker,
    file: &str,
    at: &Path,
    text: &str,
) -> Option<String> {
    match picker {
        Picker::Model => sibling_provider(s, file, at).map(str::to_owned),
        Picker::ModelRef => {
            let stored = s
                .file(file)
                .and_then(|f| f.doc())
                .and_then(|d| d.get_str(at))
                .unwrap_or_default();
            [text, stored]
                .into_iter()
                .filter(|v| !v.starts_with('{'))
                .find_map(|v| split_ref(v).map(|(p, _)| p.to_owned()))
                .filter(|p| !p.is_empty())
        }
        _ => None,
    }
}

/// The status line of the active picker's async source.
pub fn status(s: &Settings, picker: Picker, file: &str, at: &Path, text: &str) -> Status {
    match picker {
        Picker::Model | Picker::ModelRef => {
            match model_provider(s, picker, file, at, text).and_then(|p| s.lookups.models.get(&p)) {
                Some(Fetch::Pending) => Status::Loading,
                Some(Fetch::Failed) => Status::Failed(super::lookups::MODELS_FAILED.to_owned()),
                _ => Status::Ready,
            }
        }
        Picker::Currency => match s.lookups.currencies.get(&()) {
            None | Some(Fetch::Pending) => Status::Loading,
            Some(Fetch::Failed) => Status::Failed("currency list unavailable".to_owned()),
            Some(Fetch::Ok(_)) => Status::Ready,
        },
        Picker::Place => {
            let q = super::lookups::geo_key(text);
            match s.lookups.geo.get(&q) {
                Some(Fetch::Failed) => Status::Failed("place search unavailable".to_owned()),
                Some(Fetch::Ok(v)) if v.is_empty() => Status::Failed("no places found".to_owned()),
                None | Some(Fetch::Pending) if q.chars().count() >= 2 => Status::Loading,
                _ => Status::Ready,
            }
        }
        _ => Status::Ready,
    }
}

/// Currency candidates (`CODE — Name`, value = code); empty until fetched.
fn currencies(s: &Settings) -> Vec<Candidate> {
    match s.lookups.currency_list() {
        Some(cs) => cs
            .iter()
            .map(|c| Candidate {
                label: format!("{} — {}", c.code, c.name),
                secondary: String::new(),
                value: c.code.clone(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The candidate key of the `i`th place hit for a query: its label, made unique among the
/// hits (`label #n` for a repeated label) — the key a pick resolves through [`place_pick`].
fn place_keys(hits: &[cv_protocol::LookupGeoResult]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(hits.len());
    for (i, g) in hits.iter().enumerate() {
        let k = if out.contains(&g.label) {
            format!("{} #{}", g.label, i + 1)
        } else {
            g.label.clone()
        };
        out.push(k);
    }
    out
}

/// Place candidates for `query` (`City, CC`, admin1 secondary, value = the hit's key).
fn places(s: &Settings, query: &str) -> Vec<Candidate> {
    let hits = &s.lookups.places(query)[..s.lookups.places(query).len().min(LIMIT)];
    hits.iter()
        .zip(place_keys(hits))
        .map(|(g, value)| Candidate {
            label: g.label.clone(),
            secondary: g.admin1.clone().unwrap_or_default(),
            value,
        })
        .collect()
}

/// The [`PickValue`] of a place candidate `value` picked for `query` (`None`: not one of its
/// hits).
pub fn place_pick(s: &Settings, query: &str, value: &str) -> Option<PickValue> {
    let hits = &s.lookups.places(query)[..s.lookups.places(query).len().min(LIMIT)];
    let i = place_keys(hits).iter().position(|k| k == value)?;
    let g = &hits[i];
    Some(PickValue::Place {
        label: g.label.clone(),
        lat: g.lat,
        lon: g.lon,
    })
}

/// `provider:model` candidates of the reference's provider (every declared provider's models
/// when the text has no prefix yet); labels show the model part only.
fn model_refs(s: &Settings, file: &str, at: &Path, text: &str) -> Vec<Candidate> {
    if text.starts_with('{') {
        return Vec::new();
    }
    match model_provider(s, Picker::ModelRef, file, at, text) {
        Some(p) => merged_models(s, Some(&p))
            .into_iter()
            .map(|c| Candidate {
                value: format!("{p}:{}", c.value),
                ..c
            })
            .collect(),
        None => providers(s)
            .into_iter()
            .flat_map(|pc| {
                let p = pc.value;
                declared_models(s, Some(&p))
                    .into_iter()
                    .map(|c| Candidate {
                        secondary: format!("declared · {p}"),
                        value: format!("{p}:{}", c.value),
                        label: c.label,
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    }
}

/// Every candidate of a non-path picker (`Folder`/`Path` rank the raw path list instead, see
/// [`visible`]).
fn small_set(s: &Settings, picker: Picker, file: &str, at: &Path, text: &str) -> Vec<Candidate> {
    match picker {
        Picker::Provider => providers(s),
        Picker::Model => merged_models(s, sibling_provider(s, file, at)),
        Picker::Channel => all_channels(s),
        Picker::Currency => currencies(s),
        Picker::Place => places(s, text),
        Picker::ModelRef => model_refs(s, file, at, text),
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
        editor_model::rank::rank(
            query,
            list,
            editor_model::rank::RankMode::Path,
            &[],
            |_| false,
            LIMIT,
        )
        .into_iter()
        .map(|r| Candidate::plain(list[r.index].clone()))
        .collect()
    };
    match picker {
        Picker::Folder => raw(&folders(paths)),
        Picker::Path => raw(paths),
        // Geocode results are already ranked by the service for this query.
        Picker::Place => places(s, query),
        Picker::ModelRef => {
            let q = split_ref(query).map_or(query, |(_, m)| m);
            typeahead::filter(q, &small_set(s, picker, file, at, query), LIMIT)
        }
        // Channels already in the list drop out of the adder's suggestions (plan
        // round-7-feedback-decisions WP-5).
        Picker::Channel => {
            let listed = s.str_list(file, at);
            let left: Vec<Candidate> = small_set(s, picker, file, at, query)
                .into_iter()
                .filter(|c| !listed.contains(&c.value))
                .collect();
            typeahead::filter(query, &left, LIMIT)
        }
        _ => typeahead::filter(query, &small_set(s, picker, file, at, query), LIMIT),
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
        // Free text: a place label alone, a `{param.…}` model, a currency before the list loads.
        Picker::Place => true,
        Picker::ModelRef if text.starts_with('{') => true,
        Picker::Currency if s.lookups.currency_list().is_none() => true,
        _ => typeahead::is_value(text, &small_set(s, picker, file, at, text)),
    }
}

/// The display label of a stored value (Provider: its name; Currency: `CODE — Name`; Channel:
/// its name or fetched title; others: as stored).
pub fn label_of(s: &Settings, picker: Picker, stored: &str) -> Option<String> {
    match picker {
        Picker::Provider => providers(s)
            .into_iter()
            .find(|c| c.value == stored)
            .map(|c| c.label),
        Picker::Channel => Some(chip_label(s, &channels(s), stored)),
        Picker::Currency => currencies(s)
            .into_iter()
            .find(|c| c.value == stored)
            .map(|c| c.label),
        _ => None,
    }
}
