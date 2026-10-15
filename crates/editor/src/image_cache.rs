//! Bounded, off-thread image cache (WP-3).
//!
//! S3 measured image memory growing with `Handle::from_path` (`runs/spike/gui-rss/FINDINGS.md`),
//! and iced's GPU atlas keeps a texture alive as long as any live handle exists
//! (`iced_wgpu-0.14.0/src/image/raster.rs:78-103`), so bounding CPU-side handle lifetime is ours.
//!
//! - Entries are keyed by vault path, hold one decoded + downscaled RGBA [`Handle`], and are
//!   accounted by decoded RGBA bytes against a budget (`CV_IMAGE_CACHE_MB`, default
//!   [`DEFAULT_BUDGET_MB`]; WP-8 measures it). Over budget, the least-recently-used entries are
//!   evicted.
//! - Each entry is *pinned* by the owners (tab paths) that currently show it. An entry with no
//!   pins is released immediately, so **handle release** happens on: sensor `on_hide`
//!   ([`ImageCache::hide`]), tab close and mode switch away from reading/split
//!   ([`ImageCache::release_owner`]), render-model replacement dropping an image
//!   ([`ImageCache::retain_owner`]), and eviction.
//! - **Renderer reclamation** is distinct and not observable here: once our cache has dropped its
//!   `Handle` and the next frame's widget tree no longer holds a clone, iced's atlas trims the
//!   texture on its own schedule. Tests assert handle release only.
//!
//! Decoding runs off the UI thread ([`load_task`]: `Task::perform` + `spawn_blocking`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use iced::Task;
use iced::widget::image::Handle;

use crate::daemon::DaemonClient;

/// Separates a document path from its view id in a per-view owner (never in a vault path).
const VIEW_SEP: char = '\u{1f}';

/// The pin owner for view `view` (a tab id) of document `path` (plan editor-feedback-round-2
/// WP-7): two tabs of one note pin an image independently, so one duplicate scrolling an image
/// away does not release it while the other still shows it. [`ImageCache::release_owner`] /
/// [`ImageCache::retain_owner`] on the bare `path` cover every view of it.
pub fn view_owner(path: &str, view: u64) -> String {
    format!("{path}{VIEW_SEP}{view}")
}

/// `o` is `owner` itself or one of its per-view owners.
fn owned_by(o: &str, owner: &str) -> bool {
    o.strip_prefix(owner)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(VIEW_SEP))
}

/// Default byte budget, in MiB.
pub const DEFAULT_BUDGET_MB: usize = 64;
/// Display widths are rounded up to this bucket, and capped at [`MAX_WIDTH`].
const WIDTH_BUCKET: u32 = 64;
/// Largest decoded width.
pub const MAX_WIDTH: u32 = 2048;

/// A decode failure.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("read: {0}")]
    Read(String),
    #[error("decode: {0}")]
    Image(#[from] image::ImageError),
    #[error("decode task failed")]
    Join,
}

/// A decoded image ready for the cache.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub handle: Handle,
    pub width: u32,
    pub height: u32,
    /// Decoded RGBA bytes (`width * height * 4`).
    pub bytes: usize,
}

#[derive(Debug)]
struct Entry {
    handle: Handle,
    bytes: usize,
    last_used: u64,
    owners: HashSet<String>,
}

/// The cache.
#[derive(Debug)]
pub struct ImageCache {
    budget: usize,
    used: usize,
    tick: u64,
    entries: HashMap<String, Entry>,
    /// Keys with a load in flight, and who asked.
    pending: HashMap<String, HashSet<String>>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::from_env()
    }
}

