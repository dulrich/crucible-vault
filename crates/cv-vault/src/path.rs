//! Vault-relative path validation.

use crate::error::{Result, VaultError};

/// Name prefix of the writer's temp files (ignored by the watcher, never surfaced).
pub(crate) const TMP_PREFIX: &str = ".cv-tmp-";

/// Checks `path` is a clean vault-relative `/` path: non-empty, not absolute, no backslash, no
/// NUL, no empty/`.`/`..` segment, not a writer temp name, not under a cv-index skipped folder
/// (`.obsidian/`, `.git/`, `.trash/` — the daemon never writes there through the public API).
pub fn validate_rel_path(path: &str) -> Result<()> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == ".." || s.starts_with(TMP_PREFIX))
        || cv_index::is_skipped_path(path);
    if bad {
        return Err(VaultError::InvalidPath(path.to_string()));
    }
    Ok(())
}

/// The watcher/writer ignore a path whose any segment is a temp name or that is skipped.
pub(crate) fn is_ignored(path: &str) -> bool {
    cv_index::is_skipped_path(path) || path.split('/').any(|s| s.starts_with(TMP_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_paths() {
        for p in [
            "",
            "/a.md",
            "a\\b.md",
            "../a.md",
            "a/../b.md",
            "a//b.md",
            "./a.md",
            ".trash/a.md",
            ".git/x",
            ".obsidian/app.json",
            "a/.cv-tmp-x",
        ] {
            assert!(validate_rel_path(p).is_err(), "{p}");
        }
        for p in ["a.md", "f/g/h.md", "_resources/x.png", "a b.md"] {
            assert!(validate_rel_path(p).is_ok(), "{p}");
        }
    }
}
