//! Step module `search`: `search_upsert`, `search_delete` — the search-companion feed (plan
//! `v0-core-daemon` WP-13; bodies in [`cv_search::feed`]).
//!
//! Params (all optional; unknown keys and malformed values disarm, I5):
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `companion_url` | `http://127.0.0.1:4801` | Companion base URL (Crucible `searchServiceUrl`). |
//! | `vault_id` | `"default"` | Companion vault id — Crucible's own `searchVaultId` (default `'default'`, `https://github.com/dulrich/obsidian-crucible/blob/master/src/types.ts:911`), so cut-over inherits its index. |
//! | `embed` | absent | `"provider:model"` from `providers.toml`; absent = lexical-only (Crucible with semantic indexing off). |
//! | `path` | `active_path` | Target note. `search_upsert` without one is the reconcile sweep (`search-refresh`). |
//!
//! **Mode is decided fail-closed** from the vault filesystem: writer unavailable or shadow →
//! [`Mode::Shadow`] (read-only companion calls, no embedding call, outcome vars carry aggregate
//! counts only). Only a live filesystem mints the [`LiveProof`] that unlocks `/v1/chunks/*`.
//!
//! Errors: companion down / `ok: false` → `Deferred{60 s, "search-companion"}`; embedder down →
//! `Deferred{60 s, <provider id>}` (`SearchIndexWorkflow.ts:179-195`); schema ≠ 7, 4xx, config
//! errors → `Failed`.
//!
//! Dropped (Obsidian-induced): `metadataCache` (embeds, frontmatter stamps and link resolution
//! come from `index.db`), the `CompanionAvailabilityGate` latch (the runner's service breaker),
//! and Crucible's settings-driven exclusion list (`isPathExcluded`/`isUserIgnored`); indexable =
//! Crucible's default extensions (`md`, `qmd`, `txt`) outside the index's skipped dirs.

use std::sync::{Arc, OnceLock};

use cv_actions::{ConfigSet, Step};
use cv_index::{LinkKind, Reader};
use cv_providers::{
    ProviderManager, ProvidersConfig, SystemSecrets, parse_model_ref, secrets::default_secrets_dir,
};
use cv_search::chunker::{DEFAULT_MAX_CHARS, DEFAULT_OVERLAP_CHARS};
use cv_search::feed::{self, EmbedTarget, FeedConfig, FeedReport, LocalNote, Mode};
use cv_search::{
    DEFAULT_COMPANION_URL, EmbedRef, LinkedSource, LiveProof, PrepareHost, PrepareInput,
    SearchError, is_search_indexable_path, prepare_search_file,
};
use cv_state::ImageDescription;
use serde_json::{Map, Value};

use super::{StepCtx, StepError, StepOutcome};

/// Crucible's `searchVaultId` default.
pub const DEFAULT_VAULT_ID: &str = "default";
/// Deferral delay for a companion or embedder outage.
pub const RETRY_AFTER_MS: i64 = 60_000;
/// The companion's service name for deferral.
pub const COMPANION_SERVICE: &str = "search-companion";

const KEYS: [&str; 4] = ["companion_url", "vault_id", "embed", "path"];

#[derive(Debug, Default, PartialEq)]
struct Params {
    companion_url: Option<String>,
    vault_id: Option<String>,
    embed: Option<(String, String)>,
    path: Option<String>,
}

fn parse(step: &Step) -> Result<Params, String> {
    let kind = step.kind().as_str();
    let Some(t) = step.table() else {
        return Ok(Params::default());
    };
    let mut p = Params::default();
    for (k, v) in t {
        if !KEYS.contains(&k.as_str()) {
            return Err(format!("{kind}: unknown param {k:?}"));
        }
        let s = v
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{kind}: {k} must be a non-empty string"))?;
        match k.as_str() {
            "companion_url" => {
                if !(s.starts_with("http://") || s.starts_with("https://")) {
                    return Err(format!("{kind}: companion_url must be an http(s) URL"));
                }
                p.companion_url = Some(s.to_owned());
            }
            "vault_id" => p.vault_id = Some(s.to_owned()),
            "embed" => {
                let r = parse_model_ref(Some(s))
                    .filter(cv_providers::ProviderModelRef::is_complete)
                    .ok_or_else(|| format!("{kind}: embed must be \"provider:model\""))?;
                p.embed = Some((r.provider_id, r.model_id));
            }
            _ => p.path = Some(s.to_owned()),
        }
    }
    Ok(p)
}

