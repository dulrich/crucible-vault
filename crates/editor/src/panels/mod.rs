//! Side panels (WP-5): outline, outgoing links, backlinks, tags, properties, search.
//!
//! The column follows the focused pane's active tab. Buffer-derived panels (outline, outgoing
//! links, properties) come from one `ofm::parse` of the live buffer text, recomputed on the
//! panels tick only when `(path, edit_seq, base_hash)` changed (unsaved edits included, never per
//! frame). Index-derived panels (backlinks, tags, search) query the read-only local index;
//! backlink snippets and search hits read note files off the UI thread (never logged).

pub mod props;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use cv_index::{Resolver, TagCount};
use editor_model::outline::{self, OutlineItem};
use editor_model::render::{PropValue, is_remote};
use iced::widget::span;
use iced::widget::text::Span;
use iced::widget::{button, column, container, rich_text, row, scrollable, text, text_input};
use iced::{Element, Length};

use crate::context_menu::{Target, on_right};
use crate::daemon::{DaemonClient, DaemonError};
use crate::message::{Message, PanelsMsg};
use crate::source::SourceBuffer;
use crate::theme::Tokens;
use crate::widget::virtual_list::{ListWindow, VirtualList};

pub use props::{Prop, PropType, Rendered};

/// Snippet length cap (chars).
pub const SNIPPET_CHARS: usize = 120;
/// FTS result cap default (`editor.toml` `search.limit` overrides it; plan editor-settings-tab).
pub const SEARCH_LIMIT: usize = crate::settings::editor_prefs::DEFAULT_SEARCH_LIMIT;
/// Search input debounce default (`editor.toml` `search.debounce_ms` overrides it).
pub const SEARCH_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(crate::settings::editor_prefs::DEFAULT_SEARCH_DEBOUNCE_MS);
/// Single-line row height.
pub const ROW: f32 = 24.0;
/// Two-line row height (path + snippet).
pub const ROW2: f32 = 44.0;
/// The search field's widget id.
pub const SEARCH_ID: &str = "panels-search";

/// Which panel the column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelKind {
    #[default]
    Outline,
    Links,
    Backlinks,
    Tags,
    Properties,
    Search,
}

impl PanelKind {
    /// Every panel, in tab order.
    pub const ALL: [PanelKind; 6] = [
        PanelKind::Outline,
        PanelKind::Links,
        PanelKind::Backlinks,
        PanelKind::Tags,
        PanelKind::Properties,
        PanelKind::Search,
    ];

    /// The tab label.
    pub fn label(self) -> &'static str {
        match self {
            PanelKind::Outline => "Outline",
            PanelKind::Links => "Links",
            PanelKind::Backlinks => "Backlinks",
            PanelKind::Tags => "Tags",
            PanelKind::Properties => "Properties",
            PanelKind::Search => "Search",
        }
    }
}

/// The scrollable lists with a virtual window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListId {
    Backlinks,
    Tags,
    TagFiles,
    Search,
}

/// One outgoing link from the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutLink {
    /// The target as written.
    pub target: String,
    /// The vault path it resolves to.
    pub resolved: Option<String>,
}

/// Outgoing links split per S2.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    pub resolved: Vec<OutLink>,
    /// Unresolved, but a note could exist: "missing note".
    pub missing: Vec<OutLink>,
    /// Unresolved by nature: a URL scheme, or a non-`.md` file extension.
    pub external: Vec<OutLink>,
}

/// One backlink with its source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    pub src: String,
    pub line: Option<i64>,
    pub snippet: String,
}

/// One search hit (a `semsearch` chunk: note path, its heading, a snippet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub heading: String,
    pub snippet: String,
}

/// The search pane's error line when the daemon cannot answer.
pub const SEARCH_UNAVAILABLE: &str = "daemon unavailable";

/// The search pane.
#[derive(Debug, Clone, Default)]
pub struct Search {
    pub query: String,
    /// Bumped per keystroke; only the latest debounce/result is applied.
    pub seq: u64,
    pub results: Vec<SearchHit>,
    pub error: Option<String>,
}

struct ResolverCache {
    generation: u64,
    resolver: Resolver,
    ids: HashMap<String, i64>,
    paths: HashMap<i64, String>,
}

