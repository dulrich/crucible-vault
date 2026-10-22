//! File explorer tree (WP-4).
//!
//! Built from the index's folder listing (`folder_files`/`subfolders`), patched incrementally
//! from `file_changed` deltas, and fully rebuilt on `index_generation` only when a delta could
//! not be applied (an external rename names only its destination). Folders sort before files,
//! case-insensitively. Create / rename / move / trash go through `create_note` / `rename` /
//! `delete` RPC; the daemon rewrites links (Obsidian `alwaysUpdateLinks: true`), never the
//! editor (D1). Rendered through the virtual list.

use std::collections::{BTreeMap, BTreeSet};

use cv_protocol::ChangeKind;
use iced::widget::{button, column, container, row, text, text_input};
use iced::{Element, Length};

use crate::daemon::LocalIndex;
use crate::message::{ExplorerMsg, Message};
use crate::theme::Tokens;
use crate::widget::virtual_list::{ListWindow, VirtualList};

/// Row height of the tree.
pub const ROW_HEIGHT: f32 = 24.0;

/// A file operation dispatched over RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Create { path: String },
    Rename { from: String, to: String },
    Delete { path: String },
}

/// A finished file operation: the final path (create may be suffixed) or an error.
#[derive(Debug, Clone)]
pub struct FileOpDone {
    pub op: FileOp,
    pub result: Result<String, String>,
}

impl FileOp {
    /// Runs the RPC (blocking; off the UI thread).
    pub fn run(&self, d: &dyn crate::daemon::DaemonClient) -> Result<String, String> {
        match self {
            Self::Create { path } => d.create_note(path, "").map_err(|e| e.to_string()),
            Self::Rename { from, to } => d
                .rename(from, to)
                .map(|()| to.clone())
                .map_err(|e| e.to_string()),
            Self::Delete { path } => d
                .delete(path)
                .map(|()| path.clone())
                .map_err(|e| e.to_string()),
        }
    }
}

/// One visible tree row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    pub path: String,
    /// `Some(expanded)` for folders.
    pub folder: Option<bool>,
}

/// What the inline field edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// New note in the selected folder (input = name).
    New,
    /// Rename in place (input = file name).
    Rename,
    /// Move to another folder (input = folder).
    Move,
}

/// The inline field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub kind: EditKind,
    /// The selected path the edit applies to (a folder for `New`).
    pub target: String,
    pub input: String,
}

/// The explorer model.
#[derive(Debug, Default)]
pub struct Explorer {
    files: BTreeSet<String>,
    folders: BTreeSet<String>,
    /// All file paths (switcher candidates), sorted.
    pub paths: Vec<String>,
    /// Flattened visible rows.
    pub rows: Vec<Row>,
    pub window: ListWindow,
    pub selected: Option<String>,
    pub edit: Option<Edit>,
    /// Renames this editor dispatched (`from`, `to`), matched against `file_changed`.
    renames: Vec<(String, String)>,
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn shown(path: &str) -> bool {
    !cv_index::is_skipped_path(path)
}

/// The shown files and every folder above them (skipping is by first component, so a shown
/// file never sits under a skipped folder).
fn tree_of(paths: impl Iterator<Item = String>) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut files = BTreeSet::new();
    let mut folders = BTreeSet::new();
    for path in paths.filter(|p| shown(p)) {
        let mut p = parent(&path);
        while !p.is_empty() && !folders.contains(p) {
            folders.insert(p.to_owned());
            p = parent(p);
        }
        files.insert(path);
    }
    (files, folders)
}

impl Explorer {
    /// Rebuilds from the index: one `file_paths` scan, folders derived in memory (plan
    /// editor-cutover-blockers WP-2; the per-folder `subfolders` walk was quadratic, ~2.0 s on
    /// ~2,700 folders).
    pub fn build(&mut self, index: &LocalIndex, expanded: &BTreeSet<String>) -> Result<(), String> {
        let all = index
            .with_query(|q| q.file_paths())
            .map_err(|e| format!("index: {e}"))?;
        let (files, folders) = tree_of(all.into_iter().map(|(_, p)| p));
        self.files = files;
        self.folders = folders;
        self.relayout(expanded);
        Ok(())
    }

