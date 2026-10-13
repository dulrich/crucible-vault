//! The `cv uri` durable spool (plan WP-8: "If the daemon is unreachable, `cv uri` durably spools
//! the URL under the state dir and exits 0. The daemon drains the spool on start.").
//!
//! Files are `<unix_ms>-<pid>-<seq>.url` (zero-padded so lexical order = arrival order), written
//! via tmp file + `fsync` + rename + dir `fsync`. A permanently refused URL moves to `failed/`.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Name of the quarantine subdirectory.
pub const FAILED_DIR: &str = "failed";

/// Durably writes `url` into `dir`; returns the final path.
pub fn write(dir: &Path, url: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = format!(
        "{:015}-{:010}-{:06}.url",
        crate::now_ms(),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = dir.join(format!(".tmp-{name}"));
    let fin = dir.join(&name);
    {
        let mut f = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(url.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &fin).with_context(|| format!("renaming into {}", fin.display()))?;
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("fsync {}", dir.display()))?;
    Ok(fin)
}

/// The spooled files in `dir`, in arrival (filename) order. A missing dir is empty.
pub fn pending(dir: &Path) -> Result<Vec<PathBuf>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for ent in rd {
        let ent = ent?;
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with('.') && name.ends_with(".url") && ent.file_type()?.is_file() {
            out.push(ent.path());
        }
    }
    out.sort();
    Ok(out)
}

/// Moves a permanently refused spool file into `<dir>/failed/`.
pub fn quarantine(file: &Path) -> Result<PathBuf> {
    let dir = file.parent().unwrap_or(Path::new(".")).join(FAILED_DIR);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(file.file_name().unwrap_or_default());
    std::fs::rename(file, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_list_in_order_and_quarantine() {
        let dir = std::env::temp_dir().join(format!("cv-spool-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = write(&dir, "obsidian://new?file=a").unwrap();
        let b = write(&dir, "obsidian://new?file=b").unwrap();
        assert_eq!(pending(&dir).unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "obsidian://new?file=a"
        );
        let q = quarantine(&a).unwrap();
        assert!(q.exists());
        assert_eq!(pending(&dir).unwrap(), vec![b]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(pending(&dir).unwrap().is_empty());
    }
}
