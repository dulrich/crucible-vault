//! The daemon's on-demand lookup RPCs (plan `round-5-reading-pickers-intake` WP-5):
//! `lookup_currencies`, `lookup_geocode`, `lookup_models`, `lookup_channels`.
//!
//! Every result is cached in memory: currencies 24 h, geocode per lowercased query 24 h, channel
//! titles per id 24 h, live model lists per provider 1 h or until the config poller sees a change
//! ([`Lookups::invalidate_models`]). A provider that cannot be listed (unreachable, or a kind
//! without a listing endpoint) yields its `providers.toml` models with `live: false` — that
//! fallback is not cached, so the next ask retries. Other upstream failures are
//! [`UPSTREAM_FAILED`] errors whose message is the classified [`FetchError`] text (never a URL,
//! body or key). Each successful upstream call adds one `api_usage` unit, as the A2 steps do.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use cv_protocol::{
    LookupChannel, LookupCurrency, LookupGeoResult, LookupModel, LookupModelsResult, RpcError,
    error_codes,
};
use cv_providers::secrets::{SystemSecrets, default_secrets_dir, resolve_api_key, resolve_secret};
use cv_providers::web::youtube::{self, YoutubeApi};
use cv_providers::web::{FetchError, WebClient, fx, weather};
use cv_providers::{ProvidersConfig, catalog};
use cv_state::StateDb;

use crate::daemon::rpc_err;

/// Error code of a failed upstream lookup (the editor shows the message and falls back).
pub const UPSTREAM_FAILED: i64 = 1005;

/// Currency list lifetime.
pub const CURRENCY_TTL: Duration = Duration::from_secs(24 * 3600);
/// Geocode result lifetime (per lowercased query).
pub const GEOCODE_TTL: Duration = Duration::from_secs(24 * 3600);
/// Channel title lifetime (per id).
pub const CHANNEL_TTL: Duration = Duration::from_secs(24 * 3600);
/// Live model list lifetime (also dropped on any config change).
pub const MODELS_TTL: Duration = Duration::from_secs(3600);

/// A tiny TTL map.
#[derive(Debug)]
pub struct TtlCache<K, V> {
    ttl: Duration,
    map: HashMap<K, (Instant, V)>,
}

impl<K: Eq + Hash, V: Clone> TtlCache<K, V> {
    /// An empty cache whose entries live `ttl`.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            map: HashMap::new(),
        }
    }

    /// The value for `k` if stored less than `ttl` before `now`.
    pub fn get(&self, k: &K, now: Instant) -> Option<V> {
        self.map
            .get(k)
            .filter(|(at, _)| now.saturating_duration_since(*at) < self.ttl)
            .map(|(_, v)| v.clone())
    }

    /// Stores `v` for `k` at `now`.
    pub fn put(&mut self, k: K, v: V, now: Instant) {
        self.map.insert(k, (now, v));
    }

    /// Drops every entry.
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// Upstream API bases (overridable in tests).
#[derive(Debug, Clone)]
pub struct LookupBases {
    pub fx: String,
    pub geocode: String,
    pub youtube: String,
}