    /// The file set contains `path`.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    /// Whether `path` is a known folder.
    pub fn is_folder(&self, path: &str) -> bool {
        path.is_empty() || self.folders.contains(path)
    }

    /// Inserts `path` only when the index holds it (plan editor-ui-round-3 WP-2, audit B1): an
    /// ignored file stays writable, but its op completion or `file_changed` never lists it.
    fn insert(&mut self, path: &str, indexed: &dyn Fn(&str) -> bool) {
        if !shown(path) || !indexed(path) {
            return;
        }
        self.files.insert(path.to_owned());
        let mut p = parent(path);
        while !p.is_empty() {
            self.folders.insert(p.to_owned());
            p = parent(p);
        }
    }

    fn remove(&mut self, path: &str) {
        self.files.remove(path);
    }

    /// Applies a finished file operation (idempotent with the matching `file_changed`).
    /// `indexed` answers index membership.
    pub fn apply_op(
        &mut self,
        op: &FileOp,
        final_path: &str,
        expanded: &BTreeSet<String>,
        indexed: &dyn Fn(&str) -> bool,
    ) {
        match op {
            FileOp::Create { .. } => self.insert(final_path, indexed),
            FileOp::Rename { from, to } => {
                self.remove(from);
                self.insert(to, indexed);
                if self.selected.as_deref() == Some(from.as_str()) {
                    self.selected = Some(to.clone());
                }
            }
            FileOp::Delete { path } => {
                self.remove(path);
                if self.selected.as_deref() == Some(path.as_str()) {
                    self.selected = None;
                }
            }
        }
        self.relayout(expanded);
    }

    /// Records a rename this editor is about to dispatch.
    pub fn expect_rename(&mut self, from: &str, to: &str) {
        self.renames.push((from.to_owned(), to.to_owned()));
        if self.renames.len() > 16 {
            self.renames.remove(0);
        }
    }

    /// Applies a `file_changed` delta (`indexed` answers index membership).
    pub fn file_changed(
        &mut self,
        path: &str,
        kind: ChangeKind,
        from: Option<&str>,
        expanded: &BTreeSet<String>,
        indexed: &dyn Fn(&str) -> bool,
    ) {
        match kind {
            ChangeKind::Created => self.insert(path, indexed),
            ChangeKind::Deleted => self.remove(path),
            ChangeKind::Modified => {
                if !self.files.contains(path) {
                    self.insert(path, indexed);
                }
            }
            ChangeKind::Renamed => {
                if let Some(from) = from {
                    // Round 9 WP-3: the daemon names the source.
                    self.remove(from);
                    self.renames.retain(|(_, to)| to != path);
                } else if let Some(i) = self.renames.iter().position(|(_, to)| to == path) {
                    let (from, _) = self.renames.remove(i);
                    self.remove(&from);
                }
                self.insert(path, indexed);
            }
        }
        self.relayout(expanded);
    }

    /// Recomputes `paths` and the flattened rows.
    pub fn relayout(&mut self, expanded: &BTreeSet<String>) {
        self.paths = self.files.iter().cloned().collect();
        let mut kids: BTreeMap<&str, (Vec<&str>, Vec<&str>)> = BTreeMap::new();
        for f in &self.folders {
            kids.entry(parent(f)).or_default().0.push(f);
        }
        for f in &self.files {
            kids.entry(parent(f)).or_default().1.push(f);
        }
        for (dirs, files) in kids.values_mut() {
            dirs.sort_by_key(|p| (name(p).to_lowercase(), *p));
            files.sort_by_key(|p| (name(p).to_lowercase(), *p));
        }
        fn emit(
            folder: &str,
            depth: usize,
            kids: &BTreeMap<&str, (Vec<&str>, Vec<&str>)>,
            expanded: &BTreeSet<String>,
            rows: &mut Vec<Row>,
        ) {
            let Some((dirs, files)) = kids.get(folder) else {
                return;
            };
            for d in dirs {
                let open = expanded.contains(*d);
                rows.push(Row {
                    depth,
                    name: name(d).to_owned(),
                    path: (*d).to_owned(),
                    folder: Some(open),
                });
                if open {
                    emit(d, depth + 1, kids, expanded, rows);
                }
            }
            for f in files {
                rows.push(Row {
                    depth,
                    name: name(f).to_owned(),
                    path: (*f).to_owned(),
                    folder: None,
                });
            }
        }
        let mut rows = Vec::new();
        emit("", 0, &kids, expanded, &mut rows);
        self.rows = rows;
    }

