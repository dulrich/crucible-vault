# Spike: transparent editor background under CDE (2026-11-07)

Question: does an iced 0.14.0 window with `.transparent(true)` and an alpha application background blend with the desktop under CDE (Wayland), for both editor renderer builds?

Setup: a bare `[workspace]` crate pinning `iced = "=0.14.0"` (scratch, not kept); its load-bearing lines are `iced::application(..).transparent(true).style(|a, _| iced::theme::Style { background_color: Color { r: 0.07, g: 0.07, b: 0.09, a: *a }, text_color: Color::WHITE })` — the editor's exact iced pin and feature set (minus image/advanced), `Theme::Dark`, `.style()` background `rgba(0.07,0.07,0.09,0.55)`, one opaque `container` panel. Built `--release --offline` with `gpu` (default: wgpu + tiny-skia fallback) and `--no-default-features --features cpu`.

Commands: `./spike-<r> 0.55 &`, wait 3 s, `crucible msg capture $PWD/cap-<r>.png`, `crucible msg close-pid <pid>`.

Results (screen captures `cap-gpu.png` / `cap-cpu.png`, 3840x2160 — kept untracked in `runs/spike/transparent-window/`: they show the live desktop):
- gpu (wgpu): blends — wallpaper visible through the window background at the requested alpha; the opaque panel and text stay fully opaque. Works with no CDE-side change.
- cpu (tiny-skia): alpha ignored — window presents opaque near-black (the background composited over black). Transparency is wgpu-only on this host.
- The neighbouring terminal appears darker than 0.55 (its own background alpha is higher); matching it means picking the alpha (and tint) to match the terminal config, not a fixed 0.55.

Implication for a feature: window `transparent: true` + per-surround alpha on `bg` only (panels/editor surfaces stay opaque for legibility); the cpu build would silently stay opaque — acceptable, document it.

## Real editor (2026-11-07)

Throwaway patch in a scratch worktree (`/tmp/cv-transp-wt`, removed): `.transparent(true)` in `crates/editor/src/main.rs` + `iced_theme` palette `background` forced to `rgba(0,0,0,0.8)` (the user's foot `[colors-dark] alpha=0.8 background=000000`). Built `--release -p editor --features fake`, run `XDG_STATE_HOME=<scratch> cvault-editor --fake <scratch-vault>` with a seeded `editor.json` (one tab, split mode). Captures (untracked, same reason): `cap-editor.png` (no note), `cap-editor-split.png` (Source | Reading).

- Translucent as intended: empty-pane area, Reading pane, tab strip / nav row.
- Opaque by token (unchanged by the palette): side panel (`panel`), status bar, Reading frontmatter card (`inset`), any `panel_2` container.
- **Alpha stacking:** the Source pane (iced `text_editor`, default style background = `palette.background.base`) rendered ~opaque black — it repaints the alpha base over the already-alpha window background (0.8 over 0.8 ≈ 0.96). iced's extended palette preserves alpha (`iced_core-0.14.0/src/theme/palette.rs` deviate/mix go through Oklch with `a` kept), so every widget whose default style paints a `palette.background.*` shade stacks the same way.
- Consequence for the design: carry alpha once (the window/app background), and make widgets that repaint the base background paint transparent while transparency is on; token-painted chrome needs its own explicit alpha decision (configurable).
