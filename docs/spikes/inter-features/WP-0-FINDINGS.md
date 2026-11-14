# Inter 4.1 OpenType feature spike (2026-11-13)

Command: copy `zz_spike_inter_features.rs` to `crates/editor/tests/`, then `cargo test -p editor --test zz_spike_inter_features -- --nocapture` (cosmic-text 0.15 via iced_graphics re-export, Inter 4.1 from fonts-inter). Output: `out.txt`.

- Default shaping (calt on): substitutes `<-`, `<->`, `=>`, `<=>`, `<--`. Not `->`, `-->`, `--`, `!=`, `>=`, `<=`, `==`, `...`, `~>`, `|>`.
- `dlig`, `liga`: no change on any sequence. ss05/ss06 (circled/squared forms), ss07 (square punctuation), `case`, `frac` and cv01 change glyphs but are not arrows or ligatures.
- So Inter has nothing more to unlock. Separately, iced 0.14 builds `Attrs` without `font_features` (`iced_graphics-0.14.0/src/text.rs:260`), so any feature beyond the defaults would need an iced patch or a paragraph built outside iced.
- Source and Reading both use `theme::FONT_FAMILY` = "Inter".
- Inter 3.19 (rsms/inter release zip, `Inter Desktop/Inter-Regular.otf`, loaded alone via `SPIKE_FONT` with the `fontdb` variant of the test): identical default substitutions; no feature adds `->`/`--`/`!=`/`>=`/`...` forms. Output: `out-inter319.txt`.
- The pre-cutover Obsidian vault (`vault-repo` `d97ded79^`) used Inter for text and interface plus an `inter-ligatures.css` snippet setting `liga`/`calt`/`dlig` on headings only.