    /// The folder new notes go into: the selected folder, or the selected file's folder.
    pub fn target_folder(&self) -> String {
        match &self.selected {
            Some(s) if self.is_folder(s) => s.clone(),
            Some(s) => parent(s).to_owned(),
            None => String::new(),
        }
    }

    /// Starts an inline edit; `None` when it needs a selected file and there is none.
    pub fn begin(&mut self, kind: EditKind) -> bool {
        let edit = match kind {
            EditKind::New => Edit {
                kind,
                target: self.target_folder(),
                input: "Untitled".into(),
            },
            EditKind::Rename | EditKind::Move => {
                let Some(sel) = self.selected.clone().filter(|s| self.files.contains(s)) else {
                    return false;
                };
                let input = if kind == EditKind::Rename {
                    name(&sel).to_owned()
                } else {
                    parent(&sel).to_owned()
                };
                Edit {
                    kind,
                    target: sel,
                    input,
                }
            }
        };
        self.edit = Some(edit);
        true
    }

    /// Turns the inline edit into an RPC op (`Err` = a message for the status bar).
    pub fn submit(&mut self) -> Result<Option<FileOp>, String> {
        let Some(edit) = self.edit.take() else {
            return Ok(None);
        };
        let input = edit.input.trim().trim_matches('/');
        let join = |folder: &str, leaf: &str| {
            if folder.is_empty() {
                leaf.to_owned()
            } else {
                format!("{folder}/{leaf}")
            }
        };
        let op = match edit.kind {
            EditKind::New => {
                if input.is_empty() {
                    return Err("empty name".into());
                }
                let leaf = if input.ends_with(".md") {
                    input.to_owned()
                } else {
                    format!("{input}.md")
                };
                FileOp::Create {
                    path: join(&edit.target, &leaf),
                }
            }
            EditKind::Rename => {
                if input.is_empty() || input.contains('/') {
                    return Err("invalid name".into());
                }
                FileOp::Rename {
                    from: edit.target.clone(),
                    to: join(parent(&edit.target), input),
                }
            }
            EditKind::Move => FileOp::Rename {
                from: edit.target.clone(),
                to: join(input, name(&edit.target)),
            },
        };
        if let FileOp::Rename { from, to } = &op
            && from == to
        {
            return Ok(None);
        }
        Ok(Some(op))
    }
}

/// The explorer panel.
pub fn view<'a>(ex: &'a Explorer, tokens: Tokens) -> Element<'a, Message> {
    // plan editor-round-4 WP-4: no button row — New note / Rename / Move… / Delete live in the
    // row context menu (`context_menu::items`).
    let mut col = column![].spacing(4);
    if let Some(e) = &ex.edit {
        let hint = match e.kind {
            EditKind::New => "new note name",
            EditKind::Rename => "new file name",
            EditKind::Move => "destination folder",
        };
        col = col.push(
            row![
                text_input(hint, &e.input)
                    .size(14)
                    .on_input(|s| Message::Explorer(ExplorerMsg::Input(s)))
                    .on_submit(Message::Explorer(ExplorerMsg::Submit)),
                button(text("×").size(14))
                    .style(button::text)
                    .on_press(Message::Explorer(ExplorerMsg::Cancel)),
            ]
            .spacing(2),
        );
    }
    let selected = ex.selected.as_deref();
    let list = VirtualList::new(&ex.rows, ROW_HEIGHT, ex.window, |w| {
        Message::Explorer(ExplorerMsg::Scrolled(w))
    })
    .view(move |_, r| row_view(r, selected, tokens));
    col.push(list).height(Length::Fill).into()
}

