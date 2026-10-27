//! WP-R2 (review E1, E4, E12a): the one PDF store — parsed documents, the render consumers (a
//! PDF tab, a note's inline embeds), each consumer's wanted pages, the in-flight cap and the one
//! retry policy. It has no iced dependency: the placeholder type is a parameter (`Handle` in
//! the app, `()` in tests) and render jobs are plain data the message glue
//! (`app::pdf_tab`) turns into tasks.
//!
//! **Admission** (WP-R1, review P1) serves every consumer alike: a consumer's shown pages of
//! one PDF are planned nearest the viewport first ([`super::nearest_first`] + [`super::plan`])
//! against the PDF bytes the cache holds for everyone else ([`ImageCache::pdf_bytes`]); a page
//! held above its planned width is released, one held below it is upgraded (the old bitmap
//! shows until the new one lands). Each (consumer, PDF) pair plans its own pages: two
//! consumers showing the same page at the same width share one cache entry, but each counts it
//! in its own plan (the cache counts it once in `pdf_bytes`).
//!
//! **Retry policy:** a transient admission refusal (the plan gives a page no width) holds
//! nothing, so the next pump — any show, hide, open or completion — retries it. A render
//! failure is recorded per page ([`PageKey`]) on the document; every consumer reads it (the tab
//! keeps the placeholder and names the failure, an embed draws the file card) and no consumer
//! retries it until no consumer shows the page any more.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::image_cache::{ImageCache, view_owner};

use super::{PageSizes, nearest_first, plan_each, projected_bytes};

/// Concurrent page renders per (consumer, PDF) (audit E1).
pub const MAX_INFLIGHT: usize = 2;

/// One page of a vault PDF. **Numbering convention:** `page` is 0-based everywhere in code;
/// the 1-based page number appears only in user-facing text and in the string forms below
/// ([`Self::number`]), which are built here and never parsed back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageKey {
    pub path: String,
    pub page: usize,
}

impl PageKey {
    pub fn new(path: &str, page: usize) -> Self {
        Self {
            path: path.to_owned(),
            page,
        }
    }

    /// The 1-based page number (user text, string forms).
    pub fn number(&self) -> usize {
        self.page + 1
    }

    /// The page's identity string (`path#page=N`, sensor keys).
    pub fn id(&self) -> String {
        format!("{}#page={}", self.path, self.number())
    }

    /// The image-cache key of this page rendered `width` px wide. `#page=` never occurs in an
    /// image key (a bare vault path), so the two never collide.
    pub fn cache_key(&self, width: u32) -> String {
        format!("{}@{width}", self.id())
    }
}

/// Who wants pages rendered.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Consumer {
    /// A PDF tab (its tab id).
    Tab(u64),
    /// Note `note`'s inline embeds in tab `view`.
    Embed { note: String, view: u64 },
}

/// Separates an embed consumer's note path from the rest of its pin owner, so the note's own
/// image owner (`ImageCache::retain_owner(note)`) never covers it — embed pins are this
/// store's to release.
const EMBED_SEP: char = '\u{1}';

impl Consumer {
    /// The cache pin owner of this consumer's pages of `pdf`.
    pub fn owner(&self, pdf: &str) -> String {
        match self {
            Self::Tab(view) => view_owner(pdf, *view),
            Self::Embed { note, view } => view_owner(&format!("{note}{EMBED_SEP}pdf"), *view),
        }
    }

    fn view(&self) -> u64 {
        match self {
            Self::Tab(v) | Self::Embed { view: v, .. } => *v,
        }
    }
}

/// One PDF file's state (shared by every consumer of it). Only `Send` data — never a renderer
/// value (round-15 audit E3).
#[derive(Debug)]
pub enum Doc<P> {
    /// Read + parse in flight.
    Loading,
    /// Parsed: the file bytes (shared with render jobs), page sizes in points, one placeholder
    /// per page, and the pages whose render failed (0-based page → reason).
    Ready {
        bytes: Arc<Vec<u8>>,
        pages: PageSizes,
        placeholders: Vec<P>,
        failed: BTreeMap<usize, String>,
    },
    /// The read or parse failed.
    Failed(String),
}

