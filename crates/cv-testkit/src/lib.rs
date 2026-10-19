//! Deterministic synthetic-vault builder for tests (invariant I1: never the live vault).
//!
//! [`SyntheticVault::build`] writes a vault under a caller-provided directory. The same
//! [`VaultSpec`] always yields byte-identical files: content derives only from the spec and a
//! small xorshift PRNG seeded from it. No note text is ever copied from a real vault.

pub mod http;

pub use http::{MockHttp, Reply, Request};

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Shape of the synthetic vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultSpec {
    pub seed: u64,
    /// Plain topic notes under `notes/`.
    pub notes: usize,
    /// Wiki-links per note (targets chosen deterministically; some deliberately unresolved).
    pub links_per_note: usize,
    /// Task items per note (mix of `[ ]`, `[x]`, `[>]`).
    pub tasks_per_note: usize,
    /// Daily notes under `daily/YYYY/MM/YYYY-MM-DD.md`, consecutive from `start_date`.
    pub daily_notes: usize,
    /// Monthly notes under `monthly/YYYY-MM.md` covering the daily range.
    pub monthly_notes: bool,
    /// Binary-ish files under `_resources/`.
    pub resources: usize,
    /// First daily date `(year, month, day)`.
    pub start_date: (i32, u32, u32),
}

impl Default for VaultSpec {
    fn default() -> Self {
        Self {
            seed: 0x5eed,
            notes: 20,
            links_per_note: 3,
            tasks_per_note: 2,
            daily_notes: 10,
            monthly_notes: true,
            resources: 4,
            start_date: (2026, 1, 28),
        }
    }
}

/// A built vault: its root and every file written, as sorted vault-relative paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticVault {
    pub root: PathBuf,
    pub files: Vec<String>,
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15 | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa",
];
const TAGS: &[&str] = &["topic/a", "topic/b", "status/open", "kind/ref"];
const KINDS: &[&str] = &["article", "video", "idea", "person"];

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn add_day((y, m, d): (i32, u32, u32)) -> (i32, u32, u32) {
    if d < days_in_month(y, m) {
        (y, m, d + 1)
    } else if m < 12 {
        (y, m + 1, 1)
    } else {
        (y + 1, 1, 1)
    }
}

fn sentence(rng: &mut Rng, n: usize) -> String {
    (0..n)
        .map(|_| WORDS[rng.below(WORDS.len())])
        .collect::<Vec<_>>()
        .join(" ")
}

