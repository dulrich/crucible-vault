//! Folder-link discovery (plan folder-links-follow Decision 5): the one walk of the vault every
//! enumerator, the index and the writer share.
//!
//! The walk follows folder links. Each canonical directory gets exactly one vault path, its
//! **owner**: a physical in-vault path always wins over a link alias; among aliases the first in
//! sorted vault-path order wins. A folder link is skipped (never descended, never owned) when it
//! is a **loop** (its target is an ancestor of the link's own location or of the vault root), a
//! **losing alias** (its target is already owned, or lies inside the vault root), or **dangling**.
//! Skips are counted and logged as aggregates. The vault's `.gitignore` files are loaded during
//! the same walk, so exclusions (dot components, `.gitignore` matches) apply inside linked
//! subtrees exactly as in physical ones and excluded directories are never owned. File links keep
//! their behaviour: indexed under their vault path with their target's metadata.
//!
//! The result is an [`Ownership`] map (vault dir ↔ canonical dir, plus the skipped links) the
//! writer admits mutations and note ids against, and the file list the index builds from.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::path::{Path, PathBuf};

use crate::error::{Result, io};
use crate::ignore::{GITIGNORE, IgnoreRules};

/// Why a folder link was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SkipReason {
    /// The target is an ancestor of the link's own location (or of the vault root).
    Loop,
    /// The target is already owned by another vault path (a physical path or an earlier alias),
    /// or lies inside the vault root.
    Alias,
    /// The link's target cannot be resolved.
    Dangling,
}

impl SkipReason {
    /// A short lowercase label for messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loop => "a folder-link loop",
            Self::Alias => "a losing folder-link alias",
            Self::Dangling => "a dangling link",
        }
    }
}

/// A vault path addressed through a skipped link: the writer declines it (no index row, no note
/// id).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path} is reached through {link}, {}; use the owning path", reason.as_str())]
pub struct Declined {
    /// The declined vault path.
    pub path: String,
    /// The skipped link (a vault path) it is reached through (`path` itself for a link).
    pub link: String,
    /// Why the link is skipped.
    pub reason: SkipReason,
}

/// Skips of one discovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkipCounts {
    /// Folder links that loop.
    pub loops: usize,
    /// Losing folder-link aliases (including links into the vault root).
    pub aliases: usize,
    /// Dangling links (folder or file — a dangling link's kind is unknowable).
    pub dangling: usize,
}

#[derive(Debug, Clone)]
struct Skip {
    reason: SkipReason,
    /// The link's canonical target (`None` when dangling).
    target: Option<PathBuf>,
}

/// The vault-wide ownership map of one discovery: every owned (non-excluded) vault directory and
/// its canonical directory, and every skipped link. Consumed by the writer's admission rule, the
/// startup note-id repair and the watcher's watch set (WP-2b: [`Ownership::external_dirs`]).
#[derive(Debug, Clone, Default)]
pub struct Ownership {
    root: PathBuf,
    /// Vault dir ("" = root) → canonical dir.
    dirs: BTreeMap<String, PathBuf>,
    /// Canonical dir → owning vault dir.
    owners: HashMap<PathBuf, String>,
    /// Skipped link vault path → why.
    skipped: BTreeMap<String, Skip>,
}

impl Ownership {
    /// The canonical vault root.
    pub fn canonical_root(&self) -> &Path {
        &self.root
    }

    /// Every owned vault directory ("" = root) with its canonical directory, sorted.
    pub fn dirs(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.dirs.iter().map(|(k, v)| (k.as_str(), v.as_path()))
    }

    /// The canonical directory of owned vault directory `dir`.
    pub fn canonical_of(&self, dir: &str) -> Option<&Path> {
        self.dirs.get(dir).map(PathBuf::as_path)
    }

    /// The vault directory owning canonical directory `canonical`.
    pub fn owner_of(&self, canonical: &Path) -> Option<&str> {
        self.owners.get(canonical).map(String::as_str)
    }

    /// The vault path of the entry `canonical` (a file or directory whose parent is owned).
    pub fn vault_path_of(&self, canonical: &Path) -> Option<String> {
        let parent = self.owner_of(canonical.parent()?)?;
        let name = canonical.file_name()?.to_str()?;
        Some(join(parent, name))
    }

    /// The canonical directories of folder-link subtrees outside the vault root, sorted by vault
    /// path (what WP-2b watches).
    pub fn external_dirs(&self) -> impl Iterator<Item = &Path> {
        self.dirs
            .values()
            .filter(|c| !c.starts_with(&self.root))
            .map(PathBuf::as_path)
    }

    /// Every skipped link with its reason, sorted by vault path.
    pub fn skipped(&self) -> impl Iterator<Item = (&str, SkipReason)> {
        self.skipped.iter().map(|(k, s)| (k.as_str(), s.reason))
    }

