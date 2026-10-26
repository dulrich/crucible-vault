//! `tidy` (plan `tidiness-trash-orphans` WP-2): the daily vault-tidiness step. New, not carried
//! from Crucible (Obsidian owned the trash there and nothing ever swept orphans).
//!
//! One run, in this pinned order (seeding first keeps the sweep's own just-moved files, whose
//! ledger rows may still be in the `cv-trash-ledger` channel, from being seeded twice):
//!
//! 1. **Seed.** Every regular file under the vault `.trash/` without a matching `trash_entries`
//!    row gets one (`reason = seeded`, `trashed_ms` = now), and every cv-owned system-trash entry
//!    (`.trashinfo` carrying `X-Crucible-Vault-Path`) without a row is adopted the same way.
//!    A file whose ctime is under [`CTIME_GUARD_MS`] old is skipped (a rename updates ctime, so
//!    it is probably a fresh move whose row is in flight; the next run picks it up).
//! 2. **Expire** (only when `vault.toml` `[trash] retention_days > 0`). A row older than the
//!    retention is dropped; its file is deleted permanently only when the file now at the row's
//!    location still has the row's identity (`inode` + `mtime_ms` + `bytes`, each where
//!    recorded). A missing file or a different object (restored, emptied, the name reused) drops
//!    the row and leaves whatever is there (audit E1). Vault-trash deletions run on the writer
//!    thread; nothing outside `.trash/` or the XDG `files/`/`info/` directories is ever removed.
//! 3. **Orphans.** Each `cv_index::orphaned_attachments` row is re-checked inside one writer
//!    call: the **filesystem** mtime is at least `min_age_hours` old (the index mtime can lag),
//!    a fresh index read still shows no resolved incoming link, and the content hash is the one
//!    read beforehand; then the file moves to the trash with `reason = orphan`. Any failed check
//!    skips the file and counts it.
//!
//! **Index-only boundary (audit E2).** "Unreferenced" means no resolved link in the index: only
//! `.md` notes are parsed, so a reference from a `.canvas`/`.base`/text file or a plain path gives
//! no protection. The sweep is recoverable (the trash retention).
//!
//! **Localize exclusion (audit B4).** `tidy`, `localize`, `track_mail` and `track_blogs` all hold
//! the step-intrinsic queue key [`cv_actions::ATTACHMENTS_KEY`], so a sweep never runs between a
//! localize run's attachment selection and its note commit. The editor-paste RPC
//! (`daemon/notes.rs` `write_attachment`) is not a job: on reusing an existing file it bumps the
//! file's mtime to now, so the `min_age_hours` grace period protects it.
//!
//! | key | values | default |
//! |---|---|---|
//! | `min_age_hours` | integer >= 1 (no coercion: floats, strings and `0` are refused) | `24` |
//!
//! Outcome vars (counts and bytes; logs never carry paths): `orphans_trashed`, `orphan_bytes`,
//! `orphans_skipped`, `seeded`, `adopted`, `expired`, `expired_bytes`, `rows_dropped`.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use cv_actions::{ConfigSet, StepKind};
use cv_state::NewTrashEntry;
use cv_vault::{ContentHash, TrashReason};
use serde::Serialize;

use super::params::check_keys;
use crate::steps::{StepCtx, StepError, StepModule, StepOutcome};

/// Default grace period before an orphan may be trashed.
const DEFAULT_MIN_AGE_HOURS: u64 = 24;
/// A trash file whose ctime is younger than this is not seeded yet (10 minutes).
const CTIME_GUARD_MS: i64 = 10 * 60 * 1000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

/// Typed `tidy` params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Params {
    min_age_hours: u64,
}

fn parse(t: &toml::Table) -> Result<Params, String> {
    check_keys(t, StepKind::Tidy, "tidy")?;
    let min_age_hours = match t.get("min_age_hours") {
        None => DEFAULT_MIN_AGE_HOURS,
        Some(toml::Value::Integer(i)) if *i >= 1 => {
            u64::try_from(*i).map_err(|_| "tidy step: `min_age_hours` out of range".to_owned())?
        }
        Some(_) => return Err("tidy step: `min_age_hours` must be an integer >= 1".into()),
    };
    Ok(Params { min_age_hours })
}

/// The `tidy` step.
pub(crate) struct Tidy;

impl StepModule for Tidy {
    type Params = Params;

    fn parse(_: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        parse(t)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run_in(p, ctx, system_trash_root().as_deref(), now_ms())
    }
}

/// Outcome vars.
#[derive(Debug, Default, Serialize)]
struct Vars {
    orphans_trashed: u64,
    orphan_bytes: u64,
    orphans_skipped: u64,
    seeded: u64,
    adopted: u64,
    expired: u64,
    expired_bytes: u64,
    rows_dropped: u64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `$XDG_DATA_HOME/Trash` (default `$HOME/.local/share/Trash`), the same root the daemon's writer
/// is configured with (`Config::data_home`); `None` without either variable.
fn system_trash_root() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share")))?;
    Some(home.join("Trash"))
}