/// Panel state.
#[derive(Default)]
pub struct Panels {
    pub visible: bool,
    pub kind: PanelKind,
    /// `(path, edit_seq, base_hash)` the buffer-derived panels were computed for.
    buffer_key: Option<(String, u64, String)>,
    /// `(path, generation)` the backlinks were requested for.
    backlinks_key: Option<(String, u64)>,
    tags_gen: Option<u64>,
    resolver: Option<ResolverCache>,
    /// The path the panels currently describe.
    pub path: Option<String>,
    pub outline: Vec<OutlineItem>,
    pub links: Links,
    pub props: Vec<Prop>,
    /// Plan round-6 WP-5: sidebar property wikilink targets that do not resolve (dimmed; their
    /// menu offers Create note). Empty without a resolver (no daemon), like Reading's.
    pub prop_unresolved: HashSet<String>,
    pub backlinks: Vec<Backlink>,
    pub tags: Vec<TagCount>,
    /// The picked tag and its files.
    pub tag_files: Option<(String, Vec<String>)>,
    pub search: Search,
    pub windows: HashMap<ListIdKey, ListWindow>,
}

/// Hashable form of [`ListId`].
pub type ListIdKey = u8;

fn list_key(id: ListId) -> ListIdKey {
    match id {
        ListId::Backlinks => 0,
        ListId::Tags => 1,
        ListId::TagFiles => 2,
        ListId::Search => 3,
    }
}

/// Whether an unresolved target is "external-ish" (S2): a URI scheme, or a file extension other
/// than `.md` (attachments, PDFs, …). Everything else is a missing note.
pub fn is_externalish(target: &str) -> bool {
    if is_remote(target) || target.starts_with("//") {
        return true;
    }
    let base = target.rsplit('/').next().unwrap_or(target);
    match base.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && ext.starts_with(|c: char| c.is_ascii_alphabetic())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && !ext.eq_ignore_ascii_case("md")
        }
        None => false,
    }
}

/// Trims one source line to a snippet.
pub fn snippet(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() > SNIPPET_CHARS {
        let mut s: String = t.chars().take(SNIPPET_CHARS).collect();
        s.push('…');
        s
    } else {
        t.to_owned()
    }
}

/// Backlinks to `path` with one-line snippets (blocking: runs off the UI thread).
pub fn load_backlinks(d: &dyn DaemonClient, path: &str) -> Result<Vec<Backlink>, String> {
    let rows = d
        .index()
        .with_query(|q| q.backlinks(path))
        .map_err(|e| e.to_string())?;
    let mut files: HashMap<String, Option<String>> = HashMap::new();
    Ok(rows
        .into_iter()
        .map(|r| {
            let body = files.entry(r.src_path.clone()).or_insert_with(|| {
                d.read_file(&r.src_path)
                    .ok()
                    .map(|(b, _)| String::from_utf8_lossy(&b).into_owned())
            });
            let snip = match (body.as_deref(), r.line) {
                (Some(b), Some(l)) if l >= 1 => usize::try_from(l - 1)
                    .ok()
                    .and_then(|i| b.lines().nth(i))
                    .map(snippet)
                    .unwrap_or_default(),
                _ => String::new(),
            };
            Backlink {
                src: r.src_path,
                line: r.line,
                snippet: snip,
            }
        })
        .collect())
}

/// Hybrid search through the daemon's `search` request, at most `limit` hits (blocking: runs
/// off the UI thread; plan semsearch-crate WP-4). The editor never opens the search index, and
/// there is no fallback to `index.db`: a daemon that cannot answer is [`SEARCH_UNAVAILABLE`].
pub fn run_search(
    d: &dyn DaemonClient,
    input: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    let limit = u32::try_from(limit).unwrap_or(u32::MAX);
    match d.search(input, limit) {
        Ok(r) => Ok(r
            .hits
            .into_iter()
            .map(|h| SearchHit {
                path: h.path,
                heading: h.heading,
                snippet: snippet(&h.snippet.split_whitespace().collect::<Vec<_>>().join(" ")),
            })
            .collect()),
        Err(DaemonError::Disconnected | DaemonError::Io(_)) => Err(SEARCH_UNAVAILABLE.to_owned()),
        Err(e) => Err(format!("search failed: {e}")),
    }
}

impl Panels {
    /// Forces the buffer-derived panels and backlinks to recompute on the next sync.
    pub fn invalidate(&mut self) {
        self.buffer_key = None;
        self.backlinks_key = None;
    }

    /// Drops the cached resolver (a file was created, deleted or renamed; incremental indexing
    /// keeps the generation, so the generation key alone never sees new files).
    pub fn invalidate_resolver(&mut self) {
        self.resolver = None;
    }

