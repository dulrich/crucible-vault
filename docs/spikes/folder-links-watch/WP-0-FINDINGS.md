# WP-0 findings — watch ownership for folder links (plan `folder-links-follow`)

Date 2026-11-22. notify 8.2.0 (the workspace pin), Linux inotify backend, kernel `max_queued_events` 16384, `max_user_watches` 553369. Spike crate: `docs/spikes/folder-links-watch/` (promoted from `runs/spike/folder-links-watch/`) (`Cargo.toml` + `src/main.rs`, own `[workspace]`, throwaway). Synthetic trees only, under `$TMPDIR`; the live vault was read for aggregate counts only.

Reproduce:

```sh
cd docs/spikes/folder-links-watch
T=$(mktemp -d "$TMPDIR/flw-target.XXXX"); CARGO_TARGET_DIR=$T cargo build --release --offline
B=$(mktemp -d "$TMPDIR/flw.XXXX"); for p in p0 p1 p2 p3 p4; do "$T/release/flw-spike" "$B" $p; done; rm -rf "$B" "$T"
```

Event lists below drop `Access(Open)` noise (the spike's own `read_dir` calls) and are shown relative to the probe's base dir.

## Live shape (aggregates)

Commands: `find . -type l -xtype d -not -path './.git/*'` in the vault, then per link `find -L <link> -type d|-type f` and `-printf '%d'`; file links `find . -type l -xtype f -exec readlink -f {} \;` → `dirname | sort -u`; overlap by `comm -12` of the canonical sets.

| | count |
|---|---|
| folder links | 39 |
| directories behind them (roots included) | 100 |
| files behind them | 1,197 |
| max depth below a link | 4 |
| file links / distinct target dirs | 94 / 32 |
| folder-link canonical dirs ∩ file-link target dirs | 0 |
| file-link target dirs inside the vault | 0 |
| vault directories (root watch today) | 6,553 |

## (0) Two registrations of one inode — `p0`

inotify returns **one watch descriptor per inode**; notify keeps one path per descriptor (last registration wins).

- `watch(ext/d, NonRecursive)` then `watch(alias→ext/d, NonRecursive)`, write `f1` → events only as `/alias/f1`.
- `unwatch(alias)` (the canonical registration still in notify's map), write `f2` → **no events**.

So any two registrations reaching one directory — a folder-link subtree dir and a file-link target dir, or a link dir also reached by the root's recursive walk — silently share and then lose a watch. The owned set must be keyed by canonical directory and never overlap the root watch.

## (1) Overlapping recursive watches (cycle-2 B1) — `p1`

`L1→t` and `L2→t/sub`, both `watch(.., Recursive)` with `follow_symlinks(true)`:

- before: `t/sub/a` → `/vault/L2/a`, `t/top` → `/vault/L1/top`.
- after `unwatch(L2)`: `t/sub/b` → **nothing**; `t/top2` → `/vault/L1/top2`. L1 lost `sub`.
- after `unwatch(L1)` (fresh run): `t/sub/b` → `/vault/L2/b`; L2 survives (its path is not under L1's prefix).

Confirmed, and order-dependent: notify recursive watches cannot be owned independently when they overlap.

Today's daemon shape (`watch(vault, Recursive)`, default follow=true): `t/sub/c` → `/vault/L1/sub/c` — the live watcher already follows folder links (the inconsistency in the plan's Context).

## (2) Disjoint, repo-owned non-recursive registration — `p2`

The vault root watched `Recursive` with `follow_symlinks(false)`; each link's canonical subtree walked by the spike and registered one `NonRecursive` watch per directory.

| shape | watches | walk + register | unwatch all |
|---|---|---|---|
| 39 roots, 139 dirs, 1,197 files (live ×1) | 139 | 2.9–3.2 ms | 1.3–1.4 ms |
| 39 roots, 1,390 dirs, 11,970 files (×10) | 1,390 | 23.5–29.3 ms | 8.8–14.3 ms |

Events from a depth-4 write arrive with the canonical path (`/ext/r21/d21/d60/d99/probe.md`), so the repo translates canonical → vault path. A new subdirectory is reported (`Create(Folder):…/newsub`), but a file written in it before the repo watches it is **not** (`newsub/inner.md` absent): non-recursive watches do not auto-extend, so the owner registers a created directory and then scans it (the create-then-write race).

## (3) Target lifecycle with an unchanged link entry (cycle-2 B3) — `p3`

- (a) dangling `vault/dangling → ext/missing`; root watch only; `mkdir ext/missing` + write → **no events**. With a `NonRecursive` watch on the target's nearest existing ancestor (`ext`): `Create(Folder):/ext/missing`. A dangling link needs an ancestor watch (or a poll) to notice its target appearing.
- (b) watched target `ext/t` deleted → `Remove(File):/ext/t/x.md`, `Remove(Folder):/ext/t`. Recreated + write `y.md` with no re-watch → **no events**. `watch(ext/t)` again → `Ok`, then `z.md` → `Create/Modify/Close(Write)`; `unwatch(ext/t)` → `Ok`. The owner must treat `Remove(Folder)` of an owned root as "target gone": drop it, watch the nearest existing ancestor, and re-register on its reappearance. notify tolerates re-registering the stale path.

## (4) Queue overflow (cycle-2 M4) — `p4`

The notify thread held in the handler (gate armed after setup — `watch()` round-trips through the same event loop, so blocking it earlier deadlocks the caller), then 18,384 file writes in `vault/flood`, `mkdir vault/newdir`, and a write in an owned external dir:

- 16,388 event paths delivered, **1 `Rescan`** (pathless); the `newdir` create and the external `lost.md` write were **dropped**.
- after: `ext/t/after.md` → events (owned watches survive an overflow), `vault/newdir/after.md` → **nothing**: notify's recursive root never saw `newdir`'s create, so `newdir` is unwatched.

A `Rescan` therefore needs (i) a full reconcile of vault + owned set (events were lost) and (ii) a watch resync: re-register the root recursively (`unwatch(root)` + `watch(root)`; prefix removal is harmless for the sole root) and diff the owned set against a fresh walk. Without it, directories created during the overflow stay unwatched until restart.

## Recommendation: posture (a)

Keep notify 8.2.0 for event delivery and own the registrations:

- root: `RecursiveMode::Recursive` with `Config::with_follow_symlinks(false)` (the root watch never enters a link);
- one repo-owned set of `NonRecursive` watches keyed by **canonical directory** — the union of folder-link subtree dirs (Decision 5 winners) and file-link target dirs (today's `sync_link_dirs`), deduped, never inside the vault root — with a canonical → vault-path(s) map for event translation;
- on `Create(Folder)` under an owned dir: register, then scan it; on `Remove(Folder)` of an owned dir: drop it and its descendants, and if it was a link target, watch the nearest existing ancestor;
- dangling link targets: a `NonRecursive` watch on each nearest existing ancestor (in the same canonical-keyed set);
- `Rescan`: full reconcile + re-register the root + diff the owned set.

Why not (b) (direct inotify loop, ≥1 kSLOC): both notify hazards measured above — the shared descriptor per inode (0) and recursive prefix removal (1) — are avoided entirely by never registering overlapping paths and never registering recursive watches outside the root; everything (b) would add (ancestor watches, subtree extension, rescan resync) is needed under (a) too. The watch set is small (139 live; 1,390 at ×10 in < 30 ms). The existing `sync_link_dirs` (`crates/cv-vault/src/thread.rs:179-208`) is already this pattern for file links; (a) generalises it.
