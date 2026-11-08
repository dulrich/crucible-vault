# Spike: chained daemon moves lose the note's path link (2026-11-08)

Scope: the 2026-11-08 report that a fanfic clip's tab stayed on `Clippings/…` with "file deleted on disk" while the note was moved to `daily/day/2026-11-08/` and renamed to `<story> - Chapter 66` (filed by `file-as-kind`: `move` step, then `fanfic_title` rename step, back to back). Synthetic temp vault only. The live vault and daemon were not touched.

Re-run: save the test below as `crates/cv-vault/tests/spike_chain.rs` (it uses `crates/cv-vault/tests/common`), run `cargo test -p cv-vault --test spike_chain -- --nocapture --test-threads=1`, then delete it.

| Scenario | Events (real writer + watcher, debounce 150 ms) |
|---|---|
| C1: `rename A→B`, then `rename B→C` straight away | `Deleted(A)` + `Created(C)` at +149 ms. No `Renamed`. |
| C2: C1 plus a tag write on C | `Deleted(A)` + `Created(C)` |
| C3 (control): A→B, wait 400 ms, B→C | `Renamed(B from A)`, then `Renamed(C from B)` |

## Mechanism

The writer moves files by hard link + unlink and never reports the move itself. It leaves echo records instead: `register(from, None)`, `register(to, h)`, `register_move(from, to)` (`crates/cv-vault/src/writer.rs:404-406`). The watcher rebuilds a `Renamed` from those records at debounce flush (`crates/cv-vault/src/watcher.rs` `examine_file`: `move_dest`, `take_move`, `seen.remove(&from)`). That needs the intermediate path B to have been *seen*. In a chain inside one window, B is created and gone between hints. The `move_dest(A)=B` lookup fails because B no longer exists. `take_move(C)=B` fails because B is not in `seen`. Both halves therefore degrade to `Deleted` + `Created`, and the editor shows "file deleted on disk" (`crates/editor/src/source/save.rs:289`).

This is the third patch on the same reconstruction: WP-5a move echo (intake-round-2), round-11 `replaced_by` (`docs/spikes/stale-tab/FINDINGS.md`), and the `d50c55c` pairing grace. The post-deploy round-23 step (b) found 30 `clip-enrich` retries unretryable for the same reason (`<note> not found`: the stale path from before the move).

## Spike test

```rust
mod common;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use common::{Fx, h};
use cv_vault::{VaultEvent, WatchConfig, spawn_writer_with_watcher, ContentHash};

fn cfg() -> WatchConfig { WatchConfig { debounce: Duration::from_millis(150), echo_ttl: Duration::from_secs(5) } }
fn collect(rx: &Receiver<VaultEvent>, w: Duration) -> Vec<(u128, VaultEvent)> {
    let t0 = Instant::now(); let end = t0 + w; let mut out = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) { Ok(e) => out.push((t0.elapsed().as_millis(), e)), Err(_) => break }
    }
    out
}
const SRC: &str = "---\ntitle: x\n---\nChapter 66\nbody\n";

// file-as-kind: move, then fanfic_title rename, back to back (same debounce window)
#[test] fn c1_move_then_rename() {
    let fx = Fx::new(&[("Clippings/c.md", SRC)]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    wh.rename("Clippings/c.md", "day/c.md", h(SRC)).unwrap();
    wh.rename("day/c.md", "day/S - Chapter 66.md", h(SRC)).unwrap();
    eprintln!("C1 {:?}", collect(&rx, Duration::from_secs(1)));
}
// same plus tag+lint writes on the final path
#[test] fn c2_move_rename_then_writes() {
    let fx = Fx::new(&[("Clippings/c.md", SRC)]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    wh.rename("Clippings/c.md", "day/c.md", h(SRC)).unwrap();
    wh.rename("day/c.md", "day/S - Chapter 66.md", h(SRC)).unwrap();
    let b2 = format!("{SRC}#fanfic\n");
    wh.write("day/S - Chapter 66.md", b2.into_bytes(), Some(h(SRC))).unwrap();
    eprintln!("C2 {:?}", collect(&rx, Duration::from_secs(1)));
}
// control: the two moves separated by more than the debounce
#[test] fn c3_move_wait_rename() {
    let fx = Fx::new(&[("Clippings/c.md", SRC)]);
    let (wh, rx, _j) = spawn_writer_with_watcher(fx.live(), cfg()).unwrap();
    wh.rename("Clippings/c.md", "day/c.md", h(SRC)).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    wh.rename("day/c.md", "day/S - Chapter 66.md", h(SRC)).unwrap();
    eprintln!("C3 {:?}", collect(&rx, Duration::from_secs(1)));
}
#[allow(dead_code)] fn _u(_: ContentHash) {}
```
