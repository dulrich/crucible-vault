//! The view host (plan WP-8): one [`views::ViewHost`] on its **own** thread with its
//! **own** read-only [`cv_index::Reader`] (WAL reader, separate from the writer thread), so a view
//! never blocks indexing. Queries and invalidation signals are typed [`Cmd`]s on one channel, so
//! they are processed in the order they were sent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cv_actions::ConfigSet;
use cv_actions::pipelines::{SerialStatus, SourcesConfig};
use cv_protocol::{Notification, ViewResult, ViewSelect, ViewSort};
use tokio::sync::{broadcast, oneshot};

use crate::sync::lock;

/// A command for the view-host thread.
enum Cmd {
    /// Evaluate `view_id` (`this_path`: the note the view is embedded in).
    Run {
        view_id: String,
        this_path: Option<String>,
        sort: Option<ViewSort>,
        /// Plan round-13 WP-4: the host's in-place selection.
        select: Option<ViewSelect>,
        reply: oneshot::Sender<Result<ViewResult, String>>,
    },
    /// A new index generation committed: reopen the reader.
    Generation(u64),
    /// `CV_CONFIG_DIR` changed: reload definitions and take the reloaded typed settings.
    Config(PathBuf, ViewSettings),
    /// The typed settings (the runner's first config load): no definitions reload.
    Settings(ViewSettings),
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
    /// [`ViewRegistry::notify_config`]; typed settings via [`ViewRegistry::set_config`]); `state_db` feeds `source = "tracker_items"` views.
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
        // Defaults until the runner's config arrives ([`ViewRegistry::set_config`]).
        let settings = ViewSettings::of(&ConfigSet::new());
        std::thread::Builder::new()
            .name("cv-view".into())
            .spawn(move || {
                serve(host, settings, &rx, &base, &events, &thread_ids);
            })?;
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
        sort: Option<ViewSort>,
        select: Option<ViewSelect>,
    ) -> Result<ViewResult, ViewError> {
        if !lock(&self.ids).iter().any(|i| i == view_id) {
            return Err(ViewError::NotFound(view_id.to_owned()));
        }
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Run {
            view_id: view_id.to_owned(),
            this_path,
            sort,
            select,
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

    /// The config dir changed; `config` is the runner's reloaded `ConfigSet` (plan
    /// tn-review-2026-10-26-remediation WP-R6, D6: the host never re-parses config files).
    pub fn notify_config(&self, dir: &Path, config: &ConfigSet) {
        self.signal(Cmd::Config(dir.to_path_buf(), ViewSettings::of(config)));
    }

    /// Pushes `config`'s typed settings (the runner's first load) without reloading definitions.
    pub fn set_config(&self, config: &ConfigSet) {
        self.signal(Cmd::Settings(ViewSettings::of(config)));
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

/// What the view host takes from the runner's [`ConfigSet`] (plan tn-review-2026-10-26-remediation
/// WP-R6, D6): typed values only, so the host shares `ConfigSet`'s defaults and its
/// keep-the-last-valid-value policy with tidy and every other consumer.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewSettings {
    /// `sources.toml`.
    pub sources: SourcesConfig,
    /// `vault.toml` `[trash] retention_days` (the value tidy expires by).
    pub trash_retention_days: u32,
    /// `captures.toml` `[periods.day].folder` ([`DEFAULT_DAY_FOLDER`] when unset).
    pub day_folder: String,
}

impl ViewSettings {
    /// The typed settings of `c`.
    #[must_use]
    pub fn of(c: &ConfigSet) -> Self {
        Self {
            sources: c.sources().clone(),
            trash_retention_days: c.vault().trash.retention_days,
            day_folder: day_folder_of(c.table("captures.toml")),
        }
    }
}

/// The default day-note folder when `captures.toml` configures no `[periods.day].folder`.
const DEFAULT_DAY_FOLDER: &str = "daily/day";

/// The day folder from a parsed `captures.toml` (`[periods.day].folder`, slashes trimmed, the
/// same key `materialize`'s period settings read); [`DEFAULT_DAY_FOLDER`] when unset or blank.
fn day_folder_of(captures: &toml::Table) -> String {
    captures
        .get("periods")
        .and_then(toml::Value::as_table)
        .and_then(|p| p.get("day"))
        .and_then(toml::Value::as_table)
        .and_then(|d| d.get("folder"))
        .and_then(toml::Value::as_str)
        .map(|f| f.trim().trim_matches('/'))
        .filter(|f| !f.is_empty())
        .map_or_else(|| DEFAULT_DAY_FOLDER.to_owned(), str::to_owned)
}

/// One day-folder note as the source-row counts read it.
#[derive(Default)]
struct Entry {
    authors: Vec<String>,
    source: String,
    publication: String,
    read: bool,
    created: Option<views::value::Value>,
    ctime: i64,
}

/// The `source = "sources"` records and the `file.serial` match set (plan round-11-feedback-todos
/// Decision 5), over the `daily/day` notes of `q`:
/// - one record per `[[serial]]` row plus per `[[blog]]`/`[[newsletter]]`/`[[youtube]]` row with
///   `works = true` (config order), binding `name`, `publication`, `author` (bare; null when
///   unset), `prefix` (list), `kind` (serial/newsletter: its `kind`; blog/youtube: the row type),
///   `status` (serial: its status; others `active`), `row`, `entries`, `unread` (`read != true`)
///   and `last_update` (latest `created`, else file ctime; null without entries). Serial rows
///   count the notes [`SerialSource::matches`] accepts (the auto-file matcher, shared — never
///   restated as a view expression); other rows count notes whose `publication` equals theirs;
/// - note path → name of the first serial row (config order) matching it.
fn source_data(
    q: &cv_index::Query<'_>,
    cfg: &SourcesConfig,
    day_folder: &str,
) -> Result<(Vec<views::engine::Record>, BTreeMap<String, String>), String> {
    use views::value::Value;
    let ix = |e: cv_index::Error| format!("index: {e}");
    let mut notes: BTreeMap<String, Entry> = q
        .md_files_under(day_folder)
        .map_err(ix)?
        .into_iter()
        .map(|f| {
            (
                f.path,
                Entry {
                    ctime: f.ctime,
                    ..Entry::default()
                },
            )
        })
        .collect();
    for key in ["author", "source", "publication", "read", "created"] {
        for r in q.md_props_with_key(key).map_err(ix)? {
            let Some(e) = notes.get_mut(&r.path) else {
                continue;
            };
            match key {
                "author" => e.authors.extend(cv_index::prop_texts(&r.value)),
                "source" => e.source = cv_index::prop_text(&r.value),
                "publication" => e.publication = cv_index::prop_text(&r.value).trim().to_owned(),
                "read" => e.read = r.value == serde_json::Value::Bool(true),
                _ => {
                    let v = Value::from_json(&r.value);
                    e.created = matches!(v, Value::Date(..)).then_some(v);
                }
            }
        }
    }
    let mut serial_of = BTreeMap::new();
    for (path, e) in &notes {
        let a: Vec<&str> = e.authors.iter().map(String::as_str).collect();
        if let Some(s) = cfg.serial.iter().find(|s| s.matches(&a, &e.source)) {
            serial_of.insert(path.clone(), s.name.clone());
        }
    }
    let counts = |accept: &dyn Fn(&Entry) -> bool| {
        let (mut n, mut unread, mut last) = (0.0, 0.0, None::<Value>);
        for e in notes.values().filter(|e| accept(e)) {
            n += 1.0;
            if !e.read {
                unread += 1.0;
            }
            let d = e
                .created
                .clone()
                .unwrap_or_else(|| views::eval::ms_date(e.ctime));
            if last.as_ref().is_none_or(|l| {
                views::value::partial_cmp(&d, l).is_some_and(std::cmp::Ordering::is_gt)
            }) {
                last = Some(d);
            }
        }
        (n, unread, last.unwrap_or(Value::Null))
    };
    let text = |s: &str| Value::Str(s.to_owned());
    let mut out = Vec::new();
    let mut push = |row: &str,
                    name: &str,
                    publication: &str,
                    author: Option<&str>,
                    prefix: &[String],
                    kind: &str,
                    status: &str,
                    accept: &dyn Fn(&Entry) -> bool| {
        let (n, unread, last) = counts(accept);
        let mut m = BTreeMap::new();
        m.insert("row".to_owned(), text(row));
        m.insert("name".to_owned(), text(name));
        m.insert("publication".to_owned(), text(publication));
        m.insert(
            "author".to_owned(),
            author
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map_or(Value::Null, text),
        );
        m.insert(
            "prefix".to_owned(),
            Value::list(prefix.iter().map(|p| text(p)).collect()),
        );
        m.insert("kind".to_owned(), text(kind));
        m.insert("status".to_owned(), text(status));
        m.insert("entries".to_owned(), Value::Num(n));
        m.insert("unread".to_owned(), Value::Num(unread));
        m.insert("last_update".to_owned(), last);
        out.push(std::sync::Arc::new(m));
    };
    let by_pub = |p: &str| {
        let p = p.trim().to_owned();
        move |e: &Entry| e.publication == p
    };
    for s in &cfg.serial {
        let status = match s.status {
            SerialStatus::Active => "active",
            SerialStatus::Complete => "complete",
            SerialStatus::Ignored => "ignored",
        };
        push(
            "serial",
            &s.name,
            s.publication(),
            s.author.as_deref(),
            &s.prefix,
            &s.kind,
            status,
            &|e: &Entry| {
                let a: Vec<&str> = e.authors.iter().map(String::as_str).collect();
                s.matches(&a, &e.source)
            },
        );
    }
    for b in cfg.blog.iter().filter(|b| b.works) {
        let f = by_pub(b.publication());
        push(
            "blog",
            &b.name,
            b.publication(),
            None,
            &[],
            "blog",
            "active",
            &f,
        );
    }
    for n in cfg.newsletter.iter().filter(|n| n.works) {
        let f = by_pub(n.publication());
        push(
            "newsletter",
            &n.name,
            n.publication(),
            None,
            &[],
            &n.kind,
            "active",
            &f,
        );
    }
    for y in cfg.youtube.iter().filter(|y| y.works) {
        let f = by_pub(y.publication());
        push(
            "youtube",
            &y.channel,
            y.publication(),
            None,
            &[],
            "youtube",
            "active",
            &f,
        );
    }
    Ok((out, serial_of))
}

fn log_notices(host: &views::ViewHost) {
    for n in host.load_notices() {
        tracing::warn!(notice = %n, "view definitions");
    }
}

/// The host loop. `settings` holds the typed config (sources, retention, day folder); the
/// `source = "sources"` rows and the `file.serial` set are recomputed lazily — on the first run of a view that reads them after a
/// generation, notes change or config change ([`views::ViewHost::needs_sources`]).
fn serve(
    mut host: views::ViewHost,
    mut settings: ViewSettings,
    rx: &mpsc::Receiver<Cmd>,
    base: &Path,
    events: &broadcast::Sender<Notification>,
    ids: &Mutex<Vec<String>>,
) {
    host.set_trash_retention_days(settings.trash_retention_days);
    let mut reader: Option<cv_index::Reader> = None;
    let mut sources_stale = true;
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
                sort,
                select,
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
                if sources_stale
                    && host.needs_sources(&view_id)
                    && let Some(r) = reader.as_ref()
                {
                    match source_data(&r.query(), &settings.sources, &settings.day_folder) {
                        Ok((records, serial)) => {
                            host.set_sources(records, serial);
                            sources_stale = false;
                        }
                        Err(e) => tracing::warn!(error = %e, "view host: computing source rows"),
                    }
                }
                let res = match reader.as_ref() {
                    Some(r) => host.run_with_select(
                        r,
                        &view_id,
                        this_path.as_deref(),
                        sort.as_ref(),
                        select.as_ref(),
                    ),
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
                sources_stale = true;
                invalidate(host.on_index_generation(n));
                *lock(ids) = host.view_ids();
            }
            Cmd::NotesChanged => {
                sources_stale = true;
                invalidate(host.view_ids());
            }
            Cmd::StateChanged(table) => invalidate(host.on_state_changed(table)),
            Cmd::Settings(s) => {
                host.set_trash_retention_days(s.trash_retention_days);
                settings = s;
                sources_stale = true;
                invalidate(host.view_ids());
            }
            Cmd::Config(dir, s) => {
                host.set_trash_retention_days(s.trash_retention_days);
                settings = s;
                sources_stale = true;
                invalidate(host.on_config_changed(&dir));
                log_notices(&host);
                *lock(ids) = host.view_ids();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn day_folder_comes_from_the_period_config() {
        let t = |s: &str| super::day_folder_of(&s.parse::<toml::Table>().unwrap());
        assert_eq!(t(""), "daily/day");
        assert_eq!(t("[periods.day]\nformat='YYYY'"), "daily/day");
        assert_eq!(t("[periods.day]\nfolder=' '"), "daily/day");
        assert_eq!(t("[periods.day]\nfolder='/journal/days/'"), "journal/days");
        assert_eq!(t("[periods.week]\nfolder='w'"), "daily/day");
    }

    use super::*;
    use std::time::Duration;

    /// D6 (plan tn-review-2026-10-26-remediation WP-R6): the view host's retention is
    /// `ConfigSet`'s, so after an invalid `vault.toml` edit it keeps the last valid value exactly
    /// as tidy (`config.vault().trash.retention_days`) does.
    #[test]
    fn view_settings_keep_the_last_valid_retention_like_tidy() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = ConfigSet::new();
        assert_eq!(
            ViewSettings::of(&c).trash_retention_days,
            c.vault().trash.retention_days
        );
        std::fs::write(
            dir.path().join("vault.toml"),
            "[trash]\nretention_days = 7\n",
        )
        .unwrap();
        crate::runner::load_config_dir(dir.path(), &mut c);
        assert_eq!(ViewSettings::of(&c).trash_retention_days, 7);
        std::fs::write(
            dir.path().join("vault.toml"),
            "[trash]\nretention_days = \"soon\"\n",
        )
        .unwrap();
        crate::runner::load_config_dir(dir.path(), &mut c);
        assert!(!c.errors().is_empty());
        assert_eq!(
            c.vault().trash.retention_days,
            7,
            "tidy keeps the last valid value"
        );
        assert_eq!(
            ViewSettings::of(&c).trash_retention_days,
            7,
            "and so does the view host"
        );
        assert_eq!(ViewSettings::of(&c).day_folder, "daily/day");
    }

    /// `dir` loaded the way the runner loads it.
    fn loaded(dir: &Path) -> ConfigSet {
        let mut c = ConfigSet::new();
        crate::runner::load_config_dir(dir, &mut c);
        c
    }

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
            empty.run("x", None, None, None).await.unwrap_err(),
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
        assert_eq!(
            names(&reg.run(&id, None, None, None).await.unwrap()),
            ["t1"]
        );
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
            names(&reg.run(&id, None, None, None).await.unwrap())
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
        assert_eq!(
            rows(&reg.run(&id, this.clone(), None, None).await.unwrap()),
            0
        );
        let item = cv_state::TrackerItem {
            source_kind: "youtube".into(),
            source_key: "UC1".into(),
            item_id: "v1".into(),
            title: "T".into(),
            url: "https://y/1".into(),
            first_seen_ms: 1,
            day: "2026-10-17".into(),
            outcome: cv_protocol::TrackerOutcome::Line,
            score: None,
            full_body: None,
            note_path: None,
            source_name: None,
            duration_s: None,
            word_count: None,
            access: None,
            complete: None,
            full_word_count: None,
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
        assert_eq!(rows(&reg.run(&id, this, None, None).await.unwrap()), 1);
        assert!(!state.insert_tracker_item(&item).unwrap());
        // Nothing further was emitted (the duplicate raised no signal).
        let _ = reg.run(&id, None, None, None).await.unwrap();
        assert!(rx.try_recv().is_err());
    }

    /// Signals and queries share one channel, so a state change sent before a
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
        reg.run(&id, None, None, None).await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), invalidated);
        assert!(rx.try_recv().is_err());
        // After: the query's reply precedes the invalidation.
        reg.run(&id, None, None, None).await.unwrap();
        assert!(rx.try_recv().is_err());
        hook("tracker_items");
        let n = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, invalidated);
    }

    /// `(name, entries, unread, last_update)` per row of a sources view.
    fn source_rows(r: &ViewResult) -> Vec<(String, String, String, String)> {
        let cell = |c: &serde_json::Value| match &c["v"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => c["label"].as_str().unwrap_or_default().to_owned(),
            serde_json::Value::Number(n) => n.as_f64().unwrap_or_default().to_string(),
            v => v.to_string(),
        };
        r.0["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["rows"].as_array().cloned().unwrap_or_default())
            .map(|row| {
                let c = &row["cells"];
                (cell(&c[0]), cell(&c[1]), cell(&c[2]), cell(&c[3]))
            })
            .collect()
    }

    /// Plan round-11-feedback-todos WP-4: `source = "sources"` rows are every `[[serial]]` row
    /// plus the `works = true` rows, counted over `daily/day` notes (serial rows by the shared
    /// matcher, others by `publication`); a config change re-computes the rows, an index
    /// generation re-computes the counts, and `file.serial` feeds an index view.
    #[tokio::test]
    async fn sources_rows_follow_config_and_notes() {
        let root = std::env::temp_dir().join(format!("cv-views-sources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tmp = TmpRoot(root);
        let vault = tmp.path().join("vault");
        std::fs::create_dir_all(vault.join("daily/day/2026-10-01")).unwrap();
        let note = |name: &str, fm: &str| {
            std::fs::write(
                vault.join(format!("daily/day/2026-10-01/{name}.md")),
                format!("---\n{fm}\n---\n"),
            )
            .unwrap();
        };
        note(
            "c1",
            "author: \"[[Ann]]\"\nsource: https://s.example/w/1/c1\ncreated: 2026-10-01\nread: true",
        );
        note(
            "c2",
            "author: [Ann]\nsource: https://s.example/w/1/c2\ncreated: 2026-10-03",
        );
        note("off", "author: Ann\nsource: https://other.example/x");
        note("p1", "publication: Feed\ncreated: 2026-09-30");
        let config = tmp.path().join("config");
        std::fs::create_dir_all(config.join("views")).unwrap();
        std::fs::write(
            config.join("views/works.toml"),
            "[[views]]\ntype='table'\nname='All'\nsource='sources'\norder=['name','entries','unread','last_update']\nsort=[{property='name',direction='ASC'}]\n\
             [[views]]\ntype='table'\nname='Matched'\nfilters={and=['file.inFolder(\"daily/day\")','file.serial == \"Saga\"']}\norder=['file.name']\nsort=[{property='file.name',direction='ASC'}]\n\
             [[views]]\ntype='table'\nname='Grouped'\nfilters='file.serial'\norder=['file.name']\nsort=[{property='file.name',direction='ASC'}]\ngroupBy={property='file.serial',direction='ASC'}\n",
        )
        .unwrap();
        let sources = "[[serial]]\nname='Saga'\nauthor='Ann'\nprefix=['https://s.example/w/1/']\n\
                       [[blog]]\nname='Feed'\nlink='https://f.example/rss'\nworks=true\n\
                       [[blog]]\nname='Hidden'\nlink='https://h.example/rss'\n";
        std::fs::write(config.join("sources.toml"), sources).unwrap();
        let base = tmp.path().join("idx").join("index.db");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        let mut index = cv_index::Index::create(&vault, &base).unwrap();
        let (tx, _rx) = broadcast::channel(64);
        let reg = ViewRegistry::start(&config, &base, None, tx).unwrap();
        reg.set_config(&loaded(&config));
        let all = "works#All".to_owned();
        let s = |a: &str, b: &str, c: &str, d: &str| {
            (a.to_owned(), b.to_owned(), c.to_owned(), d.to_owned())
        };
        assert_eq!(
            source_rows(&reg.run(&all, None, None, None).await.unwrap()),
            [
                s("Feed", "1", "1", "2026-09-30"),
                s("Saga", "2", "1", "2026-10-03")
            ]
        );
        let matched = |r: &ViewResult| {
            source_rows(r)
                .into_iter()
                .map(|(n, ..)| n)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            matched(&reg.run("works#Matched", None, None, None).await.unwrap()),
            ["c1", "c2"]
        );
        // `groupBy` accepts the daemon-supplied `file.serial` (plan round-12 WP-4).
        let grouped = reg.run("works#Grouped", None, None, None).await.unwrap();
        let groups = grouped.0["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert!(groups[0]["key"].to_string().contains("Saga"), "{groups:?}");
        assert_eq!(groups[0]["rows"].as_array().unwrap().len(), 2);
        // Plan round-13 WP-4: the selection filters before grouping; only `[[serial]]` rows are
        // selectable (their `name` is the `file.serial` value).
        let pick = |v: &str| {
            Some(ViewSelect {
                property: "file.serial".into(),
                value: v.into(),
            })
        };
        let saga = reg
            .run("works#Grouped", None, None, pick("Saga"))
            .await
            .unwrap();
        assert_eq!(saga.0["groups"][0]["rows"].as_array().unwrap().len(), 2);
        let none = reg
            .run("works#Grouped", None, None, pick("Nope"))
            .await
            .unwrap();
        assert!(none.0["groups"].as_array().unwrap().is_empty(), "{none:?}");
        let rows = reg.run(&all, None, None, None).await.unwrap();
        let selects: Vec<&serde_json::Value> = rows.0["groups"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| &r["select"])
            .collect();
        assert_eq!(
            selects,
            [&serde_json::Value::Null, &serde_json::json!("Saga")]
        );
        // Config change: a second serial row appears and the blog row loses `works`.
        std::fs::write(
            config.join("sources.toml"),
            format!("{sources}[[serial]]\nname='Other'\nprefix=['https://other.example/']\n")
                .replace("works=true\n", ""),
        )
        .unwrap();
        reg.notify_config(&config, &loaded(&config));
        let rows = source_rows(&reg.run(&all, None, None, None).await.unwrap());
        let counts: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|(n, e, u, _)| (n.as_str(), e.as_str(), u.as_str()))
            .collect();
        assert_eq!(counts, [("Other", "1", "1"), ("Saga", "2", "1")]);
        // `off` has no `created`: its last update falls back to the file ctime (a date-time).
        assert!(rows[0].3.contains('T'), "{rows:?}");
        // Notes change: c2 is read now, a new chapter arrives.
        note(
            "c2",
            "author: [Ann]\nsource: https://s.example/w/1/c2\ncreated: 2026-10-03\nread: true",
        );
        note(
            "c3",
            "author: Ann\nsource: https://s.example/w/1/c3\ncreated: 2026-10-04",
        );
        let g = index.rebuild(&vault).unwrap();
        reg.notify_generation(g);
        let rows = source_rows(&reg.run(&all, None, None, None).await.unwrap());
        assert_eq!(rows[1], s("Saga", "3", "1", "2026-10-04"));
    }
}
