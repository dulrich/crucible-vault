# PDF renderer spike (round-14 WP-5)

Measurement only. Aggregates only: the 57 live-vault PDFs (outside `.trash/`) were read-only, identified by sorted index, and no name, path, text or image is recorded. Every command is relative to `docs/spikes/pdf-render/` (run date 2026-10-25, host: Debian-family desktop, poppler 24.02.0).

## Method

- Standalone cargo project (`Cargo.toml` has `[workspace]`, own `Cargo.lock`, own `target/`). Bins: `base` (empty baseline), `r_hayro` (hayro =0.8.0), `r_pdfium` (pdfium-render 0.9.4 + prebuilt libpdfium). Each Rust bin renders page 1 and the middle page (index n/2) of one PDF to an in-memory raster buffer at a scale factor and prints an aggregate line (in-process ms, non-white pixel count).
- Poppler candidate: `pdftoppm -png -singlefile -r <dpi> -f N -l N` as a subprocess, timed externally (so poppler ms include process start, PNG encode and a tmpfile write; the Rust ms are in-process render only; their per-PDF `wall_ms` includes process start and, for pdfium, binding the library). Temp output is deleted per page.
- 1x = 96 dpi (scale 1.3333), 2x = 192 dpi (scale 2.6667). Peak RSS = `/usr/bin/time -f %M` max resident of one process: for Rust bins one process renders both pages of one PDF at one dpi; for poppler it is the max of the two single-page processes. Median/p95 are over per-PDF values, `page_ms` over the 114 rendered pages (57 x {page 1, mid}).
- Candidates surveyed: pure-Rust `hayro` 0.8.0 (Apache-2.0 OR MIT, actively developed; `cargo search`); `pdf-render` 1.2.0 (pdfluent SDK, `pdf-interpret`-based, a hayro lineage; not measured, adds nothing over hayro and has no independent track record); `pdfium-render` 0.9.4 with the bblanchon/pdfium-binaries build; poppler subprocess. Candidate 4 (poppler glib/cpp bindings) skipped: `libpoppler-glib.so.8`/`libpoppler-cpp.so.0` exist but no `-dev` headers/`.so` symlinks were probed or installed (needs `libpoppler-glib-dev` / `libpoppler-cpp-dev`, plus a bindings crate); the subprocess already prices the poppler engine.
- pdfium library: `https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-linux-x64.tgz` (chromium/8111, pdfium 158.0.8111.0), sha256 `bdc60126399f39fe768566a18d4be055a7b3681bc9e248e8a6656b4f6cae0aa2`, extracted in `vendor/pdfium/`; `lib/libpdfium.so` = 8,049,792 bytes. No sudo or system install. `vendor/` is not committed in the promoted copy: to re-run the pdfium bench, download that URL, check the sha256, and extract it into `vendor/pdfium/` here.

## Commands

- Build: `cargo build --release` (first build of both Rust candidates from a cold target: 37.6 s wall, `/usr/bin/time -v`).
- Hayro: `./bench.sh hayro results-hayro.tsv`; pdfium: `./bench.sh pdfium results-pdfium.tsv`; poppler: `./bench.sh poppler results-poppler.tsv`.
- Aggregation: `bash summarize.sh results-<cand>.tsv` (all numbers below).
- Binary sizes: `ls -l target/release/{base,r_hayro,r_pdfium}` (release, `lto=true`, `codegen-units=1`, `strip=true`, opt-level 3).
- Fidelity sanity: `paste results-hayro.tsv results-pdfium.tsv | awk ...` over the per-page non-white pixel counts at 96 dpi (hayro/pdfium ratio mean 0.89; 0 of 114 pages outside 0.5-2x). This is a coarse sanity check, not a visual comparison; no image was inspected or kept.
- Corpus shape: 57 PDFs, median 24 pages, max 459.
- Re-run check: `./bench.sh hayro results-hayro-rerun.tsv` then `bash summarize.sh results-hayro-rerun.tsv`: 96 dpi page median 6.5 ms vs 6.9 ms (p95 26.7 vs 35.9), RSS median 14180 vs 14448 kB, wall median 24.3 vs 25.7 ms: within noise.

