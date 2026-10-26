//! `vault.toml` schema — `VaultConfig`, `crates/cv-actions/src/vault.rs` (plan
//! tidiness-trash-orphans WP-1).

use super::*;

/// `[trash]`.
pub static VAULT_TRASH: Record = Record {
    name: "trash",
    fields: &[
        field(
            "location",
            "Location",
            "Where recoverable deletes go: the vault's own .trash/ folder, or the system (desktop) trash",
            Node::Enum(&["vault", "system"]),
        )
        .dflt(D::Str("vault")),
        field(
            "retention_days",
            "Retention (days)",
            "Trashed files are deleted permanently after this many days; 0 = never",
            UINT,
        )
        .dflt(D::Int(30)),
    ],
    rules: &[],
    summary: None,
};

/// `vault.toml`.
pub static VAULT_FILE: Record = Record {
    name: "vault.toml",
    fields: &[field(
        "trash",
        "Trash",
        "Trash location and retention",
        Node::Record(&VAULT_TRASH),
    )],
    rules: &[],
    summary: None,
};
