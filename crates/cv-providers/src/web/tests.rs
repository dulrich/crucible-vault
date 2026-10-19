//! `web` tests against a minimal local `tokio` HTTP responder (no new dev-dependency).

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves canned responses by path: `/ok`, `/echo` (returns the request body), `/status/<n>`,
/// `/retry` (429 + `Retry-After: 7`), `/slow` (sleeps 2 s).
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, len) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (p + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                let path = path.split('?').next().unwrap_or("/").to_owned();
                let req_body = buf[head_end..].to_vec();
                let (status, extra, body): (u16, &str, Vec<u8>) = match path.as_str() {
                    "/ok" => (200, "", br#"{"a":1}"#.to_vec()),
                    "/echo" => (200, "", req_body),
                    "/retry" => (429, "Retry-After: 7\r\n", Vec::new()),
                    "/slow" => {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        (200, "", Vec::new())
                    }
                    p => (
                        p.strip_prefix("/status/")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(500),
                        "",
                        b"quotaExceeded".to_vec(),
                    ),
                };
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

fn client() -> WebClient {
    WebClient::new(WebConfig::default()).unwrap()
}

#[tokio::test]
async fn get_and_post_json_round_trip() {
    let base = serve().await;
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
    let base = serve().await;
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
    let base = serve().await;
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
    let base = serve().await;
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