/// A held page: its rendered width and cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub width: u32,
    pub key: String,
}

/// One consumer's pages of one PDF.
#[derive(Debug, Default)]
pub struct Pages {
    /// Pages within the near window and their laid-out width.
    pub shown: BTreeMap<usize, f32>,
    /// Pages pinned (or loading) for this consumer.
    pub held: BTreeMap<usize, Held>,
    /// WP-R1: pages re-rendering at a larger planned width (pinned too); replaces `held` when
    /// it lands.
    pub upgrading: BTreeMap<usize, Held>,
    /// Renders this consumer started and has not seen finish, and their key.
    pub inflight: BTreeMap<usize, String>,
}

/// A render the glue must run: page `page` at `width`, into cache key `key`.
#[derive(Debug, Clone)]
pub struct Job {
    pub page: PageKey,
    pub width: u32,
    pub key: String,
    pub bytes: Arc<Vec<u8>>,
}

/// What a pump asks of the glue: renders to start and documents to open.
#[derive(Debug, Default)]
pub struct Pump {
    pub jobs: Vec<Job>,
    pub open: Vec<String>,
}

/// The PDF store.
#[derive(Debug)]
pub struct PdfStore<P> {
    docs: HashMap<String, Doc<P>>,
    consumers: BTreeMap<(Consumer, String), Pages>,
    /// Plan round-15 WP-4: PDF path → 0-based page a click on an inline embed asked the tab to
    /// scroll to (consumed once the tab has a laid-out width).
    pub goto: HashMap<String, usize>,
}

impl<P> Default for PdfStore<P> {
    fn default() -> Self {
        Self {
            docs: HashMap::new(),
            consumers: BTreeMap::new(),
            goto: HashMap::new(),
        }
    }
}

impl<P> PdfStore<P> {
    pub fn doc(&self, path: &str) -> Option<&Doc<P>> {
        self.docs.get(path)
    }

    /// Every document path the store holds (loading included).
    pub fn doc_paths(&self) -> impl Iterator<Item = (&String, &Doc<P>)> {
        self.docs.iter()
    }

    pub fn has_docs(&self) -> bool {
        !self.docs.is_empty()
    }

    /// Marks `path` loading; `false` when it is already known (no open to start).
    pub fn begin_open(&mut self, path: &str) -> bool {
        if self.docs.contains_key(path) {
            return false;
        }
        self.docs.insert(path.to_owned(), Doc::Loading);
        true
    }

    /// The read + parse of `path` finished; `placeholders` builds one per page.
    pub fn opened(
        &mut self,
        path: &str,
        result: Result<(Arc<Vec<u8>>, PageSizes), String>,
        placeholders: impl FnOnce(&PageSizes) -> Vec<P>,
    ) {
        let doc = match result {
            Ok((bytes, pages)) => Doc::Ready {
                placeholders: placeholders(&pages),
                bytes,
                pages,
                failed: BTreeMap::new(),
            },
            Err(e) => {
                self.goto.remove(path);
                Doc::Failed(e)
            }
        };
        self.docs.insert(path.to_owned(), doc);
    }

    /// Drops the parsed document of `path` (the sweep: nothing needs it).
    pub fn remove_doc(&mut self, path: &str) {
        self.docs.remove(path);
    }

    /// The page sizes of a parsed `path`.
    pub fn pages(&self, path: &str) -> Option<&PageSizes> {
        match self.docs.get(path) {
            Some(Doc::Ready { pages, .. }) => Some(pages),
            _ => None,
        }
    }

    /// Why page `page` failed to render, if it did (read by every surface).
    pub fn page_failed(&self, page: &PageKey) -> Option<&str> {
        match self.docs.get(&page.path) {
            Some(Doc::Ready { failed, .. }) => failed.get(&page.page).map(String::as_str),
            _ => None,
        }
    }