/// Validates the step's params (an empty table is valid: `search-refresh`).
pub fn validate(step: &Step, _config: &ConfigSet) -> Result<(), String> {
    parse(step).map(|_| ())
}

static MANAGER: OnceLock<Result<ProviderManager, String>> = OnceLock::new();

/// Executes one step on the blocking thread, driving the async feed on the current runtime.
pub fn execute(step: &Step, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let handle =
        tokio::runtime::Handle::try_current().map_err(|_| StepError::failed("no async runtime"))?;
    let mgr = MANAGER
        .get_or_init(|| {
            let dir = default_secrets_dir().unwrap_or_default();
            ProviderManager::new(Arc::new(SystemSecrets { dir })).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| StepError::failed(format!("provider manager: {e}")))?;
    run(step, ctx, &handle, mgr)
}

fn map_err(e: SearchError) -> StepError {
    match e {
        SearchError::Unavailable { .. } => StepError::Deferred {
            retry_after_ms: RETRY_AFTER_MS,
            service: Some(COMPANION_SERVICE.to_owned()),
        },
        SearchError::EmbedderUnavailable { provider, .. } => StepError::Deferred {
            retry_after_ms: RETRY_AFTER_MS,
            service: Some(provider),
        },
        other => StepError::failed(other.to_string()),
    }
}

fn embed_target(
    ctx: &StepCtx<'_>,
    embed: Option<&(String, String)>,
) -> Result<Option<EmbedTarget>, StepError> {
    let Some((provider, model)) = embed else {
        return Ok(None);
    };
    let text = toml::to_string(ctx.config.table("providers.toml"))
        .map_err(|e| StepError::failed(format!("providers.toml: {e}")))?;
    let cfg = ProvidersConfig::from_toml(&text).map_err(|e| StepError::failed(e.to_string()))?;
    let p = cfg
        .get(provider)
        .cloned()
        .ok_or_else(|| StepError::failed(format!("embed: unknown provider {provider:?}")))?;
    Ok(Some(EmbedTarget {
        provider: p,
        model: model.clone(),
    }))
}

/// Shadow unless the writer positively reports a live filesystem (fail-closed).
fn mode(ctx: &StepCtx<'_>) -> Mode {
    let shadow = ctx
        .writer()
        .ok()
        .and_then(|w| w.call(|vw| vw.fs().is_shadow()).ok())
        .unwrap_or(true);
    LiveProof::from_shadow_flag(shadow).map_or(Mode::Shadow, Mode::Live)
}

fn vars(report: &FeedReport, live: bool) -> Map<String, Value> {
    let c = report.counts;
    let mut v = Map::new();
    for (k, n) in [
        ("matched", c.matched),
        ("mismatched", c.mismatched),
        ("missing_remote", c.missing_remote),
        ("missing_local", c.missing_local),
        ("embedding_mismatch", c.embedding_mismatch),
    ] {
        v.insert(k.into(), Value::from(n));
    }
    if live {
        v.insert("upserted".into(), Value::from(report.upserted));
        v.insert("deleted".into(), Value::from(report.deleted));
        v.insert("chunks".into(), Value::from(report.chunks));
    }
    v
}

/// The step body with an injected provider manager (tests use a fake secret source).
pub(crate) fn run(
    step: &Step,
    ctx: &mut StepCtx<'_>,
    handle: &tokio::runtime::Handle,
    mgr: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    let p = parse(step).map_err(StepError::failed)?;
    let cfg = FeedConfig {
        companion_url: p
            .companion_url
            .clone()
            .unwrap_or_else(|| DEFAULT_COMPANION_URL.to_owned()),
        vault_id: p
            .vault_id
            .clone()
            .unwrap_or_else(|| DEFAULT_VAULT_ID.to_owned()),
        embed: embed_target(ctx, p.embed.as_ref())?,
        max_chars: DEFAULT_MAX_CHARS,
        overlap_chars: DEFAULT_OVERLAP_CHARS,
    };
    let mode = mode(ctx);
    let live = matches!(mode, Mode::Live(_));
    let target = p.path.clone().or_else(|| ctx.context.active_path.clone());
    let reader = open_reader(ctx)?;
    let ctx_ref: &StepCtx<'_> = ctx;
    let host = NotePrep::new(
        Box::new(move |p| ctx_ref.read(p).map(|o| o.map(|(b, _)| b))),
        &reader,
        Box::new(move |md5| {
            let db = ctx_ref.state.lock().ok()?;
            db.image_description(md5).ok().flatten()
        }),
    );
    let report = match (step, target) {
        (Step::SearchDelete(_), Some(path)) => {
            handle.block_on(feed::delete_path(&cfg, mode, &path))
        }
        (Step::SearchDelete(_), None) => {
            return Err(StepError::failed("search_delete needs a path"));
        }
        (_, Some(path)) => {
            let note = host.local_note(&path)?;
            handle.block_on(feed::upsert_path(mgr, &cfg, mode, &path, note))
        }
        (_, None) => {
            let local = host.local_hashes()?;
            let load = |path: &str| {
                host.local_note(path)
                    .map_err(|e| SearchError::Malformed(e.to_string()))
            };
            handle.block_on(feed::reconcile(mgr, &cfg, mode, &local, &load))
        }
    }
    .map_err(map_err)?;
    tracing::info!(
        mode = if live { "live" } else { "shadow" },
        matched = report.counts.matched,
        mismatched = report.counts.mismatched,
        missing_remote = report.counts.missing_remote,
        missing_local = report.counts.missing_local,
        embedding_mismatch = report.counts.embedding_mismatch,
        "search feed step done"
    );
    Ok(StepOutcome {
        vars: vars(&report, live),
        ..StepOutcome::default()
    })
}

/// Opens the current index generation for the step (reopens once on a generation swap).
fn open_reader(ctx: &StepCtx<'_>) -> Result<Reader, StepError> {
    let open = || Reader::open_current(ctx.index_db);
    // A generation swap between reading the pointer and opening the file: reopen once.
    open()
        .or_else(|_| open())
        .map_err(|e| StepError::failed(format!("index: {e}")))
}

/// Vault read used by [`NotePrep`]: the bytes of a vault-relative path, `None` when absent.
pub(crate) type ReadFn<'r> = Box<dyn Fn(&str) -> Result<Option<Vec<u8>>, StepError> + 'r>;
/// Image-description lookup used by [`NotePrep`] (Crucible `store.has` + `store.get`).
pub(crate) type DescribeFn<'r> = Box<dyn Fn(&str) -> Option<ImageDescription> + 'r>;

/// File preparation's host: index queries, a vault read function and an image-description
/// lookup. The feed step builds it over the overlay-aware writer and the daemon's `StateDb`;
/// `cv shadow parity` (WP-14, P3) builds it over a fresh index, direct read-only vault reads and
/// a `StateReader`, so both compute `contentHash` with exactly this code.
pub(crate) struct NotePrep<'r> {
    read: ReadFn<'r>,
    reader: &'r Reader,
    describe: DescribeFn<'r>,
}