impl ImageCache {
    /// Budget from `CV_IMAGE_CACHE_MB` (default 64 MiB).
    pub fn from_env() -> Self {
        let mb = std::env::var("CV_IMAGE_CACHE_MB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_BUDGET_MB);
        Self::with_budget(mb.saturating_mul(1024 * 1024))
    }

    /// A cache with a byte budget.
    pub fn with_budget(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    /// The byte budget.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Decoded bytes held.
    pub fn used_bytes(&self) -> usize {
        self.used
    }

    /// Entries (live handles) held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Loads in flight.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Owners pinning `key` (0 = absent).
    pub fn pins(&self, key: &str) -> usize {
        self.entries.get(key).map_or(0, |e| e.owners.len())
    }

    /// Whether `key` is cached.
    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// The cached handle.
    pub fn get(&self, key: &str) -> Option<&Handle> {
        self.entries.get(key).map(|e| &e.handle)
    }

    /// `owner` shows `key`: pins a hit (and touches it), or returns `true` when the caller must
    /// start a load (none cached and none in flight).
    pub fn show(&mut self, owner: &str, key: &str) -> bool {
        self.tick += 1;
        if let Some(e) = self.entries.get_mut(key) {
            e.last_used = self.tick;
            e.owners.insert(owner.to_owned());
            return false;
        }
        let waiting = self.pending.entry(key.to_owned()).or_default();
        let start = waiting.is_empty();
        waiting.insert(owner.to_owned());
        start
    }

    /// `owner` no longer shows `key` (sensor `on_hide`): unpins; an unpinned entry is released.
    pub fn hide(&mut self, owner: &str, key: &str) {
        if let Some(w) = self.pending.get_mut(key) {
            w.remove(owner);
        }
        let release = match self.entries.get_mut(key) {
            Some(e) => {
                e.owners.remove(owner);
                e.owners.is_empty()
            }
            None => false,
        };
        if release {
            self.remove(key);
        }
    }

    /// Releases every pin `owner` holds (tab close, mode switch to source), including every
    /// per-view owner [`view_owner`]`(owner, _)` (plan editor-feedback-round-2 WP-7).
    pub fn release_owner(&mut self, owner: &str) {
        self.retain_owner(owner, &HashSet::new());
    }

    /// Drops `owner`'s pins on keys not in `keep` (render-model replacement).
    pub fn retain_owner(&mut self, owner: &str, keep: &HashSet<String>) {
        for (k, w) in &mut self.pending {
            if !keep.contains(k) {
                w.retain(|o| !owned_by(o, owner));
            }
        }
        let drop: Vec<String> = self
            .entries
            .iter_mut()
            .filter(|(k, _)| !keep.contains(*k))
            .filter_map(|(k, e)| {
                e.owners.retain(|o| !owned_by(o, owner));
                e.owners.is_empty().then(|| k.clone())
            })
            .collect();
        for k in drop {
            self.remove(&k);
        }
    }

    /// A load finished. Stored only if someone still waits for it; then evicts LRU entries
    /// until within budget (the new entry goes last). Returns whether it was stored.
    pub fn loaded(&mut self, key: &str, decoded: Option<Decoded>) -> bool {
        let owners = self.pending.remove(key).unwrap_or_default();
        let Some(d) = decoded else { return false };
        if owners.is_empty() {
            return false;
        }
        self.remove(key);
        self.tick += 1;
        self.used += d.bytes;
        self.entries.insert(
            key.to_owned(),
            Entry {
                handle: d.handle,
                bytes: d.bytes,
                last_used: self.tick,
                owners,
            },
        );
        self.evict(key);
        true
    }

    fn evict(&mut self, keep: &str) {
        while self.used > self.budget {
            let victim = self
                .entries
                .iter()
                .filter(|(k, _)| k.as_str() != keep)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => self.remove(&k),
                // Only the new entry is left: one image larger than the budget stays shown.
                None => break,
            }
        }
    }

    fn remove(&mut self, key: &str) {
        if let Some(e) = self.entries.remove(key) {
            self.used = self.used.saturating_sub(e.bytes);
        }
    }
}

/// Rounds a display width to the decode width.
pub fn decode_width(display: f32) -> u32 {
    let w = if display.is_finite() && display > 0.0 {
        // Saturating float→int: in range by the clamp.
        display.min(MAX_WIDTH as f32).ceil() as u32
    } else {
        MAX_WIDTH
    };
    w.div_ceil(WIDTH_BUCKET)
        .saturating_mul(WIDTH_BUCKET)
        .clamp(WIDTH_BUCKET, MAX_WIDTH)
}

/// Decodes `bytes`, downscaled to at most `max_width` (aspect kept), into an RGBA handle.
pub fn decode(bytes: &[u8], max_width: u32) -> Result<Decoded, DecodeError> {
    let img = image::load_from_memory(bytes)?;
    let img = if img.width() > max_width {
        let h =
            (u64::from(img.height()) * u64::from(max_width) / u64::from(img.width().max(1))).max(1);
        img.resize(
            max_width,
            u32::try_from(h).unwrap_or(u32::MAX),
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    let raw = rgba.into_raw();
    let bytes = raw.len();
    Ok(Decoded {
        handle: Handle::from_rgba(width, height, raw),
        width,
        height,
        bytes,
    })
}

/// Reads `key` (vault path) through the daemon client and decodes it, off the UI thread.
pub fn load_task<M: Send + 'static>(
    daemon: Arc<dyn DaemonClient>,
    key: String,
    width: u32,
    done: impl Fn(String, Result<Decoded, String>) -> M + Send + 'static,
) -> Task<M> {
    let k = key.clone();
    Task::perform(
        async move {
            match tokio::task::spawn_blocking(move || {
                let (bytes, _) = daemon
                    .read_file(&k)
                    .map_err(|e| DecodeError::Read(e.to_string()))?;
                decode(&bytes, width)
            })
            .await
            {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(_) => Err(DecodeError::Join.to_string()),
            }
        },
        move |r| done(key.clone(), r),
    )
}
