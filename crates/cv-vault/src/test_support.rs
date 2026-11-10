//! Shared live-writer test fixture, compiled for
//! this crate's tests and, via the `test-support` feature, for downstream dev-dependencies only.
//!
//! [`LiveFixture`] builds a vault under one [`TempDir`] (asserted to lie under
//! [`std::env::temp_dir`], so tests never touch the live vault), seeds files, opens a
//! [`VaultWriter`] on a temp index and spawns its writer thread.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use std::collections::BTreeMap;

use crate::{
    IdResult, NoteId, NoteIdSink, NoteLocation, VaultFs, VaultWriter, WriterHandle, spawn_writer,
};

/// A temp vault + index base + running writer thread.
pub struct LiveFixture {
    /// Owns every path below; removed on drop.
    pub dir: TempDir,
    /// `<dir>/vault`.
    pub vault: PathBuf,
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
        let index_db = dir.path().join("index").join("index.db");
        assert_temp_root(&vault);
        for d in [&vault, &dir.path().join("index")] {
            std::fs::create_dir_all(d).expect("fixture dir");
        }
        for (rel, bytes) in &self.files {
            write_file(&vault, rel, bytes);
        }
        let fs = VaultFs::live(&vault).expect("live fs");
        let w = VaultWriter::open(fs, &index_db, Box::new(MemNoteIds::default())).expect("writer");
        let (writer, _join) = spawn_writer(w).expect("spawn writer");
        LiveFixture {
            dir,
            vault,
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

    /// The temp root holding the vault and index.
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

/// Panics unless `root` (canonicalized when it exists) lies under the canonical
/// [`std::env::temp_dir`]: the guard that keeps live-writer test fixtures off the live vault.
pub fn assert_temp_root(root: &Path) {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let tmp = canon(&std::env::temp_dir());
    let root = canon(root);
    assert!(
        root.starts_with(&tmp) && root != tmp,
        "test vault root {} is not under the temp dir {}",
        root.display(),
        tmp.display()
    );
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("parent dir");
    std::fs::write(p, bytes).expect("write fixture file");
}

/// An in-memory [`NoteIdSink`] for tests: mint-or-get on bind, ids follow moves, retirements are
/// tombstones (no `replaced_by`), revive and locate as the daemon's store answers them.
#[derive(Debug, Default)]
pub struct MemNoteIds {
    live: BTreeMap<String, NoteId>,
    gone: BTreeMap<NoteId, String>,
    next: u64,
}

impl MemNoteIds {
    fn get(&mut self, p: &str) -> NoteId {
        if let Some(id) = self.live.get(p) {
            return *id;
        }
        self.next += 1;
        self.live.insert(p.into(), NoteId(self.next));
        NoteId(self.next)
    }
}

impl NoteIdSink for MemNoteIds {
    fn bind(&mut self, path: &str) -> IdResult<NoteId> {
        Ok(self.get(path))
    }
    fn moved(&mut self, from: &str, to: &str) -> IdResult<NoteId> {
        if let Some(stale) = self.live.remove(to) {
            self.gone.insert(stale, to.into());
        }
        let id = self.get(from);
        self.live.remove(from);
        self.live.insert(to.into(), id);
        Ok(id)
    }
    fn retired(&mut self, path: &str, by: Option<&str>) -> IdResult<NoteId> {
        if let Some(b) = by {
            self.get(b);
        }
        let id = self.get(path);
        self.live.remove(path);
        self.gone.insert(id, path.into());
        Ok(id)
    }
    fn revive(&mut self, id: NoteId, path: &str) -> IdResult<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_temp() {
        let fx = LiveFixture::builder().file("a/b.md", "hi\n").build();
        for p in [&fx.vault, &fx.index_db] {
            assert!(p.starts_with(fx.root()), "{p:?}");
        }
        assert!(fx.root().starts_with(std::env::temp_dir()));
        assert_eq!(fx.text("a/b.md"), "hi\n");
        assert_eq!(
            fx.writer.read("a/b.md").unwrap().as_deref(),
            Some(&b"hi\n"[..])
        );
    }

    #[test]
    #[should_panic(expected = "is not under the temp dir")]
    fn fixture_refuses_a_non_temp_root() {
        assert_temp_root(Path::new("/srv/vault"));
    }
}
