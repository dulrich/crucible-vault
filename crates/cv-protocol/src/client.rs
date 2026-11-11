//! The one blocking daemon client,
//! shared by `cv` (CLI, tests) and the editor: std `UnixStream` only, no async runtime. A reader
//! thread owns every socket read and demultiplexes responses (by id) from notifications; callers
//! block on a per-call channel. Dropping the client shuts the socket down, ending the thread.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::methods::{self, Method};
use crate::{
    HelloParams, HelloResult, Notification, PROTOCOL_VERSION, Request, RpcError, RpcId,
    RpcNotification, RpcRequest, error_codes,
};

/// How long a call waits for its reply.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Client failures.
#[derive(Debug)]
pub enum ClientError {
    /// Socket I/O (including "daemon not running").
    Io(std::io::Error),
    /// The daemon answered with an error.
    Rpc(RpcError),
    /// The daemon's `index_schema` is not the one this client reads.
    SchemaMismatch { expected: u32, found: u32 },
    /// Malformed traffic or a timed-out call.
    Protocol(String),
    /// The connection is gone.
    Disconnected,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "socket: {e}"),
            Self::Rpc(e) => write!(f, "daemon error {}: {}", e.code, e.message),
            Self::SchemaMismatch { expected, found } => write!(
                f,
                "index_schema mismatch: client reads {expected}, daemon has {found}"
            ),
            Self::Protocol(m) => write!(f, "protocol: {m}"),
            Self::Disconnected => write!(f, "daemon disconnected"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

type Reply = Result<Value, RpcError>;

#[derive(Debug, Default)]
struct Inbox {
    pending: Mutex<HashMap<u64, Sender<Reply>>>,
    subscriber: Mutex<Option<Sender<Notification>>>,
    /// Responses no call waits for (raw lines, null ids).
    unmatched: Mutex<Option<Sender<Value>>>,
}

/// A connected daemon client (greeted unless built with [`RpcClient::connect_raw`]).
#[derive(Debug)]
pub struct RpcClient {
    writer: Mutex<UnixStream>,
    inbox: Arc<Inbox>,
    next_id: AtomicU64,
    /// The default notification feed ([`Self::next_notification`]).
    feed: Mutex<Receiver<Notification>>,
    /// The daemon's `hello` answer.
    pub hello: HelloResult,
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        if let Ok(w) = self.writer.lock() {
            let _ = w.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl RpcClient {
    /// Connects and greets; refuses a daemon whose `index_schema != expected_index_schema`.
    pub fn connect(
        socket: &Path,
        client_name: &str,
        expected_index_schema: u32,
    ) -> Result<Self, ClientError> {
        let mut c = Self::connect_raw(socket)?;
        let hello = c.call::<methods::Hello>(HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client: client_name.to_owned(),
        })?;
        if hello.index_schema != expected_index_schema {
            return Err(ClientError::SchemaMismatch {
                expected: expected_index_schema,
                found: hello.index_schema,
            });
        }
        c.hello = hello;
        Ok(c)
    }

    /// Connects without greeting (tests of the handshake).
    pub fn connect_raw(socket: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket)?;
        let reader = BufReader::new(stream.try_clone()?);
        let (tx, feed) = mpsc::channel();
        let inbox = Arc::new(Inbox {
            subscriber: Mutex::new(Some(tx)),
            ..Inbox::default()
        });
        let rinbox = Arc::clone(&inbox);
        std::thread::Builder::new()
            .name("cv-rpc-reader".into())
            .spawn(move || read_loop(reader, &rinbox))?;
        Ok(Self {
            writer: Mutex::new(stream),
            inbox,
            next_id: AtomicU64::new(1),
            feed: Mutex::new(feed),
            hello: HelloResult::default(),
        })
    }

    /// Calls method `M`.
    pub fn call<M: Method>(&self, params: M::Params) -> Result<M::Result, ClientError> {
        let v = self.request(M::request(params))?;
        serde_json::from_value(v).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// Sends `req` and returns the raw `result` (untyped; tests and generic callers).
    pub fn request(&self, req: Request) -> Result<Value, ClientError> {
        self.request_within(req, CALL_TIMEOUT)
    }

    /// Calls method `M`, waiting at most `timeout` for the reply (a long daemon-side wait such
    /// as `reconcile_intake`).
    pub fn call_within<M: Method>(
        &self,
        params: M::Params,
        timeout: Duration,
    ) -> Result<M::Result, ClientError> {
        let v = self.request_within(M::request(params), timeout)?;
        serde_json::from_value(v).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    fn request_within(&self, req: Request, timeout: Duration) -> Result<Value, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&self.inbox.pending)?.insert(id, tx);
        let line = serde_json::to_string(&RpcRequest::new(RpcId::Num(id), req))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        self.send(&line)?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(ClientError::Rpc(e)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.inbox.pending)?.remove(&id);
                Err(ClientError::Protocol(format!("call {id} timed out")))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ClientError::Disconnected),
        }
    }

    /// Sends one raw line and returns the next response no call waits for (tests).
    pub fn raw(&self, line: &str) -> Result<Value, ClientError> {
        let (tx, rx) = mpsc::channel();
        *lock(&self.inbox.unmatched)? = Some(tx);
        self.send(line)?;
        rx.recv_timeout(CALL_TIMEOUT)
            .map_err(|_| ClientError::Disconnected)
    }

    /// Routes notifications to a new channel (replacing an earlier one); it closes when the
    /// connection drops.
    pub fn notifications(&self) -> Result<Receiver<Notification>, ClientError> {
        let (tx, rx) = mpsc::channel();
        *lock(&self.inbox.subscriber)? = Some(tx);
        Ok(rx)
    }

    /// The next notification on the default feed, waiting at most `timeout`; `Ok(None)` on
    /// timeout (empty once [`Self::notifications`] took the feed over).
    pub fn next_notification(
        &self,
        timeout: Duration,
    ) -> Result<Option<Notification>, ClientError> {
        match lock(&self.feed)?.recv_timeout(timeout) {
            Ok(n) => Ok(Some(n)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ClientError::Disconnected),
        }
    }

    fn send(&self, line: &str) -> Result<(), ClientError> {
        let mut w = lock(&self.writer)?;
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, ClientError> {
    m.lock().map_err(|_| ClientError::Disconnected)
}

/// A reply line buffer larger than this is dropped after use, so one big reply's allocation is
/// not held for the connection's life (typical lines are far smaller).
const LINE_BUF_CAP: usize = 1 << 20;

/// Empties `line`, releasing its allocation when it grew past [`LINE_BUF_CAP`].
fn reset_line(line: &mut String) {
    if line.capacity() > LINE_BUF_CAP {
        *line = String::new();
    } else {
        line.clear();
    }
}

fn read_loop(mut reader: BufReader<UnixStream>, inbox: &Inbox) {
    let mut line = String::new();
    loop {
        reset_line(&mut line);
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // A malformed line is skipped (this crate carries no logger).
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("id").is_none() {
            if let Ok(n) = serde_json::from_value::<RpcNotification>(v)
                && let Ok(mut s) = inbox.subscriber.lock()
                && let Some(tx) = s.as_ref()
                && tx.send(n.event).is_err()
            {
                *s = None;
            }
            continue;
        }
        let waiter = v
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| inbox.pending.lock().ok().and_then(|mut p| p.remove(&id)));
        match waiter {
            Some(tx) => {
                // Take `error`/`result` out of the parsed message: the payload moves, no copy.
                let mut fields = match v {
                    Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                let reply = match fields.remove("error") {
                    Some(e) => Err(serde_json::from_value(e).unwrap_or(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: "unparseable error".into(),
                        data: None,
                    })),
                    None => Ok(fields.remove("result").unwrap_or(Value::Null)),
                };
                let _ = tx.send(reply);
            }
            None => {
                if let Ok(u) = inbox.unmatched.lock()
                    && let Some(tx) = u.as_ref()
                {
                    let _ = tx.send(v);
                }
            }
        }
    }
    // Connection gone: fail every waiting call and close the notification channel.
    if let Ok(mut p) = inbox.pending.lock() {
        p.clear();
    }
    if let Ok(mut s) = inbox.subscriber.lock() {
        *s = None;
    }
    if let Ok(mut u) = inbox.unmatched.lock() {
        *u = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_line_releases_only_oversized_buffers() {
        let mut small = String::with_capacity(4096);
        small.push_str("abc");
        reset_line(&mut small);
        assert!(small.is_empty() && small.capacity() >= 4096);
        let mut big = "x".repeat(LINE_BUF_CAP + 1);
        reset_line(&mut big);
        assert!(big.is_empty() && big.capacity() < LINE_BUF_CAP);
    }

    #[test]
    fn read_loop_delivers_a_multi_mib_reply_and_an_error() {
        let (mut server, client_end) = UnixStream::pair().expect("pair");
        let inbox = Arc::new(Inbox::default());
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        {
            let mut p = inbox.pending.lock().expect("lock");
            p.insert(1, tx1);
            p.insert(2, tx2);
        }
        let rinbox = Arc::clone(&inbox);
        let reader = std::thread::spawn(move || read_loop(BufReader::new(client_end), &rinbox));
        let blob = "y".repeat(4 << 20);
        let reply = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"blob":"{blob}"}}}}"#);
        server.write_all(reply.as_bytes()).expect("write");
        server.write_all(b"\n").expect("write");
        let err = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"nope"}}"#;
        server.write_all(err.as_bytes()).expect("write");
        server.write_all(b"\n").expect("write");

        let v = rx1
            .recv_timeout(Duration::from_secs(10))
            .expect("reply")
            .expect("ok result");
        assert_eq!(v["blob"].as_str().map(str::len), Some(4 << 20));
        let e = rx2
            .recv_timeout(Duration::from_secs(10))
            .expect("reply")
            .expect_err("error reply");
        assert_eq!(e.message, "nope");
        drop(server);
        reader.join().expect("reader exits on EOF");
    }
}
