//! Writer-authored path events (plan note-path-continuity WP-1, D1): the spike's
//! chained-move repros (`docs/spikes/chained-move/FINDINGS.md` C1/C2), a seeded property test
//! over random writer op sequences, and an external write racing a writer write.

mod common;

use std::collections::BTreeSet;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use common::{Fx, h};
use cv_vault::{
    ContentHash, EventKind, NoteId, Origin, VaultEvent, WatchConfig, WriterHandle,
    spawn_writer_with_watcher,
};

const DEB: Duration = Duration::from_millis(50);

fn cfg() -> WatchConfig {
    WatchConfig {
        debounce: DEB,
        echo_ttl: Duration::from_secs(5),
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

/// An in-memory id store with the daemon sink's semantics (mint-or-get, move, retire).
fn spawn(fx: &Fx) -> (WriterHandle, Receiver<VaultEvent>) {
    let w = fx.live();
    let (wh, rx, _j) = spawn_writer_with_watcher(w, cfg()).unwrap();
    (wh, rx)
}

fn daemon(path: &str, kind: EventKind) -> VaultEvent {
    VaultEvent {
        path: path.into(),
        kind,
        origin: Origin::Daemon,
        id: None,
        hash: None,
    }
}

fn renamed(from: &str, to: &str) -> VaultEvent {
    daemon(to, EventKind::Renamed { from: from.into() })
}

const SRC: &str = "---\ntitle: x\n---\nChapter 66\nbody\n";

#[test]
fn chained_move_then_rename_is_two_renamed() {
    let fx = Fx::new(&[("Clippings/c.md", SRC)]);
    let (wh, rx) = spawn(&fx);
    wh.rename("Clippings/c.md", "day/c.md", h(SRC)).unwrap();
    wh.rename("day/c.md", "day/S - Chapter 66.md", h(SRC))
        .unwrap();
    let evs = collect(&rx, DEB * 8);
    assert_eq!(
        evs,
        vec![
            renamed("Clippings/c.md", "day/c.md"),
            renamed("day/c.md", "day/S - Chapter 66.md"),
        ]
    );
}

#[test]
fn chained_move_rename_then_write_is_two_renamed_and_modified() {
    let fx = Fx::new(&[("Clippings/c.md", SRC)]);
    let (wh, rx) = spawn(&fx);
    wh.rename("Clippings/c.md", "day/c.md", h(SRC)).unwrap();
    wh.rename("day/c.md", "day/S - Chapter 66.md", h(SRC))
        .unwrap();
    let b2 = format!("{SRC}#fanfic\n");
    wh.write("day/S - Chapter 66.md", b2.into_bytes(), Some(h(SRC)))
        .unwrap();
    let evs = collect(&rx, DEB * 8);
    assert_eq!(
        evs,
        vec![
            renamed("Clippings/c.md", "day/c.md"),
            renamed("day/c.md", "day/S - Chapter 66.md"),
            daemon("day/S - Chapter 66.md", EventKind::Modified),
        ]
    );
}

#[test]
fn external_write_racing_a_writer_write_still_surfaces() {
    let fx = Fx::new(&[("a.md", "A")]);
    let (wh, rx) = spawn(&fx);
    wh.write("a.md", b"A2 daemon".to_vec(), Some(h("A")))
        .unwrap();
    std::fs::write(fx.vault.path().join("a.md"), "A3 external edit").unwrap();
    let evs = collect(&rx, DEB * 8);
    assert_eq!(
        evs,
        vec![
            daemon("a.md", EventKind::Modified),
            VaultEvent {
                path: "a.md".into(),
                kind: EventKind::Modified,
                origin: Origin::External,
                id: None,
                hash: None,
            },
        ]
    );
    let indexed = wh
        .call(|w| w.index().query().file("a.md").unwrap().map(|r| r.size))
        .unwrap();
    assert_eq!(indexed, Some("A3 external edit".len() as i64));
}

#[test]
fn external_rename_of_a_just_written_path_is_one_renamed() {
    for gap in [Duration::ZERO, DEB / 2] {
        let fx = Fx::new(&[("a.md", "A")]);
        let (wh, rx) = spawn(&fx);
        wh.rename("a.md", "c.md", h("A")).unwrap();
        std::thread::sleep(gap);
        std::fs::rename(fx.vault.path().join("c.md"), fx.vault.path().join("d.md")).unwrap();
        let evs = collect(&rx, DEB * 8);
        assert_eq!(
            evs,
            vec![
                renamed("a.md", "c.md"),
                VaultEvent {
                    path: "d.md".into(),
                    kind: EventKind::Renamed {
                        from: "c.md".into()
                    },
                    origin: Origin::External,
                    id: None,
                    hash: None,
                },
            ],
            "gap {gap:?}"
        );
        let indexed = wh
            .call(|w| w.index().query().file("d.md").unwrap().is_some())
            .unwrap();
        assert!(indexed, "gap {gap:?}");
    }
}

/// xorshift64*: a deterministic, dependency-free generator.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }
    fn pick<'a>(&mut self, v: &'a [String]) -> Option<&'a String> {
        (!v.is_empty()).then(|| &v[self.below(v.len())])
    }
}

