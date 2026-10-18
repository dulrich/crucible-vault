# Spike: search sidecar — Node → Rust, vector/FTS performance, staying on SQLite (2026-10-17)

Source: crucible-vault `FEEDBACK.md` `# Ideas`: "evaluate search performance (ANN/int8/binary+rescore) and FTS performance" and "crucible-search sidecar node->rust". Bears on D8 (search placement, own-vs-depend deferred). Read-only against live: the live `crucible-search` container was only probed via `GET /health`. All benchmarks ran on a byte copy of its DB at `~/.local/share/crucible-vault-spike/search/` (outside every repo). Only aggregates are recorded here.

Host: Ryzen 9 7900X (12C/24T, Zen 4 with AVX-512), Node 24.21.0, SQLite 3.45.1 (python) and bundled rusqlite 0.40.2.

## 1. Corpus shape (live companion, `GET /health` + `sqlite3` on the copy)

- 91,878 chunks from 8,508 paths, one vault. 83,840 chunks are embedded with `bge-m3`, dim 1024, space `bge-m3/f16`; 8,038 (8.7%) have no embedding. The stored vectors are unit-norm f32 BLOBs.
- DB is 1.2 GB: `chunks` 615 MiB (343 MiB of it is f32 embeddings), `chunks_fts_data` 245 MiB, `chunks_fts_content` 156 MiB. The FTS table stores its own content, so chunk text is stored twice. `embedding_json` is NULL everywhere.
- Live container: 71 MiB at idle with the matrix not loaded (`vector.status = absent`), cgroup peak 939 MiB. `recentSearch.count = 0` since the 17:18 restart.

## 2. The decisive fact: nothing queries the companion anymore

- After cut-over (D19), `cv` only **feeds** the companion: the `search-refresh` action (`search_upsert`) runs on a schedule and pushes chunks plus bge-m3 embeddings through `cv-providers`. `cv-search` has no `/v1/search` caller (`crates/cv-search/src/lib.rs`; the client module docs say "not a feed" op).
- The editor's search uses the daemon's own `index.db` FTS5: a contentless table, one `body` column, `crates/cv-index/src/query.rs:442-447`.
- A fleet-wide grep for `:4801` / `/v1/search` outside `obsidian-crucible` finds no consumer. Obsidian+Crucible was the only reader.
- **So today the fleet pays for embedding and storing 1.2 GB that no reader uses, and the editor has no semantic or hybrid search.** The real question is less "port Node to Rust" than "where does hybrid search live, now that the daemon owns the vault?"

## 3. Vector scan variants (Rust, `bench/`, 200 doc-to-doc queries, self excluded, truth = exact f32 top-10)

Command: `CARGO_TARGET_DIR=~/.cache/cv-search-spike-target RUSTFLAGS="-C target-cpu=native" cargo build --release --offline` then `search-bench <copy> 200` → `vector.log`.

| Variant | ms/query (1 thr) | ms/query (4 thr) | recall@10 | resident |
|---|---|---|---|---|
| f32 exact (brute force) | 9.86 | 6.72 (12 thr: 6.37) | 1.000 | 327 MiB |
| int8 (global symmetric scale) | 3.17 | 1.07 | 0.966 | 81 MiB |
| binary sign, no rescore | 0.35 | 0.16 | 0.655 | 10 MiB |
| binary → top-100 → f32 rescore | 0.43 | 0.23 | 0.971 | 10 MiB + fetch |
| **binary → top-200 → f32 rescore** | **0.53** | 0.32 | **0.990** | 10 MiB + fetch |
| binary → top-400 → f32 rescore | 0.70 | 0.51 | 0.995 | 10 MiB + fetch |
| binary → top-200 → int8 rescore | 0.49 | — | 0.959 | 91 MiB |

