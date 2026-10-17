# Spike: Reading-view text selection on iced 0.14 (2026-10-17)

**Verdict: GO with caveat** — an owned widget built only on the public `iced_core::text::Paragraph` trait does styled rich text, char-precise drag selection, cross-block selection, copy text, link click-vs-drag, wrapping and multibyte. Caveat: the trait exposes no visual-line byte ranges, so the widget recovers them by probing `hit_test` at each line's left/right edge (works, but relies on cosmic-text `Buffer::hit` snapping out-of-bounds x to line ends).

## Results

| # | Criterion | Result | Evidence |
|---|---|---|---|
| 1 | Styled rich text (plain, bold, link, monospace code) | PASS | `out/01-single-word.png` (all span kinds visible, link blue + underlined) |
| 2 | Char-precise press-drag-release | PASS | `c1_single_word` = "world"; `c2_every_char_boundary_block2`: 35/35 boundaries exact |
| 3 | Selection across separate paragraph widgets in a column in a scrollable | PASS | `c3_cross_block`, `c3_reverse_three_blocks` (focus before anchor, 3 blocks); `out/03-cross-block.png` |
| 4 | Selected plain text, blocks joined by `\n\n`, via Ctrl+C | PASS | Ctrl+C KeyPressed → `SelEvent::Copy` → `App::selected_text()`; all `check(...)` asserts compare the copied string |
| 5 | Link click without drag still yields link; drag on link selects | PASS | `c5_link_click` (2 px jitter < 4 px threshold → link msg, no selection); `c5_drag_on_link_selects` (clicked = []) |
| 6 | Wrapped paragraph (4 lines) + multibyte (`café — naïve 日本語`, `ünïcödé`) | PASS | `c6_wrapped`, `c6_multibyte`; `out/02-wrapped-multiline.png`, `out/04-multibyte.png` (rects visually aligned, no panics) |

PNGs were inspected: rects hug glyph edges on every line, including bold-span and CJK boundaries.

## Commands

```
cd runs/spike/reading-select
cargo build --offline --release     # ok (needs iced_renderer feature tiny-skia; release build compile_error!s without a renderer feature)
./target/release/reading-select-spike   # all checks PASS, "ALL PASS"
```

Timing (release, 3 blocks): drag-move event incl. UI build + update + layout + draw-list (incl. selection rect computation) **0.229 ms/event**; full tiny-skia raster 600x400 **1.41 ms/frame**.

## Size

Widget `src/selectable.rs`: **215 non-blank non-comment lines (~0.2 kSLOC)**; harness `src/main.rs` 229. Deps: iced_core/iced_widget (advanced), iced_runtime, iced_tiny_skia, iced_renderer(tiny-skia), image(png), unicode-segmentation — all offline from cache, versions per the workspace lock.

## API approach that worked

Trait-only, no `raw()` / cosmic Buffer access:
- `Paragraph::with_spans` for layout (as `rich.rs`); `fill_paragraph` to draw; `span_bounds` for link underline; `hit_span` → link of the pressed span.
- `hit_test(point)` → `Hit::CharOffset` = **byte offset** into the concatenated span text (cosmic `Cursor.index`; `iced_graphics-0.14.0/src/text/paragraph.rs:262`). Spans must not contain `\n` (would split cosmic BufferLines and make the offset line-relative).
- Gap: no API returns visual-line ranges and `hit_test` doesn't return the line. Workaround: line count = `min_bounds().height / line_height`; line k range = `hit_test((-1, y_k))..hit_test((1e6, y_k))`.
- `grapheme_position(line, g)` takes a **grapheme index relative to the visual line** (counted with `graphemes(false)`, `paragraph.rs:351-391`), so byte→x = count graphemes in `text[line_start..byte]`. Returns glyph-left for index 0 and glyph-right otherwise — correct for selection edges.
- Cross-block: each block widget sees every `CursorMoved` (iced delivers events to all widgets); while the app says a press is active, the block whose vertical band contains the cursor publishes `Drag{block, byte}` (x/y clamped into the block). App owns `anchor/focus: (block, byte)` and the 4 px click/drag threshold.

## Spike shortcuts / what the production WP must handle

- Release and Ctrl+C are emitted by block 0 only (spike hack) — production needs a selection-root container widget (or app subscription) for document-global events, and should write the clipboard (`clipboard.write`).
- Cursor in the gap between blocks or past the last block: no Drag emitted (selection stalls); root should snap to nearest block / document end.
- Autoscroll while dragging near the scrollable edges; press point is in content coords, so scrolling mid-drag skews the threshold.
- Double-click word / triple-click paragraph, Shift+click extend, Shift+arrows, select-all.
- Find-highlight quads coexisting with selection quads (paint order + colors); hover cursor (`mouse_interaction` Pointer/Text).
- Non-paragraph blocks: list markers, tables, code blocks, headings, images, embeds — what copy text they contribute and how markers are excluded.
- Selection should be cleared/remapped when the document re-renders (block indices / byte offsets shift).
- `DEBUG_BOUNDS` thread-local in the widget is harness instrumentation only; the harness's `pt()` uses the same trait API as the widget to aim drags, so pixel exactness is confirmed by the PNGs, not independently by the asserts.
- RTL / bidi text and `align_x` other than left untested.
