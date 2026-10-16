# S1 — Obsidian crash attribution (2026-10-13)

**Verdict: Electron/V8 renderer fault triggered by Crucible plugin code. Not the graphics stack.** Both Obsidian crashes in the retained history are the *renderer* process deliberately aborting after V8 raised a fatal "Invalid object access" inside Node's `AsyncLocalStorage.getStore()`. The call came from Crucible's `NoteLockManager.withLock`, on a command-palette Enter. There were no amdgpu resets or ring timeouts in any retained boot. A Vulkan-native Rust GUI would not inherit this fault. The `--disable-gpu` A/B is not needed: the GPU process isn't involved.

## Evidence (each with the command that produced it)

| # | Fact | Command |
|---|---|---|
| 1 | Journal retains 12 boots, 2026-09-30 → 2026-10-13 | `journalctl -k --list-boots` |
| 2 | **2** Obsidian crashes, not 1: PID 31042 at 2026-10-12 19:37:16 and PID 193643 at 2026-10-13 12:52:09 (local, −06:00). Both SIGSEGV, cores present. The assessment's 30-day grep missed the 10-12 one. | `coredumpctl list obsidian` |
| 3 | Both are `--type=renderer`. Neither is the GPU process. | `coredumpctl info <pid>` → Command Line |
| 4 | Kernel line for both: `segfault at 0 … error 6`, at the **same IP offset `0x61b02ef`** (PIE base differs) | `for b in $(seq -11 0); do journalctl -k -b $b \| grep -iE 'obsidian.*segfault'; done` |
| 5 | Identical top-4 native frames in both cores: `0x61b02ef ← 0x755d147 ← 0x32f6881 ← 0x3798594` | `coredumpctl info <pid>` |
| 6 | The binary is stock Electron 43.7.7 (Chrome 150.0.7871.250). Build ID `dae4f2a4…354f` matches breakpad module `A4F2E4DA2395ADB7B44A73ECB426354F0`. | `strings /opt/Obsidian/obsidian \| grep Electron/`; `readelf -n`; `unzip -l electron-v43.7.7-linux-x64-symbols.zip` |
| 7 | Symbolized: `0x61b02ef` = `V8FatalErrorCallback` (`electron/shell/common/node_bindings.cc:184`, the deliberate null write) ← `v8::internal::Isolate::PushStackTraceAndDie` (`isolate.cc:788`) ← `LookupIterator::Start<false>` ← `Runtime_LoadIC_Miss` … ← `blink::V8ScriptRunner::CallFunction` | breakpad `.sym` FUNC/line lookup (python bisect over the module's FUNC records), symbols in `~/.cache/crucible-vault-spike/electron-syms/` |
| 8 | V8's fatal banner for the 10-13 crash is in the X session log: `# Safely terminating process due to hitting Isolate::PushStackTraceAndDie`, with "Invalid object access" in the core | `grep -n PushStackTraceAndDie ~/.xsession-errors`; `coredumpctl dump 193643 \| strings \| grep 'Invalid object access'` |
| 9 | JS stack, top → bottom (10-13): `getStore` [`node:internal/async_local_storage/async_context_frame:77`] ← `withLock` ← `withOptionalNoteLock` ← `executeChain` ← `run` ← `executeCrucibleCommand` ← `checkCallback` ← Obsidian `executeCommand(ById)` ← `onChooseSuggestion` ← `handleKey` (`Enter`) ← `onKeyEvent` | `grep -naE '^\[[0-9]+\]: ' ~/.xsession-errors` (vault paths redacted) |
| 10 | The 10-12 core has the same frame: `getStore … async_context_frame:~75 (this=<AsyncLocalStorage …>)`, plus `executeCrucibleCommand` | `coredumpctl dump 31042 \| strings \| grep -E 'getStore\|executeCrucibleCommand'` |
| 11 | The `AsyncLocalStorage` is Crucible's `NoteLockManager.heldByContext` (`src/orchestration/NoteLockManager.ts:70`). It has been in use since 2026-06-03 (`eba251f`). | `git -C ../obsidian-crucible log -S AsyncLocalStorage`; `grep -rn AsyncLocalStorage src` |
| 12 | No amdgpu ring timeout, GPU reset or hang in any of the 12 boots. The only amdgpu faults are two `[gfxhub] page fault`s on the **iGPU** (12:00.0) on 2026-10-08, attributed to **blender**. | `journalctl -k -b <n> \| grep -iE 'amdgpu.*(ring .* timeout\|gpu reset\|hang\|fault)'` + context lines |
| 13 | No Obsidian/Electron version change near onset: binary mtime 2026-10-02, app asar 1.14.4 on 2026-10-05 | `stat -c %y /opt/Obsidian/obsidian ~/.config/obsidian/obsidian-1.14.4.asar` |
| 14 | Forwarder launches (`app-md.obsidian.Obsidian-*.scope`) are mostly **agent Obsidian-CLI calls** (`obsidian eval`/`command`/`plugin:reload`, `/usr/bin/obsidian` → `/opt/Obsidian/obsidian`), not clips. They come in bursts: 95 in 10-12 22:00 and 97 in 10-13 12:00, matching obsidian-crucible plan executions. The 5 min before each crash had 0 and 1 launches. Daily: 10-09 1, 10-10 10, 10-11 9, 10-12 135, 10-13 118. | `journalctl --user \| grep 'Started app-md.obsidian'` (windowed, and `cut -c1-10 \| uniq -c`) |

## Reading

- `error 6 at 0` looked like a native null-deref. It is actually Electron's `IMMEDIATE_CRASH` after a V8 fatal error. V8's `PushStackTraceAndDie` fires when a property load finds an object whose map is invalid (heap-object integrity check). The object here came through Node's `AsyncContextFrame`-based `AsyncLocalStorage` (Node 24's default implementation, which Electron 43 ships). A plausible mechanism, **not verified**: in Electron's renderer, Node and Blink share V8's continuation-preserved embedder data slot. Blink's task-attribution value would then be read as a Node `AsyncContextFrame` map, giving an invalid object access. This is a known-risk area for Node-in-renderer.
- The onset timing is unexplained. ALS has been in Crucible since June and Electron 43 since 10-02, yet the crashes started 10-12, the same day agent sessions began driving the live Obsidian heavily via `obsidian eval`/`command` (link-registry migration) and Crucible's command/chain usage jumped. Two events can't separate "new trigger path" from "rate scales with chain executions".
- **For crucible-vault:** the fault class (Node async-context inside an Electron renderer) does not exist in a Rust daemon plus native GUI. A Tauri/WebKitGTK stack doesn't run Node in the webview, so it doesn't inherit it either. The graphics stack shows no instability here (0 GPU resets over 12 boots on RDNA4/RADV), but this is an absence of evidence, not a stress test. S3 watches the kernel log during the GUI runs.
- **For obsidian-crucible (separate repo, out of scope here):** this is an actionable defect. Obsidian crashes when a Crucible chain takes `withLock` from a palette command. Candidate fixes for that repo's own plan: drop `AsyncLocalStorage` for explicit context passing (or a re-entrancy token), or run lock bookkeeping outside the renderer. Not acted on from this stream.

## A/B (`--disable-gpu`)

Not recommended. The crash is in the renderer's JS/V8 path, and `--disable-gpu` only moves compositing and raster off the GPU process, so it can't change this signature. With 2 events in 14 days, an A/B would also take weeks to mean anything. The INITIATIVE `# Blocked on User` A/B item is withdrawn on this evidence.