    /// The scroll window of `id`.
    pub fn window(&self, id: ListId) -> ListWindow {
        self.windows.get(&list_key(id)).copied().unwrap_or_default()
    }

    /// Stores a scroll window.
    pub fn set_window(&mut self, id: ListId, w: ListWindow) {
        self.windows.insert(list_key(id), w);
    }

    fn refresh_resolver(&mut self, d: &dyn DaemonClient, generation: u64) {
        if self
            .resolver
            .as_ref()
            .is_some_and(|r| r.generation == generation)
        {
            return;
        }
        let t0 = std::time::Instant::now();
        // One scan (editor-usability-round WP-5): the old per-folder `folder_files` +
        // `subfolders` walk re-scanned every distinct folder per folder — ~1.2 s on a
        // 2.7k-folder vault (docs/spikes/panels-lag/FINDINGS.md).
        let rows = d.index().with_query(|q| q.file_paths());
        match rows {
            Ok(rows) => {
                tracing::debug!(rows = rows.len(), "panels resolver rows");
                let mut resolver = Resolver::default();
                let mut ids = HashMap::new();
                let mut paths = HashMap::new();
                for (id, path) in rows {
                    resolver.add(id, &path);
                    ids.insert(path.clone(), id);
                    paths.insert(id, path);
                }
                self.resolver = Some(ResolverCache {
                    generation,
                    resolver,
                    ids,
                    paths,
                });
            }
            Err(e) => tracing::warn!(error = %e, "panels: resolver rebuild failed"),
        }
        tracing::debug!(
            elapsed_ms = t0.elapsed().as_secs_f64() * 1e3,
            "panels refresh_resolver"
        );
    }

    fn resolve(&self, src: &str, target: &str) -> Option<String> {
        let rc = self.resolver.as_ref()?;
        let src_id = rc.ids.get(src).copied().unwrap_or(-1);
        let id = rc.resolver.resolve(src, src_id, target)?;
        rc.paths.get(&id).cloned()
    }

    fn classify(&self, src: &str, target: &str, links: &mut Links) {
        let resolved = if is_remote(target) {
            None
        } else {
            self.resolve(src, target)
        };
        let l = OutLink {
            target: target.to_owned(),
            resolved: resolved.clone(),
        };
        if resolved.is_some() {
            links.resolved.push(l);
        } else if is_externalish(target) {
            links.external.push(l);
        } else {
            links.missing.push(l);
        }
    }

    /// Recomputes outline, links and properties from `tab`'s live text when its key changed.
    pub fn sync_buffer(
        &mut self,
        tab: Option<&SourceBuffer>,
        d: Option<&dyn DaemonClient>,
        generation: u64,
    ) {
        let Some(tab) = tab else {
            if self.path.take().is_some() {
                self.buffer_key = None;
                self.outline.clear();
                self.links = Links::default();
                self.props.clear();
                self.backlinks.clear();
                self.backlinks_key = None;
            }
            return;
        };
        let t0 = std::time::Instant::now();
        if let Some(d) = d {
            let stale = self
                .resolver
                .as_ref()
                .is_none_or(|r| r.generation != generation);
            if stale {
                self.refresh_resolver(d, generation);
                self.buffer_key = None;
            }
        }
        let key = (
            tab.path().to_owned(),
            tab.edit_seq(),
            tab.base_hash().as_str().to_owned(),
        );
        if self.buffer_key.as_ref() == Some(&key) {
            return;
        }
        let src = tab.text();
        let note = ofm::parse(&src);
        self.outline = outline::from_note(&src, &note);
        self.props = props::from_note(&note);
        self.prop_unresolved = if self.resolver.is_some() {
            self.props
                .iter()
                .filter_map(|p| match &p.value {
                    Rendered::Links(v) => Some(v),
                    _ => None,
                })
                .flatten()
                .filter_map(|v| match v {
                    PropValue::Link { target, .. }
                        if !target.is_empty() && self.resolve(tab.path(), target).is_none() =>
                    {
                        Some(target.clone())
                    }
                    _ => None,
                })
                .collect()
        } else {
            HashSet::new()
        };
        let mut links = Links::default();
        for w in &note.wikilinks {
            if !w.target.is_empty() {
                self.classify(tab.path(), &w.target, &mut links);
            }
        }
        for m in &note.md_links {
            match &m.dest {
                ofm::LinkDest::External(u) => links.external.push(OutLink {
                    target: u.clone(),
                    resolved: None,
                }),
                ofm::LinkDest::Local { path, .. } if !path.is_empty() => {
                    self.classify(tab.path(), path, &mut links);
                }
                ofm::LinkDest::Local { .. } => {}
            }
        }
        for f in &note.fm_links {
            if !f.target.is_empty() {
                self.classify(tab.path(), &f.target, &mut links);
            }
        }
        self.links = links;
        if self.path.as_deref() != Some(tab.path()) {
            self.path = Some(tab.path().to_owned());
        }
        self.buffer_key = Some(key);
        tracing::debug!(
            elapsed_ms = t0.elapsed().as_secs_f64() * 1e3,
            "panels sync_buffer"
        );
    }

