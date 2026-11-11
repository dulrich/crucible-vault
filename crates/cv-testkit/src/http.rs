//! A scripted, std-only HTTP/1.1 mock server on `127.0.0.1:0`:
//! one thread accepts, one thread per connection answers with the handler's
//! [`Reply`] and closes. Every request is recorded. No async runtime, no network beyond loopback.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A held, never-listening loopback reservation: connections to [`DeadPort::port`] are refused,
/// and no other process can draw the number with `bind(0)` while the guard lives. Bind it to a
/// named variable (`let dead = DeadPort::new();`) that outlives every request in the test.
pub struct DeadPort {
    _sock: socket2::Socket,
    port: u16,
}

impl DeadPort {
    /// Binds `127.0.0.1:0` with `SO_REUSEADDR` and never calls `listen`.
    pub fn new() -> Self {
        use socket2::{Domain, Protocol, Socket, Type};
        let sock = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).expect("socket");
        sock.set_reuse_address(true).expect("SO_REUSEADDR");
        let addr: SocketAddr = "127.0.0.1:0".parse().expect("loopback addr");
        sock.bind(&addr.into()).expect("bind 127.0.0.1:0");
        let port = sock
            .local_addr()
            .expect("local addr")
            .as_socket()
            .expect("inet addr")
            .port();
        Self { _sock: sock, port }
    }

    /// The reserved port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Default for DeadPort {
    fn default() -> Self {
        Self::new()
    }
}

/// One recorded request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// `GET`, `POST`, …
    pub method: String,
    /// The request target, query included (`/v1/x?a=1`).
    pub target: String,
    /// Header `(name, value)` pairs; names lower-cased.
    pub headers: Vec<(String, String)>,
    /// The body (`Content-Length` bytes).
    pub body: Vec<u8>,
}

impl Request {
    /// The target without its query string.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }
    /// The query string (after `?`), if any.
    pub fn query(&self) -> Option<&str> {
        self.target.split_once('?').map(|(_, q)| q)
    }
    /// The first header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
    /// The body as (lossy) UTF-8.
    pub fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One scripted response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// HTTP status code.
    pub status: u16,
    /// Extra headers (`Content-Length` and `Connection: close` are always sent).
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
    /// Sleep this long before answering.
    pub delay: Option<Duration>,
    /// Never answer: hold the connection open until the mock is dropped (timeouts).
    pub hang: bool,
}

impl Reply {
    /// `status` with `body` and no extra headers.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            delay: None,
            hang: false,
        }
    }
    /// `status` with `body` as `Content-Type: application/json`.
    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::new(status, body).header("Content-Type", "application/json")
    }
    /// A reply that never comes (the client times out).
    pub fn hang() -> Self {
        Self {
            hang: true,
            ..Self::new(200, Vec::new())
        }
    }
    /// Adds a header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
    /// Answers after `d`.
    #[must_use]
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }
}

type Handler = dyn Fn(&Request) -> Reply + Send + Sync + 'static;

/// The running mock; stops accepting when dropped.
pub struct MockHttp {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
}

impl MockHttp {
    /// Serves every request with `handler` (route on [`Request::path`]).
    pub fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let addr = listener.local_addr().expect("local addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let (reqs, halt) = (Arc::clone(&requests), Arc::clone(&stop));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if halt.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(sock) = conn else { continue };
                let (h, r, s) = (Arc::clone(&handler), Arc::clone(&reqs), Arc::clone(&halt));
                std::thread::spawn(move || serve_conn(sock, &*h, &r, &s));
            }
        });
        Self {
            addr,
            requests,
            stop,
        }
    }

    /// Answers the n-th request with `replies[n]`; the last reply repeats.
    pub fn script(replies: Vec<Reply>) -> Self {
        assert!(!replies.is_empty(), "MockHttp::script needs a reply");
        let n = AtomicUsize::new(0);
        Self::start(move |_| {
            let i = n.fetch_add(1, Ordering::SeqCst).min(replies.len() - 1);
            replies[i].clone()
        })
    }

    /// Answers with the first route whose prefix starts the request target (path + query);
    /// anything else is an empty 404.
    pub fn routes<P: Into<String>>(routes: impl IntoIterator<Item = (P, Reply)>) -> Self {
        let routes: Vec<(String, Reply)> = routes.into_iter().map(|(p, r)| (p.into(), r)).collect();
        Self::start(move |req| {
            routes
                .iter()
                .find(|(p, _)| req.target.starts_with(p.as_str()))
                .map_or_else(|| Reply::new(404, Vec::new()), |(_, r)| r.clone())
        })
    }

    /// Answers every request with `reply`.
    pub fn fixed(reply: Reply) -> Self {
        Self::start(move |_| reply.clone())
    }

    /// The base URL, `http://127.0.0.1:<port>` (no trailing slash).
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The bound address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests lock").clone()
    }

    /// How many requests arrived so far.
    pub fn hits(&self) -> usize {
        self.requests.lock().expect("requests lock").len()
    }
}

impl Drop for MockHttp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it sees the flag.
        let _ = TcpStream::connect(self.addr);
    }
}

fn read_request(sock: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = sock.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next().unwrap_or("/").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = buf.len().min(head_end + len);
    Some(Request {
        method,
        target,
        headers,
        body: buf[head_end..end].to_vec(),
    })
}

fn serve_conn(
    mut sock: TcpStream,
    handler: &Handler,
    requests: &Mutex<Vec<Request>>,
    stop: &AtomicBool,
) {
    let Some(req) = read_request(&mut sock) else {
        return;
    };
    requests.lock().expect("requests lock").push(req.clone());
    let reply = handler(&req);
    if reply.hang {
        for _ in 0..3000 {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        return;
    }
    if let Some(d) = reply.delay {
        std::thread::sleep(d);
    }
    let mut head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = sock.write_all(head.as_bytes());
    let _ = sock.write_all(&reply.body);
    let _ = sock.flush();
    let _ = sock.shutdown(std::net::Shutdown::Write);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(url: &str, path: &str, body: &str) -> String {
        let addr = url.trim_start_matches("http://");
        let mut s = TcpStream::connect(addr).unwrap();
        write!(
            s,
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn scripts_replies_and_records_requests() {
        let m = MockHttp::script(vec![
            Reply::new(201, "a").header("X-K", "v"),
            Reply::new(500, "b"),
        ]);
        let r1 = get(&m.url(), "/p?q=1", "hi");
        assert!(r1.starts_with("HTTP/1.1 201") && r1.contains("X-K: v") && r1.ends_with("a"));
        assert!(get(&m.url(), "/p", "").ends_with("b"));
        assert!(get(&m.url(), "/p", "").starts_with("HTTP/1.1 500"));
        let reqs = m.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!((reqs[0].path(), reqs[0].query()), ("/p", Some("q=1")));
        assert_eq!(reqs[0].body_str(), "hi");
        assert_eq!(reqs[0].header("host"), Some("x"));
    }
}
