//! The synchronous daemon client for `cv uri`, `cv status`, `cv jobs` and tests is the shared
//! [`cv_protocol::client::RpcClient`]: one framing/handshake/error-decode implementation, also
//! used by the editor.

pub use cv_protocol::client::{ClientError, RpcClient as Client};