    /// The path whose backlinks should be (re)loaded now, if the key changed.
    pub fn backlinks_due(&mut self, generation: u64) -> Option<String> {
        let path = self.path.clone()?;
        let key = (path.clone(), generation);
        if self.backlinks_key.as_ref() == Some(&key) {
            return None;
        }
        self.backlinks_key = Some(key);
        Some(path)
    }

    /// Re-queries the tag counts when the generation moved (count desc, then name).
    pub fn sync_tags(&mut self, d: &dyn DaemonClient, generation: u64) -> Result<(), String> {
        if self.tags_gen == Some(generation) {
            return Ok(());
        }
        let t0 = std::time::Instant::now();
        let mut tags = d
            .index()
            .with_query(|q| q.tags())
            .map_err(|e| e.to_string())?;
        tags.sort_by(|a, b| {
            b.occurrences
                .cmp(&a.occurrences)
                .then_with(|| a.tag.cmp(&b.tag))
        });
        self.tags = tags;
        self.tags_gen = Some(generation);
        tracing::debug!(
            elapsed_ms = t0.elapsed().as_secs_f64() * 1e3,
            "panels sync_tags"
        );
        Ok(())
    }

    /// Files carrying `tag` (distinct, by path).
    pub fn pick_tag(&mut self, d: &dyn DaemonClient, tag: &str) -> Result<(), String> {
        let mut files: Vec<String> = d
            .index()
            .with_query(|q| q.tagged(tag))
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.path)
            .collect();
        files.sort();
        files.dedup();
        self.tag_files = Some((tag.to_owned(), files));
        Ok(())
    }
}

fn pm(m: PanelsMsg) -> Message {
    Message::Panels(m)
}

/// Plan round-6 WP-5: the link menu target of a panel entry in note `src` (`view` 0 is inert:
/// no Reading selection here).
pub(crate) fn link_target(src: &str, target: &str, external: bool, missing: bool) -> Target {
    Target::Link {
        src: src.to_owned(),
        target: target.to_owned(),
        external,
        view: 0,
        selection: false,
        missing,
    }
}

/// A resolved entry (`open` = its vault path) opens it; a missing one (`None`) is dimmed and
/// clicks through the Reading link path (WP-4 warn / create). Both carry the link menu.
fn link_button<'a>(
    src: &str,
    label: String,
    open: Option<String>,
    t: Tokens,
) -> Element<'a, Message> {
    let txt = text(label.clone()).size(13);
    let (el, target) = match open {
        Some(p) => (
            button(txt.color(t.accent_text.to_color()))
                .style(button::text)
                .padding(2)
                .on_press(pm(PanelsMsg::Open(p.clone()))),
            link_target(src, &p, false, false),
        ),
        None => (
            button(txt.color(t.text_dim.to_color()))
                .style(button::text)
                .padding(2)
                .on_press(Message::Reading(crate::message::ReadingMsg::OpenLink {
                    src: src.to_owned(),
                    target: label.clone(),
                })),
            link_target(src, &label, false, true),
        ),
    };
    on_right(el, target)
}

/// An attachment / disallowed-scheme entry: dimmed and inert (no opener, no menu).
fn inert<'a>(label: String, t: Tokens) -> Element<'a, Message> {
    container(text(label).size(13).color(t.text_dim.to_color()))
        .padding(2)
        .into()
}

/// Plan usability-sprint-captures-links WP-3: an external entry that passes the opener's
/// scheme allowlist opens in the browser; attachments and other schemes stay inert.
fn external_button<'a>(src: &str, target: String, t: Tokens) -> Element<'a, Message> {
    if crate::open_url::check_scheme(&target).is_err() {
        return inert(target, t);
    }
    let menu = link_target(src, &target, true, false);
    let b = button(
        text(target.clone())
            .size(13)
            .color(t.accent_text.to_color()),
    )
    .style(button::text)
    .padding(2)
    .on_press(Message::OpenUrl(target));
    on_right(b, menu)
}