/// Runs the three phases against `system_root` (the XDG `Trash` directory, if any) at `now`.
pub(crate) fn run_in(
    p: Params,
    ctx: &mut StepCtx<'_>,
    system_root: Option<&Path>,
    now: i64,
) -> Result<StepOutcome, StepError> {
    let mut v = Vars::default();
    seed(ctx, system_root, now, &mut v)?;
    let retention = ctx.config.vault().trash.retention_days;
    if retention > 0 {
        expire(ctx, system_root, now, i64::from(retention) * DAY_MS, &mut v)?;
    }
    orphans(ctx, p.min_age_hours, now, &mut v)?;
    tracing::info!(
        orphans_trashed = v.orphans_trashed,
        orphan_bytes = v.orphan_bytes,
        orphans_skipped = v.orphans_skipped,
        seeded = v.seeded,
        adopted = v.adopted,
        expired = v.expired,
        expired_bytes = v.expired_bytes,
        rows_dropped = v.rows_dropped,
        "tidy"
    );
    let mut out = StepOutcome::default();
    out.set_vars(&v);
    Ok(out)
}

// ---- identity -----------------------------------------------------------------------------------

/// Whether the object `(bytes, mtime_ms, inode)` is the one `row` recorded (each recorded field).
fn identity_matches(row: &NewTrashEntry, id: (u64, i64, u64)) -> bool {
    let (bytes, mtime_ms, inode) = id;
    i64::try_from(bytes).is_ok_and(|b| b == row.bytes)
        && row.mtime_ms.is_none_or(|m| m == mtime_ms)
        && row
            .inode
            .is_none_or(|i| i64::try_from(inode).is_ok_and(|n| n == i))
}

/// ctime of `md` is under the guard window at `now`.
fn too_fresh(md: &std::fs::Metadata, now: i64) -> bool {
    let ctime_ms = md.ctime() * 1000 + md.ctime_nsec() / 1_000_000;
    now - ctime_ms < CTIME_GUARD_MS
}

// ---- seed ---------------------------------------------------------------------------------------

/// Every regular file under `dir`, as vault-relative `/` paths prefixed by `rel`.
fn walk_files(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let Ok(ft) = ent.file_type() else { continue };
        let child = format!("{rel}/{name}");
        if ft.is_dir() {
            walk_files(&ent.path(), &child, out);
        } else if ft.is_file() {
            out.push(child);
        }
    }
}

fn seed(
    ctx: &StepCtx<'_>,
    system_root: Option<&Path>,
    now: i64,
    v: &mut Vars,
) -> Result<(), StepError> {
    let mut files = Vec::new();
    walk_files(&ctx.vault_root.join(".trash"), ".trash", &mut files);
    files.sort();
    for rel in files {
        let full = ctx.vault_root.join(&rel);
        let Ok(md) = full.symlink_metadata() else {
            continue;
        };
        if too_fresh(&md, now) {
            continue;
        }
        let Ok(id) = cv_vault::trash::identity(&full) else {
            continue;
        };
        let known = ctx.state().trash_entry_by_path("vault", &rel)?;
        if known.is_some_and(|e| identity_matches(&e.entry, id)) {
            continue;
        }
        let original = rel.strip_prefix(".trash/").unwrap_or(&rel).to_owned();
        ctx.state()
            .insert_trash_entry(&entry("vault", rel, original, now, id, None))?;
        v.seeded += 1;
    }
    let Some(root) = system_root else {
        return Ok(());
    };
    let owned = match cv_vault::cv_owned_system_entries(root) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "reading the system trash");
            return Ok(());
        }
    };
    for e in owned {
        let Ok(md) = root.join("files").join(&e.name).symlink_metadata() else {
            continue;
        };
        if too_fresh(&md, now) {
            continue;
        }
        let id = (e.bytes, e.mtime_ms, e.inode);
        let known = ctx.state().trash_entry_by_path("system", &e.name)?;
        if known.is_some_and(|k| identity_matches(&k.entry, id)) {
            continue;
        }
        let info = format!("{}.trashinfo", e.name);
        ctx.state().insert_trash_entry(&entry(
            "system",
            e.name,
            e.original_path,
            now,
            id,
            Some(info),
        ))?;
        v.adopted += 1;
    }
    Ok(())
}

fn entry(
    location: &str,
    trash_path: String,
    original_path: String,
    trashed_ms: i64,
    id: (u64, i64, u64),
    info_name: Option<String>,
) -> NewTrashEntry {
    NewTrashEntry {
        location: location.to_owned(),
        trash_path,
        original_path,
        trashed_ms,
        bytes: i64::try_from(id.0).unwrap_or(i64::MAX),
        reason: TrashReason::Seeded.as_str().to_owned(),
        mtime_ms: Some(id.1),
        inode: i64::try_from(id.2).ok(),
        info_name,
    }
}

// ---- expire -------------------------------------------------------------------------------------

/// A single path component (no separators, not `.`/`..`).
fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
}

