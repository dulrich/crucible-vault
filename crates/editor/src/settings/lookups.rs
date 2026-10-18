//! Network-backed picker data (plan round-5-reading-pickers-intake WP-6): a session cache of the
//! daemon lookup RPCs (`lookup_currencies`, `lookup_geocode`, `lookup_models`,
//! `lookup_channels`) and the decision of what to fetch next ([`Settings::lookup_wants`]).
//!
//! Settings holds no daemon handle: `App` asks for the wanted lookups after routing a settings
//! message, runs each through its off-thread `rpc` helper, and feeds the results back as
//! [`LookupMsg`]s. Geocode is debounced by a sequence number: only the latest query's due tick
//! issues a call, and a response for any other query is cached but never shown.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use cv_protocol::{
    LookupChannel, LookupCurrency, LookupGeoResult, LookupModel, LookupModelsResult,
};

use super::Settings;
use super::pickers::{self, Picker};

/// The geocode debounce (ms) between the last keystroke and the lookup.
pub const GEO_DEBOUNCE_MS: u64 = 350;
/// Geocode queries kept in the cache.
const GEO_CACHE: usize = 32;
/// The model list's status line when the live list is unavailable.
pub const MODELS_FAILED: &str = "live list unavailable — showing declared models";

/// A lookup result (wrapped in [`super::SettingsMsg::Lookup`]).
#[derive(Debug, Clone)]
pub enum LookupMsg {
    Currencies(Result<Vec<LookupCurrency>, String>),
    /// The geocode debounce for sequence `.0` elapsed.
    GeoDue(u64),
    Geo(String, Result<Vec<LookupGeoResult>, String>),
    Models(String, Result<LookupModelsResult, String>),
    Channels(Vec<String>, Result<Vec<LookupChannel>, String>),
}

/// A lookup the app should issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    Currencies,
    /// Start the geocode debounce for sequence `.0`.
    GeoDebounce(u64),
    Models(String),
    Channels(Vec<String>),
}

/// One provider's live model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Models {
    Loading,
    Live(Vec<LookupModel>),
    /// RPC error or `live: false`: declared models only.
    Failed,
}

/// The session cache.
#[derive(Debug, Default)]
pub struct Lookups {
    /// `None` = never fetched; `Some(Err)` = failed (not retried this session).
    pub currencies: Option<Result<Vec<LookupCurrency>, String>>,
    currencies_pending: bool,
    /// Geocode hits per normalized query (bounded, oldest evicted).
    pub geo: BTreeMap<String, Result<Vec<LookupGeoResult>, String>>,
    geo_order: VecDeque<String>,
    /// The query the latest debounce tick is for, and its sequence.
    geo_wanted: Option<(u64, String)>,
    geo_seq: u64,
    /// The query in flight.
    geo_pending: Option<String>,
    /// Live models per provider id. A `Failed` entry is re-fetched when a model field's
    /// typeahead is next opened ([`Lookups::reopened`]) or on the field's refresh control
    /// ([`Lookups::refresh_models`]) (plan round-6-feedback-todos WP-8).
    pub models: BTreeMap<String, Models>,
    /// A new typeahead was opened since the last [`Settings::lookup_wants`].
    pub(super) reopened: bool,
    /// Providers whose model list the refresh control asked for.
    refresh: Vec<String>,
    /// Fetched channel titles (`None` = YouTube does not know the id).
    pub channels: BTreeMap<String, Option<String>>,
    channels_pending: BTreeSet<String>,
}

/// The cache key of a geocode query.
pub fn geo_key(q: &str) -> String {
    q.trim().to_lowercase()
}

impl Lookups {
    /// The refresh control beside a model field: re-fetch `provider`'s live list.
    pub fn refresh_models(&mut self, provider: String) {
        if !self.refresh.contains(&provider) {
            self.refresh.push(provider);
        }
    }

    /// A fetched title for channel `id`.
    pub fn channel_title(&self, id: &str) -> Option<&str> {
        self.channels.get(id).and_then(|t| t.as_deref())
    }

    /// The debounce tick for `seq` fired: the query to look up, when still current and not
    /// cached.
    pub fn geo_due(&mut self, seq: u64) -> Option<String> {
        let (s, q) = self.geo_wanted.as_ref()?;
        if *s != seq || self.geo.contains_key(q) {
            return None;
        }
        let q = q.clone();
        self.geo_pending = Some(q.clone());
        Some(q)
    }

