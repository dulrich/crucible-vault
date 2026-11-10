#![allow(dead_code)]

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use cv_vault::{
    ContentHash, IdResult, NoteId, NoteIdError, NoteIdSink, NoteLocation, VaultFs, VaultWriter,
};
use tempfile::TempDir;

pub struct Fx {
    pub vault: TempDir,
    pub idx: TempDir,
}

impl Fx {
    pub fn new(files: &[(&str, &str)]) -> Self {
        let vault = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let full = vault.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, c).unwrap();
        }
        Self {
            vault,
            idx: tempfile::tempdir().unwrap(),
        }
    }

    pub fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.vault.path()).unwrap()
    }

    pub fn base(&self) -> PathBuf {
        self.idx.path().join("index.db")
    }

    pub fn live(&self) -> VaultWriter {
        VaultWriter::open(
            live_fs(self.vault.path()),
            &self.base(),
            Box::new(MemIds::default()),
        )
        .unwrap()
    }

    pub fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.vault.path().join(p)).unwrap()
    }
}

/// Live [`VaultFs`] on `root`, which must lie under the temp dir (keeps tests off the live vault).
pub fn live_fs(root: &Path) -> VaultFs {
    let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let canon = std::fs::canonicalize(root).unwrap();
    assert!(
        canon.starts_with(&tmp) && canon != tmp,
        "{canon:?} is not under {tmp:?}"
    );
    VaultFs::live(root).unwrap()
}

pub fn h(s: &str) -> ContentHash {
    ContentHash::of(s.as_bytes())
}

/// Every file name under `dir`, recursively.
pub fn all_names(dir: &Path) -> Vec<String> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// An in-memory [`NoteIdSink`] for tests: mint-or-get on bind, ids follow moves, retirements are
/// tombstones (no `replaced_by`), revive and locate as the daemon's store answers them.
#[derive(Debug, Default)]
pub struct MemIds {
    /// While set, every call fails (Decision 4 tests).
    pub fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
    live: BTreeMap<String, NoteId>,
    gone: BTreeMap<NoteId, String>,
    next: u64,
}

impl MemIds {
    fn ok(&self) -> IdResult<()> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(NoteIdError("store down".into()));
        }
        Ok(())
    }

    fn get(&mut self, p: &str) -> NoteId {
        if let Some(id) = self.live.get(p) {
            return *id;
        }
        self.next += 1;
        self.live.insert(p.into(), NoteId(self.next));
        NoteId(self.next)
    }
}

impl NoteIdSink for MemIds {
    // Every method checks `fail` first through `ok()`.
    fn bind(&mut self, path: &str) -> IdResult<NoteId> {
        self.ok()?;
        Ok(self.get(path))
    }
    fn moved(&mut self, from: &str, to: &str) -> IdResult<NoteId> {
        self.ok()?;
        if let Some(stale) = self.live.remove(to) {
            self.gone.insert(stale, to.into());
        }
        let id = self.get(from);
        self.live.remove(from);
        self.live.insert(to.into(), id);
        Ok(id)
    }
    fn retired(&mut self, path: &str, by: Option<&str>) -> IdResult<NoteId> {
        self.ok()?;
        if let Some(b) = by {
            self.get(b);
        }
        let id = self.get(path);
        self.live.remove(path);
        self.gone.insert(id, path.into());
        Ok(id)
    }
    fn revive(&mut self, id: NoteId, path: &str) -> IdResult<bool> {
        self.ok()?;
        if self.live.get(path) == Some(&id) {
            return Ok(true);
        }
        if self.live.contains_key(path) || self.gone.remove(&id).is_none() {
            return Ok(false);
        }
        self.live.insert(path.into(), id);
        Ok(true)
    }
    fn locate(&mut self, id: NoteId) -> IdResult<NoteLocation> {
        self.ok()?;
        if let Some((p, _)) = self.live.iter().find(|(_, v)| **v == id) {
            return Ok(NoteLocation::Live { path: p.clone() });
        }
        Ok(if self.gone.contains_key(&id) {
            NoteLocation::Gone { replaced_by: None }
        } else {
            NoteLocation::Unknown
        })
    }
}
