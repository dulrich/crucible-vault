//! The `ViewService` seam (plan WP-8): hosted services answer `views_list` / `view_run`, each on
//! its **own** thread with its **own** read-only [`cv_index::Reader`] (WAL reader, separate from
//! the writer thread), so a view never blocks indexing. A1 registers nothing; plan B's WP-6 adds
//! one line to [`register_views`].

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use cv_protocol::{Notification, ViewResult};
use tokio::sync::{broadcast, oneshot};

/// A hosted view evaluator.
pub trait ViewService: Send {
    /// The view ids this service answers.
    fn view_ids(&self) -> Vec<String>;
    /// Evaluates `view_id` against `reader` (`this_path`: the note the view is embedded in).
    fn run(
        &mut self,
        reader: &cv_index::Reader,
        view_id: &str,
        this_path: Option<&str>,
    ) -> Result<ViewResult, String>;
    /// A new index generation `n` committed; returns the invalidated view ids.
    fn on_index_generation(&mut self, n: u64) -> Vec<String>;
    /// `CV_CONFIG_DIR` changed; returns the invalidated view ids.
    fn on_config_changed(&mut self, config_dir: &Path) -> Vec<String>;
}

/// The A1 registration point. Plan B's WP-6 adds its one `reg.register(...)` line here.
///
/// `config_dir` is `CV_CONFIG_DIR` at startup, so a service loads its definitions when it is
/// constructed (the config poller only reports later *changes* through
/// [`ViewService::on_config_changed`]). [`ViewRegistry::index_base`] gives the index base path
/// (`cv_index::generation_path(base, reader.generation())` is the generation file a service may
/// open its own read-only connection on).
pub fn register_views(reg: &mut ViewRegistry, config_dir: &Path) {
    // `views` cannot depend on `cv`, so the trait impl for its host lives here.
    struct Hosted(views::ViewHost);
    impl ViewService for Hosted {
        fn view_ids(&self) -> Vec<String> {
            self.0.view_ids()
        }
        fn run(
            &mut self,
            reader: &cv_index::Reader,
            view_id: &str,
            this_path: Option<&str>,
        ) -> Result<ViewResult, String> {
            self.0.run(reader, view_id, this_path)
        }
        fn on_index_generation(&mut self, n: u64) -> Vec<String> {
            self.0.on_index_generation(n)
        }
        fn on_config_changed(&mut self, config_dir: &Path) -> Vec<String> {
            let ids = self.0.on_config_changed(config_dir);
            for n in self.0.load_notices() {
                tracing::warn!(notice = %n, "view definitions");
            }
            ids
        }
    }
    let host = views::ViewHost::new(config_dir, reg.index_base());
    for n in host.load_notices() {
        tracing::warn!(notice = %n, "view definitions");
    }
    if let Err(e) = reg.register(Box::new(Hosted(host))) {
        tracing::warn!(error = %e, "registering the views service");
    }
}

enum Cmd {
    Run {
        view_id: String,
        this_path: Option<String>,
        reply: oneshot::Sender<Result<ViewResult, String>>,
    },
    Generation(u64),
    Config(PathBuf),
}

struct Host {
    ids: Arc<Mutex<Vec<String>>>,
    tx: mpsc::Sender<Cmd>,
}

/// Hosts every registered [`ViewService`]; threads exit when the registry is dropped.
pub struct ViewRegistry {
    index_base: PathBuf,
    events: broadcast::Sender<Notification>,
    hosts: Vec<Host>,
}

impl std::fmt::Debug for ViewRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewRegistry")
            .field("services", &self.hosts.len())
            .finish_non_exhaustive()
    }
}

