//! Drives each HTTP provider kind against the shared std-only `cv_testkit::MockHttp` — never the
//! network or a real model. Asserts request shape (path, headers, `max_tokens`, body) and
//! response parsing.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cv_providers::{
    CompletionRequest, FinishReason, ProviderConfig, ProviderManager, ProvidersConfig, SecretSource,
};
use cv_testkit::{MockHttp, Reply};
use serde_json::{Value, json};

/// One request as the assertions read it (target, lower-cased headers, JSON body).
#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

impl From<&cv_testkit::Request> for Recorded {
    fn from(r: &cv_testkit::Request) -> Self {
        Self {
            path: r.target.clone(),
            headers: r.headers.iter().cloned().collect(),
            body: serde_json::from_slice(&r.body).unwrap_or(Value::Null),
        }
    }
}

type Handler = Arc<dyn Fn(Recorded) -> (u16, Value) + Send + Sync>;

struct Mock {
    base: String,
    http: MockHttp,
}

impl Mock {
    /// Every request received so far.
    fn log(&self) -> Vec<Recorded> {
        self.http.requests().iter().map(Recorded::from).collect()
    }
}

async fn mock(handler: Handler) -> Mock {
    let http = MockHttp::start(move |req| {
        let (status, body) = handler(Recorded::from(req));
        Reply::json(status, body.to_string())
    });
    Mock {
        base: http.url(),
        http,
    }
}

fn fixed(status: u16, body: Value) -> Handler {
    Arc::new(move |_| (status, body.clone()))
}

/// A blocking permit gate for the mock's handler threads (`Notify`-like: permits are stored).
#[derive(Default)]
struct Gate(Mutex<usize>, Condvar);
impl Gate {
    fn wait(&self) {
        let mut n = self.0.lock().unwrap();
        while *n == 0 {
            n = self.1.wait(n).unwrap();
        }
        *n -= 1;
    }
    fn notify_one(&self) {
        *self.0.lock().unwrap() += 1;
        self.1.notify_one();
    }
}

struct Secrets(HashMap<String, String>);
impl SecretSource for Secrets {
    fn env_var(&self, n: &str) -> Option<String> {
        self.0.get(n).cloned()
    }
    fn secret_file(&self, _: &str) -> Option<String> {
        None
    }
}

fn manager() -> ProviderManager {
    let s = Secrets(HashMap::from([(
        "TEST_KEY".to_owned(),
        "sk-test".to_owned(),
    )]));
    ProviderManager::new(Arc::new(s)).unwrap()
}

fn provider(kind: &str, base: &str, extra: &str) -> ProviderConfig {
    ProvidersConfig::from_toml(&format!(
        "[[provider]]\nid = \"p-{kind}\"\nkind = \"{kind}\"\nbase_url = \"{base}\"\n{extra}"
    ))
    .unwrap()
    .providers
    .remove(0)
}

fn req(max: Option<u32>) -> CompletionRequest {
    CompletionRequest {
        system: "SYS".into(),
        user: "USER".into(),
        max_tokens: max,
        images: Vec::new(),
    }
}

#[tokio::test]
async fn openai_compatible_completion_shape_and_parse() {
    let m = mock(fixed(
        200,
        json!({"choices": [{"message": {"content": "hi"}, "finish_reason": "length"}]}),
    ))
    .await;
    let p = provider(
        "openai-compatible",
        &format!("{}/v1", m.base),
        "api_key_env = \"TEST_KEY\"\n",
    );
    let c = manager().complete(&p, "gemma-4", req(None)).await.unwrap();
    assert_eq!(c.text, "hi");
    assert_eq!(c.finish_reason, FinishReason::Length);
    let r = m.log()[0].clone();
    assert_eq!(r.path, "/v1/chat/completions");
    assert_eq!(r.headers["authorization"], "Bearer sk-test");
    assert_eq!(r.body["model"], "gemma-4");
    assert_eq!(r.body["max_tokens"], 4096);
    assert_eq!(
        r.body["messages"][0],
        json!({"role": "system", "content": "SYS"})
    );
    assert_eq!(
        r.body["messages"][1],
        json!({"role": "user", "content": "USER"})
    );
}

#[tokio::test]
async fn keyless_local_sends_no_auth_and_provider_max_tokens_applies() {
    let m = mock(fixed(
        200,
        json!({"choices": [{"message": {"content": "x"}}]}),
    ))
    .await;
    let p = provider("openai-compatible", &m.base, "max_tokens = 512\n");
    manager().complete(&p, "m", req(None)).await.unwrap();
    manager().complete(&p, "m", req(Some(64))).await.unwrap();
    let log = m.log();
    assert!(!log[0].headers.contains_key("authorization"));
    assert_eq!(log[0].body["max_tokens"], 512);
    assert_eq!(log[1].body["max_tokens"], 64);
}