    /// Consumer `c`'s pages of `pdf`.
    pub fn pages_of(&self, c: &Consumer, pdf: &str) -> Option<&Pages> {
        self.consumers.get(&(c.clone(), pdf.to_owned()))
    }

    /// The notes with embed consumers.
    pub fn embed_notes(&self) -> impl Iterator<Item = &str> {
        self.consumers.keys().filter_map(|(c, _)| match c {
            Consumer::Embed { note, .. } => Some(note.as_str()),
            Consumer::Tab(_) => None,
        })
    }

    /// The cache key consumer `c` holds for `page` (what it draws).
    pub fn held(&self, c: &Consumer, page: &PageKey) -> Option<&str> {
        self.pages_of(c, &page.path)?
            .held
            .get(&page.page)
            .map(|h| h.key.as_str())
    }

    /// The laid-out width of tab `view` showing `pdf` (0 when it has none yet).
    pub fn tab_width(&self, view: u64, pdf: &str) -> f32 {
        self.pages_of(&Consumer::Tab(view), pdf)
            .and_then(|p| p.shown.values().next().copied())
            .unwrap_or(0.0)
    }

    /// `c` shows `page` at laid-out `width`.
    pub fn show(&mut self, c: Consumer, page: PageKey, width: f32) {
        self.consumers
            .entry((c, page.path))
            .or_default()
            .shown
            .insert(page.page, width);
    }

    /// `c` no longer shows `page`: its pins go; a recorded failure no consumer shows any more
    /// is forgotten (the page retries when shown again).
    pub fn hide(&mut self, c: &Consumer, page: &PageKey, cache: &mut ImageCache) {
        let id = (c.clone(), page.path.clone());
        if let Some(p) = self.consumers.get_mut(&id) {
            p.shown.remove(&page.page);
            p.inflight.remove(&page.page);
            let owner = c.owner(&page.path);
            for h in [p.held.remove(&page.page), p.upgrading.remove(&page.page)]
                .into_iter()
                .flatten()
            {
                cache.hide(&owner, &h.key);
            }
        }
        let still = self
            .consumers
            .iter()
            .any(|((_, pdf), p)| *pdf == page.path && p.shown.contains_key(&page.page));
        if !still && let Some(Doc::Ready { failed, .. }) = self.docs.get_mut(&page.path) {
            failed.remove(&page.page);
        }
    }

    /// Drops every consumer `keep` rejects, releasing its pins.
    pub fn retain(
        &mut self,
        cache: &mut ImageCache,
        mut keep: impl FnMut(&Consumer, &str) -> bool,
    ) {
        let gone: Vec<(Consumer, String)> = self
            .consumers
            .keys()
            .filter(|(c, pdf)| !keep(c, pdf))
            .cloned()
            .collect();
        for id in gone {
            if let Some(p) = self.consumers.remove(&id) {
                let owner = id.0.owner(&id.1);
                for h in p.held.values().chain(p.upgrading.values()) {
                    cache.hide(&owner, &h.key);
                }
            }
        }
    }

    /// Tab `view` closed or moved away: every consumer of that tab (a PDF tab, or a note's
    /// embeds in it) goes.
    pub fn view_closed(&mut self, view: u64, cache: &mut ImageCache) {
        self.retain(cache, |c, _| c.view() != view);
    }

    /// Note `note`'s embeds now show exactly the `live` pages: the rest go.
    pub fn retain_embeds(&mut self, note: &str, live: &HashSet<PageKey>, cache: &mut ImageCache) {
        let mut drop: Vec<(Consumer, PageKey)> = Vec::new();
        for ((c, pdf), p) in &self.consumers {
            if matches!(c, Consumer::Embed { note: n, .. } if n == note) {
                for &page in p.shown.keys().chain(p.held.keys()) {
                    let k = PageKey::new(pdf, page);
                    if !live.contains(&k) {
                        drop.push((c.clone(), k));
                    }
                }
            }
        }
        for (c, k) in drop {
            self.hide(&c, &k, cache);
        }
        self.consumers.retain(|(c, _), p| {
            !matches!(c, Consumer::Embed { note: n, .. } if n == note) || !p.shown.is_empty()
        });
    }

