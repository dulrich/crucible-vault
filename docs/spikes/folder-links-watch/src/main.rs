//! Throwaway spike (plan folder-links-follow WP-0): notify 8.2.0 watch ownership for folder links.
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

type Rx = Receiver<notify::Result<Event>>;

fn watcher(follow: bool) -> (RecommendedWatcher, Rx) {
    let (tx, rx) = channel();
    let w = RecommendedWatcher::new(
        move |e| {
            let _ = tx.send(e);
        },
        Config::default().with_follow_symlinks(follow),
    )
    .unwrap();
    (w, rx)
}

/// Drains events for `ms`, returning (paths seen, rescan count).
fn drain(rx: &Rx, ms: u64) -> (Vec<String>, usize) {
    let end = Instant::now() + Duration::from_millis(ms);
    let (mut paths, mut rescans) = (Vec::new(), 0);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Ok(ev)) => {
                if ev.need_rescan() {
                    rescans += 1;
                }
                for p in ev.paths {
                    paths.push(format!("{:?}:{}", ev.kind, p.display()));
                }
            }
            Ok(Err(e)) => paths.push(format!("ERR:{e}")),
            Err(_) => break,
        }
    }
    (paths, rescans)
}

fn touch(p: &Path) {
    fs::write(p, b"x").unwrap();
}

