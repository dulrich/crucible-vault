//! Plan round-16 WP-3: an open Reading tab keeps its localized images across a daemon move or
//! an in-place body rewrite, with normal tick processing and no reopen.

use cv_protocol::{ChangeKind, Notification, Origin};

use super::*;
use crate::message::StatusMsg;

const NOTE: &str = "Clippings/Clip.md";
const FILED: &str = "Articles/Clip.md";

type Env = (tempfile::TempDir, Arc<FakeDaemon>, Arc<dyn DaemonClient>);

fn fake_env(files: &[(&str, &[u8])]) -> Env {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    for (p, body) in files {
        let abs = vault.join(p);
        std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
        std::fs::write(abs, body).expect("write");
    }
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    (tmp, d, dc)
}

/// Feeds one notification and runs a Reading tick, as the live event loop interleaves them.
fn feed(app: &mut App, n: Notification) {
    let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
}

fn changed(path: &str, kind: ChangeKind, hash: Option<String>) -> Notification {
    Notification::FileChanged {
        path: path.to_owned(),
        kind,
        hash,
        origin: Origin::Daemon,
        from: None,
        replaced_by: None,
    }
}

/// Writes an attachment as `localize` does and feeds its `Created` + generation.
fn attach(app: &mut App, d: &FakeDaemon, note: &str, bytes: &[u8]) -> String {
    let key = d.write_attachment(note, "png", bytes).expect("attach");
    let h = crate::daemon::ContentHash::of(bytes);
    feed(
        app,
        changed(&key, ChangeKind::Created, Some(h.as_str().to_owned())),
    );
    let n = d.generation().expect("gen");
    feed(app, Notification::IndexGeneration { n });
    key
}

/// Rewrites `path`'s body in place (localize / supersede) and feeds `Modified` + generation.
fn rewrite(app: &mut App, d: &FakeDaemon, path: &str, body: &str) {
    let (_, base) = d.read_file(path).expect("read");
    let h = d.write_note(path, body, &base).expect("write");
    feed(
        app,
        changed(path, ChangeKind::Modified, Some(h.as_str().to_owned())),
    );
    let n = d.generation().expect("gen");
    feed(app, Notification::IndexGeneration { n });
}

fn body_for(keys: &[&str]) -> String {
    let mut s = String::from("# Clip\n\n");
    for k in keys {
        s.push_str("![](");
        s.push_str(&k.replace('_', "%5F"));
        s.push_str(")\n\n");
    }
    s
}

/// Every image of `path`'s Doc resolves (exactly `n` images listed).
fn assert_resolved(app: &App, path: &str, n: usize) {
    let doc = app.reading.doc(&app.docs, path).expect("doc rendered");
    assert_eq!(doc.images.len(), n, "images listed: {:?}", doc.images);
    for (src, key) in &doc.images {
        assert!(key.is_some(), "{src} unresolved in {:?}", doc.images);
    }
}

/// The image sensor of the active tab showed `key` and its load landed (what the view does
/// once; an iced sensor keyed by `key` never re-fires `on_show` while it stays in the tree).
fn shown_and_loaded(app: &mut App, path: &str, key: &str) {
    let view = app.active_tab().map(|t| t.id).expect("tab");
    let owner = crate::image_cache::view_owner(path, view);
    assert!(app.reading.images.show(&owner, key));
    assert!(
        app.reading
            .images
            .loaded(key, crate::image_cache::decode(&png(8, 4), 64).ok())
    );
    assert!(app.reading.images.contains(key));
}

/// Every loaded image of the tab is still resident (the unchanged sensor will not ask again).
fn assert_resident(app: &App, keys: &[&str]) {
    for k in keys {
        assert!(app.reading.images.contains(k), "{k} evicted from the cache");
    }
}

fn open_reading(dc: &Arc<dyn DaemonClient>, path: &str) -> App {
    let mut app = app_on(dc, path);
    reading(&mut app, path, ViewMode::Reading);
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    app
}

/// (a) Old order: attachments created and the body localized, then the daemon move.
#[test]
fn images_survive_a_daemon_move_after_localize() {
    let (_tmp, d, dc) = fake_env(&[(NOTE, b"# Clip\n"), ("Articles/x.md", b"x\n")]);
    let mut app = open_reading(&dc, NOTE);
    let k1 = attach(&mut app, &d, NOTE, &png(8, 4));
    let k2 = attach(&mut app, &d, NOTE, &png(4, 8));
    rewrite(&mut app, &d, NOTE, &body_for(&[&k1, &k2]));
    assert_resolved(&app, NOTE, 2);
    shown_and_loaded(&mut app, NOTE, &k1);
    shown_and_loaded(&mut app, NOTE, &k2);
    for n in d.daemon_move(NOTE, FILED).expect("move") {
        feed(&mut app, n);
    }
    assert!(app.doc(NOTE).is_none());
    assert_resolved(&app, FILED, 2);
    assert_resident(&app, &[&k1, &k2]);
}

/// (b) WP-5's order: the daemon move first, then attachments created at vault paths and the
/// moved note's body rewritten in place.
#[test]
fn images_survive_localize_after_a_daemon_move() {
    let (_tmp, d, dc) = fake_env(&[(NOTE, b"# Clip\n"), ("Articles/x.md", b"x\n")]);
    let mut app = open_reading(&dc, NOTE);
    for n in d.daemon_move(NOTE, FILED).expect("move") {
        feed(&mut app, n);
    }
    let k1 = attach(&mut app, &d, FILED, &png(8, 4));
    let k2 = attach(&mut app, &d, FILED, &png(4, 8));
    rewrite(&mut app, &d, FILED, &body_for(&[&k1, &k2]));
    assert_resolved(&app, FILED, 2);
    shown_and_loaded(&mut app, FILED, &k1);
    // A later in-place rewrite (describe, tidy) keeps the shown image resident.
    rewrite(&mut app, &d, FILED, &(body_for(&[&k1, &k2]) + "more\n"));
    assert_resolved(&app, FILED, 2);
    assert_resident(&app, &[&k1]);
}

/// (c) A supersede target whose body is replaced in place by the clip's (already localized
/// under the clip), then localized again in place.
#[test]
fn images_survive_a_supersede_body_replacement() {
    let (_tmp, d, dc) = fake_env(&[(FILED, b"# Old\n"), (NOTE, b"# Clip\n")]);
    let mut app = open_reading(&dc, FILED);
    let k1 = attach(&mut app, &d, NOTE, &png(8, 4));
    rewrite(&mut app, &d, FILED, &body_for(&[&k1]));
    assert_resolved(&app, FILED, 1);
    shown_and_loaded(&mut app, FILED, &k1);
    let k2 = attach(&mut app, &d, FILED, &png(4, 8));
    rewrite(&mut app, &d, FILED, &body_for(&[&k1, &k2]));
    assert_resolved(&app, FILED, 2);
    assert_resident(&app, &[&k1]);
}