    /// Applies a result.
    pub fn apply(&mut self, m: LookupMsg) {
        match m {
            LookupMsg::Currencies(r) => {
                self.currencies_pending = false;
                self.currencies = Some(r);
            }
            LookupMsg::GeoDue(_) => {}
            LookupMsg::Geo(q, r) => {
                if self.geo_pending.as_deref() == Some(q.as_str()) {
                    self.geo_pending = None;
                }
                if self.geo.insert(q.clone(), r).is_none() {
                    self.geo_order.push_back(q);
                    while self.geo_order.len() > GEO_CACHE {
                        if let Some(old) = self.geo_order.pop_front() {
                            self.geo.remove(&old);
                        }
                    }
                }
            }
            LookupMsg::Models(p, r) => {
                let e = match r {
                    Ok(m) if m.live => Models::Live(m.models),
                    _ => Models::Failed,
                };
                self.models.insert(p, e);
            }
            LookupMsg::Channels(ids, r) => {
                let titles: BTreeMap<String, String> = r
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| (c.id, c.title))
                    .collect();
                for id in ids {
                    self.channels_pending.remove(&id);
                    let t = titles.get(&id).cloned();
                    self.channels.insert(id, t);
                }
            }
        }
    }

    /// The geocode hits for `query` (empty when not cached).
    pub fn places(&self, query: &str) -> &[LookupGeoResult] {
        match self.geo.get(&geo_key(query)) {
            Some(Ok(v)) => v,
            _ => &[],
        }
    }
}

/// A channel id YouTube can resolve (`UC…`, no spaces); free-text names never are.
fn looks_like_channel_id(s: &str) -> bool {
    s.len() > 2 && s.starts_with("UC") && !s.contains(char::is_whitespace)
}

impl Settings {
    /// The lookups to issue now: the active network picker's data, and titles of channel ids
    /// not named in sources.toml. Marks each as pending.
    pub fn lookup_wants(&mut self) -> Vec<Want> {
        let mut out = Vec::new();
        let reopened = std::mem::take(&mut self.lookups.reopened);
        for p in std::mem::take(&mut self.lookups.refresh) {
            if self.lookups.models.get(&p) != Some(&Models::Loading) {
                self.lookups.models.insert(p.clone(), Models::Loading);
                out.push(Want::Models(p));
            }
        }
        if let Some(a) = self.form.typeahead.as_ref() {
            let (picker, file, path) = (a.picker, a.file.clone(), a.path.clone());
            let text = a.draft.clone();
            match picker {
                Picker::Currency => {
                    if self.lookups.currencies.is_none() && !self.lookups.currencies_pending {
                        self.lookups.currencies_pending = true;
                        out.push(Want::Currencies);
                    }
                }
                Picker::Model | Picker::ModelRef => {
                    if let Some(p) = pickers::model_provider(self, picker, &file, &path, &text)
                        && match self.lookups.models.get(&p) {
                            None => true,
                            Some(Models::Failed) => reopened,
                            Some(_) => false,
                        }
                    {
                        self.lookups.models.insert(p.clone(), Models::Loading);
                        out.push(Want::Models(p));
                    }
                }
                Picker::Place => {
                    let q = geo_key(&text);
                    let current = self.lookups.geo_wanted.as_ref().map(|(_, w)| w.as_str());
                    if q.chars().count() >= 2
                        && !self.lookups.geo.contains_key(&q)
                        && current != Some(q.as_str())
                    {
                        self.lookups.geo_seq += 1;
                        self.lookups.geo_wanted = Some((self.lookups.geo_seq, q));
                        out.push(Want::GeoDebounce(self.lookups.geo_seq));
                    }
                }
                _ => {}
            }
        }
        let ids: Vec<String> = self
            .unknown_channel_ids()
            .into_iter()
            .filter(|id| {
                !self.lookups.channels.contains_key(id)
                    && !self.lookups.channels_pending.contains(id)
            })
            .collect();
        if !ids.is_empty() {
            self.lookups.channels_pending.extend(ids.iter().cloned());
            out.push(Want::Channels(ids));
        }
        out
    }

    /// Channel ids without a sources.toml name: Ignore Shorts chips not declared in
    /// sources.toml, and `[[youtube]]` rows whose `channel` is empty.
    fn unknown_channel_ids(&self) -> Vec<String> {
        let named: Vec<String> = pickers::channels(self)
            .into_iter()
            .filter(|c| c.label != c.value)
            .map(|c| c.value)
            .collect();
        let mut out: BTreeSet<String> = BTreeSet::new();
        if let Some(doc) = pickers::doc_named(self, "pipelines.toml") {
            let p = cv_schema::Path::root().key("ignore_shorts").key("channels");
            out.extend(doc.get_str_list(&p).unwrap_or_default());
        }
        if let Some(doc) = pickers::doc_named(self, "sources.toml") {
            let root = cv_schema::Path::root().key("youtube");
            for i in 0..doc.len(&root) {
                let ip = root.index(i);
                if let Some(id) = doc.get_str(&ip.key("id")) {
                    out.insert(id.to_owned());
                }
            }
        }
        out.into_iter()
            .filter(|id| looks_like_channel_id(id) && !named.contains(id))
            .collect()
    }
}
