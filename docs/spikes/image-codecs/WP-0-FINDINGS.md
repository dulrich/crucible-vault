# WP-0 findings — editor binary cost per `image` codec group (2026-11-22)

Editor release binary (`cvault-editor`), `image` =0.25.10, HEAD `7f2b41ad`. Each row adds its codecs to the previous row's set; base = png, jpeg, webp, gif, bmp.

| Set | Bytes | Δ vs base | New lock crates |
|---|---|---|---|
| base | 51,870,592 | — | — |
| + pnm, tga, hdr, dds, ff, ico | 52,163,504 | +293 KB | none |
| + qoi | 52,190,520 | +320 KB | qoi |
| + tiff | 53,085,744 | +1.215 MB | qoi, tiff, fax |
| + exr | 54,076,400 | +2.206 MB | + bit_field, exr, lebe, num-complex, pulp, pulp-wasm-simd-flag, raw-cpuid, reborrow, zune-inflate |

Not measured: AVIF decode (`avif-native` links the C library dav1d; the `avif` feature only encodes) and SVG (not an `image` codec).

Reproduce (raw output in `out.txt`): `git worktree add --detach <spike>/wt HEAD`; symlink `crucible-tui` and `semsearch` into `<spike>/` (the workspace's `../../../` path deps resolve there); `sh docs/spikes/image-codecs/measure.sh <spike>`; `git worktree remove --force <spike>/wt`.
