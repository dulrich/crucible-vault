# source-scroll spike (2026-10-17)

Question: does `scroll_content`'s "scroll up 4×line_count, then down `line`" (crates/editor/src/source/mod.rs:1477-1486) land on `line`?

Command: `cd docs/spikes/source-scroll && cargo run -q --offline` (cosmic-text =0.15.0, 200 lines, metrics 14/18.2, 30-line viewport).

Result:
- up 4n then down 50 → `Scroll { line: 0, vertical: 0.0 }` (lands at top)
- single down 50 from 0 → `Scroll { line: 49, vertical: 18.2 }` (lands on target)
- up 4n then down 190 (go-to-end) → `Scroll { line: 0 }`

Conclusion: the relative scrolls accumulate into one negative offset that clamps to 0 at shaping. Every carried Source place lands at the top. Fix direction: reset to a known origin without a negative accumulation (e.g. a single relative scroll from the current measured line, or setting the buffer scroll absolutely), then verify with a layout-backed test, not the `top_line` field.
