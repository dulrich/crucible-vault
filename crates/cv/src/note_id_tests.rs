//! The daemon's note-id sink on a real writer thread and `state.db` (plan note-identity WP-1).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cv_state::{QueueConfig, StateDb};
use cv_vault::{
    ContentHash, NoteId, NoteLocation, VaultFs, VaultWriter, WatchConfig, WriterHandle,
};

fn hash(s: &str) -> ContentHash {
    ContentHash::of(s.as_bytes())
}

struct Env {
    _dir: tempfile::TempDir,
    vault: std::path::PathBuf,
    index: std::path::PathBuf,
    state: std::path::PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(vault.join("f")).unwrap();
        std::fs::write(vault.join("f/a.md"), "A").unwrap();
        Self {
            vault,
            index: dir.path().join("index.db"),
            state: dir.path().join("state.db"),
            _dir: dir,
        }
    }

    /// A daemon-like writer: both sinks installed over one `state.db`, watcher running.
    fn start(
        &self,
    ) -> (
        WriterHandle,
        std::sync::mpsc::Receiver<cv_vault::VaultEvent>,
    ) {
        let state = Arc::new(Mutex::new(
            StateDb::open(&self.state, 1, QueueConfig::default()).unwrap(),
        ));
        let fs = VaultFs::live(&self.vault).unwrap();
        let w = VaultWriter::open(fs, &self.index, crate::note_id::sink(&state)).unwrap();
        crate::note_id::install(&w, &state);
        let cfg = WatchConfig {
            debounce: Duration::from_millis(50),
            echo_ttl: Duration::from_secs(5),
        };
        let (wh, rx, _j) = cv_vault::spawn_writer_with_watcher(w, cfg).unwrap();
        (wh, rx)
    }
}

#[test]
fn writer_thread_binds_and_locates_and_a_restart_keeps_the_ids() {
    let env = Env::new();
    let (a, b) = {
        let (wh, rx) = env.start();
        let a = wh.bind_note("f/a.md").unwrap();
        assert_eq!(wh.bind_note("f/a.md").unwrap(), a, "mint-or-get");
        wh.rename_folder("f", "g").unwrap();
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((ev.path.as_str(), ev.id), ("g/a.md", Some(a)));
        assert_eq!(
            wh.locate_note(a).unwrap(),
            NoteLocation::Live {
                path: "g/a.md".into()
            }
        );
        wh.create("b.md", b"B".to_vec()).unwrap();
        let b = rx.recv_timeout(Duration::from_secs(2)).unwrap().id.unwrap();
        assert_ne!(a, b);
        (a, b)
    };
    // Restart: reopen the same state.db; bindings and ids survive.
    let (wh, rx) = env.start();
    assert_eq!(wh.bind_note("g/a.md").unwrap(), a);
    assert_eq!(wh.bind_note("b.md").unwrap(), b);
    // Replace: a is absorbed by b.
    wh.call(|w| w.delete_replaced("g/a.md", hash("A"), "b.md"))
        .unwrap()
        .unwrap();
    let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(ev.id, Some(a), "the delete carries the retired id");
    assert_eq!(
        wh.locate_note(a).unwrap(),
        NoteLocation::Gone {
            replaced_by: Some(b)
        }
    );
    assert_eq!(
        wh.locate_note(NoteId(u64::MAX >> 2)).unwrap(),
        NoteLocation::Unknown
    );
    // Revive (D3 storage half): the same id is live again at its last path.
    assert!(
        wh.call(move |w| w.revive_note(a, "g/a.md").unwrap())
            .unwrap()
    );
    assert_eq!(
        wh.locate_note(a).unwrap(),
        NoteLocation::Live {
            path: "g/a.md".into()
        }
    );
}

/// Decision 3: a file removed while the daemon is down is tombstoned at install.
#[test]
fn install_retires_a_row_whose_file_vanished_while_down() {
    let env = Env::new();
    let a = {
        let (wh, _rx) = env.start();
        wh.bind_note("f/a.md").unwrap()
    };
    std::fs::remove_file(env.vault.join("f/a.md")).unwrap();
    let (wh, _rx) = env.start();
    assert_eq!(
        wh.locate_note(a).unwrap(),
        NoteLocation::Gone { replaced_by: None }
    );
}

/// Audit F1: a `.gitignore`d file is outside the index's file set but still exists, so its
/// bound row stays live across a restart.
#[test]
fn install_keeps_a_gitignored_existing_file_live() {
    let env = Env::new();
    std::fs::write(env.vault.join(".gitignore"), "ign.md\n").unwrap();
    std::fs::write(env.vault.join("ign.md"), "I").unwrap();
    let id = {
        let (wh, _rx) = env.start();
        wh.bind_note("ign.md").unwrap()
    };
    let (wh, _rx) = env.start();
    let indexed = wh
        .call(|w| w.index().query().file("ign.md").map(|r| r.is_some()))
        .unwrap()
        .unwrap();
    assert!(!indexed, "the index prunes the ignored file");
    assert_eq!(
        wh.locate_note(id).unwrap(),
        NoteLocation::Live {
            path: "ign.md".into()
        }
    );
}