#[tokio::test]
async fn anthropic_shape() {
    let m = mock(fixed(
        200,
        json!({"content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn"}),
    ))
    .await;
    let p = provider("anthropic", &m.base, "api_key_env = \"TEST_KEY\"\n");
    let c = manager()
        .complete(&p, "claude-x", req(Some(100)))
        .await
        .unwrap();
    assert_eq!(
        (c.text.as_str(), c.finish_reason),
        ("ok", FinishReason::Stop)
    );
    let r = m.log()[0].clone();
    assert_eq!(r.path, "/v1/messages");
    assert_eq!(r.headers["x-api-key"], "sk-test");
    assert_eq!(r.headers["anthropic-version"], "2023-06-01");
    assert_eq!(r.body["system"], "SYS");
    assert_eq!(r.body["max_tokens"], 100);
    assert_eq!(
        r.body["messages"],
        json!([{"role": "user", "content": "USER"}])
    );
}

#[tokio::test]
async fn anthropic_without_key_fails_before_any_request() {
    let m = mock(fixed(200, json!({}))).await;
    let p = provider("anthropic", &m.base, "");
    assert!(manager().complete(&p, "c", req(None)).await.is_err());
    assert!(m.log().is_empty());
}

#[tokio::test]
async fn google_shape() {
    let m = mock(fixed(200, json!({"candidates": [{"content": {"parts": [{"text": "a"}, {"text": "b"}]}, "finishReason": "STOP"}]}))).await;
    let p = provider("google", &m.base, "api_key_env = \"TEST_KEY\"\n");
    let c = manager().complete(&p, "gemini-x", req(None)).await.unwrap();
    assert_eq!(c.text, "ab");
    let r = m.log()[0].clone();
    assert_eq!(r.path, "/v1beta/models/gemini-x:generateContent");
    assert_eq!(r.headers["x-goog-api-key"], "sk-test");
    assert_eq!(r.body["generationConfig"]["maxOutputTokens"], 4096);
    assert_eq!(r.body["system_instruction"]["parts"][0]["text"], "SYS");
    assert_eq!(r.body["contents"][0]["parts"][0]["text"], "USER");
}

#[tokio::test]
async fn ollama_completion_and_embed_shape() {
    let h: Handler = Arc::new(|r: Recorded| {
        if r.path == "/api/embed" {
            (200, json!({"embeddings": [[0.1, 0.2], [0.3, 0.4]]}))
        } else {
            (
                200,
                json!({"message": {"content": "o"}, "done_reason": "stop"}),
            )
        }
    });
    let m = mock(h).await;
    let p = provider("ollama", &m.base, "");
    let mgr = manager();
    let c = mgr.complete(&p, "qwen", req(None)).await.unwrap();
    assert_eq!(c.text, "o");
    let e = mgr
        .embed(&p, "nomic", &["a".into(), "b".into()])
        .await
        .unwrap();
    assert_eq!(e.dimensions, Some(2));
    let log = m.log();
    assert_eq!(log[0].path, "/api/chat");
    assert_eq!(log[0].body["stream"], false);
    assert_eq!(log[0].body["options"]["num_predict"], 4096);
    assert_eq!(log[1].body, json!({"model": "nomic", "input": ["a", "b"]}));
}

#[tokio::test]
async fn openai_embed_and_count_mismatch() {
    let m = mock(fixed(
        200,
        json!({"data": [{"index": 1, "embedding": [2.0]}, {"index": 0, "embedding": [1.0]}]}),
    ))
    .await;
    let p = provider("openai-compatible", &m.base, "");
    let mgr = manager();
    let e = mgr
        .embed(&p, "bge-m3", &["x".into(), "y".into()])
        .await
        .unwrap();
    assert_eq!(e.vectors, vec![vec![1.0], vec![2.0]]);
    assert_eq!(m.log()[0].path, "/embeddings");
    let err = mgr
        .embed(&p, "bge-m3", &["x".into()])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("returned 2 embeddings for 1 inputs"), "{err}");
    let a = provider("anthropic", &m.base, "api_key_env = \"TEST_KEY\"\n");
    assert!(mgr.embed(&a, "c", &["x".into()]).await.is_err());
}

#[tokio::test]
async fn http_error_status_surfaces() {
    let m = mock(fixed(500, json!({"error": "nope"}))).await;
    let p = provider("openai-compatible", &m.base, "");
    let e = manager()
        .complete(&p, "m", req(None))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("HTTP 500"), "{e}");
}

