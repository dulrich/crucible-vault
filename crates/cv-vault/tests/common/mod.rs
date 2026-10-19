#![allow(dead_code)]

use std::path::{Path, PathBuf};

use cv_vault::{ContentHash, VaultFs, VaultWriter};
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
        VaultWriter::open(live_fs(self.vault.path()), &self.base()).unwrap()
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
