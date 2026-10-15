//! `editor-model` — the editor's pure logic, free of iced and any async runtime (plan `v0-editor.md` Summary): the undo model, source-editing assists (list continuation, smart Home/End), the markdown line highlighter, the switcher ranking layer, period date math, the outline, and the OFM→render-model lowering.

pub mod editing;
pub mod find;
pub mod highlight;
pub mod outline;
pub mod period;
pub mod rank;
pub mod render;
pub mod undo;
