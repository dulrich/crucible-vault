//! `web` tests against the shared std-only `cv_testkit::MockHttp` responder.

use super::*;
use cv_testkit::{MockHttp, Reply};

/// Serves canned responses by path: `/ok`, `/echo` (returns the request body), `/status/<n>`,
/// `/retry` (429 + `Retry-After: 7`), `/slow` (answers after 2 s).
fn serve() -> MockHttp {
    MockHttp::start(|req| match req.path() {
        "/ok" => Reply::new(200, br#"{"a":1}"#.to_vec()),
        "/echo" => Reply::new(200, req.body.clone()),
        "/retry" => Reply::new(429, Vec::new()).header("Retry-After", "7"),
        "/slow" => Reply::new(200, Vec::new()).delay(Duration::from_secs(2)),
        p => Reply::new(
            p.strip_prefix("/status/")
                .and_then(|s| s.parse().ok())
                .unwrap_or(500),
            b"quotaExceeded".to_vec(),
        ),
    })
}

fn client() -> WebClient {
    WebClient::new(WebConfig::default()).unwrap()
}

#[tokio::test]
async fn get_and_post_json_round_trip() {
    let mock = serve();
    let base = mock.url();
    let c = client();
    let v: serde_json::Value = c.get_json("svc", &format!("{base}/ok")).await.unwrap();
    assert_eq!(v["a"], 1);
    assert_eq!(
        c.get_text("svc", &format!("{base}/ok")).await.unwrap(),
        r#"{"a":1}"#
    );
    let echoed: serde_json::Value = c
        .post_json(
            "svc",
            &format!("{base}/echo"),
            &serde_json::json!({"k": "v"}),
        )
        .await
        .unwrap();
    assert_eq!(echoed["k"], "v");
}

#[tokio::test]
async fn statuses_classify_and_never_leak_the_url() {
    let mock = serve();
    let base = mock.url();
    let c = client();
    let get = |p: &str| format!("{base}{p}?key=SECRET");
    let e = c.get_bytes("yt", &get("/status/404")).await.unwrap_err();
    assert_eq!(
        e,
        FetchError::NotFound {
            service: "yt".into(),
            status: 404
        }
    );
    let e = c.get_bytes("yt", &get("/status/410")).await.unwrap_err();
    assert!(matches!(e, FetchError::NotFound { status: 410, .. }));
    let e = c.get_bytes("yt", &get("/status/400")).await.unwrap_err();
    assert_eq!(
        e,
        FetchError::Permanent {
            service: "yt".into(),
            status: 400
        }
    );
    let e = c.get_bytes("yt", &get("/status/503")).await.unwrap_err();
    assert!(matches!(
        e,
        FetchError::Transient {
            retry_after: None,
            ..
        }
    ));
    let e = c.get_bytes("yt", &get("/status/408")).await.unwrap_err();
    assert!(matches!(e, FetchError::Transient { .. }));
    let e = c.get_bytes("yt", &get("/retry")).await.unwrap_err();
    assert_eq!(
        e,
        FetchError::Transient {
            service: "yt".into(),
            retry_after: Some(Duration::from_secs(7)),
            kind: crate::failure::FailureKind::RateLimited,
            detail: format!("HTTP {}", 429)
        }
    );
    // Network failure (nothing listens on port 1).
    let e = c
        .get_bytes("yt", "http://127.0.0.1:1/x?key=SECRET")
        .await
        .unwrap_err();
    assert!(matches!(e, FetchError::Transient { .. }), "{e:?}");
    assert!(!format!("{e} {e:?}").contains("SECRET"));
    assert_eq!(
        redact_url("https://h.example/p?key=SECRET#f"),
        "https://h.example/p"
    );
}

#[tokio::test]
async fn classifier_overrides_and_timeout_is_transient() {
    let mock = serve();
    let base = mock.url();
    let c = client();
    let mut req = FetchRequest::get("youtube", "");
    let url = format!("{base}/status/403");
    req.url = &url;
    req.classify = Some(|service, status, retry_after, body| {
        (status == 403 && body.windows(5).any(|w| w == b"quota")).then(|| FetchError::Quota {
            service: service.to_owned(),
            retry_after,
        })
    });
    assert!(matches!(
        c.fetch(req).await.unwrap_err(),
        FetchError::Quota { .. }
    ));
    let slow = format!("{base}/slow");
    let mut req = FetchRequest::get("svc", &slow);
    req.timeout = Some(Duration::from_millis(200));
    let e = c.fetch(req).await.unwrap_err();
    assert_eq!(
        e,
        FetchError::Transient {
            service: "svc".into(),
            retry_after: None,
            kind: crate::failure::FailureKind::Timeout,
            detail: "timeout".into()
        }
    );
}

#[tokio::test]
async fn per_host_interval_spaces_requests() {
    let mock = serve();
    let base = mock.url();
    let c = WebClient::new(WebConfig {
        min_host_interval: Duration::from_millis(150),
        ..WebConfig::default()
    })
    .unwrap();
    let t = Instant::now();
    for _ in 0..3 {
        c.get_bytes("svc", &format!("{base}/ok")).await.unwrap();
    }
    assert!(
        t.elapsed() >= Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn retry_after_parses_seconds_and_http_dates() {
    let now = UNIX_EPOCH + Duration::from_secs(784_111_777); // Sun, 06 Nov 1994 08:49:37 GMT
    assert_eq!(
        parse_retry_after("120", now),
        Some(Duration::from_secs(120))
    );
    assert_eq!(
        parse_retry_after("Sun, 06 Nov 1994 08:50:37 GMT", now),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        parse_retry_after("Sun, 06 Nov 1994 08:00:00 GMT", now),
        Some(Duration::ZERO)
    );
    assert_eq!(parse_retry_after("soon", now), None);
}
