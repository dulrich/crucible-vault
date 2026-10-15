//! `cvault-editor` — the iced GUI client of the crucible-vault daemon (plan `v0-editor.md`).
//!
//! The daemon is the sole writer (D1): every mutation goes through [`daemon::DaemonClient`];
//! reads come from disk and the read-only `cv-index`/`cv-state` handles.

pub mod app;
pub mod bookmarks;
pub mod clipboard_image;
pub mod daemon;
pub mod dialog;
pub mod explorer;
pub mod find;
pub mod header;
pub mod history;
pub mod icons;
pub mod image_cache;
pub mod message;
pub mod migrate;
pub mod nav;
pub mod palette;
pub mod panels;
pub mod panes;
pub mod reading;
pub mod settings;
pub mod source;
pub mod switcher;
pub mod theme;
pub mod ui_state;
pub mod views;
pub mod widget;

#[cfg(test)]
mod tests;