## Results (57 PDFs, page 1 + mid page, 114 pages per dpi)

| | hayro 0.8.0 | pdfium (render + libpdfium) | poppler `pdftoppm` subprocess |
|---|---|---|---|
| 1x page ms median / p95 / max | 6.9 / 35.9 / 142.5 | 19.1 / 92.3 / 135.2 | 151.1 / 319.8 / 646.4 |
| 2x page ms median / p95 / max | 13.7 / 44.5 / 531.5 | 40.0 / 125.5 / 182.5 | 380.6 / 755.8 / 1388.4 |
| 1x per-PDF process wall ms (2 pages) median / p95 | 25.7 / 101.0 | 81.2 / 178.3 | 292.8 / 628.8 |
| 2x per-PDF process wall ms median / p95 | 40.0 / 112.2 | 118.7 / 261.2 | 797.9 / 1508.4 |
| 1x peak RSS kB median / p95 / max | 14448 / 41924 / 82180 | 16944 / 52052 / 74676 | 26548 / 34872 / 44856 |
| 2x peak RSS kB median / p95 / max | 24812 / 54512 / 84224 | 37340 / 63360 / 97128 | 37552 / 46960 / 59004 |
| failures (error/panic/crash) of 57 | 0 | 0 | 0 |
| blank output (Rust: zero non-white px; poppler: PNG < 3 KB) | 0 of 114 pages at each dpi | 0 of 114 | 0 of 114 |
| release binary size (baseline `base` = 309,696 B) | 6,026,520 B (+5.72 MB) | 1,166,784 B (+0.82 MB) plus `libpdfium.so` 8.05 MB at runtime | 0 in-app (`pdftoppm` 35 KB + `libpoppler.so.134` 3.6 MB, system) |
| unique crates in tree (`cargo tree -p X --prefix none -e normal \| sort -u \| wc -l`) | 87 (includes `image` 0.25.10, same as the editor) | 42 | n/a |
| licence | crate Apache-2.0 OR MIT, pure Rust, no native lib | crate MIT OR Apache-2.0; pdfium BSD-3-Clause / Apache-2.0 plus bundled freetype, lcms, openjpeg, libjpeg-turbo, zlib, ICU licences (`vendor/pdfium/licenses/`); the prebuilt wrapper is MIT | poppler GPL-2.0-or-later (subprocess boundary only, nothing linked) |
| Debian-family install need | none | `libpdfium.so` is not in the Debian/Ubuntu archive (`apt-cache policy` empty); must be shipped beside the binary or downloaded (pinned URL + checksum) | `poppler-utils` (718 kB installed) + `libpoppler134` (3.6 MB): already present on this host |

Caveats: the Rust 6.9/19.1 ms are in-process render only (no PNG encode, no output file); poppler's 151 ms includes process start (about 20 ms), PNG encode and file write, so it is not an apples-to-apples engine comparison but is the real cost of the subprocess route. Hayro's default features include `embed-fonts` and `embed-cmaps` (most of the 5.7 MB); a `default-features = false` size was not measured. Hayro's RSS includes the whole-file read into memory and its own thread pool (`vello_cpu`, `simd` feature). RSS is per fresh process; a long-lived editor with several rendered pages cached will add the RGBA buffers (a 2x A4 page is about 1587x2245x4 = 14 MB each).

## Recommendation (for a later in-app PDF viewing plan)