impl Default for LookupBases {
    fn default() -> Self {
        Self {
            fx: fx::DEFAULT_BASE.to_owned(),
            geocode: weather::GEOCODE_BASE.to_owned(),
            youtube: youtube::DEFAULT_BASE.to_owned(),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn upstream(e: &FetchError) -> RpcError {
    rpc_err(UPSTREAM_FAILED, e.to_string())
}

/// The lookup service: caches + upstream clients.
#[derive(Debug)]
pub struct Lookups {
    bases: LookupBases,
    config_dir: PathBuf,
    secrets: SystemSecrets,
    state: Arc<Mutex<StateDb>>,
    client: Mutex<Option<Arc<WebClient>>>,
    currencies: Mutex<TtlCache<(), Vec<LookupCurrency>>>,
    geocode: Mutex<TtlCache<String, Vec<LookupGeoResult>>>,
    models: Mutex<TtlCache<String, Vec<LookupModel>>>,
    channels: Mutex<TtlCache<String, Option<String>>>,
}

impl Lookups {
    /// A service reading `providers.toml` from `config_dir` and recording usage in `state`.
    #[must_use]
    pub fn new(config_dir: &Path, state: Arc<Mutex<StateDb>>, bases: LookupBases) -> Self {
        Self {
            bases,
            config_dir: config_dir.to_path_buf(),
            secrets: SystemSecrets {
                dir: default_secrets_dir().unwrap_or_default(),
            },
            state,
            client: Mutex::new(None),
            currencies: Mutex::new(TtlCache::new(CURRENCY_TTL)),
            geocode: Mutex::new(TtlCache::new(GEOCODE_TTL)),
            models: Mutex::new(TtlCache::new(MODELS_TTL)),
            channels: Mutex::new(TtlCache::new(CHANNEL_TTL)),
        }
    }

    /// Forgets every live model list (the config poller saw a change).
    pub fn invalidate_models(&self) {
        lock(&self.models).clear();
    }

    fn client(&self) -> Result<Arc<WebClient>, RpcError> {
        let mut c = lock(&self.client);
        if let Some(c) = &*c {
            return Ok(Arc::clone(c));
        }
        let made = Arc::new(
            WebClient::new(cv_providers::web::WebConfig {
                timeout: Duration::from_secs(10),
                ..Default::default()
            })
            .map_err(|e| upstream(&e))?,
        );
        *c = Some(Arc::clone(&made));
        Ok(made)
    }

    fn usage(&self, service: &str) {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        if let Err(e) = lock(&self.state).add_api_usage(service, &day, 1) {
            tracing::warn!(service, error = %e, "api_usage update failed");
        }
    }

    /// `lookup_currencies`.
    pub async fn currencies(&self) -> Result<Vec<LookupCurrency>, RpcError> {
        if let Some(v) = lock(&self.currencies).get(&(), Instant::now()) {
            return Ok(v);
        }
        let client = self.client()?;
        let list = fx::FxApi::new(&self.bases.fx)
            .currencies(&client)
            .await
            .map_err(|e| upstream(&e))?;
        self.usage(fx::SERVICE);
        let out: Vec<LookupCurrency> = list
            .into_iter()
            .map(|c| LookupCurrency {
                code: c.code,
                name: c.name,
            })
            .collect();
        lock(&self.currencies).put((), out.clone(), Instant::now());
        Ok(out)
    }

    /// `lookup_geocode`.
    pub async fn geocode(&self, query: &str) -> Result<Vec<LookupGeoResult>, RpcError> {
        let key = query.trim().to_lowercase();
        if key.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(v) = lock(&self.geocode).get(&key, Instant::now()) {
            return Ok(v);
        }
        let client = self.client()?;
        let hits = weather::geocode(&client, &self.bases.geocode, &key)
            .await
            .map_err(|e| upstream(&e))?;
        self.usage(weather::SERVICE);
        let out: Vec<LookupGeoResult> = hits
            .into_iter()
            .map(|h| LookupGeoResult {
                label: h.label,
                admin1: h.admin1,
                country_code: h.country_code,
                lat: h.lat,
                lon: h.lon,
            })
            .collect();
        lock(&self.geocode).put(key, out.clone(), Instant::now());
        Ok(out)
    }

    fn providers(&self) -> Result<ProvidersConfig, RpcError> {
        match std::fs::read_to_string(self.config_dir.join("providers.toml")) {
            Ok(text) => ProvidersConfig::from_toml(&text)
                .map_err(|e| rpc_err(error_codes::INTERNAL_ERROR, format!("providers.toml: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProvidersConfig::default()),
            Err(e) => Err(rpc_err(
                error_codes::INTERNAL_ERROR,
                format!("providers.toml: {e}"),
            )),
        }
    }

    /// `lookup_models`: unknown provider → `NOT_FOUND`; unlistable → declared, `live: false`.
    pub async fn models(&self, provider: &str) -> Result<LookupModelsResult, RpcError> {
        let cfg = self.providers()?;
        let p = cfg
            .providers
            .iter()
            .find(|p| p.id == provider)
            .ok_or_else(|| rpc_err(error_codes::NOT_FOUND, format!("no provider {provider:?}")))?;
        if let Some(models) = lock(&self.models).get(&p.id, Instant::now()) {
            return Ok(LookupModelsResult { models, live: true });
        }
        let declared = || LookupModelsResult {
            models: p
                .models
                .iter()
                .map(|m| LookupModel {
                    id: m.id.clone(),
                    display_name: None,
                })
                .collect(),
            live: false,
        };
        let key = resolve_api_key(p, &self.secrets).ok().flatten();
        let client = self.client()?;
        match catalog::list_models(&client, p, key.as_deref()).await {
            Ok(Some(list)) => {
                let models: Vec<LookupModel> = list
                    .into_iter()
                    .map(|m| LookupModel {
                        id: m.id,
                        display_name: m.display_name,
                    })
                    .collect();
                lock(&self.models).put(p.id.clone(), models.clone(), Instant::now());
                Ok(LookupModelsResult { models, live: true })
            }
            Ok(None) => Ok(declared()),
            Err(e) => {
                tracing::info!(provider = %p.id, error = %e, "model list unavailable; declared models");
                Ok(declared())
            }
        }
    }

    /// `lookup_channels`: titles for `ids` (unknown ids absent); cached per id.
    pub async fn channels(&self, ids: &[String]) -> Result<Vec<LookupChannel>, RpcError> {
        let now = Instant::now();
        let mut titles: HashMap<String, Option<String>> = HashMap::new();
        let mut missing: Vec<&str> = Vec::new();
        {
            let cache = lock(&self.channels);
            for id in ids {
                match cache.get(id, now) {
                    Some(t) => {
                        titles.insert(id.clone(), t);
                    }
                    None if !missing.contains(&id.as_str()) => missing.push(id),
                    None => {}
                }
            }
        }
        if !missing.is_empty() {
            let key = resolve_secret(crate::steps::youtube::API_KEY_SECRET, &self.secrets)
                .ok_or_else(|| rpc_err(UPSTREAM_FAILED, "YouTube API key not configured"))?;
            let client = self.client()?;
            let api = YoutubeApi::new(&self.bases.youtube);
            for chunk in missing.chunks(youtube::MAX_IDS) {
                let found = api
                    .channels(&client, &key, chunk)
                    .await
                    .map_err(|e| upstream(&e))?;
                self.usage(youtube::SERVICE);
                let mut cache = lock(&self.channels);
                for id in chunk {
                    let t = found
                        .iter()
                        .find(|c| c.id == *id)
                        .map(|c| c.snippet.title.clone());
                    cache.put((*id).to_owned(), t.clone(), now);
                    titles.insert((*id).to_owned(), t);
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        Ok(ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .filter_map(|id| {
                titles
                    .get(id)
                    .cloned()
                    .flatten()
                    .map(|title| LookupChannel {
                        id: id.clone(),
                        title,
                    })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_cache_hits_then_expires() {
        let mut c: TtlCache<String, u32> = TtlCache::new(Duration::from_secs(10));
        let t0 = Instant::now();
        c.put("a".into(), 1, t0);
        assert_eq!(c.get(&"a".into(), t0 + Duration::from_secs(9)), Some(1));
        assert_eq!(c.get(&"a".into(), t0 + Duration::from_secs(10)), None);
        assert_eq!(c.get(&"b".into(), t0), None);
        c.clear();
        assert_eq!(c.get(&"a".into(), t0), None);
    }

    fn service(dir: &Path) -> Lookups {
        let state =
            StateDb::open(&dir.join("state.db"), 0, cv_state::QueueConfig::default()).unwrap();
        Lookups::new(dir, Arc::new(Mutex::new(state)), LookupBases::default())
    }

    #[tokio::test]
    async fn models_fall_back_to_declared_when_unreachable() {
        let dir = std::env::temp_dir().join(format!("cv-lookup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("providers.toml"),
            "[[provider]]\nid = \"local\"\nkind = \"ollama\"\nbase_url = \"http://127.0.0.1:1\"\nmodels = [{ id = \"m1\" }]\n\n[[provider]]\nid = \"claude\"\nkind = \"anthropic\"\nmodels = [{ id = \"sonnet\" }]\n",
        )
        .unwrap();
        let l = service(&dir);
        let r = l.models("local").await.unwrap();
        assert!(!r.live);
        assert_eq!(r.models[0].id, "m1");
        let r = l.models("claude").await.unwrap();
        assert_eq!((r.live, r.models[0].id.as_str()), (false, "sonnet"));
        assert_eq!(
            l.models("nope").await.unwrap_err().code,
            error_codes::NOT_FOUND
        );
        assert!(l.geocode("  ").await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
