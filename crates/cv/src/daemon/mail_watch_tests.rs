//! Maildir watch tests (plan intake-followups-mail-youtube WP-3): a temp Maildir, shortened
//! timings, an injected enqueue counter (no daemon, no live paths).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cv_actions::pipelines::MailPipeline;
use tokio::sync::watch;

use super::{Timings, is_message, run, wanted};

const T: Timings = Timings {
    quiet: Duration::from_millis(400),
    check: Duration::from_millis(100),
};

struct Rig {
    count: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

impl Rig {
    fn start(root: Option<PathBuf>) -> Self {
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let cfg = Arc::new(Mutex::new(root));
        let (stop, rx) = watch::channel(false);
        let join = tokio::spawn(run(
            move || {
                cfg.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            },
            move || {
                c.fetch_add(1, Ordering::SeqCst);
            },
            T,
            rx,
        ));
        Self { count, stop, join }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    async fn finish(self) -> usize {
        let _ = self.stop.send(true);
        tokio::time::timeout(Duration::from_secs(5), self.join)
            .await
            .expect("loop stops")
            .expect("loop joins");
        self.count.load(Ordering::SeqCst)
    }
}

fn maildir() -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(d.path().join("new")).expect("new/");
    d
}

async fn settle_in() {
    // Let the loop build its watcher before files appear.
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_drop_enqueues_once() {
    let d = maildir();
    let rig = Rig::start(Some(d.path().to_path_buf()));
    settle_in().await;
    std::fs::write(d.path().join("a.eml"), b"Subject: x\r\n\r\nbody").expect("write");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(rig.finish().await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn burst_of_five_enqueues_once() {
    let d = maildir();
    let rig = Rig::start(Some(d.path().to_path_buf()));
    settle_in().await;
    for i in 0..5 {
        std::fs::write(d.path().join("new").join(format!("msg{i}")), b"x").expect("write");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(rig.finish().await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_off_enqueues_nothing() {
    let d = maildir();
    let cfg = MailPipeline {
        enabled: true,
        maildir: d.path().display().to_string(),
        watch: false,
        ..MailPipeline::default()
    };
    let rig = Rig::start(wanted(&cfg));
    settle_in().await;
    std::fs::write(d.path().join("a.eml"), b"x").expect("write");
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(rig.finish().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_maildir_keeps_running() {
    let d = tempfile::tempdir().expect("tempdir");
    let rig = Rig::start(Some(d.path().join("absent")));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!rig.join.is_finished(), "loop survives a missing maildir");
    assert_eq!(rig.finish().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn growing_file_waits_until_settled() {
    let d = maildir();
    let rig = Rig::start(Some(d.path().to_path_buf()));
    settle_in().await;
    let p = d.path().join("new").join("slow");
    let mut f = std::fs::File::create(&p).expect("create");
    // Keep appending for well past the quiet window.
    for _ in 0..12 {
        f.write_all(b"chunk").expect("append");
        f.flush().expect("flush");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(rig.count(), 0, "not enqueued while still being written");
    drop(f);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(rig.finish().await, 1);
}

#[test]
fn message_paths_and_wanted() {
    let root = Path::new("/m");
    assert!(is_message(root, Path::new("/m/a.eml")));
    assert!(!is_message(root, Path::new("/m/a.txt")));
    assert!(!is_message(root, Path::new("/m/.a.eml")));
    assert!(is_message(root, Path::new("/m/new/1234.host")));
    assert!(!is_message(root, Path::new("/m/new/.tmp")));
    assert!(!is_message(root, Path::new("/m/cur/1234.host")));
    let mut cfg = MailPipeline::default();
    assert!(cfg.watch, "watch defaults on");
    assert_eq!(wanted(&cfg), None, "disabled");
    cfg.enabled = true;
    assert_eq!(wanted(&cfg), None, "empty maildir");
    cfg.maildir = "/m".into();
    assert_eq!(wanted(&cfg), Some(PathBuf::from("/m")));
}
