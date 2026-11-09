# S2 core daemon / index prototype — FINDINGS (2026-10-13)

Throwaway spike. Code: `runs/spike/core-index/` (`src/main.rs`, binary `cvx`; `watch_bench.py`; `bases_daily_reading.sql`). Scratch data: `~/.local/share/crucible-vault-spike/` (`live.db`, `vault-copy/`, `copy.db`, `synthetic-vault/`, `synth.db`). Live vault only read. All numbers are aggregates. Rust 1.98.1, release profile, **warm page cache on every run** (no sudo to drop caches).

Build: `cd runs/spike/core-index && cargo build --release` (first build 40.8 s, dominated by mdview's syntect/two-face `build.rs`). Binary 7.4 MB unstripped.

## Index design (as built)
- `walkdir`; skip `.obsidian/`, `.git/`, `.trash/`. `_resources/` (`userIgnoreFilters`) is **not parsed but recorded as link targets** — the first run excluded it entirely and resolution collapsed (1427/1667 embeds, 6101/6104 md-images unresolved). Obsidian's ignore filter hides files from search/graph; it does not remove link targets.
- Tables: `files(path, folder, name, ext, is_md, size, mtime, has_fm)`, `props(file_id, key, value-as-JSON)`, `links(src_id, kind[wiki|embed|md|mdimg|fm], raw, target, fragment, resolved_id NULL=unresolved)`, `tags(file_id, tag, source[fm|inline])`, `blocks(file_id, kind[callout|fndef|fnref], label)`, `fts` = contentless FTS5 (`contentless_delete=1`) over body. WAL, `synchronous=NORMAL`, one transaction per full index.
- Resolution: lowercase exact vault path (± `.md`, also relative to source dir) → unique basename → shortest path among basename matches (path-suffix filtered when link has folders); empty target (`[[#h]]`) → self. Frontmatter string values `[[x]]` indexed as `fm` links.
- Lesson: the first full index took 16.5–16.9 s (3 runs) because per-file `DELETE … WHERE file_id=?` had no `file_id` index (O(n²)). With indexes: 4.4–4.6 s.

## Measurements

| Metric | Runs | Value / spread | Command |
|---|---|---|---|
| Cold full index wall (warm cache) | 5 across builds; final build 2 | 4.40–4.56 s (final: 4.56, 4.56) | `/usr/bin/time -v ./target/release/cvx index $VAULT ~/.local/share/crucible-vault-spike/live.db` |
| …parse share (mdview split+yaml+lex, regex tags) | same | 2.72–2.76 s (≈61 %); walk 29–45 ms; SQLite writes ≈1.7 s | stderr `index:` line of same command |
| Peak RSS full index | same | 26.3–26.8 MB without `_resources` rows; 31.3–31.9 MB with | same (`Maximum resident set size`) |
| DB size | same | 53.4 MB final (deterministic across runs) | `stat -c %s ~/.local/share/crucible-vault-spike/live.db` |
| Single-file re-index write→commit (N=20 appends, 250 ms apart, vault-copy of 8008 `.md`) | 2 seeds | seed1 p50 10.65 ms, max 22.39, min 4.95; seed2 p50 7.16, max 26.62, min 5.01 | `python3 -I watch_bench.py ./target/release/cvx ~/.local/share/crucible-vault-spike/vault-copy ~/.local/share/crucible-vault-spike/copy.db 20 <1|2>` |
| …handler time (event→commit) | same | p50 10.50 / 6.99 ms, max 22.28 / 26.43 ms; 0 duplicate commits (size+mtime dedupe) | same |
| Idle daemon RSS / PSS at 60 s and 90 s | 2 runs × 2 samples | RSS 16.2 / 16.8 MB, PSS 14.0 / 14.6 MB, Private_Dirty 9.0 / 9.6 MB; flat between samples | same script (`/proc/<pid>/smaps_rollup`) |

Re-index latency is dominated by a naive `Resolver::load` (re-reads all 15.5k file rows per event); an in-memory resolver kept in sync should put it near 1 ms. inotify delivery itself is sub-ms (synthetic run: `Create(File)` handled in 85 µs).

## Aggregates (live vault, final run)
- Walked 15,503 files = 8,008 `.md` indexed (85.6 MB) + 118 non-md outside `_resources` + 7,377 `_resources` targets (1.5 GB; 2 are `.md`, not parsed).
- Frontmatter: 7,939 / 8,008 files. 71 distinct prop keys, 100,806 prop rows.
- Links: 17,913 total, 5,293 unresolved. By kind (total / unresolved): wiki 902 / 647, embed 1,667 / 1, md-link 3,618 / 1,512, md-image 6,104 / 225, frontmatter `[[…]]` 5,622 / 2,908.
- Unresolved shape: 1,438 md-links are root-absolute (`/…`) site-relative URLs from web clippings (no such file); 624 wikilinks and ~2,800 fm links are bare names with no note (Obsidian reports these unresolved too); images mostly remote-clip leftovers. 509 ambiguous md basenames (shortest-path rule exercised).
- Tags: 23,629 frontmatter occurrences (7,427 distinct) + 6,556 inline (1,924 distinct); 8,480 distinct case-insensitive; 5,229 files tagged. (First regex counted `](#anchor)` as tags — fixed; inline-tag rules need a proper tokenizer pass, not a regex.)
- Blocks: 111 callouts (quote 52, note 42, tip 8, warning 4, important 4, caution 1), 995 footnote defs, 1,220 footnote refs.

## mdview reuse verdict
Path dependency on `https://github.com/dulrich/crucible-tui/tree/master/crates/mdview` (unmodified) works and produced the numbers above.
- **Fit:** `frontmatter::split_frontmatter`, `frontmatter::parse_frontmatter` → `JsValue` (`JsObject::entries()`), `lex::lex(body, glyphs::Tier::Unicode)` → `token::Token` tree with `Wikilink{slug, fragment, embed, alias}`, `Link{href}`, `Image{href}`, nested List/Table/Blockquote. `#heading`, `|alias`, `![[embed]]` correct with no extra work.
- **Reimplemented:** tags (fm list/string + inline), footnotes, callouts, fm `[[link]]` values, link resolution (mdview `wikiresolve` probes the filesystem per lookup — viewer-shaped, unusable for batch), percent-decoding md hrefs, `JsValue`→JSON. The glyph `Tier` argument to the lexer is a rendering leak. Tokens carry no byte spans (needed later for rename/link rewrite).
- **Unwanted deps:** `syntect` + `two-face` (runtime and a grammar-dumping `build.rs` — most of the 40 s first build), `rustix` with `termios`, `ryu-js`, `unicode-segmentation`; pager/CLI/ANSI modules compile in unused. 85 crates in the normal-deps tree. The lexer carries marked.js render-parity machinery (BOM stand-ins, text re-slicing); parse = 61 % of cold index (~0.34 ms/file).
- **Recommendation: extract a shared `ofm` crate** (pure: frontmatter split + YAML→value, comrak options, OFM walk exposing wikilinks/embeds/md links/tags/footnotes/callouts/block ids with byte spans; no glyph tier, no syntect/rustix), consumed by mdview (render layer on top) and crucible-vault. Depending on mdview as-is is wrong for v0 (render deps, wrong abstraction, cross-repo path dep); owning a second parser forks OFM semantics. Not measured: comrak-only parse time (would size the marked-parity overhead — cheap follow-up in the extraction plan).

## Bases probe — `_custom/bases/daily-reading-list.base`
Translated to `bases_daily_reading.sql`; executed (python `sqlite3`, `:this_name` bound per folder) for all 189 `daily/day/<name>` folders against `live.db`: 2,713 rows total, 1.2 ms average per evaluation.

| Expression | Mapping |
|---|---|
| `file.folder == "daily/day/" + this.file.name` | Clean: `f.folder = 'daily/day/' \|\| :this_name` (needs embedding-context binding for `this`) |
| `note["word-count"]`, `number(...)` | Clean: `CAST(props.value AS REAL)` |
| `file("_custom/Settings.md").properties.reading_wpm` | Clean: join on `files.path` + `props.key` |
| `(a / b).round(1)` | Clean: `round(a/b, 1)` |
| `if(note.read == true, x, y)` | Clean: `CASE WHEN value='true'` (requires JSON-typed prop storage: `true` vs `"true"`) |
| `list(note.tags).map(value.toString().replace("#","")).filter(value != …).join(", ")` | Hand-mapped via `json_each`+`replace`+`NOT IN`+`group_concat`, but general lambda/list methods need a **custom evaluator** (`group_concat` order unspecified, `toString` semantics) |
| `views[].order`, `summaries: Sum` | Clean: SELECT list + `SUM()` over result set |

Verdict: SQL for selection (filters, property lookups, summaries); a small Rust expression evaluator for formulas with list methods and `this` context.

## URI probe
`CVX_VAULTS=~/.local/share/crucible-vault-spike/synthetic-vault ./target/release/cvx uri '<url>'` (direct invocation; no scheme handler touched):
- `obsidian://new?vault=Synth&file=Clipped%20Article&content=%23%20Hello%0A%0Atag%20%23clip%20see%20%5B%5Bseed%5D%5D` → `Clipped Article.md`, 31 bytes, decoded correctly.
- Same `file` twice more (one with `&silent=true`) → `Clipped Article 1.md`, `Clipped Article 2.md` (Obsidian ` N` suffix); `silent` parsed.
- `file=%2Fnotes%2F.%2F..%2Fnotes%2FDeep%20Note&content=deep%20%C3%A9%E2%9C%85` → `notes/Deep Note.md`, UTF-8 intact.
- `file=..%2F..%2Fescape` → clamped to vault root `escape.md`; `vault=..` → rejected.
- `&clipboard`: spike reads a `CVX_CLIPBOARD` stand-in (user clipboard untouched); missing → explicit error. Real impl needs `wl-paste`/portal.
- Writes are tmp+rename. **Index pickup:** with `cvx daemon` on the synthetic vault, two URI creations were indexed (inline tag `clip` present) and **previously-unresolved links in `seed.md` re-resolved** (`[[Clipped Article]]`, `[[notes/Deep Note#Intro|alias]]`). Noise: one URI write = 5 events (Create/Modify/Name From/To/Both across `.tmp` and final) — the daemon must ignore its own temp names and coalesce; better, since the daemon is the only writer, index its own writes directly and suppress the echo.

## Recommendation
- **SQLite viable** for the derived index: 4.5 s full rebuild of 8k notes at ~32 MB peak RSS, 53 MB DB, ~1 ms Bases-style queries, FTS5 contentless via bundled rusqlite. Rebuild is cheap enough to never migrate the index — drop and rebuild on schema change.
- **Daemon shape:** single writer thread owning the `Connection`; in-memory resolver map kept in sync; `notify` → dedupe (size, mtime) → per-file transaction; on create/rename re-resolve null links, on delete null inbound. Idle 14–15 MB PSS.
- **Surprises:** (1) `_resources` must stay a link-target set despite being ignored; (2) missing `file_id` indexes cost ~4× on full index; (3) parse, not I/O or SQLite, is the biggest cost; (4) ~30 % of links are unresolved by nature (clip URLs, uncreated notes) — UI should separate "external-ish" from "missing note".