fn fresh(base: &Path, name: &str) -> PathBuf {
    let d = base.join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn rel(paths: &[String], base: &Path) -> Vec<String> {
    let b = base.display().to_string();
    let mut v: Vec<String> = paths
        .iter()
        .filter(|p| !p.starts_with("Access(Open"))
        .map(|p| p.replace(&b, "")).collect();
    v.sort();
    v.dedup();
    v
}

fn p0_alias_wd(base: &Path) {
    let b = fresh(base, "p0");
    let d = b.join("ext/d");
    fs::create_dir_all(&d).unwrap();
    symlink(&d, b.join("alias")).unwrap();
    let (mut w, rx) = watcher(false);
    w.watch(&d, RecursiveMode::NonRecursive).unwrap();
    w.watch(&b.join("alias"), RecursiveMode::NonRecursive).unwrap();
    touch(&d.join("f1"));
    println!("P0 two non-recursive registrations of one inode (canonical, then alias); write f1 -> {:?}", rel(&drain(&rx, 300).0, &b));
    w.unwatch(&b.join("alias")).unwrap();
    touch(&d.join("f2"));
    println!("P0 after unwatch(alias) only; write f2 (canonical still 'registered' in notify) -> {:?}", rel(&drain(&rx, 300).0, &b));
}

fn p1_overlap(base: &Path) {
    let b = fresh(base, "p1");
    let t = b.join("ext/t");
    fs::create_dir_all(t.join("sub")).unwrap();
    let v = b.join("vault");
    fs::create_dir_all(&v).unwrap();
    symlink(&t, v.join("L1")).unwrap();
    symlink(t.join("sub"), v.join("L2")).unwrap();
    for (label, drop) in [("unwatch(L2)", "L2"), ("unwatch(L1)", "L1")] {
        let (mut w, rx) = watcher(true);
        w.watch(&v.join("L1"), RecursiveMode::Recursive).unwrap();
        w.watch(&v.join("L2"), RecursiveMode::Recursive).unwrap();
        touch(&t.join("sub/a"));
        touch(&t.join("top"));
        println!("P1 recursive L1->t and L2->t/sub, write t/sub/a + t/top -> {:?}", rel(&drain(&rx, 300).0, &b));
        w.unwatch(&v.join(drop)).unwrap();
        touch(&t.join("sub/b"));
        touch(&t.join("top2"));
        println!("P1 after {label}; write t/sub/b + t/top2 -> {:?}", rel(&drain(&rx, 300).0, &b));
    }
    // Today's daemon shape: one recursive root watch, follow_symlinks default (true).
    let (mut w, rx) = watcher(true);
    w.watch(&v, RecursiveMode::Recursive).unwrap();
    touch(&t.join("sub/c"));
    println!("P1 daemon shape (root recursive, follow=true), write t/sub/c -> {:?}", rel(&drain(&rx, 300).0, &b));
}

/// 39 roots, `dirs` dirs total, `files` files, depth up to 4.
fn build_links(b: &Path, roots: usize, dirs: usize, files: usize) -> Vec<PathBuf> {
    let mut all = Vec::new();
    for r in 0..roots {
        let root = b.join(format!("ext/r{r}"));
        fs::create_dir_all(&root).unwrap();
        all.push(root);
    }
    let mut i = 0;
    while all.len() < dirs {
        let parent = all[i % all.len()].clone();
        let depth = parent.strip_prefix(b.join("ext")).unwrap().components().count();
        if depth < 4 {
            let d = parent.join(format!("d{i}"));
            fs::create_dir_all(&d).unwrap();
            all.push(d);
        }
        i += 1;
    }
    for f in 0..files {
        touch(&all[f % all.len()].join(format!("n{f}.md")));
    }
    let v = b.join("vault");
    fs::create_dir_all(&v).unwrap();
    for r in 0..roots {
        symlink(b.join(format!("ext/r{r}")), v.join(format!("link{r}"))).unwrap();
    }
    all
}

fn p2_disjoint(base: &Path) {
    for (dirs, files) in [(139usize, 1197usize), (1390, 11970)] {
        let b = fresh(base, &format!("p2-{dirs}"));
        let all = build_links(&b, 39, dirs, files);
        let (mut w, rx) = watcher(false);
        w.watch(&b.join("vault"), RecursiveMode::Recursive).unwrap();
        let t0 = Instant::now();
        // The repo-owned set: a walk from each link (canonical dirs) + one non-recursive watch each.
        let mut n = 0;
        for r in 0..39 {
            let mut stack = vec![fs::canonicalize(b.join(format!("vault/link{r}"))).unwrap()];
            while let Some(d) = stack.pop() {
                w.watch(&d, RecursiveMode::NonRecursive).unwrap();
                n += 1;
                for e in fs::read_dir(&d).unwrap() {
                    let e = e.unwrap();
                    if e.file_type().unwrap().is_dir() {
                        stack.push(e.path());
                    }
                }
            }
        }
        let el = t0.elapsed();
        println!("P2 dirs={dirs} files={files}: {n} non-recursive watches registered (walk+watch) in {:.1} ms", el.as_secs_f64() * 1e3);
        let deep = all.iter().max_by_key(|p| p.components().count()).unwrap().clone();
        touch(&deep.join("probe.md"));
        fs::create_dir(deep.join("newsub")).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        touch(&deep.join("newsub/inner.md"));
        let got = rel(&drain(&rx, 300).0, &b);
        println!("P2 write deep/probe.md, mkdir deep/newsub, write newsub/inner.md -> {got:?}");
        let t1 = Instant::now();
        for d in &all {
            w.unwatch(d).unwrap();
        }
        println!("P2 unwatch all {} in {:.1} ms", all.len(), t1.elapsed().as_secs_f64() * 1e3);
    }
}

fn p3_lifecycle(base: &Path) {
    let b = fresh(base, "p3");
    let v = b.join("vault");
    fs::create_dir_all(&v).unwrap();
    fs::create_dir_all(b.join("ext")).unwrap();
    symlink(b.join("ext/missing"), v.join("dangling")).unwrap();
    let (mut w, rx) = watcher(false);
    w.watch(&v, RecursiveMode::Recursive).unwrap();
    fs::create_dir(b.join("ext/missing")).unwrap();
    touch(&b.join("ext/missing/a.md"));
    println!("P3a dangling target appears (root watch only) -> {:?}", rel(&drain(&rx, 300).0, &b));
    fs::remove_dir_all(b.join("ext/missing")).unwrap();
    drain(&rx, 100);
    w.watch(&b.join("ext"), RecursiveMode::NonRecursive).unwrap();
    fs::create_dir(b.join("ext/missing")).unwrap();
    println!("P3a with a non-recursive watch on the target's nearest existing ancestor -> {:?}", rel(&drain(&rx, 300).0, &b));
    w.unwatch(&b.join("ext")).unwrap();

    let t = b.join("ext/t");
    fs::create_dir_all(&t).unwrap();
    symlink(&t, v.join("live")).unwrap();
    drain(&rx, 100);
    w.watch(&t, RecursiveMode::NonRecursive).unwrap();
    touch(&t.join("x.md"));
    drain(&rx, 200);
    fs::remove_dir_all(&t).unwrap();
    println!("P3b watched target deleted -> {:?}", rel(&drain(&rx, 300).0, &b));
    fs::create_dir(&t).unwrap();
    touch(&t.join("y.md"));
    println!("P3b recreated + write y.md (no re-watch) -> {:?}", rel(&drain(&rx, 300).0, &b));
    println!("P3b re-watch(t) -> {:?}", w.watch(&t, RecursiveMode::NonRecursive).map_err(|e| e.to_string()));
    touch(&t.join("z.md"));
    println!("P3b after re-watch, write z.md -> {:?}", rel(&drain(&rx, 300).0, &b));
    println!("P3b unwatch(t) -> {:?}", w.unwatch(&t).map_err(|e| e.to_string()));
}

fn p4_overflow(base: &Path) {
    let b = fresh(base, "p4");
    let v = b.join("vault");
    fs::create_dir_all(v.join("flood")).unwrap();
    let ext = b.join("ext/t");
    fs::create_dir_all(&ext).unwrap();
    let (tx, rx) = channel();
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let g = gate.clone();
    let mut w = RecommendedWatcher::new(
        move |e| {
            while g.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = tx.send(e);
        },
        Config::default().with_follow_symlinks(false),
    )
    .unwrap();
    w.watch(&v, RecursiveMode::Recursive).unwrap();
    w.watch(&ext, RecursiveMode::NonRecursive).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    while rx.try_recv().is_ok() {}
    // Armed after setup: watch() round-trips through the event loop the gate blocks.
    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    touch(&v.join("first")); // blocks the notify thread in the handler
    std::thread::sleep(Duration::from_millis(100));
    let max: usize = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events").unwrap().trim().parse().unwrap();
    for i in 0..(max + 2000) {
        touch(&v.join(format!("flood/f{i}")));
    }
    fs::create_dir(v.join("newdir")).unwrap(); // its IN_CREATE is past the overflow
    touch(&ext.join("lost.md"));
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    let (paths, rescans) = drain(&rx, 3000);
    let lost = paths.iter().any(|p| p.ends_with("lost.md"));
    let nd = paths.iter().any(|p| p.ends_with("/newdir"));
    println!("P4 queue max {max}; wrote {} files + mkdir newdir + ext write: {} event paths, {rescans} Rescan, newdir create seen={nd}, ext lost.md seen={lost}", max + 2000, paths.len());
    touch(&v.join("newdir/after.md"));
    touch(&ext.join("after.md"));
    let got = rel(&drain(&rx, 500).0, &b);
    println!("P4 after overflow: write newdir/after.md + ext/after.md -> {got:?}");
}

fn main() {
    let base = PathBuf::from(std::env::args().nth(1).expect("base dir"));
    fs::create_dir_all(&base).unwrap();
    let base = fs::canonicalize(&base).unwrap();
    match std::env::args().nth(2).as_deref() {
        Some("p0") => p0_alias_wd(&base),
        Some("p1") => p1_overlap(&base),
        Some("p2") => p2_disjoint(&base),
        Some("p3") => p3_lifecycle(&base),
        Some("p4") => p4_overflow(&base),
        _ => {
            p0_alias_wd(&base);
            p1_overlap(&base);
            p2_disjoint(&base);
            p3_lifecycle(&base);
            p4_overflow(&base);
        }
    }
}
