//! Shared live-writer test fixture (plan tn-review-baseline-remediation WP-R14, M6), compiled for
//! this crate's tests and, via the `test-support` feature, for downstream dev-dependencies only.
//!
//! [`LiveFixture`] builds a vault and a *temp* protected root under one [`TempDir`] (invariant I1:
//! never the live vault), seeds files, opens a [`VaultWriter`] on a temp index and spawns its
//! writer thread.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::{NoCutover, VaultFs, VaultWriter, WriterHandle, spawn_writer};

/// A temp vault + protected root + index base + running writer thread.
pub struct LiveFixture {
    /// Owns every path below; removed on drop.
    pub dir: TempDir,
    /// `<dir>/vault`.
    pub vault: PathBuf,
    /// `<dir>/protected`, the temp protected root the writer guards against.
    pub protected: PathBuf,
    /// `<dir>/index/index.db`, the writer's index base.
    pub index_db: PathBuf,
    /// The spawned writer.
    pub writer: WriterHandle,
}

/// Seeds files, then opens the fixture with [`LiveFixtureBuilder::build`].
#[derive(Default)]
pub struct LiveFixtureBuilder {
    files: Vec<(String, Vec<u8>)>,
}

impl LiveFixtureBuilder {
    /// Writes `bytes` at vault-relative `rel` before the writer (and its index) opens.
    #[must_use]
    pub fn file(mut self, rel: &str, bytes: impl AsRef<[u8]>) -> Self {
        self.files.push((rel.to_owned(), bytes.as_ref().to_vec()));
        self
    }

    /// [`Self::file`] for each pair.
    #[must_use]
    pub fn files<R: AsRef<str>, B: AsRef<[u8]>>(
        mut self,
        files: impl IntoIterator<Item = (R, B)>,
    ) -> Self {
        for (r, b) in files {
            self = self.file(r.as_ref(), b);
        }
        self
    }

    /// Creates the dirs, seeds the files and spawns the writer.
    pub fn build(self) -> LiveFixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path().join("vault");
        let protected = dir.path().join("protected");
        let index_db = dir.path().join("index").join("index.db");
        for d in [&vault, &protected, &dir.path().join("index")] {
            std::fs::create_dir_all(d).expect("fixture dir");
        }
        for (rel, bytes) in &self.files {
            write_file(&vault, rel, bytes);
        }
        let fs = VaultFs::live(&vault, &protected, &NoCutover).expect("live fs");
        let w = VaultWriter::open(fs, &index_db).expect("writer");
        let (writer, _join) = spawn_writer(w).expect("spawn writer");
        LiveFixture {
            dir,
            vault,
            protected,
            index_db,
            writer,
        }
    }
}

impl LiveFixture {
    /// A builder for seeding files.
    pub fn builder() -> LiveFixtureBuilder {
        LiveFixtureBuilder::default()
    }

    /// A fixture seeded with `files`.
    pub fn new<R: AsRef<str>, B: AsRef<[u8]>>(files: impl IntoIterator<Item = (R, B)>) -> Self {
        Self::builder().files(files).build()
    }

    /// The temp root holding the vault, protected root and index.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The absolute path of vault-relative `rel`.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.vault.join(rel)
    }

    /// The raw bytes on disk at `rel`, if present.
    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(rel)).ok()
    }

    /// The UTF-8 text on disk at `rel` (panics when absent).
    pub fn text(&self, rel: &str) -> String {
        String::from_utf8(self.read(rel).unwrap_or_else(|| panic!("missing {rel}")))
            .expect("utf-8 note")
    }

    /// Writes `bytes` at `rel` directly on disk (bypassing the writer), creating parents.
    pub fn write(&self, rel: &str, bytes: impl AsRef<[u8]>) {
        write_file(&self.vault, rel, bytes.as_ref());
    }
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("parent dir");
    std::fs::write(p, bytes).expect("write fixture file");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_temp_and_never_the_live_vault() {
        let fx = LiveFixture::builder().file("a/b.md", "hi\n").build();
        let live = Path::new(crate::DEFAULT_PROTECTED_ROOT);
        assert_eq!(live, Path::new("~/vault"));
        for p in [&fx.vault, &fx.protected, &fx.index_db] {
            assert!(p.starts_with(fx.root()), "{p:?}");
            assert!(!p.starts_with(live) && !live.starts_with(p), "{p:?}");
        }
        assert!(fx.root().starts_with(std::env::temp_dir()));
        assert_eq!(fx.text("a/b.md"), "hi\n");
        assert_eq!(
            fx.writer.read("a/b.md").unwrap().as_deref(),
            Some(&b"hi\n"[..])
        );
    }
}
