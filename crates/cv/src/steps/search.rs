//! Step module `search`: `search_upsert`, `search_delete` — the search feed into the daemon's
//! in-process `semsearch` index (plan `v0-core-daemon` WP-13; plan semsearch-crate WP-3, D22;
//! bodies in [`cv_search::feed`]).
//!
//! Params (all optional; unknown keys and malformed values disarm, I5):
//!
//! | Key | Default | Meaning |
//! |---|---|---|
//! | `embed` | absent | `"provider:model"` from `providers.toml`; absent = lexical-only (Crucible with semantic indexing off). The index space is the model id. |
//! | `path` | `active_path` | Target note. `search_upsert` without one is the reconcile sweep (`search-refresh`). |
//!
//! **Mode is decided fail-closed** from the vault writer: writer unavailable → report-only:
//! a read-only open of `CV_SEARCH_DB` (a missing file counts as empty, never created),
//! no embedding call, no write, outcome vars carry aggregate counts only. Only a reachable writer
//! (live filesystem) writes, through the runner's one shared index handle ([`StepCtx::search`]).
//!
//! Errors: embedder down → `Deferred{60 s, <provider id>}` (`SearchIndexWorkflow.ts:179-195`);
//! an embedding-space mismatch with the index, an index open/SQL error, config errors →
//! `Failed`.
//!
//! Dropped (Obsidian-induced): `metadataCache` (embeds, frontmatter stamps and link resolution
//! come from `index.db`), and Crucible's settings-driven exclusion list
//! (`isPathExcluded`/`isUserIgnored`); indexable = Crucible's default extensions (`md`, `qmd`,
//! `txt`) outside the index's skipped dirs.

use cv_actions::{ConfigSet, StepKind};
use cv_index::{LinkKind, Reader};
use cv_providers::{ProviderManager, ProvidersConfig, parse_model_ref};
use cv_search::chunker::{DEFAULT_MAX_CHARS, DEFAULT_OVERLAP_CHARS};
use cv_search::feed::{self, EmbedTarget, FeedConfig, FeedReport, LocalNote, ReportScope};
use cv_search::{
    EmbedRef, LinkedSource, PrepareHost, PrepareInput, SearchError, is_search_indexable_path,
    prepare_search_file,
};
use cv_state::{ImageDescription, ServiceFailureKind};
use serde::Serialize;

use super::{DeferCause, StepCtx, StepError, StepModule, StepOutcome};

/// Deferral delay for an embedder outage.
pub const RETRY_AFTER_MS: i64 = 60_000;

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Params {
    delete: bool,
    embed: Option<(String, String)>,
    path: Option<String>,
}