impl<'r> NotePrep<'r> {
    /// A preparation host over `reader`, reading notes with `read` and descriptions with
    /// `describe`.
    pub(crate) fn new(read: ReadFn<'r>, reader: &'r Reader, describe: DescribeFn<'r>) -> Self {
        Self {
            read,
            reader,
            describe,
        }
    }

    fn idx<T>(r: cv_index::Result<T>) -> Result<T, StepError> {
        r.map_err(|e| StepError::failed(format!("index: {e}")))
    }

    /// Whether the feed indexes `path`.
    pub(crate) fn indexable(path: &str) -> bool {
        is_search_indexable_path(path) && !cv_index::is_skipped_path(path)
    }

    /// Every indexable note, prepared, in path order.
    pub(crate) fn local_notes(&self) -> Result<Vec<(String, LocalNote)>, StepError> {
        let q = self.reader.query();
        let mut out = Vec::new();
        let mut folders = vec![String::new()];
        while let Some(folder) = folders.pop() {
            for f in Self::idx(q.folder_files(&folder))? {
                if !Self::indexable(&f.path) {
                    continue;
                }
                if let Some(n) = self.local_note(&f.path)? {
                    out.push((f.path, n));
                }
            }
            folders.extend(Self::idx(q.subfolders(&folder))?);
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Every indexable note's `(path, contentHash)`.
    pub(crate) fn local_hashes(&self) -> Result<Vec<(String, String)>, StepError> {
        let mut out: Vec<(String, String)> = self
            .local_notes()?
            .into_iter()
            .map(|(p, n)| (p, n.prepared.content_hash))
            .collect();
        out.sort();
        Ok(out)
    }

    /// Prepares `path`, or `None` when it is gone or not indexable.
    pub(crate) fn local_note(&self, path: &str) -> Result<Option<LocalNote>, StepError> {
        if !Self::indexable(path) {
            return Ok(None);
        }
        let Some(bytes) = (self.read)(path)? else {
            return Ok(None);
        };
        let q = self.reader.query();
        let mtime = Self::idx(q.file(path))?.map_or(0, |f| f.mtime);
        let links = Self::idx(q.outgoing(path))?;
        let embeds = links
            .iter()
            .filter(|l| matches!(l.kind, LinkKind::Embed | LinkKind::MdImage) && !l.external)
            .map(|l| EmbedRef {
                link: l.target.clone(),
                resolved: l.resolved_path.clone(),
            })
            .collect();
        let props = Self::idx(q.props(path))?;
        let prop = |k: &str| props.iter().find(|p| p.key == k).map(|p| p.value.clone());
        let input = PrepareInput {
            path: path.to_owned(),
            content: String::from_utf8_lossy(&bytes).into_owned(),
            embeds,
            x_metadata: prop("x-metadata"),
            yt_metadata: prop("yt-metadata"),
        };
        Ok(Some(LocalNote {
            prepared: prepare_search_file(self, &input),
            basename: cv_search::prepare::basename(path),
            mtime,
        }))
    }
}

impl PrepareHost for NotePrep<'_> {
    fn image_description(&self, md5: &str) -> Option<ImageDescription> {
        (self.describe)(md5)
    }

    /// Obsidian `getFirstLinkpathDest`, through the index: the note's own resolved link to
    /// `linkpath` (any kind), else an exact path / `path.md` match.
    fn resolve_linked(&self, linkpath: &str, from: &str) -> Option<LinkedSource> {
        let q = self.reader.query();
        let resolved = q
            .outgoing(from)
            .ok()?
            .into_iter()
            .find(|l| l.target == linkpath && l.resolved_path.is_some())
            .and_then(|l| l.resolved_path)
            .or_else(|| {
                [linkpath.to_owned(), format!("{linkpath}.md")]
                    .into_iter()
                    .find(|p| q.file(p).ok().flatten().is_some())
            })?;
        let bytes = (self.read)(&resolved).ok()??;
        Some(LinkedSource {
            basename: cv_search::prepare::basename(&resolved),
            path: resolved,
            content: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(src: &str) -> Step {
        let t: toml::Table = toml::from_str(src).unwrap();
        Step::SearchUpsert(t)
    }

    #[test]
    fn param_less_step_validates_and_bad_values_disarm() {
        let c = ConfigSet::new();
        assert!(validate(&step(""), &c).is_ok());
        assert!(validate(&step("embed = \"local:bge-m3\"\nvault_id = \"v\""), &c).is_ok());
        assert!(validate(&step("nope = \"x\""), &c).is_err());
        assert!(validate(&step("embed = \"no-model\""), &c).is_err());
        assert!(validate(&step("companion_url = \"ftp://x\""), &c).is_err());
        assert!(validate(&step("path = 3"), &c).is_err());
    }
}
