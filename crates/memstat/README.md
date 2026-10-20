# memstat

In-process memory snapshot for fleet Rust services: process memory from `/proc/self/status` (`VmRSS`, `RssAnon`, `RssFile`, `RssShmem`, `VmSwap`, `VmHWM`) and `/proc/self/smaps_rollup` (`Pss`, `Pss_Anon`, `Pss_File`, `Pss_Shmem`, `Swap`), plus a registry of named gauges the tool reports (e.g. cache entry counts). Framework-agnostic: each tool mounts its own route. Dependencies: `std` + `serde` + `serde_json` only. No background thread; the only I/O is the two `/proc` reads made on each `snapshot()` call. A missing/unreadable file is an absent (`null`) field, never a panic.

This directory is canonical. Consumers get a byte-identical vendored copy at `<tool>/crates/memstat/` via `install.sh`; never edit a copy.

## API

```rust
use memstat::{GaugeKind, Registry};
use std::sync::{Arc, OnceLock};

static MEMSTAT: OnceLock<Registry> = OnceLock::new();

let reg = MEMSTAT.get_or_init(Registry::new);
let cache = Arc::clone(&my_cache);
reg.register("meterCacheEntries", GaugeKind::Count, move || {
    cache.lock().map(|c| c.len() as u64).map_err(|_| "meter cache lock poisoned".into())
})?; // Err only if the registry lock itself was poisoned

// in the route handler:
let snap: memstat::Snapshot = reg.snapshot(); // impl Serialize, camelCase JSON
```

Public items: `Registry::{new, register, snapshot}`, `GaugeKind::{Count, Bytes}`, `GaugeResult = Result<u64, String>`, `Snapshot { status, smapsRollup, gauges, uptimeSecs, registryStartedUnix, registryError? }`, `StatusMem`, `RollupMem`, `GaugeReading { name, kind, value?, error? }`, and the pure parsers `parse_status(&str)` / `parse_smaps_rollup(&str)`. All memory fields are bytes. `registryStartedUnix` is the registry's creation time (create it at startup; it stands in for process start).

## Gauges must not panic

Fleet release builds use `panic = "abort"`, so a panicking gauge kills the process — it cannot be caught. A gauge closure returns `Result<u64, String>`: use `lock().map_err(..)`, `try_into().map_err(..)` etc., never `unwrap`/`expect`/indexing that can fail. An `Err` appears in the snapshot as that gauge's `error` field. A poisoned registry lock is reported as `registryError`, not a panic.

## Consumer integration

1. Register the tool dir in `memstat/consumers.txt` and run `bash memstat/install.sh --dest <tool>` (from the repo root).
2. In the consumer's root `Cargo.toml` add `exclude = ["crates/memstat"]` to `[workspace]` (do **not** list it in `members`).
3. In the using crate: `memstat = { path = "../memstat" }`; refresh `Cargo.lock`.

Why `exclude` and not `members`: the crate's `Cargo.toml` carries an empty `[workspace]` table so the canonical copy builds standalone at the repo root (`cd memstat && cargo test`). That table is shipped unchanged (copies stay byte-identical), and a workspace member may not declare its own `[workspace]`. A path dependency works regardless — Cargo ignores a dependency's own `[workspace]` — but because the copy lives inside the consumer's workspace directory, it must be listed in `exclude` or Cargo reports it as an unlisted member. The vendored crate is then compiled as a normal path dependency into the consumer's single `Cargo.lock` and `target/`.

## Verify gate

Each consumer adds to its `package.json` scripts and prescribed gates:

```json
"memstat:verify": "bash ../memstat/install.sh --verify --dest ."
```

`install.sh --verify` checks the copy against its `.manifest` (local edits) and against this source (unsynced changes); with no `--dest` it verifies every consumer in `consumers.txt`. Canonical gate: `cd memstat && cargo fmt --check && cargo test && cargo clippy --all-targets -- -D warnings`.