/// The model: live paths → content, plus paths vacated by a move (candidates for reuse).
struct Model {
    live: std::collections::BTreeMap<String, String>,
    vacated: Vec<String>,
    next: usize,
    events: Vec<VaultEvent>,
    /// Model file identity per live path, and the identity each expected event concerns.
    fids: std::collections::BTreeMap<String, usize>,
    ev_fids: Vec<usize>,
    next_fid: usize,
}

impl Model {
    fn ev(&mut self, e: VaultEvent, fid: usize) {
        self.events.push(e);
        self.ev_fids.push(fid);
    }
    fn born(&mut self, p: &str) -> usize {
        self.next_fid += 1;
        self.fids.insert(p.to_string(), self.next_fid);
        self.next_fid
    }
    fn gone(&mut self, p: &str) -> usize {
        self.fids.remove(p).unwrap()
    }
    fn fresh(&mut self, dir: &str) -> String {
        self.next += 1;
        format!("{dir}/n{}.md", self.next)
    }
    fn paths(&self) -> Vec<String> {
        self.live.keys().cloned().collect()
    }
    fn dirs(&self) -> Vec<String> {
        let set: BTreeSet<String> = self
            .live
            .keys()
            .map(|p| p.split('/').next().unwrap().to_string())
            .collect();
        set.into_iter().collect()
    }
    fn hash(&self, p: &str) -> ContentHash {
        ContentHash::of(self.live[p].as_bytes())
    }
    fn mv(&mut self, from: &str, to: &str) {
        let c = self.live.remove(from).unwrap();
        self.live.insert(to.to_string(), c);
        self.vacated.push(from.to_string());
        let fid = self.gone(from);
        self.fids.insert(to.to_string(), fid);
        self.ev(renamed(from, to), fid);
    }
}

/// One random writer op applied to the writer and mirrored in the model.
fn step(rng: &mut Rng, wh: &WriterHandle, m: &mut Model) {
    let dirs = ["d0", "d1", "d2"];
    match rng.below(7) {
        0 => {
            let p = m.fresh(dirs[rng.below(3)]);
            let c = format!("created {p}");
            wh.create(&p, c.clone().into_bytes()).unwrap();
            m.live.insert(p.clone(), c);
            let fid = m.born(&p);
            m.ev(daemon(&p, EventKind::Created), fid);
        }
        1 => {
            let Some(p) = rng.pick(&m.paths()).cloned() else {
                return;
            };
            let c = format!("{} +w{}", m.live[&p], rng.below(1000));
            wh.write(&p, c.clone().into_bytes(), Some(m.hash(&p)))
                .unwrap();
            m.live.insert(p.clone(), c);
            let fid = m.fids[&p];
            m.ev(daemon(&p, EventKind::Modified), fid);
        }
        2 => {
            let Some(mut p) = rng.pick(&m.paths()).cloned() else {
                return;
            };
            for _ in 0..=rng.below(4) {
                let to = m.fresh(dirs[rng.below(3)]);
                wh.rename(&p, &to, m.hash(&p)).unwrap();
                m.mv(&p, &to);
                p = to;
            }
        }
        3 => {
            let Some(d) = rng.pick(&m.dirs()).cloned() else {
                return;
            };
            m.next += 1;
            let to = format!("e{}", m.next);
            wh.rename_folder(&d, &to).unwrap();
            let files: Vec<String> = m
                .paths()
                .into_iter()
                .filter(|p| p.starts_with(&format!("{d}/")))
                .collect();
            for f in files {
                let t = format!("{to}/{}", &f[d.len() + 1..]);
                m.mv(&f, &t);
            }
        }
        4 => {
            let ps = m.paths();
            if ps.len() < 2 {
                return;
            }
            let a = ps[rng.below(ps.len())].clone();
            let by = ps.iter().find(|p| **p != a).unwrap().clone();
            wh.call({
                let (a, by, hh) = (a.clone(), by.clone(), m.hash(&a));
                move |w| w.delete_replaced(&a, hh, &by)
            })
            .unwrap()
            .unwrap();
            m.live.remove(&a);
            let fid = m.gone(&a);
            m.ev(
                daemon(
                    &a,
                    EventKind::Deleted {
                        replaced_by: Some(by.clone()),
                    },
                ),
                fid,
            );
        }
        5 => {
            let Some(p) = rng.pick(&m.paths()).cloned() else {
                return;
            };
            wh.delete(&p, m.hash(&p)).unwrap();
            m.live.remove(&p);
            let fid = m.gone(&p);
            m.ev(daemon(&p, EventKind::Deleted { replaced_by: None }), fid);
        }
        _ => {
            // Path reuse: create at a path just moved away from.
            let free: Vec<String> = m
                .vacated
                .iter()
                .filter(|p| !m.live.contains_key(*p))
                .cloned()
                .collect();
            let Some(p) = free.last().cloned() else {
                return;
            };
            let c = format!("reused {p} {}", rng.below(1000));
            wh.create(&p, c.clone().into_bytes()).unwrap();
            m.live.insert(p.clone(), c);
            let fid = m.born(&p);
            m.ev(daemon(&p, EventKind::Created), fid);
        }
    }
}