    /// The admission rule: `path` is declined when it is reached through a skipped link (a
    /// proper ancestor is one), or is itself a skipped folder link (loop or alias). A dangling
    /// link entry itself stays admissible (it may be a file link).
    pub fn admit(&self, path: &str) -> std::result::Result<(), Declined> {
        let decline = |link: &str, s: &Skip| Declined {
            path: path.to_string(),
            link: link.to_string(),
            reason: s.reason,
        };
        for (i, b) in path.bytes().enumerate() {
            if b == b'/'
                && let Some(s) = self.skipped.get(&path[..i])
            {
                return Err(decline(&path[..i], s));
            }
        }
        match self.skipped.get(path) {
            Some(s) if s.reason != SkipReason::Dangling => Err(decline(path, s)),
            _ => Ok(()),
        }
    }

    /// For a path declined through a losing alias: the owning vault path of the same canonical
    /// location (`None` for loops, dangling links, or an alias of an unowned directory).
    pub fn winner_of(&self, path: &str) -> Option<String> {
        let d = self.admit(path).err()?;
        if d.reason != SkipReason::Alias {
            return None;
        }
        let target = self.skipped.get(&d.link)?.target.as_ref()?;
        let owner = self.owner_of(target)?;
        let rest = &path[d.link.len()..];
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        Some(if rest.is_empty() {
            owner.to_string()
        } else {
            join(owner, rest)
        })
    }
}

/// One discovered file.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Vault-relative `/` path.
    pub path: String,
    /// The full path through the vault root (links unresolved).
    pub full: PathBuf,
    /// Metadata (a file link's target's).
    pub meta: std::fs::Metadata,
    /// The entry itself is a symlink to a regular file.
    pub file_link: bool,
    /// Excluded from the index (only in a [`discover_listing`] result).
    pub excluded: bool,
    /// The file's canonical location (what reconcile compares).
    pub canonical: PathBuf,
}

/// The result of one walk.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Every file, sorted by vault path.
    pub files: Vec<DiscoveredFile>,
    /// The ownership map.
    pub ownership: Ownership,
    /// The `.gitignore` rules loaded during the walk.
    pub rules: IgnoreRules,
    /// What was skipped.
    pub counts: SkipCounts,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// A directory to scan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Pending {
    rel: String,
    full: PathBuf,
    canon: PathBuf,
    /// The canonical directory holding the entry (for loop detection).
    parent: PathBuf,
    /// Only listed (an excluded subtree in listing mode), never owned.
    excluded: bool,
}

struct Walk<'a> {
    d: Discovery,
    /// `Some` in listing mode: excluded subtrees are walked (physically) unless pruned.
    listing: Option<&'a dyn Fn(&str) -> bool>,
}

