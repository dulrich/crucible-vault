mod common;

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use common::{Fx, h};
use cv_vault::{EventKind, Origin, VaultEvent, WatchConfig, spawn_writer_with_watcher};

const DEB: Duration = Duration::from_millis(50);

fn cfg() -> WatchConfig {
    WatchConfig {
        debounce: DEB,
        echo_ttl: Duration::from_secs(2),
    }
}

/// Every event arriving within `window` (a bounded collection window, not an assertion sleep).
fn collect(rx: &Receiver<VaultEvent>, window: Duration) -> Vec<VaultEvent> {
    let end = Instant::now() + window;
    let mut out = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) => out.push(e),
            Err(_) => break,
        }
    }
    out
}

/// Waits (≤ 2 s) for the first event, then collects the quiet tail.
fn first_then_tail(rx: &Receiver<VaultEvent>) -> Vec<VaultEvent> {
    let mut out = Vec::new();
    if let Ok(e) = rx.recv_timeout(Duration::from_secs(2)) {
        out.push(e);
        out.extend(collect(rx, DEB * 6));
    }
    out
}

#[test]
fn initial_scan_emits_nothing() {
    let fx = Fx::new(&[
        ("a.md", "A"),
        ("f/b.md", "[[a]]"),
        ("_resources/x.png", "png"),
    ]);
    let (_h, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    let evs = collect(&rx, DEB * 8);
    assert!(evs.is_empty(), "{evs:?}");
}

#[test]
fn daemon_write_is_echo_external_write_is_applied() {
    let fx = Fx::new(&[("a.md", "A"), ("b.md", "B")]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    wh.write("a.md", b"A2 #t".to_vec(), Some(h("A"))).unwrap();
    let evs = first_then_tail(&rx);
    assert!(evs.iter().all(|e| e.origin == Origin::Daemon), "{evs:?}");
    assert!(evs.len() <= 1, "{evs:?}");
    assert!(evs.iter().all(|e| !e.path.contains(".cv-tmp-")), "{evs:?}");

    std::fs::write(fx.vault.path().join("b.md"), "B2 #ext").unwrap();
    let evs = first_then_tail(&rx);
    assert_eq!(
        evs,
        vec![VaultEvent {
            path: "b.md".into(),
            kind: EventKind::Modified,
            origin: Origin::External
        }]
    );
    let tags = wh
        .call(|w| w.index().query().tags_of("b.md").unwrap())
        .unwrap();
    assert_eq!(tags.len(), 1);

    std::fs::write(fx.vault.path().join("n.md"), "new").unwrap();
    let evs = first_then_tail(&rx);
    assert_eq!(
        evs,
        vec![VaultEvent {
            path: "n.md".into(),
            kind: EventKind::Created,
            origin: Origin::External
        }]
    );
    assert!(
        wh.call(|w| w.index().query().file("n.md").unwrap().is_some())
            .unwrap()
    );

    // Touch without content change: dropped by the hash dedupe.
    let f = std::fs::File::options()
        .append(true)
        .open(fx.vault.path().join("n.md"))
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(f);
    assert!(collect(&rx, DEB * 6).is_empty());

    std::fs::remove_file(fx.vault.path().join("n.md")).unwrap();
    let evs = first_then_tail(&rx);
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert_eq!(evs[0].kind, EventKind::Deleted { replaced_by: None });
    assert_eq!(evs[0].origin, Origin::External);
    assert!(
        wh.call(|w| w.index().query().file("n.md").unwrap().is_none())
            .unwrap()
    );
}

#[test]
fn daemon_rename_and_delete_echo_and_external_rename() {
    let fx = Fx::new(&[("a.md", "A"), ("b.md", "B")]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    wh.rename("a.md", "c.md", h("A")).unwrap();
    wh.delete("b.md", h("B")).unwrap();
    let mut evs = first_then_tail(&rx);
    evs.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        evs,
        vec![
            VaultEvent {
                path: "b.md".into(),
                kind: EventKind::Deleted { replaced_by: None },
                origin: Origin::Daemon
            },
            VaultEvent {
                path: "c.md".into(),
                kind: EventKind::Renamed {
                    from: "a.md".into()
                },
                origin: Origin::Daemon
            },
        ]
    );

    std::fs::rename(fx.vault.path().join("c.md"), fx.vault.path().join("d.md")).unwrap();
    let evs = first_then_tail(&rx);
    assert_eq!(
        evs,
        vec![VaultEvent {
            path: "d.md".into(),
            kind: EventKind::Renamed {
                from: "c.md".into()
            },
            origin: Origin::External
        }]
    );
    assert!(
        wh.call(|w| w.index().query().file("d.md").unwrap().is_some())
            .unwrap()
    );
    assert!(
        wh.call(|w| w.index().query().file("c.md").unwrap().is_none())
            .unwrap()
    );
}

#[test]
fn tmp_names_never_surface() {
    let fx = Fx::new(&[]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    for i in 0..5 {
        wh.create(&format!("n{i}.md"), b"x".to_vec()).unwrap();
    }
    std::fs::write(fx.vault.path().join(".cv-tmp-foreign"), "x").unwrap();
    let evs = first_then_tail(&rx);
    assert!(
        evs.iter()
            .all(|e| !e.path.contains(".cv-tmp-") && e.origin == Origin::Daemon),
        "{evs:?}"
    );
    assert!(
        !common::all_names(fx.vault.path())
            .iter()
            .any(|n| n.starts_with(".cv-tmp-") && n != ".cv-tmp-foreign")
    );
}

#[test]
fn daemon_folder_rename_is_one_renamed_per_file() {
    let fx = Fx::new(&[("f/a.md", "A"), ("f/b.md", "B")]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    wh.rename_folder("f", "g").unwrap();
    let mut evs = first_then_tail(&rx);
    evs.sort_by(|a, b| a.path.cmp(&b.path));
    let want: Vec<VaultEvent> = ["a", "b"]
        .iter()
        .map(|n| VaultEvent {
            path: format!("g/{n}.md"),
            kind: EventKind::Renamed {
                from: format!("f/{n}.md"),
            },
            origin: Origin::Daemon,
        })
        .collect();
    assert_eq!(evs, want);
}

#[test]
fn expired_move_echo_degrades_to_delete_and_create() {
    let fx = Fx::new(&[("a.md", "A")]);
    let quick = WatchConfig {
        debounce: DEB,
        echo_ttl: Duration::from_millis(1),
    };
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), quick).unwrap();
    wh.rename("a.md", "c.md", h("A")).unwrap();
    let mut evs = first_then_tail(&rx);
    evs.sort_by(|a, b| a.path.cmp(&b.path));
    // Echoes expired: both halves surface (as external), nothing is lost or paired.
    assert_eq!(
        evs,
        vec![
            VaultEvent {
                path: "a.md".into(),
                kind: EventKind::Deleted { replaced_by: None },
                origin: Origin::External
            },
            VaultEvent {
                path: "c.md".into(),
                kind: EventKind::Created,
                origin: Origin::External
            },
        ]
    );
    assert!(
        wh.call(|w| w.index().query().file("c.md").unwrap().is_some())
            .unwrap()
    );
}

#[test]
fn delete_replaced_carries_replaced_by_on_the_event() {
    let fx = Fx::new(&[("a.md", "A"), ("b.md", "B")]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    wh.call(|w| w.delete_replaced("a.md", h("A"), "b.md"))
        .unwrap()
        .unwrap();
    let evs = first_then_tail(&rx);
    assert_eq!(
        evs,
        vec![VaultEvent {
            path: "a.md".into(),
            kind: EventKind::Deleted {
                replaced_by: Some("b.md".into())
            },
            origin: Origin::Daemon
        }]
    );
}