pub(crate) fn parse(step_kind: StepKind, t: &toml::Table) -> Result<Params, String> {
    let kind = step_kind.as_str();
    let mut p = Params {
        delete: step_kind == StepKind::SearchDelete,
        ..Params::default()
    };
    for (k, v) in t {
        if !super::params::is_param(step_kind, k) {
            return Err(format!("{kind}: unknown param {k:?}"));
        }
        let s = v
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{kind}: {k} must be a non-empty string"))?;
        match k.as_str() {
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

/// `search_upsert` / `search_delete` (an empty table is valid: `search-refresh`). Runs on the
/// blocking thread, driving the async feed on the current runtime. Embedding goes through the
/// process-wide [`super::agent::manager`] (shared with the daemon's `search` request).
pub(crate) struct Search;

impl StepModule for Search {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        parse(kind, t)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| StepError::failed("no async runtime"))?;
        let mgr = super::agent::manager()?;
        run(&p, ctx, &handle, mgr)
    }
}

fn map_err(e: SearchError) -> StepError {
    match e {
        SearchError::EmbedderUnavailable { provider, .. } => StepError::Deferred {
            retry_after_ms: RETRY_AFTER_MS,
            service: Some(provider),
            cause: DeferCause::Transient(ServiceFailureKind::ServerError),
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
    resolve_target(ctx.config, provider, model)
        .map(Some)
        .map_err(StepError::failed)
}

/// Resolves `provider:model` against the config's `providers.toml`.
fn resolve_target(config: &ConfigSet, provider: &str, model: &str) -> Result<EmbedTarget, String> {
    let text = toml::to_string(config.table("providers.toml"))
        .map_err(|e| format!("providers.toml: {e}"))?;
    let cfg = ProvidersConfig::from_toml(&text).map_err(|e| e.to_string())?;
    let p = cfg
        .get(provider)
        .cloned()
        .ok_or_else(|| format!("embed: unknown provider {provider:?}"))?;
    Ok(EmbedTarget {
        provider: p,
        model: model.to_owned(),
    })
}

/// The query-side embed target for the daemon's `search` request (plan semsearch-crate WP-4):
/// the `embed` of the first `search_upsert` step (armed actions in id order) — the same
/// `provider:model` the feed writes with. `None` = no embed configured or it does not resolve.
pub(crate) fn query_embed_target(config: &ConfigSet) -> Option<EmbedTarget> {
    let (provider, model) = config
        .armed_actions()
        .flat_map(|a| a.steps.iter())
        .filter(|s| s.kind == StepKind::SearchUpsert)
        .find_map(|s| parse(s.kind, &s.params).ok().and_then(|p| p.embed))?;
    resolve_target(config, &provider, &model)
        .inspect_err(|e| tracing::warn!(error = %e, "search: query embed target unresolved"))
        .ok()
}

/// Live only when the writer thread positively answers (fail-closed; otherwise the step only
/// reports counts).
fn is_live(ctx: &StepCtx<'_>) -> bool {
    ctx.writer()
        .ok()
        .and_then(|w| w.call(|_| ()).ok())
        .is_some()
}

/// `search_upsert` / `search_delete` outcome vars (the write counts only when live).
#[derive(Serialize)]
struct Vars {
    matched: usize,
    mismatched: usize,
    missing_index: usize,
    missing_local: usize,
    embedding_mismatch: usize,
    #[serde(flatten)]
    live: Option<LiveVars>,
}

/// The live feed's write counts.
#[derive(Serialize)]
struct LiveVars {
    upserted: usize,
    deleted: usize,
    chunks: usize,
    embedded: usize,
}

fn vars(report: &FeedReport, live: bool) -> StepOutcome {
    let c = report.counts;
    let mut out = StepOutcome::default();
    out.set_vars(&Vars {
        matched: c.matched,
        mismatched: c.mismatched,
        missing_index: c.missing_index,
        missing_local: c.missing_local,
        embedding_mismatch: c.embedding_mismatch,
        live: live.then_some(LiveVars {
            upserted: report.upserted,
            deleted: report.deleted,
            chunks: report.chunks,
            embedded: report.embedded,
        }),
    });
    out
}

/// The step body with an injected provider manager (tests use a fake secret source).
pub(crate) fn run(
    p: &Params,
    ctx: &mut StepCtx<'_>,
    handle: &tokio::runtime::Handle,
    mgr: &ProviderManager,
) -> Result<StepOutcome, StepError> {
    let cfg = FeedConfig {
        embed: embed_target(ctx, p.embed.as_ref())?,
        max_chars: DEFAULT_MAX_CHARS,
        overlap_chars: DEFAULT_OVERLAP_CHARS,
    };
    let live = is_live(ctx);
    let search = ctx
        .search
        .ok_or_else(|| StepError::failed("search index unavailable outside the runner"))?;
    let target = p.path.clone().or_else(|| ctx.context.active_path.clone());
    if p.delete && target.is_none() {
        return Err(StepError::failed("search_delete needs a path"));
    }
    let reader = ctx.index()?;
    let ctx_ref: &StepCtx<'_> = ctx;
    let host = NotePrep::new(
        Box::new(move |p| ctx_ref.read(p).map(|o| o.map(|(b, _)| b))),
        &reader,
        Box::new(move |md5| ctx_ref.state().image_description(md5).ok().flatten()),
    );
    let report = if live {
        let index = search.shared().map_err(map_err)?;
        match (p.delete, target) {
            (true, Some(path)) => feed::delete_path(&index, &path),
            (_, Some(path)) => {
                let note = host.local_note(&path)?;
                handle.block_on(feed::upsert_path(mgr, &cfg, &index, &path, note))
            }
            (_, None) => {
                let local = host.local_hashes()?;
                let load = |path: &str| {
                    host.local_note(path)
                        .map_err(|e| SearchError::Malformed(e.to_string()))
                };
                handle.block_on(feed::reconcile(mgr, &cfg, &index, &local, &load))
            }
        }
    } else {
        match (p.delete, target) {
            (true, Some(path)) => {
                feed::report_only(search.path(), &cfg, ReportScope::Path(&path, None))
            }
            (_, Some(path)) => {
                let hash = host.local_note(&path)?.map(|n| n.prepared.content_hash);
                feed::report_only(
                    search.path(),
                    &cfg,
                    ReportScope::Path(&path, hash.as_deref()),
                )
            }
            (_, None) => {
                let local = host.local_hashes()?;
                feed::report_only(search.path(), &cfg, ReportScope::All(&local))
            }
        }
    }
    .map_err(map_err)?;
    tracing::info!(
        mode = if live { "live" } else { "report_only" },
        matched = report.counts.matched,
        mismatched = report.counts.mismatched,
        missing_index = report.counts.missing_index,
        missing_local = report.counts.missing_local,
        embedding_mismatch = report.counts.embedding_mismatch,
        "search feed step done"
    );
    Ok(vars(&report, live))
}

/// Vault read used by [`NotePrep`]: the bytes of a vault-relative path, `None` when absent.
pub(crate) type ReadFn<'r> = Box<dyn Fn(&str) -> Result<Option<Vec<u8>>, StepError> + 'r>;
/// Image-description lookup used by [`NotePrep`] (Crucible `store.has` + `store.get`).
pub(crate) type DescribeFn<'r> = Box<dyn Fn(&str) -> Option<ImageDescription> + 'r>;

/// File preparation's host: index queries, a vault read function and an image-description
/// lookup. The feed step builds it over the daemon's writer and the daemon's `StateDb`.
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
            basename: cv_text::vpath::stem_last_ext(path).to_owned(),
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
            basename: cv_text::vpath::stem_last_ext(&resolved).to_owned(),
            path: resolved,
            content: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(src: &str) -> cv_actions::Step {
        let params: toml::Table = toml::from_str(src).unwrap();
        cv_actions::Step {
            kind: StepKind::SearchUpsert,
            params,
        }
    }

    #[test]
    fn param_less_step_validates_and_bad_values_disarm() {
        let c = ConfigSet::new();
        assert!(super::super::validate(&step(""), &c).is_ok());
        assert!(
            super::super::validate(&step("embed = \"local:bge-m3\"\npath = \"n.md\""), &c).is_ok()
        );
        assert!(super::super::validate(&step("nope = \"x\""), &c).is_err());
        assert!(super::super::validate(&step("embed = \"no-model\""), &c).is_err());
        // Retired in semsearch-crate WP-4: the old companion params now disarm (I5).
        assert!(super::super::validate(&step("vault_id = \"v\""), &c).is_err());
        assert!(super::super::validate(&step("path = 3"), &c).is_err());
    }

    #[test]
    fn query_embed_target_is_the_first_search_upsert_embed() {
        let mut c = ConfigSet::new();
        let actions = "[[action]]\nid = \"a\"\n[[action.steps]]\ntype = \"search_upsert\"\n\n[[action]]\nid = \"b\"\n[[action.steps]]\ntype = \"search_upsert\"\nembed = \"emb:bge-m3-f16\"\n";
        let providers = "[[provider]]\nid = \"emb\"\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:9\"\n";
        c.reload(
            &std::collections::BTreeMap::from([
                ("actions.toml".to_owned(), Some(actions.to_owned())),
                ("providers.toml".to_owned(), Some(providers.to_owned())),
            ]),
            &super::super::validate,
        );
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        let t = query_embed_target(&c).unwrap();
        assert_eq!(
            (t.provider.id.as_str(), t.model.as_str()),
            ("emb", "bge-m3-f16")
        );
        assert!(query_embed_target(&ConfigSet::new()).is_none());
    }
}
