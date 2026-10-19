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

/// One fetch's state (WP-R21 SM16: every source shares it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch<T> {
    /// In flight.
    Pending,
    Ok(T),
    /// The RPC failed (or, for models, the list is not live). Each source's status line names
    /// its own failure; the error text is not shown.
    Failed,
}

/// One provider's live model list (`Failed`: declared models only).
pub type Models = Fetch<Vec<LookupModel>>;

/// When a failed entry is fetched again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Never this session.
    Never,
    /// When the field's typeahead is next opened ([`Lookups::reopened`]); the explicit refresh
    /// control ([`Lookups::refresh_models`]) refetches any settled entry.
    OnReopen,
}

/// A keyed session cache of [`Fetch`]es with a retry policy and an optional bound (oldest
/// settled entry evicted first).
#[derive(Debug, Clone)]
pub struct KeyedCache<K: Ord + Clone, T> {
    entries: BTreeMap<K, Fetch<T>>,
    /// Settled keys, oldest first (eviction order).
    order: VecDeque<K>,
    cap: usize,
    retry: Retry,
}

impl<K: Ord + Clone, T> KeyedCache<K, T> {
    pub fn new(retry: Retry, cap: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            order: VecDeque::new(),
            cap,
            retry,
        }
    }

    pub fn get<Q: Ord + ?Sized>(&self, k: &Q) -> Option<&Fetch<T>>
    where
        K: std::borrow::Borrow<Q>,
    {
        self.entries.get(k)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `k` has a settled (ok or failed) result.
    pub fn settled<Q: Ord + ?Sized>(&self, k: &Q) -> bool
    where
        K: std::borrow::Borrow<Q>,
    {
        matches!(self.entries.get(k), Some(Fetch::Ok(_) | Fetch::Failed))
    }

    /// Whether `k` should be fetched now under the policy (`reopened`: a typeahead was opened).
    pub fn wants<Q: Ord + ?Sized>(&self, k: &Q, reopened: bool) -> bool
    where
        K: std::borrow::Borrow<Q>,
    {
        match self.entries.get(k) {
            None => true,
            Some(Fetch::Failed) => self.retry == Retry::OnReopen && reopened,
            Some(_) => false,
        }
    }

    /// Marks `k` in flight (a settled entry keeps its eviction slot).
    pub fn pending(&mut self, k: K) {
        self.entries.insert(k, Fetch::Pending);
    }

    /// Lands a result for `k`, evicting the oldest settled entries past the bound.
    pub fn settle(&mut self, k: K, v: Result<T, String>) {
        let fresh = !self.settled(&k);
        self.entries.insert(
            k.clone(),
            match v {
                Ok(v) => Fetch::Ok(v),
                Err(_) => Fetch::Failed,
            },
        );
        if fresh {
            self.order.push_back(k);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.entries.remove(&old);
                }
            }
        }
    }

    /// The settled ok entries.
    pub fn oks(&self) -> impl Iterator<Item = (&K, &T)> {
        self.entries.iter().filter_map(|(k, v)| match v {
            Fetch::Ok(t) => Some((k, t)),
            _ => None,
        })
    }
}

/// The session cache. Per-source policy (unchanged by WP-R21): currencies and channel titles
/// are never refetched; geocode never refetches a cached query and keeps the latest
/// [`GEO_CACHE`] queries; a failed model list refetches when its typeahead re-opens and on the
/// refresh control.
#[derive(Debug)]
pub struct Lookups {
    /// The currency list (one key).
    pub currencies: KeyedCache<(), Vec<LookupCurrency>>,
    /// Geocode hits per normalized query (only settled queries are cached; bounded).
    pub geo: KeyedCache<String, Vec<LookupGeoResult>>,
    /// The query the latest debounce tick is for, and its sequence.
    geo_wanted: Option<(u64, String)>,
    geo_seq: u64,
    /// Live models per provider id (plan round-6-feedback-todos WP-8).
    pub models: KeyedCache<String, Vec<LookupModel>>,
    /// A new typeahead was opened since the last [`Settings::lookup_wants`].
    pub(super) reopened: bool,
    /// Providers whose model list the refresh control asked for.
    refresh: Vec<String>,
    /// Fetched channel titles (`Ok(None)` = YouTube does not know the id).
    pub channels: KeyedCache<String, Option<String>>,
}

