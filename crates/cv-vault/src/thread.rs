//! The writer thread: one std thread owns the [`VaultWriter`] (and so the only `index.db` writer
//! connection) and, optionally, the watcher; commands arrive over `std::sync::mpsc`.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use std::collections::BTreeSet;
use std::path::PathBuf;

use notify::{RecursiveMode, Watcher};

use crate::error::{Result, VaultError};
use crate::hash::ContentHash;
use crate::watcher::{VaultEvent, WatchConfig, WatchState};
use crate::writer::{RenameOutcome, VaultWriter};

type Job = Box<dyn FnOnce(&mut VaultWriter) + Send>;

/// A cloneable, `Send` handle to the writer thread. The thread exits once every handle is
/// dropped.
#[derive(Clone)]
pub struct WriterHandle {
    tx: Sender<Job>,
}

impl std::fmt::Debug for WriterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterHandle").finish_non_exhaustive()
    }
}

impl WriterHandle {
    /// Runs `f` on the writer thread and returns its result.
    pub fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut VaultWriter) -> T + Send + 'static,
    ) -> Result<T> {
        let (rtx, rrx) = mpsc::channel();
        let job: Job = Box::new(move |w| {
            if rtx.send(f(w)).is_err() {
                tracing::warn!("writer reply dropped: caller went away");
            }
        });
        self.tx.send(job).map_err(|_| VaultError::WriterGone)?;
        rrx.recv().map_err(|_| VaultError::WriterGone)
    }

    /// [`VaultWriter::read`].
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let p = path.to_string();
        self.call(move |w| w.read(&p))?
    }

    /// [`VaultWriter::write`].
    pub fn write(
        &self,
        path: &str,
        bytes: Vec<u8>,
        expected: Option<ContentHash>,
    ) -> Result<ContentHash> {
        let p = path.to_string();
        self.call(move |w| w.write(&p, &bytes, expected))?
    }

    /// [`VaultWriter::create`].
    pub fn create(&self, path: &str, bytes: Vec<u8>) -> Result<ContentHash> {
        let p = path.to_string();
        self.call(move |w| w.create(&p, &bytes))?
    }

    /// [`VaultWriter::delete`].
    pub fn delete(&self, path: &str, expected: ContentHash) -> Result<()> {
        let p = path.to_string();
        self.call(move |w| w.delete(&p, expected))?
    }

    /// [`VaultWriter::rename`].
    pub fn rename(&self, from: &str, to: &str, expected: ContentHash) -> Result<RenameOutcome> {
        let (f, t) = (from.to_string(), to.to_string());
        self.call(move |w| w.rename(&f, &t, expected))?
    }

    /// [`VaultWriter::rename_folder`].
    pub fn rename_folder(&self, from: &str, to: &str) -> Result<RenameOutcome> {
        let (f, t) = (from.to_string(), to.to_string());
        self.call(move |w| w.rename_folder(&f, &t))?
    }

    /// [`VaultWriter::rebuild`].
    pub fn rebuild(&self) -> Result<u64> {
        self.call(VaultWriter::rebuild)?
    }
}

/// Spawns the writer thread without a watcher.
pub fn spawn_writer(writer: VaultWriter) -> Result<(WriterHandle, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Job>();
    let mut writer = writer;
    let join = std::thread::Builder::new()
        .name("cv-writer".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                job(&mut writer);
            }
        })
        .map_err(|e| crate::error::io(std::path::Path::new("cv-writer thread"), e))?;
    Ok((WriterHandle { tx }, join))
}

/// Spawns the writer thread with a recursive watcher on the live root. The watcher is started,
/// then the `(size, mtime)` map is seeded from a walk — before this returns, and emitting nothing
/// (I4). External changes are applied to the index on the writer thread, then sent on the
/// returned receiver; the daemon's own mutations are sent by the writer itself (exactly once,
/// `Origin::Daemon`, before the mutating call returns — plan note-path-continuity D1).
pub fn spawn_writer_with_watcher(
    writer: VaultWriter,
    cfg: WatchConfig,
) -> Result<(WriterHandle, Receiver<VaultEvent>, JoinHandle<()>)> {
    let mut writer = writer.with_echo_ttl(cfg.echo_ttl);
    let root = writer.fs().root().to_path_buf();
    let (fs_tx, fs_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |ev| {
        // The receiver only goes away when the writer thread exits.
        if fs_tx.send(ev).is_err() {
            tracing::debug!("watcher event after writer exit");
        }
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    let mut state = WatchState::seed(&root, cfg.debounce);
    let mut link_dirs = BTreeSet::new();
    sync_link_dirs(&mut watcher, &mut state, &mut link_dirs);
    let (tx, rx) = mpsc::channel::<Job>();
    let (ev_tx, ev_rx) = mpsc::channel::<VaultEvent>();
    writer.set_event_sender(ev_tx.clone());
    let idle = cfg.debounce.max(Duration::from_millis(10));
    let join = std::thread::Builder::new()
        .name("cv-writer".into())
        .spawn(move || {
            let mut watcher = watcher; // lives as long as the thread
            loop {
                let wait = state.next_due(Instant::now()).unwrap_or(idle).min(idle);
                match rx.recv_timeout(wait.max(Duration::from_millis(1))) {
                    Ok(job) => job(&mut writer),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                while let Ok(ev) = fs_rx.try_recv() {
                    match ev {
                        Ok(ev) => state.on_notify(&ev),
                        Err(e) => tracing::warn!(error = %e, "watcher error"),
                    }
                }
                for ev in state.flush(&mut writer) {
                    if ev_tx.send(ev).is_err() {
                        tracing::debug!("vault event dropped: no subscriber");
                    }
                }
                sync_link_dirs(&mut watcher, &mut state, &mut link_dirs);
            }
        })
        .map_err(|e| crate::error::io(std::path::Path::new("cv-writer thread"), e))?;
    Ok((WriterHandle { tx }, ev_rx, join))
}

/// Brings the non-recursive watches on symlink-target directories in line with the link set
/// (plan editor-cutover-blockers WP-1): one watch per directory, added and dropped as links are
/// added, removed or retargeted inside the vault.
fn sync_link_dirs(
    watcher: &mut impl Watcher,
    state: &mut WatchState,
    watched: &mut BTreeSet<PathBuf>,
) {
    let Some(want) = state.take_watch_dirs() else {
        return;
    };
    for d in watched.difference(&want) {
        if let Err(e) = watcher.unwatch(d) {
            tracing::debug!(error = %e, "unwatching a link-target directory");
        }
    }
    let mut now = BTreeSet::new();
    for d in want {
        if watched.contains(&d) {
            now.insert(d);
            continue;
        }
        match watcher.watch(&d, RecursiveMode::NonRecursive) {
            Ok(()) => {
                now.insert(d);
            }
            Err(e) => tracing::warn!(error = %e, "watching a link-target directory failed"),
        }
    }
    *watched = now;
}
