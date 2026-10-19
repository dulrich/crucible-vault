//! RPC error mapping: every vault/state error → its JSON-RPC code.

use super::*;

/// An RPC failure (code + message + optional data).
pub fn rpc_err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        data: None,
    }
}

pub(super) fn internal(e: impl std::fmt::Display) -> RpcError {
    rpc_err(error_codes::INTERNAL_ERROR, e.to_string())
}

/// Every [`VaultError`] → its RPC code (exhaustive by design: a new variant must pick one).
pub(crate) fn rpc_from_vault(e: VaultError) -> RpcError {
    use error_codes as c;
    match e {
        VaultError::Conflict { found, .. } => RpcError {
            code: c::CONFLICT,
            message: "content hash conflict".into(),
            data: Some(json!({ "current_hash": found.map(|h| h.as_str().to_owned()) })),
        },
        e @ VaultError::NotFound(_) => rpc_err(c::NOT_FOUND, e.to_string()),
        e @ (VaultError::InvalidPath(_) | VaultError::Exists(_)) => {
            rpc_err(c::INVALID_PARAMS, e.to_string())
        }
        e @ (VaultError::ProtectedRoot { .. } | VaultError::LinkWriteRefused { .. }) => {
            rpc_err(c::REFUSED, e.to_string())
        }
        e @ VaultError::WriterGone => rpc_err(c::UNAVAILABLE, e.to_string()),
        e @ (VaultError::Io { .. }
        | VaultError::Index(_)
        | VaultError::Watch(_)
        | VaultError::Json(_)) => internal(e),
    }
}

/// Every [`StateError`] → its RPC code (exhaustive by design).
pub(crate) fn rpc_from_state(e: StateError) -> RpcError {
    use error_codes as c;
    match e {
        StateError::NoSuchJob(id) => rpc_err(c::NOT_FOUND, format!("no job {id}")),
        e @ StateError::WrongStatus { .. } => rpc_err(c::INVALID_PARAMS, e.to_string()),
        e @ (StateError::Sqlite(_)
        | StateError::Json(_)
        | StateError::NewerSchema { .. }
        | StateError::Corrupt { .. }) => internal(e),
    }
}

pub(super) fn unavailable(m: &str) -> RpcError {
    rpc_err(error_codes::UNAVAILABLE, m)
}

/// A writer-job failure: the vault's, or a refusal of the request's own params.
pub(crate) enum JobError {
    Vault(VaultError),
    Invalid(String),
}

impl From<VaultError> for JobError {
    fn from(e: VaultError) -> Self {
        Self::Vault(e)
    }
}

/// Serialises method `M`'s typed result (ties each handler's return type to the contract).
pub(super) fn reply<M: Method>(r: Result<M::Result, RpcError>) -> Result<Value, RpcError> {
    serde_json::to_value(r?).map_err(internal)
}

#[cfg(test)]
mod error_map_tests {
    use super::*;

    /// WP-R3 (B2): policy refusals and unavailability get their own codes, not INTERNAL_ERROR.
    #[test]
    fn vault_and_state_errors_map_to_their_codes() {
        use error_codes as c;
        let s = |s: &str| s.to_owned();
        let cases = [
            (
                VaultError::LinkWriteRefused {
                    path: s("a.md"),
                    reason: s("r"),
                },
                c::REFUSED,
            ),
            (
                VaultError::ProtectedRoot {
                    root: s("r"),
                    protected: s("p"),
                },
                c::REFUSED,
            ),
            (VaultError::WriterGone, c::UNAVAILABLE),
            (VaultError::NotFound(s("a.md")), c::NOT_FOUND),
            (VaultError::Exists(s("a.md")), c::INVALID_PARAMS),
            (VaultError::InvalidPath(s("../a")), c::INVALID_PARAMS),
        ];
        for (e, code) in cases {
            let msg = e.to_string();
            let r = rpc_from_vault(e);
            assert_eq!(r.code, code, "{msg}");
        }
        let wrong = StateError::WrongStatus {
            id: s("j"),
            status: cv_state::JobStatus::Running,
            expected: "queued",
        };
        let r = rpc_from_state(wrong);
        assert_eq!(
            (r.code, r.message.as_str()),
            (c::INVALID_PARAMS, "job j is running, expected queued")
        );
        assert_eq!(
            rpc_from_state(StateError::NoSuchJob(s("j"))).message,
            "no job j"
        );
        let conflict = rpc_from_vault(VaultError::Conflict {
            path: s("a"),
            expected: None,
            found: None,
        });
        assert_eq!(conflict.code, c::CONFLICT);
        assert_eq!(conflict.data, Some(json!({ "current_hash": null })));
    }
}
