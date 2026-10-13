//! Vault-wide property rename/remove — Crucible `renamePropertyInVault` /
//! `removePropertyFromVault` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/lint.ts:520-640`,
//! `src/ingestion/AGENTS.md:26`).
//!
//! Expressed as a caller-supplied list of matching paths (the index query lives in `cv-index`;
//! this crate does not depend on it) plus a pure per-file edit, [`PropertyOp::apply`]. Dropped
//! (Obsidian-induced): the `withMaterializing` wrapper and the per-file `updateFrontmatter`
//! write barrier; progress notices are the client's.

use ofm::frontmatter::JsValue;
use thiserror::Error;

use crate::frontmatter::{FmError, FrontmatterDoc};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PropertyOpError {
    #[error("both old and new key names are required")]
    MissingKey,
    #[error("old and new keys are identical")]
    SameKey,
}

/// One vault-wide property edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyOp {
    /// Move `old`'s value to `new`; an existing non-empty `new` wins and `old` is just dropped.
    Rename {
        old: String,
        new: String,
    },
    Remove {
        key: String,
    },
}

impl PropertyOp {
    /// Trim and validate the key names (Crucible's prompt-side checks).
    pub fn validated(self) -> Result<Self, PropertyOpError> {
        match self {
            Self::Rename { old, new } => {
                let (old, new) = (old.trim().to_owned(), new.trim().to_owned());
                if old.is_empty() || new.is_empty() {
                    return Err(PropertyOpError::MissingKey);
                }
                if old == new {
                    return Err(PropertyOpError::SameKey);
                }
                Ok(Self::Rename { old, new })
            }
            Self::Remove { key } => {
                let key = key.trim().to_owned();
                if key.is_empty() {
                    return Err(PropertyOpError::MissingKey);
                }
                Ok(Self::Remove { key })
            }
        }
    }

    /// Apply to one note's text: `Some(new text)` when it changed, `None` when untouched.
    pub fn apply(&self, text: &str) -> Result<Option<String>, FmError> {
        let mut doc = FrontmatterDoc::new(text);
        let changed = match self {
            Self::Remove { key } => doc.remove(key)?,
            Self::Rename { old, new } => {
                let Some(value) = doc.get(old)? else {
                    return Ok(None);
                };
                match doc.get(new)? {
                    None => doc.rename_key(old, new)?,
                    Some(JsValue::Null) => {
                        doc.remove(old)?;
                        doc.set(new, &value)?
                    }
                    Some(JsValue::Str(s)) if s.is_empty() => {
                        doc.remove(old)?;
                        doc.set(new, &value)?
                    }
                    Some(_) => doc.remove(old)?,
                }
            }
        };
        Ok(changed.then(|| doc.into_text()))
    }
}

/// Plan the edits for `paths` (already filtered by the caller's index query and exclusions):
/// `read` yields each note's text; returns the `(path, new text)` writes and per-path errors.
pub fn plan_vault_edits<P: Clone, E>(
    paths: &[P],
    op: &PropertyOp,
    mut read: impl FnMut(&P) -> Result<String, E>,
) -> VaultEditPlan<P, E> {
    let mut writes = Vec::new();
    let mut failures = Vec::new();
    for p in paths {
        match read(p) {
            Err(e) => failures.push((p.clone(), VaultEditError::Read(e))),
            Ok(text) => match op.apply(&text) {
                Ok(Some(t)) => writes.push((p.clone(), t)),
                Ok(None) => {}
                Err(e) => failures.push((p.clone(), VaultEditError::Frontmatter(e))),
            },
        }
    }
    VaultEditPlan { writes, failures }
}

/// The outcome of [`plan_vault_edits`]: writes to submit, and per-path failures.
#[derive(Debug)]
pub struct VaultEditPlan<P, E> {
    pub writes: Vec<(P, String)>,
    pub failures: Vec<(P, VaultEditError<E>)>,
}

#[derive(Debug)]
pub enum VaultEditError<E> {
    Read(E),
    Frontmatter(FmError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(o: &str, n: &str) -> PropertyOp {
        PropertyOp::Rename {
            old: o.into(),
            new: n.into(),
        }
        .validated()
        .unwrap()
    }

    #[test]
    fn validation() {
        assert_eq!(
            PropertyOp::Rename {
                old: " a ".into(),
                new: "a".into()
            }
            .validated(),
            Err(PropertyOpError::SameKey)
        );
        assert_eq!(
            PropertyOp::Remove { key: " ".into() }.validated(),
            Err(PropertyOpError::MissingKey)
        );
    }

    #[test]
    fn rename_variants() {
        let op = rename("video_ids", "yt-video-ids");
        assert_eq!(
            op.apply("---\nvideo_ids:\n  - a\nz: 1\n---\nB")
                .unwrap()
                .as_deref(),
            Some("---\nyt-video-ids:\n  - a\nz: 1\n---\nB")
        );
        assert_eq!(
            op.apply("---\nvideo_ids: a\nyt-video-ids:\n---\n")
                .unwrap()
                .as_deref(),
            Some("---\nyt-video-ids: a\n---\n")
        );
        assert_eq!(
            op.apply("---\nvideo_ids: a\nyt-video-ids: b\n---\n")
                .unwrap()
                .as_deref(),
            Some("---\nyt-video-ids: b\n---\n")
        );
        assert_eq!(op.apply("---\nz: 1\n---\n").unwrap(), None);
        assert_eq!(op.apply("no fm").unwrap(), None);
    }

    #[test]
    fn plan_over_paths() {
        let notes = [
            ("a.md", "---\nk: 1\n---\n"),
            ("b.md", "---\nj: 1\n---\n"),
            ("c.md", "---\n{x: 1}\n---\n"),
        ];
        let paths: Vec<&str> = notes.iter().map(|n| n.0).collect();
        let op = PropertyOp::Remove { key: "k".into() };
        let plan = plan_vault_edits(&paths, &op, |p| {
            notes
                .iter()
                .find(|n| n.0 == *p)
                .map(|n| n.1.to_owned())
                .ok_or(())
        });
        assert_eq!(plan.writes, vec![("a.md", "---\n---\n".to_owned())]);
        assert_eq!(plan.failures.len(), 1);
    }
}