impl SyntheticVault {
    /// Write the vault described by `spec` under `root` (created if missing).
    pub fn build(root: &Path, spec: &VaultSpec) -> io::Result<Self> {
        let mut rng = Rng::new(spec.seed);
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let titles: Vec<String> = (0..spec.notes).map(|i| format!("Note {i:04}")).collect();

        for (i, title) in titles.iter().enumerate() {
            let mut s = String::new();
            let tag = TAGS[rng.below(TAGS.len())];
            let kind = KINDS[rng.below(KINDS.len())];
            let _ = writeln!(s, "---");
            let _ = writeln!(s, "kind: {kind}");
            let _ = writeln!(s, "tags:\n  - {tag}");
            let _ = writeln!(s, "rating: {}", rng.below(6));
            let _ = writeln!(s, "done: {}", rng.below(2) == 1);
            let _ = writeln!(s, "source: \"https://example.invalid/{i}\"");
            let _ = writeln!(s, "---\n# {title}\n");
            let _ = writeln!(s, "{} #{tag}\n", sentence(&mut rng, 8));
            for _ in 0..spec.links_per_note {
                let r = rng.below(titles.len() + 1);
                match titles.get(r) {
                    Some(t) => {
                        let _ = writeln!(s, "- see [[{t}]]");
                    }
                    None => {
                        let _ = writeln!(s, "- see [[Missing {}]]", rng.below(5));
                    }
                }
            }
            if spec.resources > 0 {
                let _ = writeln!(s, "\n![[res-{:03}.png]]", rng.below(spec.resources));
            }
            let _ = writeln!(s, "\n## Tasks\n");
            for t in 0..spec.tasks_per_note {
                let mark = [" ", "x", ">"][rng.below(3)];
                let _ = writeln!(s, "- [{mark}] task {t} {}", sentence(&mut rng, 3));
            }
            files.push((format!("notes/{title}.md"), s.into_bytes()));
        }

        let mut date = spec.start_date;
        let mut months: Vec<(i32, u32)> = Vec::new();
        for _ in 0..spec.daily_notes {
            let (y, m, d) = date;
            if months.last() != Some(&(y, m)) {
                months.push((y, m));
            }
            let mut s = format!(
                "---\ndate: {y:04}-{m:02}-{d:02}\n---\n# {y:04}-{m:02}-{d:02}\n\n## Log\n\n"
            );
            let _ = writeln!(s, "- {}", sentence(&mut rng, 5));
            let _ = writeln!(s, "- [ ] daily {}", sentence(&mut rng, 2));
            if let Some(t) = titles.get(rng.below(titles.len().max(1))) {
                let _ = writeln!(s, "- ref [[{t}]]");
            }
            files.push((
                format!("daily/{y:04}/{m:02}/{y:04}-{m:02}-{d:02}.md"),
                s.into_bytes(),
            ));
            date = add_day(date);
        }
        if spec.monthly_notes {
            for (y, m) in months {
                let s = format!("# {y:04}-{m:02}\n\n## Goals\n\n- [ ] monthly goal\n");
                files.push((format!("monthly/{y:04}-{m:02}.md"), s.into_bytes()));
            }
        }

        for r in 0..spec.resources {
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend((0..32).map(|_| (rng.next() & 0xff) as u8));
            files.push((format!("_resources/res-{r:03}.png"), bytes));
        }

        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (rel, bytes) in &files {
            let p = root.join(rel);
            if let Some(dir) = p.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(p, bytes)?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            files: files.into_iter().map(|(p, _)| p).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(v: &SyntheticVault) -> Vec<(String, Vec<u8>)> {
        v.files
            .iter()
            .map(|f| (f.clone(), fs::read(v.root.join(f)).expect("read")))
            .collect()
    }

    #[test]
    fn same_spec_is_byte_identical() {
        let (a, b) = (
            tempfile::tempdir().expect("tmp"),
            tempfile::tempdir().expect("tmp"),
        );
        let spec = VaultSpec::default();
        let va = SyntheticVault::build(a.path(), &spec).expect("build a");
        let vb = SyntheticVault::build(b.path(), &spec).expect("build b");
        assert_eq!(va.files, vb.files);
        assert_eq!(snapshot(&va), snapshot(&vb));
    }

    #[test]
    fn different_seed_differs() {
        let (a, b) = (
            tempfile::tempdir().expect("tmp"),
            tempfile::tempdir().expect("tmp"),
        );
        let va = SyntheticVault::build(a.path(), &VaultSpec::default()).expect("a");
        let spec = VaultSpec {
            seed: 99,
            ..VaultSpec::default()
        };
        let vb = SyntheticVault::build(b.path(), &spec).expect("b");
        assert_ne!(snapshot(&va), snapshot(&vb));
    }

    #[test]
    fn layout_covers_every_shape() {
        let t = tempfile::tempdir().expect("tmp");
        let v = SyntheticVault::build(t.path(), &VaultSpec::default()).expect("build");
        let has = |prefix: &str| v.files.iter().any(|f| f.starts_with(prefix));
        assert!(has("notes/") && has("daily/2026/01/") && has("daily/2026/02/"));
        assert!(has("monthly/2026-01.md") && has("monthly/2026-02.md"));
        assert!(has("_resources/res-000.png"));
        let note = fs::read_to_string(t.path().join(&v.files[v.files.len() - 1])).expect("read");
        assert!(note.starts_with("---\nkind: "));
        assert!(note.contains("[[") && note.contains("- ["));
    }
}