/// Why `view_run` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewError {
    /// No service answers this id.
    NotFound(String),
    /// The service failed.
    Failed(String),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ViewRegistry {
    /// An empty registry over the index at base path `index_base`; invalidations go to `events`.
    pub fn new(index_base: &Path, events: broadcast::Sender<Notification>) -> Self {
        Self {
            index_base: index_base.to_path_buf(),
            events,
            hosts: Vec::new(),
        }
    }

    /// The index base path (`CV_INDEX_DB`) this registry's readers open generations under.
    pub fn index_base(&self) -> &Path {
        &self.index_base
    }

    /// Hosts `service` on its own thread.
    pub fn register(&mut self, service: Box<dyn ViewService>) -> std::io::Result<()> {
        let ids = Arc::new(Mutex::new(service.view_ids()));
        let (tx, rx) = mpsc::channel::<Cmd>();
        let base = self.index_base.clone();
        let events = self.events.clone();
        let thread_ids = Arc::clone(&ids);
        std::thread::Builder::new()
            .name("cv-view".into())
            .spawn(move || serve(service, rx, &base, &events, &thread_ids))?;
        self.hosts.push(Host { ids, tx });
        Ok(())
    }

    /// Every registered view id, sorted.
    pub fn view_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .hosts
            .iter()
            .flat_map(|h| lock(&h.ids).clone())
            .collect();
        v.sort();
        v
    }

    /// Runs `view_id` on its service's thread.
    pub async fn run(
        &self,
        view_id: &str,
        this_path: Option<String>,
    ) -> Result<ViewResult, ViewError> {
        let host = self
            .hosts
            .iter()
            .find(|h| lock(&h.ids).iter().any(|i| i == view_id))
            .ok_or_else(|| ViewError::NotFound(view_id.to_owned()))?;
        let (reply, rx) = oneshot::channel();
        host.tx
            .send(Cmd::Run {
                view_id: view_id.to_owned(),
                this_path,
                reply,
            })
            .map_err(|_| ViewError::Failed("view service thread is gone".into()))?;
        rx.await
            .map_err(|_| ViewError::Failed("view service dropped the request".into()))?
            .map_err(ViewError::Failed)
    }

    /// Tells every service about index generation `n` (each reopens its reader).
    pub fn notify_generation(&self, n: u64) {
        for h in &self.hosts {
            if h.tx.send(Cmd::Generation(n)).is_err() {
                tracing::warn!("view service thread is gone");
            }
        }
    }

    /// Tells every service the config dir changed.
    pub fn notify_config(&self, dir: &Path) {
        for h in &self.hosts {
            if h.tx.send(Cmd::Config(dir.to_path_buf())).is_err() {
                tracing::warn!("view service thread is gone");
            }
        }
    }
}

fn serve(
    mut service: Box<dyn ViewService>,
    rx: mpsc::Receiver<Cmd>,
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
                    Some(r) => service.run(r, &view_id, this_path.as_deref()),
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
                invalidate(service.on_index_generation(n));
                *lock(ids) = service.view_ids();
            }
            Cmd::Config(dir) => {
                invalidate(service.on_config_changed(&dir));
                *lock(ids) = service.view_ids();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Fake;
    impl ViewService for Fake {
        fn view_ids(&self) -> Vec<String> {
            vec!["fake.count".into()]
        }
        fn run(
            &mut self,
            reader: &cv_index::Reader,
            view_id: &str,
            this_path: Option<&str>,
        ) -> Result<ViewResult, String> {
            let n = reader.query().file_count().map_err(|e| e.to_string())?;
            Ok(ViewResult(serde_json::json!({
                "view": view_id, "this": this_path, "files": n, "gen": reader.generation()
            })))
        }
        fn on_index_generation(&mut self, _n: u64) -> Vec<String> {
            vec!["fake.count".into()]
        }
        fn on_config_changed(&mut self, _dir: &Path) -> Vec<String> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn fake_service_listed_run_and_invalidated() {
        let tmp = std::env::temp_dir().join(format!("cv-views-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let vault = tmp.join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("a.md"), "# a\n").unwrap();
        let base = tmp.join("idx").join("index.db");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let mut index = cv_index::Index::create(&vault, &base).unwrap();
        let (tx, mut rx) = broadcast::channel(16);

        let mut empty = ViewRegistry::new(&base, tx.clone());
        register_views(&mut empty, &tmp);
        assert!(empty.view_ids().is_empty());
        assert_eq!(empty.index_base(), base.as_path());
        assert_eq!(
            empty.run("x", None).await.unwrap_err(),
            ViewError::NotFound("x".into())
        );

        let mut reg = ViewRegistry::new(&base, tx);
        reg.register(Box::new(Fake)).unwrap();
        assert_eq!(reg.view_ids(), vec!["fake.count".to_owned()]);
        let r = reg.run("fake.count", Some("a.md".into())).await.unwrap();
        assert_eq!(r.0["files"], 1);
        assert_eq!(r.0["this"], "a.md");

        std::fs::write(vault.join("b.md"), "# b\n").unwrap();
        let g = index.rebuild(&vault).unwrap();
        reg.notify_generation(g);
        let n = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            n,
            Notification::ViewsInvalidated {
                view_ids: vec!["fake.count".into()]
            }
        );
        let r = reg.run("fake.count", None).await.unwrap();
        assert_eq!(r.0["files"], 2);
        assert_eq!(r.0["gen"], g);
        drop(index);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