- f32 brute force is memory-bandwidth bound: threading past 4 gains nothing.
- int8 rescore is worse than f32 rescore and costs 81 MiB. Rescore with f32 instead.
- Fetching the f32 vectors to rescore from SQLite by rowid on a warm cache (`rescore-io.log`): 100 → 0.18 ms, **200 → 0.37 ms**, 400 → 0.99 ms. So **binary in RAM + f32 kept in SQLite** comes to about 0.9 ms per query at recall 0.99, with ~10 MiB resident.
- Load from SQLite: 0.43 s for all 83,840 f32 rows. Building the binary codes is negligible on top.
- **ANN (HNSW etc.) is not needed at this scale.** A full binary scan takes 0.35 ms single-threaded. ANN would add an index structure, build time and a recall knob to save under 1 ms. Revisit only around 10× the corpus.
- Caveat: these are doc-to-doc queries. Real short-query embeddings have a different distribution, so binary recall should be re-checked on ~50 real queries embedded via the live embedder before choosing the candidate count. Use 200 as the default for now and 400 if recall drops.

## 4. Node companion end-to-end (scratch copy on :4899, `node_bench.py` → `node.log`, `node-warm.log`)

- Keywords only: p50 4.7 ms, p90 14.9 ms, max 61 ms.
- Hybrid (FTS + vector + RRF), matrix warm: **p50 22.2 ms, p90 25.4 ms, max 51.8 ms**. Server-side timings for one request were `primaryMs 15.2, vectorMs 15.8, totalMs 32`.
- **RSS 650 MiB** with the matrix loaded, which is the f32 `SharedArrayBuffer` plus the V8 baseline. The matrix rebuild took about 2 s on the copy.
- A Rust binary+rescore leg (~0.9 ms) would replace the ~16 ms vector leg and cut about 640 MiB of RSS. FTS (the other ~5–15 ms) is SQLite either way.

## 5. FTS5 on the companion schema (`fts_bench.py` → `fts.log`; bm25 weights title 4, heading 2; 40 queries per row)

| Query shape | p50 | p90 | max |
|---|---|---|---|
| 1 rare term (<10 docs) | 0.04 ms | 0.13 | 0.24 |
| 1 mid term (100–1k docs) | 0.23 ms | 0.55 | 1.85 |
| 1 common term (>5k docs) | 4.5 ms | 10.5 | 28.7 |
| 2 terms AND | 0.37 ms | 0.73 | 2.0 |
| 2 terms OR | 4.1 ms | 11.6 | 27.9 |
| 3 common terms OR | 13.4 ms | 24.2 | 35.1 |
| 2-char prefix `xx*` (typeahead) | **24.8 ms** | 42.6 | 46.1 |
| 3-char prefix | 5.5 ms | 22.4 | 44.7 |
| 5-char prefix (no prefix index) | 1.3 ms | 3.4 | 6.8 |

- FTS cost is driven by posting-list length and the ORDER BY bm25 over every match, not by Node. **SQLite FTS5 is adequate.** The tail sits on common-term OR queries and 1–2 char prefixes.
- Cheap fixes without leaving SQLite:
  - Start typeahead at 3 chars, or do not prefix-expand the last token below 3 chars. The companion always ORs `"t" OR "t"*`, which is the 2-char row above.
  - Drop `prefix='1 2'`, which mainly speeds the 1–2 char case typeahead should skip, and keep `'3'`.
  - Rank common-term OR queries over a capped candidate set.
- **Storage:** an external-content FTS (`content='chunks'`) or a contentless FTS removes the 156 MiB duplicate copy of the text. The cv `index.db` FTS is already contentless.

## 6. Staying on SQLite

- **Yes.** Everything measured runs comfortably on SQLite: FTS5 for lexical search, a BLOB column for the f32 vectors, and a ~10 MiB in-memory binary code array for the scan.
- sqlite-vec / `vec0` brings no benefit at this size: the brute-force binary scan already runs under 1 ms. It would add a loadable extension to a `bundled` rusqlite build for no measured gain.
- Tantivy, LanceDB or Qdrant would be a second store to keep consistent with the vault, which D14 avoided.

## 7. Node → Rust: what the port actually is

- The companion is about 3.9k LOC of `.mjs`. Much of it is Node workarounds, which per AGENTS.md should not be carried forward:
  - the off-request time-sliced matrix rebuild (`vectors.mjs` WP-2);
  - the `SharedArrayBuffer` worker-shard pool (WP-3);
  - deadline/supersede plumbing for a cross-process HTTP client (`deadline.mjs`, `searchClients.mjs`);
  - the coverage map and the route-table guard.