- Primary: **hayro** in-process. It is the fastest here (median 7 ms/page at 1x, 14 ms at 2x), needs nothing installed, is pure Rust and permissively licensed, returns an RGBA pixmap that feeds the existing iced image path directly, and its tree already shares `image` 0.25.10. The price is +5.7 MB binary and a new 87-crate subtree (pin exactly, one lock edit in that plan's dependency WP), plus the maturity risk of a young renderer (0.x, fidelity on unusual PDFs not visually verified here; the one outlier was a 531 ms 2x page).
- Fallback if hayro fidelity disappoints on the user's real documents: **pdfium** (2.5-3x slower than hayro but still sub-130 ms p95 at 2x, best fidelity by reputation, +0.8 MB binary) at the cost of shipping an 8 MB `libpdfium.so` and its licence bundle, and one more dynamic library for `install.sh`.
- Poppler subprocess: viable as a zero-dependency stopgap (works today, 0 failures) but 150-400 ms per page, per-render process and PNG round-trip, and a GPL tool the user's host must keep installed; not recommended as the viewer engine.
- Sizing for the follow-up plan: about 1.2-1.8 kSLOC. Page-render worker + LRU of rendered pages + cap on resident bitmaps (about 0.4 kSLOC `editor-model`/editor), PDF tab with scrolling, zoom and page navigation in iced (about 0.6-0.8 kSLOC), file-open/tab wiring and tests (about 0.3 kSLOC), dependency pin and install/migration touch (about 0.1 kSLOC). Roughly 2 WPs plus a visual-fidelity verification.
- Risks: editor RSS budget (render off the UI thread, render only visible pages at the device scale, evict aggressively, fixed cap on cached megapixels; the glibc arena cap in `cv-malloc` interacts with hayro's worker threads, so re-measure with `cap_arenas()`); large PDFs (max here 459 pages) must lazy-load per page; rendering is CPU-only; password-protected/encrypted PDFs and text selection/search are out of scope for a render-only first cut.

## Open questions

- Visual fidelity of hayro vs pdfium on the user's actual PDFs (scans, CJK, forms) was not judged: images could not be kept or inspected under the opacity rule. A synthetic-corpus or user-eyeballed side-by-side is needed before committing.
- Size with `hayro` `default-features = false` plus an own font resolver (to trim the 5.7 MB).
- Does the editor want text selection/search in PDFs (needs a text layer, not provided by a pure rasterizer)?
- Peak RSS inside the real editor process with hayro's thread pool and the arena cap.

## Fidelity follow-up (orchestrator, 2026-10-25, planning round 15)

Pixel agreement with no image kept or viewed: `cargo build --release --offline --bin fidelity && ./fidelity.sh` → `results-fidelity.tsv`. Each of the 57 PDFs has page 1 and its mid page rendered at 96 dpi by hayro, pdfium and poppler (`pdftoppm -gray` at the same pixel size, in a temp dir deleted per page). Metric: mean absolute gray difference (0–255) per page, and the share of pixels differing by more than 64. All 114 hayro/pdfium page pairs had identical dimensions.

| pair | mean diff median / p95 / max | >64 share % median / p95 / max |
|---|---|---|
| pdfium vs poppler (two mature renderers, control) | 7.12 / 13.52 / 36.79 | 3.35 / 6.12 / 8.59 |
| hayro vs poppler | 10.95 / 17.88 / 21.06 | 6.94 / 11.85 / 12.62 |
| hayro vs pdfium | 10.40 / 16.01 / 35.93 | 5.99 / 9.75 / 12.73 |

- hayro is further from poppler than pdfium on 95 of 114 pages, by a mean ratio of 1.56×.
- 0 pages are outliers (hayro vs poppler more than 2× the pdfium vs poppler difference and more than 10 levels above it). hayro's worst page (21.06) is milder than the control pair's worst (36.79).
- **Reading:** the difference is systematic and moderate, not page-specific. That fits a different text antialiasing/gamma/hinting style rather than missing content. A wrong or missing page element would show up as an outlier, and none does. Whether the style is acceptable to the eye is a user hand-check on the in-app viewer.
