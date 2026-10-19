//! Shared plumbing for the A2 fetch steps (plan `v0-automation-pipelines` WP-4; reused by
//! WP-5/7/8/9):
//!
//! - [`client`]: the process-wide [`WebClient`] (30 s timeout);
//! - [`block_on`]: drive a future from a step's blocking thread (steps run in `spawn_blocking`);
//! - [`secret`]: a bare-named secret (env, then `~/.config/crucible-vault/<name>`);
//! - [`api_base`]: a service's API base URL (overridable in tests);
//! - [`record_usage`]: add quota units to `api_usage` for the local day;
//! - [`find_in_subfolders`] / [`create_or_found`]: the metadata-note find-or-create (an
//!   `Exists` from the single writer means a concurrent job won — treat as found);
//! - [`append_link`]: append-only frontmatter link lists (`yt-metadata`, `x-metadata`).
//!
//! Tests inject API bases and a [`SecretSource`] with [`test_overrides`] (thread-local: steps run
//! on the test thread), never through env vars.

use std::future::Future;
use std::sync::OnceLock;

use cv_note::{FrontmatterDoc, JsValue};
#[cfg(test)]
use cv_providers::secrets::SecretSource;
use cv_providers::secrets::{SystemSecrets, default_secrets_dir, resolve_secret};
use cv_providers::web::{WebClient, WebConfig};
use cv_vault::VaultError;

use super::{StepCtx, StepError};

/// The process-wide fetch client.
pub(crate) fn client() -> Result<&'static WebClient, StepError> {
    static CLIENT: OnceLock<Result<WebClient, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| WebClient::new(WebConfig::default()).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| StepError::failed(format!("web client: {e}")))
}

/// Runs `f` to completion on the current tokio runtime (from a blocking thread).
pub(crate) fn block_on<F: Future>(f: F) -> Result<F::Output, StepError> {
    let handle =
        tokio::runtime::Handle::try_current().map_err(|_| StepError::failed("no async runtime"))?;
    Ok(handle.block_on(f))
}

/// The `SecretSource` steps resolve through (the real environment + secrets dir).
fn system_secrets() -> &'static SystemSecrets {
    static S: OnceLock<SystemSecrets> = OnceLock::new();
    S.get_or_init(|| SystemSecrets {
        dir: default_secrets_dir().unwrap_or_default(),
    })
}

/// Resolves the bare-named secret `name` (`None` = missing → the step fails `NoApiKey`).
pub(crate) fn secret(name: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(Some(v)) =
        overrides::with(|o| o.secrets.as_ref().map(|s| resolve_secret(name, &**s)))
    {
        return v;
    }
    resolve_secret(name, system_secrets())
}

/// `service`'s API base: `default` unless a test overrides it.
pub(crate) fn api_base(service: &str, default: &str) -> String {
    #[cfg(test)]
    if let Some(Some(b)) = overrides::with(|o| o.bases.get(service).cloned()) {
        return b;
    }
    let _ = service;
    default.to_owned()
}

/// Today's local date (`%Y-%m-%d`), the `api_usage` day.
pub(crate) fn local_day() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Adds `units` to `service`'s usage for today. Best-effort: a state error is logged, not fatal
/// (the fetch already happened).
pub(crate) fn record_usage(ctx: &StepCtx<'_>, service: &str, units: i64) {
    let day = local_day();
    let r = ctx.state().add_api_usage(service, &day, units);
    if let Err(e) = r {
        tracing::warn!(service, error = %e, "api_usage update failed");
    }
}

