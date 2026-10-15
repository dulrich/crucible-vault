//! Bookmarks (WP-4, inventory #10).
//!
//! Stored in the vault at [`BOOKMARKS_PATH`] as `[[bookmark]] name = "…", path = "…"`, the shape
//! of Crucible's `Shortcut{name, file}` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/types.ts:20-23`; the
//! Obsidian-induced part — living inside plugin `data.json` — is dropped: the list is a vault
//! file the daemon writes). Reads are local disk reads; every edit rewrites the whole file through
//! `write_note` with the last-read hash (CAS), or the empty-hash absence-CAS when the file is absent
//! — the shared exact-name config write [`crate::settings::write_config`] (plan
//! `editor-settings-tab`, audit M6: never `create_note`, so a file that appeared since load is a
//! conflict, never a suffixed copy).

use cv_protocol::Mode;
use iced::widget::{button, column, row, text};
use iced::{Element, Length};
use serde::{Deserialize, Serialize};

use crate::daemon::{ContentHash, DaemonClient, DaemonError};
use crate::message::{Message, PanesMsg};
pub use crate::settings::WriteOutcome;
use crate::theme::Tokens;

/// The vault file.
pub const BOOKMARKS_PATH: &str = "_custom/crucible-vault/bookmarks.toml";

/// One bookmark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    bookmark: Vec<Bookmark>,
}

/// Bookmark messages (wrapped in [`PanesMsg::Bookmark`]).
#[derive(Debug, Clone)]
pub enum BookmarkMsg {
    /// Bookmark the focused pane's active tab.
    AddActive,
    Remove(usize),
    Up(usize),
    Down(usize),
    Open(String),
    /// A write finished.
    Written(Result<WriteOutcome, String>),
}

/// A whole-file write ready to run off the UI thread.
#[derive(Debug, Clone)]
pub struct WriteJob {
    content: String,
    base: Option<ContentHash>,
}

impl WriteJob {
    /// Runs the write (blocking).
    pub fn run(&self, d: &dyn DaemonClient) -> Result<WriteOutcome, String> {
        crate::settings::write_config(d, BOOKMARKS_PATH, &self.content, self.base.as_ref())
    }
}

/// The loaded list and the hash it was read at (`None` = file absent).
#[derive(Debug, Default)]
pub struct Bookmarks {
    pub items: Vec<Bookmark>,
    pub hash: Option<ContentHash>,
    /// A write is in flight (edits wait for it).
    pub writing: bool,
}

/// Parses the file body.
pub fn parse(bytes: &[u8]) -> Result<Vec<Bookmark>, String> {
    let s = std::str::from_utf8(bytes).map_err(|_| "bookmarks: not UTF-8".to_owned())?;
    toml::from_str::<File>(s)
        .map(|f| f.bookmark)
        .map_err(|e| format!("bookmarks: {}", e.message()))
}

/// Serializes the list.
pub fn render(items: &[Bookmark]) -> Result<String, String> {
    toml::to_string(&File {
        bookmark: items.to_vec(),
    })
    .map_err(|e| format!("bookmarks: {e}"))
}

impl Bookmarks {
    /// (Re)loads from disk; a missing file is an empty list.
    pub fn load(&mut self, d: &dyn DaemonClient) -> Result<(), String> {
        match d.read_file(BOOKMARKS_PATH) {
            Ok((bytes, hash)) => {
                self.items = parse(&bytes)?;
                self.hash = Some(hash);
            }
            Err(DaemonError::NotFound(_)) => {
                self.items.clear();
                self.hash = None;
            }
            Err(e) => return Err(format!("bookmarks: {e}")),
        }
        Ok(())
    }

    /// Applies an edit locally and returns the write that persists it.
    pub fn edit(&mut self, m: &BookmarkMsg, active: Option<&str>) -> Option<WriteJob> {
        let n = self.items.len();
        match *m {
            BookmarkMsg::AddActive => {
                let path = active?;
                if self.items.iter().any(|b| b.path == path) {
                    return None;
                }
                let leaf = path.rsplit('/').next().unwrap_or(path);
                let name = leaf.strip_suffix(".md").unwrap_or(leaf).to_owned();
                self.items.push(Bookmark {
                    name,
                    path: path.to_owned(),
                });
            }
            BookmarkMsg::Remove(i) if i < n => {
                self.items.remove(i);
            }
            BookmarkMsg::Up(i) if i > 0 && i < n => self.items.swap(i - 1, i),
            BookmarkMsg::Down(i) if i + 1 < n => self.items.swap(i, i + 1),
            _ => return None,
        }
        let content = render(&self.items).ok()?;
        self.writing = true;
        Some(WriteJob {
            content,
            base: self.hash.clone(),
        })
    }
}

/// The bookmarks list (pane side).
pub fn view<'a>(b: &'a Bookmarks, tokens: Tokens, mode: Mode) -> Element<'a, Message> {
    let editable = mode != Mode::Shadow && !b.writing;
    let msg = |m: BookmarkMsg| Message::Panes(PanesMsg::Bookmark(m));
    let small = |label: &'a str, m: BookmarkMsg| {
        let bt = button(text(label).size(13)).style(button::text);
        if editable { bt.on_press(msg(m)) } else { bt }
    };
    let mut col = column![
        row![
            text("Bookmarks").size(14).color(tokens.heading.to_color()),
            small("+ active", BookmarkMsg::AddActive),
        ]
        .spacing(6)
    ]
    .spacing(2);
    if mode == Mode::Shadow {
        col = col.push(
            text("bookmark edits are disabled in shadow mode")
                .size(12)
                .color(tokens.warn_text.to_color()),
        );
    }
    for (i, bm) in b.items.iter().enumerate() {
        col = col.push(
            row![
                button(text(bm.name.as_str()).size(14))
                    .style(button::text)
                    .width(Length::Fill)
                    .on_press(msg(BookmarkMsg::Open(bm.path.clone()))),
                small("↑", BookmarkMsg::Up(i)),
                small("↓", BookmarkMsg::Down(i)),
                small("×", BookmarkMsg::Remove(i)),
            ]
            .spacing(2),
        );
    }
    col.into()
}