    /// The PDF at `from` moved to `to`: its parsed document moves with the file; the tab
    /// consumers of `from` go (their sensors re-show under the new path).
    pub fn renamed(&mut self, from: &str, to: &str, cache: &mut ImageCache) {
        self.retain(cache, |c, pdf| {
            !(matches!(c, Consumer::Tab(_)) && pdf == from)
        });
        if let Some(doc) = self.docs.remove(from) {
            self.docs.insert(to.to_owned(), doc);
        }
    }

    /// A render of `page` into `key` finished (`error` = it failed; the glue already told the
    /// cache). Every consumer of the page sees it: an upgrade lands (the smaller bitmap goes),
    /// a failure is recorded on the document and the pins on `key` go.
    pub fn rendered(
        &mut self,
        page: &PageKey,
        key: &str,
        error: Option<String>,
        cache: &mut ImageCache,
    ) {
        for ((c, pdf), p) in &mut self.consumers {
            if *pdf != page.path {
                continue;
            }
            let owner = c.owner(pdf);
            if p.inflight.get(&page.page).is_some_and(|k| k == key) {
                p.inflight.remove(&page.page);
            }
            if p.upgrading.get(&page.page).is_some_and(|h| h.key == key)
                && let Some(h) = p.upgrading.remove(&page.page)
            {
                if error.is_some() {
                    cache.hide(&owner, &h.key);
                } else if let Some(old) = p.held.insert(page.page, h) {
                    cache.hide(&owner, &old.key);
                }
            } else if error.is_some() && p.held.get(&page.page).is_some_and(|h| h.key == key) {
                p.held.remove(&page.page);
                cache.hide(&owner, key);
            }
        }
        if let Some(e) = error
            && let Some(Doc::Ready { failed, .. }) = self.docs.get_mut(&page.path)
        {
            failed.insert(page.page, e);
        }
    }