/// Finds `<root>/<child>/<file_name>` for any direct child folder of `root` (sorted, first
/// hit wins; Crucible `findExistingMetadataNote`, `youtubeApi.ts:377-386`). Lists folders
/// read-only on disk and confirms the hit through the writer.
pub(crate) fn find_in_subfolders(
    ctx: &StepCtx<'_>,
    root: &str,
    file_name: &str,
) -> Result<Option<String>, StepError> {
    let root = root.trim_matches('/');
    let Ok(rd) = std::fs::read_dir(ctx.vault_root.join(root)) else {
        return Ok(None);
    };
    let mut dirs: Vec<String> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    dirs.sort();
    for d in dirs {
        let p = format!("{root}/{d}/{file_name}");
        if ctx.read(&p)?.is_some() {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// Creates `path`; `Ok(false)` when it already exists (a concurrent job created it first — the
/// single writer's exact-name absence check makes the loser treat it as found).
pub(crate) fn create_or_found(
    ctx: &mut StepCtx<'_>,
    path: &str,
    bytes: Vec<u8>,
) -> Result<bool, StepError> {
    let p = path.to_owned();
    let touched = [path];
    match ctx.with_writer(&touched, move |vw| vw.create(&p, &bytes))? {
        Ok(_) => Ok(true),
        Err(VaultError::Exists(_)) => {
            ctx.written.retain(|w| w != path);
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// Appends `link` to the frontmatter list `key` of `doc` (Crucible `appendYtMetadataLink`,
/// `youtubeApi.ts:648-664`): append-only (entry [0] stays first); already present → no change;
/// a non-empty scalar becomes `[old, link]`; absent/empty → `[link]` inserted directly after
/// `anchor` (end of block when `anchor` is absent). Returns whether `doc` changed.
pub(crate) fn append_link(
    doc: &mut FrontmatterDoc,
    anchor: &str,
    key: &str,
    link: &str,
) -> Result<bool, StepError> {
    let item = JsValue::Str(link.to_owned());
    match doc.get(key).map_err(StepError::from)? {
        None | Some(JsValue::Null) => doc
            .insert_after(anchor, key, &JsValue::Arr(vec![item]))
            .map_err(StepError::from),
        Some(JsValue::Str(s)) if s.trim().is_empty() => doc
            .insert_after(anchor, key, &JsValue::Arr(vec![item]))
            .map_err(StepError::from),
        Some(JsValue::Arr(a)) if a.is_empty() => doc
            .set(key, &JsValue::Arr(vec![item]))
            .map_err(StepError::from),
        Some(_) => doc.append_to_list(key, &item).map_err(StepError::from),
    }
}

/// Wikilink targets of a frontmatter value (`[[a/b]]`, `[[a/b|alias]]`, bare strings), in order.
pub(crate) fn link_targets(v: Option<&JsValue>) -> Vec<String> {
    let one = |s: &str| {
        let s = s.trim();
        let s = s
            .strip_prefix("[[")
            .and_then(|s| s.strip_suffix("]]"))
            .unwrap_or(s);
        let s = s.split('|').next().unwrap_or(s).trim();
        s.split('#').next().unwrap_or(s).trim().to_owned()
    };
    match v {
        Some(JsValue::Str(s)) if !s.trim().is_empty() => vec![one(s)],
        Some(JsValue::Arr(a)) => a
            .iter()
            .filter_map(|x| match x {
                JsValue::Str(s) if !s.trim().is_empty() => Some(one(s)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
pub(crate) use overrides::{Overrides, test_overrides};

#[cfg(test)]
mod overrides {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::SecretSource;

    /// Test-only injections for steps run on this thread.
    #[derive(Default, Clone)]
    pub(crate) struct Overrides {
        /// Service → API base URL.
        pub bases: HashMap<String, String>,
        /// Secret source replacing the environment.
        pub secrets: Option<Arc<dyn SecretSource>>,
    }

    thread_local! {
        static CUR: RefCell<Option<Overrides>> = const { RefCell::new(None) };
    }

    pub(super) fn with<T>(f: impl FnOnce(&Overrides) -> T) -> Option<T> {
        CUR.with(|c| c.borrow().as_ref().map(f))
    }

    /// Installs `o` for this thread until the guard drops.
    pub(crate) fn test_overrides(o: Overrides) -> Guard {
        CUR.with(|c| *c.borrow_mut() = Some(o));
        Guard
    }

    /// Clears the overrides on drop.
    pub(crate) struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            CUR.with(|c| *c.borrow_mut() = None);
        }
    }
}
