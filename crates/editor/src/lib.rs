//! `cvault-editor` — the iced GUI client of the crucible-vault daemon (plan `v0-editor.md`).
//!
//! The daemon is the sole writer (D1): every mutation goes through [`daemon::DaemonClient`];
//! reads come from disk and the read-only `cv-index`/`cv-state` handles.

pub mod app;
pub mod bookmarks;
pub mod clipboard_image;
pub mod context_menu;
pub mod daemon;
pub mod dialog;
pub mod explorer;
pub mod find;
pub mod header;
pub mod history;
pub mod icons;
pub mod image_cache;
pub mod link_ta;
pub mod log;
pub mod memory;
pub mod message;
pub mod nav;
pub mod open_url;
pub mod page;
pub mod palette;
pub mod panels;
pub mod panes;
pub mod pdf;
pub mod reading;
pub mod settings;
pub mod source;
pub mod switcher;
pub mod task_status;
pub mod theme;
pub mod title_sync;
pub mod ui_state;
pub mod views;
pub mod widget;