- The parts worth porting:
  - the schema and upsert/delete/files-state semantics, which `cv-search` already speaks as a client;
  - the RRF hybrid ranking with the title/heading boosts (`ranking.mjs`, 355 LOC);
  - query construction (`search.mjs`, the FTS match-string building).
- `cv` already has the chunker (`cv-search/src/chunker.rs`, a byte-compatible port), the embedder client (`cv-providers`), FTS5 in rusqlite, and a durable queue for embedding jobs.
- **Option A: a separate Rust sidecar** (the D8 end state). This keeps an HTTP hop, a second SQLite owner and the feed/compare/reconcile protocol (`feed.rs` + `compare.rs`, ~600 LOC that exist only because the index lives in another process).
- **Option B (recommended for the plan to weigh): fold search into the daemon.**
  - Add a `search.db` (derived and rebuildable like `index.db`, D14) owned by `cv`, plus a `Request::Search { query, mode }` that the editor calls.
  - The vector leg is a 10 MiB binary array in daemon memory, with f32 rescore read from `search.db`.
  - The cost is ~10 MiB of resident memory added to the daemon's ~330 MiB plateau, against ~650 MiB and a Docker container saved.
  - It removes the HTTP feed, schema-version handshake and remote compare entirely.
  - Rough size: ~1.2–1.8 kSLOC of Rust: schema + upsert ~400, query building + RRF ~400, binary scan + rescore ~200, protocol + editor hook ~300–500, tests. The existing feed/compare code retires.
- Either option is a plan, not a direct change. It touches `cv-search`, `cv-protocol`, the `cv` daemon and an editor search surface. Also, `cv-search` today has no `rusqlite` edge, so the new edge (on an already-locked package) is the kind of exception AGENTS.md records.

## 8. Open questions for the plan (not answered by this spike)

1. Is semantic/hybrid search in the editor wanted at all? If not, the cheapest right move is to stop the `search-refresh` feed and retire the container (it saves embedding calls plus 1.2 GB), and keep `index.db` FTS.
2. Does anything outside the editor still need an HTTP search API (agents, MCP)? If so, the daemon's existing socket or a thin HTTP shim over `Request::Search` covers it. That decides Option A vs Option B.
3. Recall of binary+rescore on real query embeddings (see the §3 caveat): a ~50-query check against the live embedder.
4. The 8.7% unembedded chunks: are they images or empty chunks by design, or a backlog?

## Files

- `bench/` — Rust vector bench (throwaway; `[workspace]`-detached, own target dir).
- `fts_bench.py`, `node_bench.py` — python harnesses (run with `python3 -I`).
- `vector.log`, `fts.log`, `node.log`, `node-warm.log`, `rescore-io.log` — raw outputs.

## 9. Addendum (2026-10-17, planning pass): real-query recall + embedder

- The live embedder serves `bge-m3-f16` at `http://127.0.0.1:4811/v1` (≈10 ms warm, 2.3 s cold swap). Re-embedding a stored chunk's text gives cosine **1.0000** against the stored vector, so the 83,840 existing embeddings are importable as-is; nothing needs re-embedding.
- Real short queries (`real_query_recall.py` → `real-query-recall.log`, 40 generic queries, no vault text) behave worse than the doc-to-doc proxy in §3, so §3's 200-candidate default is wrong:

| Candidate pool (symmetric Hamming → f32 rescore) | mean recall@10 | min |
|---|---|---|
| 100 | 0.875 | 0.3 |
| 200 | 0.930 | 0.3 |
| 400 | 0.978 | 0.9 |
| **800** | **0.995** | 0.9 |

- Asymmetric scoring (f32 query · ±1 doc signs) reaches 0.978 at 200 and 0.990 at 400, but its scan costs about as much as int8. Symmetric Hamming + an 800-row f32 rescore (~2 ms fetch, from §3's I/O scaling) is the simpler choice at the same recall. **Default pool: 800, as a tunable.**