fn run_seed(seed: u64) {
    let start = [("d0/a.md", "A"), ("d1/b.md", "B"), ("d2/c.md", "C")];
    let fx = Fx::new(&start);
    let (wh, rx) = spawn(&fx);
    let mut m = Model {
        live: start
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect(),
        vacated: Vec::new(),
        next: 0,
        events: Vec::new(),
        fids: start
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (p.to_string(), i + 1))
            .collect(),
        ev_fids: Vec::new(),
        next_fid: start.len(),
    };
    let mut rng = Rng::new(seed);
    let mut got = Vec::new();
    for _ in 0..12 {
        step(&mut rng, &wh, &mut m);
        let gap = [Duration::ZERO, DEB / 2, DEB * 2][rng.below(3)];
        if !gap.is_zero() {
            got.extend(collect(&rx, gap));
        }
    }
    got.extend(collect(&rx, DEB * 6));
    assert_eq!(got, m.events, "seed {seed}: event stream");
    // Folding the stream over the starting set yields the on-disk set.
    let mut fold: BTreeSet<String> = start.iter().map(|(p, _)| p.to_string()).collect();
    for e in &got {
        match &e.kind {
            EventKind::Created => assert!(fold.insert(e.path.clone()), "seed {seed}: {e:?}"),
            EventKind::Modified => assert!(fold.contains(&e.path), "seed {seed}: {e:?}"),
            EventKind::Deleted { .. } => assert!(fold.remove(&e.path), "seed {seed}: {e:?}"),
            EventKind::Renamed { from } => {
                assert!(fold.remove(from), "seed {seed}: {e:?}");
                assert!(fold.insert(e.path.clone()), "seed {seed}: {e:?}");
            }
        }
    }
    let disk: BTreeSet<String> = wh
        .call(|w| w.fs().list_files("").unwrap())
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(fold, disk, "seed {seed}: fold vs disk");
    // Plan note-identity WP-1: every event carries an id; ids and model files correspond one to
    // one; folding by id tracks each file to its final path, never two live paths per id.
    let mut id_of_fid = std::collections::BTreeMap::new();
    let mut fid_of_id = std::collections::BTreeMap::new();
    let mut by_id: std::collections::BTreeMap<NoteId, String> = std::collections::BTreeMap::new();
    for (e, fid) in got.iter().zip(&m.ev_fids) {
        let id =
            e.id.unwrap_or_else(|| panic!("seed {seed}: no id on {e:?}"));
        assert_eq!(
            *id_of_fid.entry(*fid).or_insert(id),
            id,
            "seed {seed}: {e:?}"
        );
        assert_eq!(
            *fid_of_id.entry(id).or_insert(*fid),
            *fid,
            "seed {seed}: {e:?}"
        );
        match &e.kind {
            EventKind::Created => assert!(by_id.insert(id, e.path.clone()).is_none()),
            EventKind::Modified => {
                let p = by_id.entry(id).or_insert_with(|| e.path.clone());
                assert_eq!(*p, e.path, "seed {seed}: {e:?}");
            }
            EventKind::Renamed { from } => {
                let p = by_id.entry(id).or_insert_with(|| from.clone());
                assert_eq!(p, from, "seed {seed}: {e:?}");
                *p = e.path.clone();
            }
            EventKind::Deleted { .. } => {
                let p = by_id.remove(&id).unwrap_or_else(|| e.path.clone());
                assert_eq!(p, e.path, "seed {seed}: {e:?}");
            }
        }
        let live: BTreeSet<&String> = by_id.values().collect();
        assert_eq!(
            live.len(),
            by_id.len(),
            "seed {seed}: a path bound to two ids"
        );
    }
    for (p, fid) in &m.fids {
        if let Some(id) = id_of_fid.get(fid) {
            assert_eq!(by_id.get(id), Some(p), "seed {seed}: id {id:?} final path");
        }
    }
    assert_eq!(
        disk,
        m.live.keys().cloned().collect::<BTreeSet<_>>(),
        "seed {seed}: model vs disk"
    );
}

/// Seeds via `CV_PROP_SEEDS` (default 4).
#[test]
fn random_writer_ops_fold_to_the_disk_set_and_match_the_model() {
    let n: u64 = std::env::var("CV_PROP_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    for seed in 1..=n {
        run_seed(seed);
    }
}
