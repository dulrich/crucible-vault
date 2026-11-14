//! `editor-model` — the editor's pure logic, free of iced and any async runtime (plan `v0-editor.md` Summary): the undo model, source-editing assists (list continuation, smart Home/End), the markdown line highlighter, the switcher ranking layer, period date math, the outline, and the OFM→render-model lowering.

pub mod autolint;
pub mod carry;
pub mod chord;
pub mod comments;
pub mod editing;
pub mod edits;
pub mod find;
pub mod highlight;
pub mod hints;
pub mod keymap;
pub mod link_at;
pub mod link_form;
pub mod outline;
pub mod paths;
pub mod period;
pub mod place;
pub mod props;
pub mod rank;
pub mod reading;
pub mod render;
pub mod select_all;
pub mod undo;
pub mod visible;