/// Plan round-6 WP-5: one sidebar property element — a link / URL draws like Reading's
/// frontmatter link (accent-ui, dimmed when unresolved), clicks like it and carries the link
/// menu; other elements are plain text.
fn prop_el<'a>(
    src: &str,
    v: &PropValue,
    unresolved: &HashSet<String>,
    t: Tokens,
) -> Element<'a, Message> {
    let (label, msg, target) = match v {
        PropValue::Text(s) => return text(s.clone()).size(13).into(),
        PropValue::Bool(b) => return text(b.to_string()).size(13).into(),
        PropValue::Url { url, label } => (
            label.clone(),
            Message::OpenUrl(url.clone()),
            link_target(src, url, true, false),
        ),
        PropValue::Link { target, alias, .. } => (
            alias.clone().unwrap_or_else(|| target.clone()),
            Message::Reading(crate::message::ReadingMsg::OpenLink {
                src: src.to_owned(),
                target: target.clone(),
            }),
            link_target(src, target, false, unresolved.contains(target)),
        ),
    };
    let mut c = t.accent_ui.to_color();
    if matches!(&target, Target::Link { missing: true, .. }) {
        c.a *= 0.5;
    }
    let sp: Span<'a, ()> = span(label).color(c).link(());
    on_right(
        rich_text(vec![sp])
            .size(13)
            .on_link_click(move |()| msg.clone()),
        target,
    )
}

fn heading<'a>(s: String, t: Tokens) -> Element<'a, Message> {
    text(s).size(12).color(t.text_faint.to_color()).into()
}

/// Plan round-6 WP-5: an Outline heading's menu target (an in-note anchor).
pub(crate) fn anchor_target(path: &str, line: usize) -> Target {
    Target::Anchor {
        path: path.to_owned(),
        line,
    }
}

/// The panels column.
/// Plan round-6-feedback-todos WP-6: the click message of a sidebar boolean property — the same
/// `ToggleProp` Reading's frontmatter checkbox sends (`frontmatter_bool_set(.., None)` on the buffer).
pub(crate) fn check_press(src: &str, key: &str) -> Message {
    Message::Reading(crate::message::ReadingMsg::ToggleProp {
        path: src.to_owned(),
        key: key.to_owned(),
    })
}