    /// The one pump: for every consumer, plans its shown pages and starts what the plan asks
    /// (see the module docs); a consumer whose document is unknown asks for it to be opened.
    pub fn pump(&mut self, cache: &mut ImageCache) -> Pump {
        let mut out = Pump::default();
        for ((c, pdf), p) in &mut self.consumers {
            if p.shown.is_empty() {
                continue;
            }
            let (bytes, pages, failed) = match self.docs.get(pdf) {
                None => {
                    if !out.open.contains(pdf) {
                        out.open.push(pdf.clone());
                    }
                    continue;
                }
                Some(Doc::Ready {
                    bytes,
                    pages,
                    failed,
                    ..
                }) => (bytes, pages, failed),
                Some(_) => continue,
            };
            let owner = c.owner(pdf);
            let shown: BTreeSet<usize> = p.shown.keys().copied().collect();
            let order: Vec<(usize, (f32, f32), f32)> = nearest_first(&shown)
                .into_iter()
                .filter(|pg| !failed.contains_key(pg))
                .filter_map(|pg| Some((pg, *pages.get(pg)?, p.shown[&pg])))
                .collect();
            let own: usize = p
                .held
                .values()
                .chain(p.upgrading.values())
                .map(|h| cache.pdf_bytes_of(&h.key))
                .sum();
            let other = cache.pdf_bytes().saturating_sub(own);
            let want: Vec<((f32, f32), f32)> = order.iter().map(|&(_, s, d)| (s, d)).collect();
            let plan = plan_each(&want, other, cache.budget());
            // Release what the plan shrinks or refuses.
            for (&(page, _, _), want) in order.iter().zip(&plan) {
                let held = p.upgrading.get(&page).or_else(|| p.held.get(&page));
                if !held.is_some_and(|h| want.is_none_or(|w| w < h.width)) {
                    continue;
                }
                p.inflight.remove(&page);
                for h in [p.held.remove(&page), p.upgrading.remove(&page)]
                    .into_iter()
                    .flatten()
                {
                    cache.hide(&owner, &h.key);
                }
            }
            for (&(page, size, _), want) in order.iter().zip(&plan) {
                let Some(width) = *want else { continue };
                let held = p.upgrading.get(&page).or_else(|| p.held.get(&page));
                if held.is_some_and(|h| h.width >= width) {
                    continue;
                }
                if p.inflight.len() >= MAX_INFLIGHT {
                    break;
                }
                let pk = PageKey::new(pdf, page);
                let key = pk.cache_key(width);
                let h = Held {
                    width,
                    key: key.clone(),
                };
                let start = cache.show(&owner, &key);
                if p.held.contains_key(&page) {
                    if let Some(old) = p.upgrading.insert(page, h) {
                        cache.hide(&owner, &old.key);
                    }
                    if cache.contains(&key) {
                        // Already resident (another consumer's render): the upgrade is immediate.
                        if let Some(new) = p.upgrading.remove(&page)
                            && let Some(old) = p.held.insert(page, new)
                        {
                            cache.hide(&owner, &old.key);
                        }
                    }
                } else {
                    p.held.insert(page, h);
                }
                if start {
                    cache.reserve(&key, projected_bytes(size, width));
                    p.inflight.insert(page, key.clone());
                    out.jobs.push(Job {
                        page: pk,
                        width,
                        key,
                        bytes: Arc::clone(bytes),
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_cache::Decoded;

    const LETTER: (f32, f32) = (612.0, 792.0);

    fn decoded(bytes: usize) -> Decoded {
        Decoded {
            handle: iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
            width: 1,
            height: 1,
            bytes,
        }
    }

    fn store(pages: usize) -> PdfStore<()> {
        let mut s = PdfStore::default();
        assert!(s.begin_open("d.pdf"));
        s.opened(
            "d.pdf",
            Ok((Arc::new(Vec::new()), vec![LETTER; pages])),
            |p| vec![(); p.len()],
        );
        s
    }

    fn embed() -> Consumer {
        Consumer::Embed {
            note: "A.md".into(),
            view: 7,
        }
    }

    /// A transient admission refusal holds nothing and the next pump retries it once room
    /// frees; embeds plan like tabs.
    #[test]
    fn a_refused_page_retries_on_the_next_pump() {
        let mut s = store(2);
        let one = projected_bytes(LETTER, 704);
        let mut cache = ImageCache::with_budget(one + one / 2);
        cache.show("x", "other");
        cache.reserve("other", one);
        s.show(embed(), PageKey::new("d.pdf", 0), 700.0);
        let p = s.pump(&mut cache);
        assert!(p.jobs.iter().all(|j| j.width < 704), "{:?}", p.jobs.len());
        // Take the whole budget: the page is refused (nothing held, not failed).
        let mut cache = ImageCache::with_budget(one);
        cache.show("x", "other");
        cache.reserve("other", one);
        let mut s = store(2);
        s.show(embed(), PageKey::new("d.pdf", 0), 700.0);
        assert!(s.pump(&mut cache).jobs.is_empty());
        assert!(s.held(&embed(), &PageKey::new("d.pdf", 0)).is_none());
        assert!(s.page_failed(&PageKey::new("d.pdf", 0)).is_none());
        cache.hide("x", "other");
        let p = s.pump(&mut cache);
        assert_eq!(p.jobs.len(), 1, "retried");
        assert_eq!(p.jobs[0].key, "d.pdf#page=1@704");
    }

    /// A render failure is recorded per page, read by a tab and an embed alike, not retried
    /// while shown, and forgotten once no consumer shows the page.
    #[test]
    fn a_render_failure_is_recorded_per_page_for_every_consumer() {
        let mut s = store(1);
        let mut cache = ImageCache::with_budget(64 << 20);
        let pk = PageKey::new("d.pdf", 0);
        s.show(Consumer::Tab(1), pk.clone(), 700.0);
        s.show(embed(), pk.clone(), 700.0);
        let jobs = s.pump(&mut cache).jobs;
        assert_eq!(jobs.len(), 1, "one render serves both");
        cache.loaded(&jobs[0].key, None);
        s.rendered(&pk, &jobs[0].key, Some("boom".into()), &mut cache);
        assert_eq!(s.page_failed(&pk), Some("boom"));
        assert!(s.held(&Consumer::Tab(1), &pk).is_none());
        assert!(s.held(&embed(), &pk).is_none());
        assert!(
            s.pump(&mut cache).jobs.is_empty(),
            "not retried while shown"
        );
        s.hide(&Consumer::Tab(1), &pk, &mut cache);
        assert_eq!(s.page_failed(&pk), Some("boom"), "the embed still shows it");
        s.hide(&embed(), &pk, &mut cache);
        assert_eq!(s.page_failed(&pk), None);
        assert_eq!(cache.pdf_bytes(), 0);
    }

    /// One completion lands for a tab and an embed of the same key: both hold it, one entry.
    #[test]
    fn one_completion_serves_a_tab_and_an_embed_of_the_same_key() {
        let mut s = store(1);
        let mut cache = ImageCache::with_budget(64 << 20);
        let pk = PageKey::new("d.pdf", 0);
        s.show(Consumer::Tab(1), pk.clone(), 700.0);
        s.show(embed(), pk.clone(), 700.0);
        let jobs = s.pump(&mut cache).jobs;
        let key = jobs[0].key.clone();
        assert!(cache.loaded(&key, Some(decoded(projected_bytes(LETTER, 704)))));
        s.rendered(&pk, &key, None, &mut cache);
        assert!(s.pump(&mut cache).jobs.is_empty());
        assert_eq!(s.held(&Consumer::Tab(1), &pk), Some(key.as_str()));
        assert_eq!(s.held(&embed(), &pk), Some(key.as_str()));
        assert_eq!(cache.pins(&key), 2);
        assert_eq!(cache.pdf_bytes(), projected_bytes(LETTER, 704));
        s.view_closed(1, &mut cache);
        assert_eq!(cache.pins(&key), 1, "the embed keeps it");
        s.retain_embeds("A.md", &HashSet::new(), &mut cache);
        assert_eq!(cache.pins(&key), 0);
        assert!(s.pages_of(&embed(), "d.pdf").is_none());
    }

    /// An embed below its planned width upgrades like a tab page (WP-R1's rule, now shared).
    #[test]
    fn an_embed_upgrades_when_room_frees() {
        let mut s = store(1);
        let full = projected_bytes(LETTER, 1920);
        let mut cache = ImageCache::with_budget(full + full / 2);
        cache.show("x", "other");
        cache.reserve("other", full);
        let pk = PageKey::new("d.pdf", 0);
        s.show(embed(), pk.clone(), 1900.0);
        let small = s.pump(&mut cache).jobs.remove(0);
        assert!(small.width < 1920);
        assert!(cache.loaded(
            &small.key,
            Some(decoded(projected_bytes(LETTER, small.width)))
        ));
        s.rendered(&pk, &small.key, None, &mut cache);
        cache.hide("x", "other");
        let up = s.pump(&mut cache).jobs.remove(0);
        assert_eq!(up.width, 1920);
        assert_eq!(
            s.held(&embed(), &pk),
            Some(small.key.as_str()),
            "old shows meanwhile"
        );
        assert!(cache.loaded(&up.key, Some(decoded(full))));
        s.rendered(&pk, &up.key, None, &mut cache);
        assert_eq!(s.held(&embed(), &pk), Some(up.key.as_str()));
        assert!(!cache.contains(&small.key));
    }

    #[test]
    fn page_keys_number_from_one_in_their_string_forms() {
        let k = PageKey::new("a/d.pdf", 0);
        assert_eq!((k.number(), k.id()), (1, "a/d.pdf#page=1".to_owned()));
        assert_eq!(k.cache_key(320), "a/d.pdf#page=1@320");
    }
}
