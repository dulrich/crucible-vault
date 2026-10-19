//! The view host (plan WP-8, WP-R12 SM15): one [`views::ViewHost`] on its **own** thread with its
//! **own** read-only [`cv_index::Reader`] (WAL reader, separate from the writer thread), so a view
//! never blocks indexing. Queries and invalidation signals are typed [`Cmd`]s on one channel, so
//! they are processed in the order they were sent.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cv_protocol::{Notification, ViewResult};
use tokio::sync::{broadcast, oneshot};

use crate::sync::lock;

/// A command for the view-host thread.
enum Cmd {
    /// Evaluate `view_id` (`this_path`: the note the view is embedded in).
    Run {
        view_id: String,
        this_path: Option<String>,
        reply: oneshot::Sender<Result<ViewResult, String>>,
    },
    /// A new index generation committed: reopen the reader.
    Generation(u64),
    /// `CV_CONFIG_DIR` changed.
    Config(PathBuf),
    /// Notes changed incrementally (coalesced by the daemon): every view id goes stale.
    NotesChanged,
    /// A `state.db` table changed (from [`ViewRegistry::change_hook`]).
    StateChanged(&'static str),
}

/// The daemon's view host: one thread, one [`views::ViewHost`]; the thread exits when the
/// registry (and every [`ViewRegistry::change_hook`]) is dropped.
pub struct ViewRegistry {
    ids: Arc<Mutex<Vec<String>>>,
    tx: mpsc::Sender<Cmd>,
}

impl std::fmt::Debug for ViewRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewRegistry").finish_non_exhaustive()
    }
}

/// Why `view_run` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewError {
    /// No view has this id.
    NotFound(String),
    /// The host failed.
    Failed(String),
}

impl ViewRegistry {
    /// Starts the host over the index at base path `index_base`, loading definitions from
    /// `config_dir` (`CV_CONFIG_DIR` at startup; later changes arrive via
    /// [`ViewRegistry::notify_config`]); `state_db` feeds `source = "tracker_items"` views.
    /// Invalidations go to `events`.
    pub fn start(
        config_dir: &Path,
        index_base: &Path,
        state_db: Option<&Path>,
        events: broadcast::Sender<Notification>,
    ) -> std::io::Result<Self> {
        let mut host = views::ViewHost::new(config_dir, index_base);
        if let Some(p) = state_db {
            host = host.with_state_db(p);
        }
        log_notices(&host);
        let ids = Arc::new(Mutex::new(host.view_ids()));
        let (tx, rx) = mpsc::channel::<Cmd>();
        let base = index_base.to_path_buf();
        let thread_ids = Arc::clone(&ids);
        std::thread::Builder::new()
            .name("cv-view".into())
            .spawn(move || serve(host, &rx, &base, &events, &thread_ids))?;
        Ok(Self { ids, tx })
    }

    /// The daemon-internal `state.db` change signal (install with `StateDb::set_change_hook`):
    /// queues a [`Cmd::StateChanged`] behind every command already sent.
    pub fn change_hook(&self) -> cv_state::ChangeHook {
        let tx = self.tx.clone();
        // A gone thread means shutdown; nothing to invalidate.
        Box::new(move |table| drop(tx.send(Cmd::StateChanged(table))))
    }

    /// Every view id, sorted.
    pub fn view_ids(&self) -> Vec<String> {
        let mut v = lock(&self.ids).clone();
        v.sort();
        v
    }

    /// Runs `view_id` on the host thread.
    pub async fn run(
        &self,
        view_id: &str,
        this_path: Option<String>,
    ) -> Result<ViewResult, ViewError> {
        if !lock(&self.ids).iter().any(|i| i == view_id) {
            return Err(ViewError::NotFound(view_id.to_owned()));
        }
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Run {
            view_id: view_id.to_owned(),
            this_path,
            reply,
        })
        .map_err(|()| ViewError::Failed("view service thread is gone".into()))?;
        rx.await
            .map_err(|_| ViewError::Failed("view service dropped the request".into()))?
            .map_err(ViewError::Failed)
    }

    /// Index generation `n` committed (the host reopens its reader).
    pub fn notify_generation(&self, n: u64) {
        self.signal(Cmd::Generation(n));
    }

    /// Notes changed incrementally (plan editor-ui-round-3 WP-2): every view id is invalidated
    /// (no dependency tracking). The caller coalesces.
    pub fn notify_notes_changed(&self) {
        self.signal(Cmd::NotesChanged);
    }

    /// The config dir changed.
    pub fn notify_config(&self, dir: &Path) {
        self.signal(Cmd::Config(dir.to_path_buf()));
    }

    fn send(&self, cmd: Cmd) -> Result<(), ()> {
        self.tx.send(cmd).map_err(|_| ())
    }

    fn signal(&self, cmd: Cmd) {
        if self.send(cmd).is_err() {
            tracing::warn!("view service thread is gone");
        }
    }
}