pub fn view<'a>(p: &'a Panels, t: Tokens) -> Element<'a, Message> {
    let src: &'a str = p.path.as_deref().unwrap_or_default();
    let tabs = row(PanelKind::ALL.iter().map(|&k| {
        let mut l = text(k.label()).size(12);
        if k == p.kind {
            l = l.color(t.accent_text.to_color());
        }
        button(l)
            .style(button::text)
            .padding(2)
            .on_press(pm(PanelsMsg::Show(k)))
            .into()
    }))
    .spacing(2)
    .wrap();
    let body: Element<'a, Message> = match p.kind {
        PanelKind::Outline => scrollable(column(p.outline.iter().map(|h| {
            let indent = "  ".repeat(usize::from(h.level.saturating_sub(1)));
            on_right(
                button(text(format!("{indent}{}", h.text)).size(13))
                    .style(button::text)
                    .padding(2)
                    .on_press(pm(PanelsMsg::OutlineJump(h.line))),
                anchor_target(src, h.line),
            )
        })))
        .height(Length::Fill)
        .into(),
        PanelKind::Links => {
            let mut c = column![heading(format!("Linked ({})", p.links.resolved.len()), t)];
            for l in &p.links.resolved {
                c = c.push(link_button(src, l.target.clone(), l.resolved.clone(), t));
            }
            c = c.push(heading(
                format!("Missing notes ({})", p.links.missing.len()),
                t,
            ));
            for l in &p.links.missing {
                c = c.push(link_button(src, l.target.clone(), None, t));
            }
            c = c.push(heading(
                format!("External / attachments ({})", p.links.external.len()),
                t,
            ));
            for l in &p.links.external {
                c = c.push(external_button(src, l.target.clone(), t));
            }
            scrollable(c.spacing(2)).height(Length::Fill).into()
        }
        PanelKind::Backlinks => {
            VirtualList::new(&p.backlinks, ROW2, p.window(ListId::Backlinks), |w| {
                pm(PanelsMsg::Scrolled(ListId::Backlinks, w))
            })
            .view(move |_, b| {
                let title = match b.line {
                    Some(l) => format!("{}:{l}", b.src),
                    None => b.src.clone(),
                };
                on_right(
                    button(column![
                        text(title).size(13).color(t.accent_text.to_color()),
                        text(b.snippet.as_str())
                            .size(12)
                            .color(t.text_dim.to_color()),
                    ])
                    .style(button::text)
                    .padding(1)
                    .height(ROW2)
                    .on_press(pm(PanelsMsg::Open(b.src.clone()))),
                    link_target(src, &b.src, false, false),
                )
            })
        }
        PanelKind::Tags => match &p.tag_files {
            Some((tag, files)) => column![
                button(text(format!("← #{tag} ({})", files.len())).size(13))
                    .style(button::text)
                    .on_press(pm(PanelsMsg::TagBack)),
                VirtualList::new(files, ROW, p.window(ListId::TagFiles), |w| {
                    pm(PanelsMsg::Scrolled(ListId::TagFiles, w))
                })
                .view(move |_, f| {
                    on_right(
                        button(text(f.as_str()).size(13))
                            .style(button::text)
                            .padding(1)
                            .height(ROW)
                            .on_press(pm(PanelsMsg::Open(f.clone()))),
                        link_target(src, f, false, false),
                    )
                }),
            ]
            .into(),
            None => VirtualList::new(&p.tags, ROW, p.window(ListId::Tags), |w| {
                pm(PanelsMsg::Scrolled(ListId::Tags, w))
            })
            .view(move |_, tc| {
                button(text(format!("#{} ({})", tc.tag, tc.occurrences)).size(13))
                    .style(button::text)
                    .padding(1)
                    .height(ROW)
                    .on_press(pm(PanelsMsg::TagPick(tc.tag.clone())))
                    .into()
            }),
        },
        PanelKind::Properties => scrollable(
            column(p.props.iter().map(|pr| {
                let v: Element<'a, Message> = match &pr.value {
                    Rendered::Links(vs) => row(vs.iter().enumerate().map(|(i, v)| {
                        let el = prop_el(src, v, &p.prop_unresolved, t);
                        if i + 1 < vs.len() {
                            row![el, text(",").size(13).color(t.text_faint.to_color())].into()
                        } else {
                            el
                        }
                    }))
                    .spacing(4)
                    .wrap()
                    .into(),
                    Rendered::Flagged(_) => text(pr.value.display())
                        .size(13)
                        .color(t.warn_text.to_color())
                        .into(),
                    // Plan round-6-feedback-todos WP-6: a boolean toggles through Reading's
                    // frontmatter path (no status menu), drawn with the shared checkbox.
                    Rendered::Check(b) => button(crate::task_status::task_box(
                        if *b { 'x' } else { ' ' },
                        14.0,
                        t,
                    ))
                    .padding(0)
                    .style(button::text)
                    .on_press(check_press(src, &pr.key))
                    .into(),
                    other => text(other.display()).size(13).into(),
                };
                column![
                    text(pr.key.as_str())
                        .size(12)
                        .color(t.text_faint.to_color()),
                    v
                ]
                .spacing(1)
                .into()
            }))
            .spacing(6),
        )
        .height(Length::Fill)
        .into(),
        PanelKind::Search => {
            let mut c = column![
                text_input("Search the vault…", &p.search.query)
                    .id(SEARCH_ID)
                    .on_input(|q| pm(PanelsMsg::SearchInput(q)))
            ]
            .spacing(4);
            if let Some(e) = &p.search.error {
                c = c.push(text(e.as_str()).size(12).color(t.warn_text.to_color()));
            }
            c.push(
                VirtualList::new(&p.search.results, ROW2, p.window(ListId::Search), |w| {
                    pm(PanelsMsg::Scrolled(ListId::Search, w))
                })
                .view(move |_, h| {
                    button(column![
                        row![
                            text(h.path.as_str())
                                .size(13)
                                .color(t.accent_text.to_color()),
                            text(h.heading.as_str())
                                .size(12)
                                .color(t.text_dim.to_color()),
                        ]
                        .spacing(6),
                        text(h.snippet.as_str())
                            .size(12)
                            .color(t.text_dim.to_color()),
                    ])
                    .style(button::text)
                    .padding(1)
                    .height(ROW2)
                    .on_press(pm(PanelsMsg::Open(h.path.clone())))
                    .into()
                }),
            )
            .into()
        }
    };
    column![tabs, body].spacing(6).padding(4).into()
}