/// A `.trash/…` vault path with only plain components.
fn vault_trash_path(s: &str) -> bool {
    s.strip_prefix(".trash/")
        .is_some_and(|rest| !rest.is_empty() && rest.split('/').all(plain_name))
}

enum Removal {
    /// The file was identical to the row and is now deleted.
    Deleted,
    /// Missing or a different object (nothing deleted).
    Gone,
    /// The delete failed; keep the row.
    Failed,
}

fn expire(
    ctx: &mut StepCtx<'_>,
    system_root: Option<&Path>,
    now: i64,
    retention_ms: i64,
    v: &mut Vars,
) -> Result<(), StepError> {
    let rows = ctx.state().trash_entries()?;
    for row in rows {
        let e = &row.entry;
        if e.trashed_ms.saturating_add(retention_ms) >= now {
            continue;
        }
        let removal = match e.location.as_str() {
            "vault" => {
                if vault_trash_path(&e.trash_path) {
                    remove_vault(ctx, e)?
                } else {
                    Removal::Gone
                }
            }
            "system" => {
                let Some(root) = system_root else { continue };
                remove_system(root, e)
            }
            _ => Removal::Gone,
        };
        match removal {
            Removal::Deleted => {
                v.expired += 1;
                v.expired_bytes += u64::try_from(e.bytes).unwrap_or(0);
            }
            Removal::Gone => v.rows_dropped += 1,
            Removal::Failed => continue,
        }
        ctx.state().delete_trash_entry(row.id)?;
    }
    Ok(())
}

fn remove_vault(ctx: &mut StepCtx<'_>, e: &NewTrashEntry) -> Result<Removal, StepError> {
    let full = ctx.vault_root.join(&e.trash_path);
    let row = e.clone();
    ctx.with_writer(&[], move |_| match cv_vault::trash::identity(&full) {
        Ok(id) if identity_matches(&row, id) => match std::fs::remove_file(&full) {
            Ok(()) => Removal::Deleted,
            Err(err) => {
                tracing::warn!(error = %err, "expiring a vault trash file");
                Removal::Failed
            }
        },
        _ => Removal::Gone,
    })
}

fn remove_system(root: &Path, e: &NewTrashEntry) -> Removal {
    if !plain_name(&e.trash_path) {
        return Removal::Gone;
    }
    let file = root.join("files").join(&e.trash_path);
    match cv_vault::trash::identity(&file) {
        Ok(id) if identity_matches(e, id) => {}
        _ => return Removal::Gone,
    }
    if let Err(err) = std::fs::remove_file(&file) {
        tracing::warn!(error = %err, "expiring a system trash file");
        return Removal::Failed;
    }
    if let Some(info) = e.info_name.as_deref().filter(|i| plain_name(i)) {
        let _ = std::fs::remove_file(root.join("info").join(info));
    }
    Removal::Deleted
}

// ---- orphans ------------------------------------------------------------------------------------

fn orphans(
    ctx: &mut StepCtx<'_>,
    min_age_hours: u64,
    now: i64,
    v: &mut Vars,
) -> Result<(), StepError> {
    let rows = {
        let reader = ctx.index()?;
        cv_index::orphaned_attachments(&reader.query())
            .map_err(|e| StepError::failed(format!("index: {e}")))?
    };
    let min_age_ms = i64::try_from(min_age_hours)
        .unwrap_or(i64::MAX)
        .saturating_mul(HOUR_MS);
    for row in rows {
        let Some((bytes, hash)) = ctx.read(&row.path)? else {
            v.orphans_skipped += 1;
            continue;
        };
        let (root, index_db, path) = (
            ctx.vault_root.to_path_buf(),
            ctx.index_db.to_path_buf(),
            row.path.clone(),
        );
        let ok = ctx.with_writer(&[], move |vw| {
            sweep_one(vw, &root, &index_db, &path, hash, min_age_ms, now)
        })?;
        if ok {
            v.orphans_trashed += 1;
            v.orphan_bytes += bytes.len() as u64;
        } else {
            v.orphans_skipped += 1;
        }
    }
    Ok(())
}

/// The writer-side re-check and move for one orphan; `true` = trashed.
pub(crate) fn sweep_one(
    vw: &mut cv_vault::VaultWriter,
    root: &Path,
    index_db: &Path,
    path: &str,
    hash: ContentHash,
    min_age_ms: i64,
    now: i64,
) -> bool {
    let Ok(md) = root.join(path).symlink_metadata() else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    let mtime_ms = md.mtime() * 1000 + md.mtime_nsec() / 1_000_000;
    if now - mtime_ms < min_age_ms {
        return false;
    }
    let unreferenced = cv_index::Reader::open_current(index_db)
        .ok()
        .and_then(|r| cv_index::referenced_elsewhere(&r.query(), path, "").ok())
        .is_some_and(|referenced| !referenced);
    if !unreferenced {
        return false;
    }
    vw.trash_with_reason(path, hash, TrashReason::Orphan)
        .is_ok()
}

#[cfg(test)]
#[path = "tidy_tests.rs"]
mod tests;