impl Default for Lookups {
    fn default() -> Self {
        Self {
            currencies: KeyedCache::new(Retry::Never, usize::MAX),
            geo: KeyedCache::new(Retry::Never, GEO_CACHE),
            geo_wanted: None,
            geo_seq: 0,
            models: KeyedCache::new(Retry::OnReopen, usize::MAX),
            reopened: false,
            refresh: Vec::new(),
            channels: KeyedCache::new(Retry::Never, usize::MAX),
        }
    }
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
        match self.channels.get(id) {
            Some(Fetch::Ok(t)) => t.as_deref(),
            _ => None,
        }
    }

    /// The fetched currency list (`None` until it lands ok).
    pub fn currency_list(&self) -> Option<&[LookupCurrency]> {
        match self.currencies.get(&()) {
            Some(Fetch::Ok(v)) => Some(v),
            _ => None,
        }
    }

    /// The debounce tick for `seq` fired: the query to look up, when still current and not
    /// cached.
    pub fn geo_due(&mut self, seq: u64) -> Option<String> {
        let (s, q) = self.geo_wanted.as_ref()?;
        if *s != seq || self.geo.settled(q) {
            return None;
        }
        let q = q.clone();
        self.geo.pending(q.clone());
        Some(q)
    }

    /// Applies a result.
    pub fn apply(&mut self, m: LookupMsg) {
        match m {
            LookupMsg::Currencies(r) => self.currencies.settle((), r),
            LookupMsg::GeoDue(_) => {}
            LookupMsg::Geo(q, r) => self.geo.settle(q, r),
            LookupMsg::Models(p, r) => {
                let r = match r {
                    Ok(m) if m.live => Ok(m.models),
                    Ok(_) => Err("not live".to_owned()),
                    Err(e) => Err(e),
                };
                self.models.settle(p, r);
            }
            LookupMsg::Channels(ids, r) => {
                let titles: BTreeMap<String, String> = r
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| (c.id, c.title))
                    .collect();
                for id in ids {
                    let t = titles.get(&id).cloned();
                    self.channels.settle(id, Ok(t));
                }
            }
        }
    }

    /// The geocode hits for `query` (empty when not cached).
    pub fn places(&self, query: &str) -> &[LookupGeoResult] {
        match self.geo.get(&geo_key(query)) {
            Some(Fetch::Ok(v)) => v,
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
            if self.lookups.models.get(&p) != Some(&Fetch::Pending) {
                self.lookups.models.pending(p.clone());
                out.push(Want::Models(p));
            }
        }
        if let Some(a) = self.form.typeahead.as_ref() {
            let (picker, file, path) = (a.picker, a.file.clone(), a.path.clone());
            let text = a.draft.clone();
            match picker {
                Picker::Currency => {
                    if self.lookups.currencies.wants(&(), reopened) {
                        self.lookups.currencies.pending(());
                        out.push(Want::Currencies);
                    }
                }
                Picker::Model | Picker::ModelRef => {
                    if let Some(p) = pickers::model_provider(self, picker, &file, &path, &text)
                        && self.lookups.models.wants(&p, reopened)
                    {
                        self.lookups.models.pending(p.clone());
                        out.push(Want::Models(p));
                    }
                }
                Picker::Place => {
                    let q = geo_key(&text);
                    let current = self.lookups.geo_wanted.as_ref().map(|(_, w)| w.as_str());
                    if q.chars().count() >= 2
                        && !self.lookups.geo.settled(&q)
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
            .filter(|id| self.lookups.channels.wants(id, reopened))
            .collect();
        if !ids.is_empty() {
            for id in &ids {
                self.lookups.channels.pending(id.clone());
            }
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
