//! Per-provider completion-class concurrency limiter.
//!
//! Carried from Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/providers.ts:17-110`
//! (`resolveProviderConcurrencyLimit`, `ProviderConcurrencyLimiter`) and
//! `src/providers/AGENTS.md:16`. Load-bearing rule: the slot is released when the underlying
//! request SETTLES, never when a caller's timeout abandons it — here the permit is moved into a
//! spawned task that owns the in-flight request, so dropping or timing out the caller's
//! [`tokio::task::JoinHandle`] wait leaves the permit held until the request finishes. A caller
//! dropped while still queued (before acquisition) never dispatches anything. Waiters are FIFO
//! (`tokio::sync::Semaphore` is fair). Embed and native rerank never route through this.
//! Nothing Obsidian-induced to drop (Crucible's limiter was already plugin-agnostic).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;

use crate::config::{ProviderConfig, ProviderKind};
use crate::error::{ProviderError, Result};

/// `None` = unlimited.
pub fn resolve_concurrency_limit(p: &ProviderConfig) -> Option<usize> {
    match p.max_concurrent_requests {
        Some(n) if n > 0 => Some(n as usize),
        _ if is_local(p) => Some(1),
        _ => None,
    }
}

/// Local-default heuristic. Crucible's `isLocal` (`openaiCompatible.ts:35`) was "kind is
/// `openai-compatible`", because OpenAI/OpenRouter had their own kinds. Here OpenRouter shares
/// the kind, so: explicit `local` wins; else `openai-compatible` whose base URL host is
/// loopback/`localhost` (or unset, which defaults to LM Studio's localhost port). Ollama and CLI
/// stay unlimited by default, as in Crucible (`providerConcurrencyLimiter.test.mjs:100-107`).
pub fn is_local(p: &ProviderConfig) -> bool {
    if let Some(l) = p.local {
        return l;
    }
    if p.kind != ProviderKind::OpenaiCompatible {
        return false;
    }
    let Some(url) = p.base_url.as_deref() else {
        return true;
    };
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    host.eq_ignore_ascii_case("localhost")
        || host.starts_with("127.")
        || host == "::1"
        || host == "0.0.0.0"
}

/// Keyed by provider id (not object identity), so requests issued after a config edit share a
/// queue with in-flight ones. A changed limit installs a fresh semaphore; in-flight requests
/// keep their old permits.
#[derive(Debug, Default)]
pub struct ConcurrencyLimiter {
    states: Mutex<HashMap<String, (usize, Arc<Semaphore>)>>,
}

impl ConcurrencyLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn semaphore(&self, id: &str, limit: usize) -> Arc<Semaphore> {
        let mut g = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match g.get(id) {
            Some((l, s)) if *l == limit => s.clone(),
            _ => {
                let s = Arc::new(Semaphore::new(limit));
                g.insert(id.to_owned(), (limit, s.clone()));
                s
            }
        }
    }

    /// Runs `fut` with at most `limit` in flight per `provider_id`. The future is spawned onto
    /// the runtime once a slot is acquired and owns the permit until it settles.
    pub async fn run<F, T>(&self, provider_id: &str, limit: Option<usize>, fut: F) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        let Some(limit) = limit else {
            return fut.await;
        };
        let sem = self.semaphore(provider_id, limit);
        let permit = sem
            .acquire_owned()
            .await
            .map_err(|e| ProviderError::Task(e.to_string()))?;
        let handle = tokio::spawn(async move {
            let out = fut.await;
            drop(permit);
            out
        });
        handle
            .await
            .map_err(|e| ProviderError::Task(e.to_string()))?
    }

    /// Slots currently free for `provider_id` (test/diagnostic aid).
    pub fn available(&self, provider_id: &str) -> Option<usize> {
        let g = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.get(provider_id).map(|(_, s)| s.available_permits())
    }
}

#[cfg(test)]
mod tests {
    //! Ported from `https://github.com/dulrich/obsidian-crucible/blob/master/tests/providerConcurrencyLimiter.test.mjs:96-422`
    //! (limiter-level; describeImage timeout-label cases are image-describe, out of scope).
    use super::*;
    use crate::config::ProvidersConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::oneshot;

    fn p(extra: &str) -> ProviderConfig {
        ProvidersConfig::from_toml(&format!("[[provider]]\nid = \"x\"\n{extra}"))
            .unwrap()
            .providers
            .remove(0)
    }

