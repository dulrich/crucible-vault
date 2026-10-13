//! A small synchronous client for the daemon socket (used by `cv uri`, `cv status`, tests).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use cv_protocol::{
    HelloParams, HelloResult, Notification, PROTOCOL_VERSION, Request, RpcError, RpcId,
    RpcNotification, RpcRequest,
};
use serde_json::Value;

/// Client failures.
#[derive(Debug)]
pub enum ClientError {
    /// Socket I/O (including "daemon not running").
    Io(std::io::Error),
    /// The daemon answered with an error.
    Rpc(RpcError),
    /// The daemon's `index_schema` is not the one this client reads.
    SchemaMismatch { expected: u32, found: u32 },
    /// Malformed traffic.
    Protocol(String),
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
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A connected, greeted client.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    pending: VecDeque<Notification>,
    /// The daemon's `hello` answer.
    pub hello: HelloResult,
}

impl Client {
    /// Connects and greets; refuses a daemon whose `index_schema != expected_index_schema`.
    pub fn connect(
        socket: &Path,
        client_name: &str,
        expected_index_schema: u32,
    ) -> Result<Self, ClientError> {
        let mut c = Self::connect_raw(socket)?;
        let v = c.call(Request::Hello(HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client: client_name.to_owned(),
        }))?;
        let hello: HelloResult =
            serde_json::from_value(v).map_err(|e| ClientError::Protocol(e.to_string()))?;
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
        let s = UnixStream::connect(socket)?;
        s.set_read_timeout(Some(Duration::from_secs(30)))?;
        Ok(Self {
            reader: BufReader::new(s.try_clone()?),
            writer: s,
            next_id: 1,
            pending: VecDeque::new(),
            hello: HelloResult {
                protocol_version: 0,
                index_schema: 0,
                mode: cv_protocol::Mode::Shadow,
                vault_root: String::new(),
                index_db: String::new(),
                state_db: String::new(),
                generation: 0,
            },
        })
    }

    /// Sends one raw line and returns the next non-notification line (tests).
    pub fn raw(&mut self, line: &str) -> Result<Value, ClientError> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.read_response(None)
    }

    /// Calls `req`; `Ok(result)` or `Err(Rpc(error))`.
    pub fn call(&mut self, req: Request) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_string(&RpcRequest::new(RpcId::Num(id), req))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        let v = self.read_response(Some(id))?;
        if let Some(e) = v.get("error") {
            let e: RpcError = serde_json::from_value(e.clone())
                .map_err(|e| ClientError::Protocol(e.to_string()))?;
            return Err(ClientError::Rpc(e));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    fn read_line(&mut self) -> Result<Value, ClientError> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(ClientError::Protocol("daemon closed the connection".into()));
        }
        serde_json::from_str(&line).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    fn read_response(&mut self, id: Option<u64>) -> Result<Value, ClientError> {
        loop {
            let v = self.read_line()?;
            if v.get("id").is_none() {
                let n: RpcNotification =
                    serde_json::from_value(v).map_err(|e| ClientError::Protocol(e.to_string()))?;
                self.pending.push_back(n.event);
                continue;
            }
            match (id, v.get("id").and_then(Value::as_u64)) {
                (Some(want), Some(got)) if want != got => {
                    return Err(ClientError::Protocol(format!(
                        "response id {got} != {want}"
                    )));
                }
                _ => return Ok(v),
            }
        }
    }

    /// The next notification, waiting at most `timeout`; `Ok(None)` on timeout.
    pub fn next_notification(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Notification>, ClientError> {
        if let Some(n) = self.pending.pop_front() {
            return Ok(Some(n));
        }
        self.reader.get_ref().set_read_timeout(Some(timeout))?;
        let res = self.read_line();
        self.reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(30)))?;
        match res {
            Ok(v) => {
                let n: RpcNotification =
                    serde_json::from_value(v).map_err(|e| ClientError::Protocol(e.to_string()))?;
                Ok(Some(n.event))
            }
            Err(ClientError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