impl Walk<'_> {
    fn skip(&mut self, rel: String, reason: SkipReason, target: Option<PathBuf>) {
        tracing::debug!(path = %rel, reason = reason.as_str(), "folder link skipped");
        match reason {
            SkipReason::Loop => self.d.counts.loops += 1,
            SkipReason::Alias => self.d.counts.aliases += 1,
            SkipReason::Dangling => self.d.counts.dangling += 1,
        }
        self.d
            .ownership
            .skipped
            .insert(rel, Skip { reason, target });
    }

    fn own(&mut self, p: &Pending) {
        self.d.ownership.dirs.insert(p.rel.clone(), p.canon.clone());
        self.d
            .ownership
            .owners
            .insert(p.canon.clone(), p.rel.clone());
        let file = p.full.join(GITIGNORE);
        match std::fs::read_to_string(&file) {
            Ok(text) => self.d.rules.add(&p.rel, &text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "reading a vault .gitignore"),
        }
    }

    /// Scans `p`'s entries: records its files, returns its subdirectories (physical ones and
    /// followable folder links) to scan.
    fn scan(&mut self, p: &Pending, linked: bool) -> Result<Vec<(Pending, bool)>> {
        let rd = match std::fs::read_dir(&p.full) {
            Ok(rd) => rd,
            // A linked target that vanished after discovery is treated as gone (E2).
            Err(e) if linked && e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(&p.full, e)),
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.map_err(|e| io(&p.full, e))?;
            let Ok(name) = entry.file_name().into_string() else {
                tracing::warn!("skipping a non-UTF-8 vault path");
                continue;
            };
            let rel = join(&p.rel, &name);
            let full = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) if linked && e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io(&full, e)),
            };
            if ft.is_dir() {
                let excluded = p.excluded || self.d.rules.is_excluded(&rel, true);
                if excluded && self.listing.is_none_or(|prune| prune(&rel)) {
                    continue;
                }
                let canon = p.canon.join(&name);
                out.push((
                    Pending {
                        rel,
                        full,
                        parent: p.canon.clone(),
                        canon,
                        excluded,
                    },
                    false,
                ));
            } else if ft.is_file() {
                let excluded = p.excluded || self.d.rules.is_excluded(&rel, false);
                if excluded && self.listing.is_none() {
                    continue;
                }
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(io(&full, e)),
                };
                let canonical = p.canon.join(&name);
                self.d.files.push(DiscoveredFile {
                    path: rel,
                    full,
                    meta,
                    file_link: false,
                    excluded,
                    canonical,
                });
            } else if ft.is_symlink() {
                let excluded_file = p.excluded || self.d.rules.is_excluded(&rel, false);
                match std::fs::metadata(&full) {
                    Ok(m) if m.is_file() => {
                        if excluded_file && self.listing.is_none() {
                            continue;
                        }
                        let canonical =
                            std::fs::canonicalize(&full).unwrap_or_else(|_| full.clone());
                        self.d.files.push(DiscoveredFile {
                            path: rel,
                            full,
                            meta: m,
                            file_link: true,
                            excluded: excluded_file,
                            canonical,
                        });
                    }
                    Ok(m) if m.is_dir() => {
                        // Folder links inside excluded subtrees are never followed.
                        if p.excluded || self.d.rules.is_excluded(&rel, true) {
                            continue;
                        }
                        match std::fs::canonicalize(&full) {
                            Ok(canon) => out.push((
                                Pending {
                                    rel,
                                    full,
                                    canon,
                                    parent: p.canon.clone(),
                                    excluded: false,
                                },
                                true,
                            )),
                            Err(_) => self.skip(rel, SkipReason::Dangling, None),
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        if !excluded_file {
                            self.skip(rel, SkipReason::Dangling, None);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn run(mut self, root: &Path) -> Result<Discovery> {
        let canon_root = std::fs::canonicalize(root).map_err(|e| io(root, e))?;
        self.d.ownership.root = canon_root.clone();
        // Phase 1: the physical tree (physical paths always win).
        let mut links: Vec<Pending> = Vec::new();
        let mut stack = vec![Pending {
            rel: String::new(),
            full: root.to_path_buf(),
            canon: canon_root.clone(),
            parent: canon_root.clone(),
            excluded: false,
        }];
        while let Some(p) = stack.pop() {
            if !p.excluded {
                self.own(&p);
            }
            for (c, is_link) in self.scan(&p, false)? {
                if is_link {
                    links.push(c);
                } else {
                    stack.push(c);
                }
            }
        }
        // Phase 2: folder links and their subtrees, in sorted vault-path order (a min-heap: any
        // newly found entry is a descendant of the one being scanned, so it sorts after it).
        let mut heap: BinaryHeap<Reverse<(Pending, bool)>> =
            links.into_iter().map(|l| Reverse((l, true))).collect();
        while let Some(Reverse((p, is_link))) = heap.pop() {
            if p.excluded {
                if !self.d.ownership.owners.contains_key(&p.canon) {
                    for c in self.scan(&p, true)? {
                        heap.push(Reverse(c));
                    }
                }
                continue;
            }
            if is_link && (canon_root.starts_with(&p.canon) || p.parent.starts_with(&p.canon)) {
                self.skip(p.rel, SkipReason::Loop, Some(p.canon));
                continue;
            }
            if (is_link && p.canon.starts_with(&canon_root))
                || self.d.ownership.owners.contains_key(&p.canon)
            {
                self.skip(p.rel, SkipReason::Alias, Some(p.canon));
                continue;
            }
            self.own(&p);
            for c in self.scan(&p, true)? {
                heap.push(Reverse(c));
            }
        }
        self.d.files.sort_by(|a, b| a.path.cmp(&b.path));
        let c = self.d.counts;
        if c != SkipCounts::default() {
            tracing::info!(
                loops = c.loops,
                aliases = c.aliases,
                dangling = c.dangling,
                "vault discovery skipped links"
            );
        }
        Ok(self.d)
    }
}

/// The index's view of `root` (Decision 5): owned, non-excluded files only.
pub fn discover(root: &Path) -> Result<Discovery> {
    Walk {
        d: Discovery::default(),
        listing: None,
    }
    .run(root)
}

/// [`discover`], plus the files of excluded subtrees (flagged [`DiscoveredFile::excluded`]) for
/// enumerators that list more than the index does. `prune(dir)` is asked for each excluded
/// directory and skips it when true. Ownership is identical to [`discover`]'s: excluded subtrees
/// are listed physically and their folder links are never followed.
pub fn discover_listing(root: &Path, prune: &dyn Fn(&str) -> bool) -> Result<Discovery> {
    Walk {
        d: Discovery::default(),
        listing: Some(prune),
    }
    .run(root)
}