fn log_notices(host: &views::ViewHost) {
    for n in host.load_notices() {
        tracing::warn!(notice = %n, "view definitions");
    }
}

fn serve(
    mut host: views::ViewHost,
    rx: &mpsc::Receiver<Cmd>,
    base: &Path,
    events: &broadcast::Sender<Notification>,
    ids: &Mutex<Vec<String>>,
) {
    let mut reader: Option<cv_index::Reader> = None;
    let invalidate = |v: Vec<String>| {
        if !v.is_empty() {
            // No receivers is fine (no subscribers yet).
            let _ = events.send(Notification::ViewsInvalidated { view_ids: v });
        }
    };
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Run {
                view_id,
                this_path,
                reply,
            } => {
                if reader.is_none() {
                    match cv_index::Reader::open_current(base) {
                        Ok(r) => reader = Some(r),
                        Err(e) => {
                            let _ = reply.send(Err(format!("opening index: {e}")));
                            continue;
                        }
                    }
                }
                let res = match reader.as_ref() {
                    Some(r) => host.run(r, &view_id, this_path.as_deref()),
                    None => Err("no index reader".into()),
                };
                let _ = reply.send(res);
            }
            Cmd::Generation(n) => {
                reader = match cv_index::Reader::open(base, n) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::debug!(error = %e, generation = n, "reopening current generation");
                        cv_index::Reader::open_current(base).ok()
                    }
                };
                invalidate(host.on_index_generation(n));
                *lock(ids) = host.view_ids();
            }
            Cmd::NotesChanged => invalidate(host.view_ids()),
            Cmd::StateChanged(table) => invalidate(host.on_state_changed(table)),
            Cmd::Config(dir) => {
                invalidate(host.on_config_changed(&dir));
                log_notices(&host);
                *lock(ids) = host.view_ids();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// No definitions: nothing is listed and an unknown id is `NotFound`.
    #[tokio::test]
    async fn empty_host_lists_nothing() {
        let tmp = std::env::temp_dir().join(format!("cv-views-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let vault = tmp.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let base = tmp.join("idx").join("index.db");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let index = cv_index::Index::create(&vault, &base).unwrap();
        let (tx, _rx) = broadcast::channel(16);
        let empty = ViewRegistry::start(&tmp, &base, None, tx).unwrap();
        assert!(empty.view_ids().is_empty());
        assert_eq!(
            empty.run("x", None).await.unwrap_err(),
            ViewError::NotFound("x".into())
        );
        drop(index);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// editor-usability-round WP-5 (view live-update invariant): a rename, or a frontmatter
    /// change that moves a note in/out of a view's rows, commits a new generation, which
    /// invalidates the view; the re-run reflects the change.
    #[tokio::test]
    async fn rename_and_frontmatter_change_invalidate_the_hosted_view() {
        let tmp = std::env::temp_dir().join(format!("cv-views-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let vault = tmp.join("vault");
        std::fs::create_dir_all(vault.join("stack")).unwrap();
        std::fs::write(vault.join("stack/t1.md"), "---\nstatus: on\n---\n").unwrap();
        std::fs::write(vault.join("stack/t2.md"), "---\nstatus: off\n---\n").unwrap();
        let config = tmp.join("config");
        std::fs::create_dir_all(config.join("views")).unwrap();
        std::fs::write(
            config.join("views/hubs.toml"),
            "[[views]]\ntype='table'\nname='On'\nfilters='note.status == \"on\"'\norder=['file.name']\n",
        )
        .unwrap();
        let base = tmp.join("idx").join("index.db");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let mut index = cv_index::Index::create(&vault, &base).unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        let reg = ViewRegistry::start(&config, &base, None, tx).unwrap();
        let names = |r: &ViewResult| -> Vec<String> {
            r.0["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g["rows"].as_array().cloned().unwrap_or_default())
                .map(|row| {
                    let c = &row["cells"][0];
                    c["v"]
                        .as_str()
                        .or(c["label"].as_str())
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        };
        let id = "hubs#On".to_owned();
        assert_eq!(names(&reg.run(&id, None).await.unwrap()), ["t1"]);
        let mut step = async |change: &dyn Fn()| {
            change();
            let g = index.rebuild(&vault).unwrap();
            reg.notify_generation(g);
            let n = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let Notification::ViewsInvalidated { view_ids } = n else {
                panic!("expected views_invalidated, got {n:?}");
            };
            assert!(view_ids.contains(&id));
            names(&reg.run(&id, None).await.unwrap())
        };
        // Rename: the row follows the new name.
        let rows = step(&|| {
            std::fs::rename(vault.join("stack/t1.md"), vault.join("stack/renamed.md")).unwrap();
        })
        .await;
        assert_eq!(rows, ["renamed"]);
        // Frontmatter change: t2 enters the view.
        let rows = step(&|| {
            std::fs::write(vault.join("stack/t2.md"), "---\nstatus: on\n---\n").unwrap();
        })
        .await;
        assert_eq!(rows, ["renamed", "t2"]);
        drop(index);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    struct TmpRoot(PathBuf);
    impl TmpRoot {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Plan round-5-reading-pickers-intake WP-10: a `tracker_items` insert raises the state
    /// change hook, which invalidates only the `source = "tracker_items"` view; the re-run sees
    /// the new row. A duplicate insert (returns false) raises nothing.
    #[tokio::test]
    async fn tracker_insert_invalidates_tracker_views_only() {
        let root = std::env::temp_dir().join(format!("cv-views-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tmp = TmpRoot(root);
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(vault.join("daily")).unwrap();
        std::fs::write(vault.join("daily/2026-10-17.md"), "# d\n").unwrap();
        let config = tmp.path().join("config");
        std::fs::create_dir_all(config.join("views")).unwrap();
        std::fs::write(
            config.join("views/intake.toml"),
            "source='tracker_items'\n[[views]]\ntype='table'\nname='Day'\nfilters='day == this.file.name'\norder=['title']\n",
        )
        .unwrap();
        std::fs::write(
            config.join("views/notes.toml"),
            "[[views]]\ntype='table'\nname='All'\norder=['file.name']\n",
        )
        .unwrap();
        let base = tmp.path().join("idx").join("index.db");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let _index = cv_index::Index::create(&vault, &base).unwrap();
        let state_path = tmp.path().join("state.db");
        let mut state =
            cv_state::StateDb::open(&state_path, 1, cv_state::QueueConfig::default()).unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        let reg = ViewRegistry::start(&config, &base, Some(&state_path), tx).unwrap();
        state.set_change_hook(reg.change_hook());
        let id = "intake#Day".to_owned();
        let this = Some("daily/2026-10-17.md".to_owned());
        let rows = |r: &ViewResult| r.0["groups"][0]["rows"].as_array().map_or(0, Vec::len);
        assert_eq!(rows(&reg.run(&id, this.clone()).await.unwrap()), 0);
        let item = cv_state::TrackerItem {
            source_kind: "youtube".into(),
            source_key: "UC1".into(),
            item_id: "v1".into(),
            title: "T".into(),
            url: "https://y/1".into(),
            first_seen_ms: 1,
            day: "2026-10-17".into(),
            outcome: cv_state::TrackerOutcome::Line,
            score: None,
            full_body: None,
            note_path: None,
            source_name: None,
            duration_s: None,
            word_count: None,
        };
        assert!(state.insert_tracker_item(&item).unwrap());
        let n = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            n,
            Notification::ViewsInvalidated {
                view_ids: vec![id.clone()]
            }
        );
        assert_eq!(rows(&reg.run(&id, this).await.unwrap()), 1);
        assert!(!state.insert_tracker_item(&item).unwrap());
        // Nothing further was emitted (the duplicate raised no signal).
        let _ = reg.run(&id, None).await.unwrap();
        assert!(rx.try_recv().is_err());
    }

    /// WP-R12 (SM15): signals and queries share one channel, so a state change sent before a
    /// query is handled before it, and one sent after is handled after it.
    #[tokio::test]
    async fn state_change_and_query_are_processed_in_send_order() {
        let root = std::env::temp_dir().join(format!("cv-views-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tmp = TmpRoot(root);
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        let config = tmp.path().join("config");
        std::fs::create_dir_all(config.join("views")).unwrap();
        std::fs::write(
            config.join("views/intake.toml"),
            "source='tracker_items'\n[[views]]\ntype='table'\nname='Day'\norder=['title']\n",
        )
        .unwrap();
        let base = tmp.path().join("idx").join("index.db");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let _index = cv_index::Index::create(&vault, &base).unwrap();
        let state_path = tmp.path().join("state.db");
        drop(cv_state::StateDb::open(&state_path, 1, cv_state::QueueConfig::default()).unwrap());
        let (tx, mut rx) = broadcast::channel(16);
        let reg = ViewRegistry::start(&config, &base, Some(&state_path), tx).unwrap();
        let hook = reg.change_hook();
        let id = "intake#Day".to_owned();
        let invalidated = Notification::ViewsInvalidated {
            view_ids: vec![id.clone()],
        };
        // Before: the invalidation is already out when the query's reply arrives.
        hook("tracker_items");
        reg.run(&id, None).await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), invalidated);
        assert!(rx.try_recv().is_err());
        // After: the query's reply precedes the invalidation.
        reg.run(&id, None).await.unwrap();
        assert!(rx.try_recv().is_err());
        hook("tracker_items");
        let n = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, invalidated);
    }
}
