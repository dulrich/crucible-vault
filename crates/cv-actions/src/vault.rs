//! Typed `vault.toml` (plan tidiness-trash-orphans WP-1): `[trash] location`, `retention_days`.
//!
//! New, not carried from Crucible: Obsidian owned the trash there (`.trash/` or the system trash
//! by its own setting); cv owns the vault, so the choice and the retention live here.

use serde::Deserialize;

/// Where a recoverable delete moves the file (the one protocol definition, WP-R6).
pub use cv_protocol::TrashLocation;

/// `[trash]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrashConfig {
    /// Destination of recoverable deletes.
    pub location: TrashLocation,
    /// Days until a trashed file is deleted permanently; `0` = never.
    pub retention_days: u32,
}

impl Default for TrashConfig {
    fn default() -> Self {
        Self {
            location: TrashLocation::Vault,
            retention_days: 30,
        }
    }
}

/// `vault.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct VaultConfig {
    /// `[trash]`.
    pub trash: TrashConfig,
}

impl VaultConfig {
    /// Parses and checks a `vault.toml` table (an unknown `location` is an error, never coerced).
    pub fn from_table(t: &toml::Table) -> Result<Self, String> {
        toml::Value::Table(t.clone())
            .try_into()
            .map_err(|e: toml::de::Error| e.message().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<VaultConfig, String> {
        VaultConfig::from_table(&s.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn absent_is_defaults() {
        let c = parse("").unwrap();
        assert_eq!(c.trash.location, TrashLocation::Vault);
        assert_eq!(c.trash.retention_days, 30);
    }

    #[test]
    fn valid_values_parse() {
        let c = parse("[trash]\nlocation = \"system\"\nretention_days = 0\n").unwrap();
        assert_eq!(c.trash.location, TrashLocation::System);
        assert_eq!(c.trash.retention_days, 0);
    }

    #[test]
    fn unknown_location_and_negative_days_rejected() {
        assert!(parse("[trash]\nlocation = \"bin\"\n").is_err());
        assert!(parse("[trash]\nretention_days = -1\n").is_err());
        assert!(parse("[trash]\nextra = 1\n").is_err());
    }
}
