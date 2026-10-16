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

use cv_schema::{ConfigDoc, Path};

use crate::message::{Message, PanesMsg};
pub use crate::settings::WriteOutcome;
use crate::settings::{ConfigFile, DocEdit, ScalarValue};
use crate::theme::Tokens;

/// The vault file.
pub const BOOKMARKS_PATH: &str = "_custom/crucible-vault/bookmarks.toml";

/// One bookmark.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: String,
    /// Plan editor-ui-round-3 WP-9: the chords bound to "Open bookmark: <name>" (the shape of
    /// an action's `keys`); absent = none. Stored on the record, so it follows the bookmark
    /// through renames and reorders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
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
    /// WP-9: set (`Some`) or drop (`None`) the `keys` of the bookmark at this path (Hotkeys
    /// page), written straight through the shared CAS write.
    SetKeys(String, Option<Vec<String>>),
    /// A write finished.
    Written(Result<WriteOutcome, String>),
}

/// The panel's list, derived from the shared `bookmarks.toml` settings [`ConfigFile`] — the one
/// loaded doc + hash both the panel and the Settings Bookmarks page edit (plan
/// `editor-settings-tab` WP-3), so a save from either surface never conflicts with the other.
#[derive(Debug, Default)]
pub struct Bookmarks {
    pub items: Vec<Bookmark>,
    /// A write of the shared file is in flight (edits wait for it).
    pub writing: bool,
    /// WP-9: paths of bookmarks the index does not hold (refreshed on load, `FileChanged` and
    /// `IndexGeneration`; never queried per frame).
    pub missing: std::collections::BTreeSet<String>,
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

/// The bookmarks in a `bookmarks.toml` doc (entries without a `path` are skipped).
pub fn items_of(doc: &ConfigDoc) -> Vec<Bookmark> {
    let list = Path::root().key("bookmark");
    (0..doc.len(&list))
        .filter_map(|i| {
            let p = list.index(i);
            let path = doc.get_str(&p.key("path"))?.to_owned();
            let name = doc
                .get_str(&p.key("name"))
                .map_or_else(|| path.clone(), str::to_owned);
            let keys = doc.get_str_list(&p.key("keys")).unwrap_or_default();
            Some(Bookmark { name, path, keys })
        })
        .collect()
}

/// The `[[bookmark]]` index whose `path` is `path` in the doc.
pub fn doc_index(doc: &ConfigDoc, path: &str) -> Option<usize> {
    let list = Path::root().key("bookmark");
    (0..doc.len(&list)).find(|&i| doc.get_str(&list.index(i).key("path")) == Some(path))
}

/// WP-9: the edit setting (`Some`) or dropping (`None`) the bookmark's `keys` (`None`: the
/// bookmark is gone, or there is nothing to drop).
pub fn keys_edit(doc: &ConfigDoc, path: &str, keys: Option<&[String]>) -> Option<DocEdit> {
    let p = Path::root()
        .key("bookmark")
        .index(doc_index(doc, path)?)
        .key("keys");
    match keys {
        Some(k) => Some(DocEdit::Set(p, ScalarValue::StrList(k.to_vec()))),
        None => doc.get(&p).map(|_| DocEdit::Remove(p)),
    }
}

/// The document edit a panel message makes (`None`: nothing to do).
pub fn doc_edit(m: &BookmarkMsg, items: &[Bookmark], active: Option<&str>) -> Option<DocEdit> {
    let list = Path::root().key("bookmark");
    let n = items.len();
    Some(match *m {
        BookmarkMsg::AddActive => {
            let path = active?;
            if items.iter().any(|b| b.path == path) {
                return None;
            }
            let leaf = path.rsplit('/').next().unwrap_or(path);
            let name = leaf.strip_suffix(".md").unwrap_or(leaf).to_owned();
            DocEdit::InsertRecord(
                list,
                n,
                vec![
                    ("name".into(), ScalarValue::Str(name)),
                    ("path".into(), ScalarValue::Str(path.to_owned())),
                ],
            )
        }
        BookmarkMsg::Remove(i) if i < n => DocEdit::Remove(list.index(i)),
        BookmarkMsg::Up(i) if i > 0 && i < n => DocEdit::Move(list, i, i - 1),
        BookmarkMsg::Down(i) if i + 1 < n => DocEdit::Move(list, i, i + 1),
        _ => return None,
    })
}

impl Bookmarks {
    /// WP-9: recomputes [`Self::missing`] with `held` (index membership).
    pub fn refresh_missing(&mut self, held: impl Fn(&str) -> bool) {
        self.missing = self
            .items
            .iter()
            .filter(|b| !held(&b.path))
            .map(|b| b.path.clone())
            .collect();
    }

    /// Re-derives the list from the shared file.
    pub fn sync(&mut self, f: &ConfigFile) {
        self.items = f.doc.as_ref().map(items_of).unwrap_or_default();
        self.writing = f.saving;
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
    let mut head = row![
        text("Bookmarks").size(14).color(tokens.heading.to_color()),
        small("+ active", BookmarkMsg::AddActive),
    ]
    .spacing(6);
    if !b.missing.is_empty() {
        // WP-9: the warning pill — how many bookmarks point at paths the index lacks.
        head = head.push(
            text(format!("{} missing", b.missing.len()))
                .size(12)
                .color(tokens.warn_text.to_color()),
        );
    }
    let mut col = column![head].spacing(2);
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
                button({
                    let t = text(bm.name.as_str()).size(14);
                    if b.missing.contains(&bm.path) {
                        t.color(tokens.warn_text.to_color())
                    } else {
                        t
                    }
                })
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