/// One tree row (plan editor-ui-round-3 WP-7): a single clipped line of `ROW_HEIGHT` — the label
/// never wraps (iced 0.14 has no ellipsis, so it is clipped to the row width) and the full path
/// shows in a delayed hover tooltip, so a long name never overlaps the next row.
pub fn row_view<'a>(r: &'a Row, selected: Option<&str>, tokens: Tokens) -> Element<'a, Message> {
    let indent = "  ".repeat(r.depth);
    let icon = match r.folder {
        Some(true) => "▾ ",
        Some(false) => "▸ ",
        None => "  ",
    };
    let label = format!("{indent}{icon}{}", r.name);
    let msg = if r.folder.is_some() {
        ExplorerMsg::Toggle(r.path.clone())
    } else {
        ExplorerMsg::Open(r.path.clone())
    };
    let mut t = text(label)
        .size(14)
        .wrapping(iced::widget::text::Wrapping::None)
        .align_y(iced::alignment::Vertical::Center)
        .height(Length::Fill);
    if selected == Some(r.path.as_str()) {
        t = t.color(tokens.accent_text.to_color());
    }
    let b = button(
        container(t)
            .clip(true)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .style(button::text)
    .padding([2, 4])
    .width(Length::Fill)
    .height(ROW_HEIGHT)
    .on_press(Message::Explorer(msg));
    let tip = iced::widget::tooltip(
        b,
        container(text(r.path.clone()).size(13))
            .padding(6)
            .style(crate::theme::seg_group_style(tokens)),
        iced::widget::tooltip::Position::Bottom,
    )
    .delay(std::time::Duration::from_millis(600));
    // plan editor-ui-round-3 WP-4: right-click a row for its context menu.
    crate::context_menu::on_right(
        container(tip).clip(true).height(ROW_HEIGHT),
        crate::context_menu::Target::Row {
            path: r.path.clone(),
            folder: r.folder.is_some(),
        },
    )
}

#[cfg(test)]
mod tests {
    use cv_testkit::{SyntheticVault, VaultSpec};

    use super::*;
    use crate::daemon::{DaemonClient, FakeDaemon};

    /// The pre-WP-2 build: a `folder_files` + `subfolders` call per folder.
    fn per_folder(index: &LocalIndex) -> (BTreeSet<String>, BTreeSet<String>) {
        index
            .with_query(|q| {
                let mut files = BTreeSet::new();
                let mut folders = BTreeSet::new();
                let mut stack = vec![String::new()];
                while let Some(folder) = stack.pop() {
                    for f in q.folder_files(&folder)? {
                        if shown(&f.path) {
                            files.insert(f.path);
                        }
                    }
                    for sub in q.subfolders(&folder)? {
                        if shown(&sub) {
                            folders.insert(sub.clone());
                            stack.push(sub);
                        }
                    }
                }
                Ok((files, folders))
            })
            .expect("walk")
    }

    #[test]
    fn single_query_tree_equals_per_folder_build() {
        let tmp = tempfile::tempdir().expect("tmp");
        let vault = tmp.path().join("vault");
        SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
        for p in [
            "deep/a/b/c/n.md",
            "deep/a/x.md",
            "Deep2/y.md",
            ".trash/gone.md",
            "att/pic.png",
        ] {
            let abs = vault.join(p);
            std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
            std::fs::write(abs, "x\n").expect("write");
        }
        let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
        let (files, folders) = per_folder(d.index());
        assert!(files.len() > 5 && folders.contains("deep/a/b/c"));
        let mut e = Explorer::default();
        e.build(d.index(), &BTreeSet::new()).expect("build");
        assert_eq!(e.files, files);
        assert_eq!(e.folders, folders);
        assert!(!e.files.iter().any(|p| p.starts_with(".trash")));
    }

    /// Plan editor-ui-round-3 WP-7: a 200-char name builds as one clipped row.
    #[test]
    fn long_name_row_builds() {
        let name = "x".repeat(200);
        let r = Row {
            depth: 3,
            path: format!("a/b/c/{name}.md"),
            name,
            folder: None,
        };
        let t = crate::theme::Surround::Dark.tokens().expect("tokens");
        let _el: Element<'_, Message> = row_view(&r, Some(r.path.as_str()), t);
        assert!((ROW_HEIGHT - 24.0).abs() < f32::EPSILON);
    }
}