    #[test]
    fn resolve_defaults_and_overrides() {
        let local = "kind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:1234/v1\"\n";
        assert_eq!(resolve_concurrency_limit(&p(local)), Some(1));
        assert_eq!(
            resolve_concurrency_limit(&p("kind = \"openai-compatible\"\n")),
            Some(1)
        );
        assert_eq!(
            resolve_concurrency_limit(&p(
                "kind = \"openai-compatible\"\nbase_url = \"http://localhost:8080\"\n"
            )),
            Some(1)
        );
        assert_eq!(
            resolve_concurrency_limit(&p(
                "kind = \"openai-compatible\"\nbase_url = \"https://openrouter.ai/api/v1\"\n"
            )),
            None
        );
        assert_eq!(
            resolve_concurrency_limit(&p("kind = \"anthropic\"\n")),
            None
        );
        assert_eq!(
            resolve_concurrency_limit(&p("kind = \"cli\"\ncommand = \"codex\"\n")),
            None
        );
        assert_eq!(
            resolve_concurrency_limit(&p(&format!("{local}max_concurrent_requests = 3\n"))),
            Some(3)
        );
        assert_eq!(
            resolve_concurrency_limit(&p("kind = \"google\"\nmax_concurrent_requests = 2\n")),
            Some(2)
        );
        // zero falls back to the default rather than disabling the limiter
        assert_eq!(
            resolve_concurrency_limit(&p(&format!("{local}max_concurrent_requests = 0\n"))),
            Some(1)
        );
        assert_eq!(
            resolve_concurrency_limit(&p("kind = \"ollama\"\nlocal = true\n")),
            Some(1)
        );
    }

    /// Spawns `run` for a gated job; returns (started-rx, release-tx, join).
    fn gated(
        lim: &Arc<ConcurrencyLimiter>,
        limit: Option<usize>,
        log: &Arc<Mutex<Vec<usize>>>,
        n: usize,
    ) -> (oneshot::Sender<()>, tokio::task::JoinHandle<Result<usize>>) {
        let (tx, rx) = oneshot::channel::<()>();
        let (lim, log) = (lim.clone(), log.clone());
        let h = tokio::spawn(async move {
            lim.run("x", limit, async move {
                log.lock().unwrap().push(n);
                let _ = rx.await;
                Ok(n)
            })
            .await
        });
        (tx, h)
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    #[tokio::test]
    async fn limit_one_serializes_fifo() {
        let lim = Arc::new(ConcurrencyLimiter::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let (r1, h1) = gated(&lim, Some(1), &log, 1);
        settle().await;
        let (r2, h2) = gated(&lim, Some(1), &log, 2);
        settle().await;
        let (r3, h3) = gated(&lim, Some(1), &log, 3);
        settle().await;
        assert_eq!(*log.lock().unwrap(), vec![1]);
        r1.send(()).unwrap();
        assert_eq!(h1.await.unwrap().unwrap(), 1);
        settle().await;
        assert_eq!(*log.lock().unwrap(), vec![1, 2]);
        r2.send(()).unwrap();
        h2.await.unwrap().unwrap();
        settle().await;
        assert_eq!(*log.lock().unwrap(), vec![1, 2, 3]);
        r3.send(()).unwrap();
        h3.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unlimited_and_raised_limits_dispatch_concurrently() {
        for limit in [None, Some(2)] {
            let lim = Arc::new(ConcurrencyLimiter::new());
            let log = Arc::new(Mutex::new(Vec::new()));
            let (r1, h1) = gated(&lim, limit, &log, 1);
            let (r2, h2) = gated(&lim, limit, &log, 2);
            settle().await;
            assert_eq!(log.lock().unwrap().len(), 2, "{limit:?}");
            r1.send(()).unwrap();
            r2.send(()).unwrap();
            h1.await.unwrap().unwrap();
            h2.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn caller_timeout_does_not_release_slot_before_settle() {
        let lim = Arc::new(ConcurrencyLimiter::new());
        let settled = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = oneshot::channel::<()>();
        let s2 = settled.clone();
        // Caller races the request against its own short timeout and gives up.
        let r = tokio::time::timeout(
            Duration::from_millis(20),
            lim.run("x", Some(1), async move {
                let _ = rx.await;
                s2.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        )
        .await;
        assert!(r.is_err(), "caller timed out");
        assert_eq!(
            lim.available("x"),
            Some(0),
            "slot still held by in-flight request"
        );
        let log = Arc::new(Mutex::new(Vec::new()));
        let (r2, h2) = gated(&lim, Some(1), &log, 2);
        settle().await;
        assert!(log.lock().unwrap().is_empty(), "second stays queued");
        tx.send(()).unwrap();
        settle().await;
        assert_eq!(settled.load(Ordering::SeqCst), 1);
        assert_eq!(*log.lock().unwrap(), vec![2]);
        r2.send(()).unwrap();
        h2.await.unwrap().unwrap();
        assert_eq!(lim.available("x"), Some(1));
    }

    #[tokio::test]
    async fn caller_dropped_while_queued_never_dispatches() {
        let lim = Arc::new(ConcurrencyLimiter::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let (r1, h1) = gated(&lim, Some(1), &log, 1);
        settle().await;
        let (_r2, h2) = gated(&lim, Some(1), &log, 2);
        settle().await;
        h2.abort();
        settle().await;
        r1.send(()).unwrap();
        h1.await.unwrap().unwrap();
        settle().await;
        assert_eq!(*log.lock().unwrap(), vec![1]);
        assert_eq!(lim.available("x"), Some(1));
    }
}