// providerRerank.test.mjs:73 — native rerank maps by index.
#[tokio::test]
async fn native_rerank_request_and_index_mapping() {
    let m = mock(fixed(200, json!({"results": [
        {"index": 2, "relevance_score": 0.9}, {"index": 0, "relevance_score": 0.1}, {"index": 1, "relevance_score": 0.5}
    ]}))).await;
    let p = provider("openai-compatible", &m.base, "");
    let docs: Vec<String> = ["doc-zero", "doc-one", "doc-two"]
        .map(String::from)
        .to_vec();
    let r = manager()
        .rerank(&p, "bge-reranker", "q", &docs)
        .await
        .unwrap();
    let get = |i| r.iter().find(|x| x.index == i).unwrap().relevance_score;
    assert_eq!((get(0), get(1), get(2)), (0.1, 0.5, 0.9));
    let rec = m.log()[0].clone();
    assert_eq!(rec.path, "/rerank");
    assert_eq!(rec.body["documents"], json!(docs));
    assert_eq!(rec.body["query"], "q");
}

// providerRerank.test.mjs:114/220 — no native rerank routes to the completion fallback.
#[tokio::test]
async fn ollama_rerank_uses_completion_fallback() {
    let m = mock(fixed(200, json!({"message": {"content": "{\"results\":[{\"index\":0,\"relevance_score\":0.2},{\"index\":1,\"relevance_score\":0.8}]}"}}))).await;
    let p = provider("ollama", &m.base, "");
    let r = manager()
        .rerank(&p, "qwen", "q", &["a".into(), "b".into()])
        .await
        .unwrap();
    assert_eq!(r.len(), 2);
    let rec = m.log()[0].clone();
    assert_eq!(rec.path, "/api/chat");
    assert!(
        rec.body["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("[1] b")
    );

    let bad = mock(fixed(
        200,
        json!({"message": {"content": "I cannot rank these, sorry."}}),
    ))
    .await;
    let p = provider("ollama", &bad.base, "");
    let e = manager()
        .rerank(&p, "qwen", "q", &["a".into()])
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Rerank fallback response was not usable"), "{e}");
}

// providerConcurrencyLimiter.test.mjs:122/231 — a local (limit-1) provider serializes
// completions, while embed is never gated behind an in-flight completion.
#[tokio::test]
async fn local_completions_serialize_but_embed_is_exempt() {
    let gate = Arc::new(Gate::default());
    let g2 = gate.clone();
    let h: Handler = Arc::new(move |r: Recorded| {
        if r.path == "/chat/completions" {
            g2.wait();
            (200, json!({"choices": [{"message": {"content": "done"}}]}))
        } else {
            (200, json!({"data": [{"index": 0, "embedding": [1.0]}]}))
        }
    });
    let m = mock(h).await;
    let p = provider("openai-compatible", &m.base, "");
    let mgr = Arc::new(manager());
    let (m1, p1) = (mgr.clone(), p.clone());
    let first = tokio::spawn(async move { m1.complete(&p1, "m", req(None)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (m2, p2) = (mgr.clone(), p.clone());
    let second = tokio::spawn(async move { m2.complete(&p2, "m", req(None)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let chat = |m: &Mock| {
        m.log()
            .iter()
            .filter(|r| r.path == "/chat/completions")
            .count()
    };
    assert_eq!(
        chat(&m),
        1,
        "second completion must not dispatch until the first settles"
    );
    let e = tokio::time::timeout(Duration::from_secs(2), mgr.embed(&p, "bge", &["x".into()])).await;
    assert!(e.unwrap().is_ok(), "embed is not gated");
    gate.notify_one();
    first.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(chat(&m), 2);
    gate.notify_one();
    second.await.unwrap().unwrap();
}

// SR8: a non-200 body never reaches the error's Display (debug log only).
#[tokio::test]
async fn http_error_display_drops_the_body() {
    let m = mock(fixed(503, json!({"error": "secret-echo"}))).await;
    let p = provider("openai-compatible", &m.base, "");
    let e = manager().complete(&p, "m", req(None)).await.unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("HTTP 503"), "{msg}");
    assert!(!msg.contains("secret-echo"), "{msg}");
    assert_eq!(e.failure().kind, cv_providers::FailureKind::Server);
}

// SR8: a server that accepts but never answers times out per the provider's `timeout_seconds`
// and classifies as `FailureKind::Timeout`.
#[tokio::test]
async fn http_request_times_out_per_provider_timeout() {
    let never = MockHttp::fixed(Reply::hang()); // accepts, never responds
    let p = provider("openai-compatible", &never.url(), "timeout_seconds = 1\n");
    let started = std::time::Instant::now();
    let e = tokio::time::timeout(
        Duration::from_secs(10),
        manager().complete(&p, "m", req(None)),
    )
    .await
    .expect("the provider timeout fires first")
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(e.failure().kind, cv_providers::FailureKind::Timeout, "{e}");
}
