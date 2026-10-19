//! `cv search import --from <companion.sqlite> [--to <search.db>]` (plan semsearch-crate WP-3):
//! the one-shot import of the retired Node companion's chunks and embeddings into the daemon's
//! `semsearch` index (`CV_SEARCH_DB`, or `--to`). Body in [`cv_search::import`]; structure and
//! the daemon-socket refusal follow the retired `cv import link-records`.
//! Prints aggregate counts only.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cv_search::ImportCounts;
use cv_search::semsearch::{Index, IndexOptions};

use crate::config::Config;

/// `cv search` usage.
pub const USAGE: &str = "usage:\n  cv search import --from <companion.sqlite> [--to <search.db>]   (daemon stopped; default --to is CV_SEARCH_DB)\n  cv search import --status [--to <search.db>]   exit 0 = import state present, 1 = absent\n";

/// Runs `cv search <args>`; returns the exit code (0 ok, 1 error, 2 usage). `import --status`
/// is the migration probe (plan semsearch-crate WP-4): 0 = import state present, 1 = absent
/// (a missing file is absent), 2 = usage or an unreadable index.
pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("import")
        && args.get(1).map(String::as_str) == Some("--status")
    {
        return status_cmd(args.get(2..).unwrap_or_default());
    }
    let (Some("import"), Some((from, to))) = (
        args.first().map(String::as_str),
        parse(args.get(1..).unwrap_or_default()),
    ) else {
        eprint!("{USAGE}");
        return 2;
    };
    match Config::from_env().and_then(|c| import(&c, &from, to.as_deref())) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

fn status_cmd(rest: &[String]) -> i32 {
    let to = match rest {
        [] => Config::from_env().map(|c| c.search_db),
        [k, v] if k == "--to" => Ok(PathBuf::from(v)),
        _ => {
            eprint!("{USAGE}");
            return 2;
        }
    };
    match to.and_then(|p| has_import_state(&p)) {
        Ok(true) => {
            println!("import state: present");
            0
        }
        Ok(false) => {
            println!("import state: absent");
            1
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            2
        }
    }
}

/// Whether the index at `path` records a completed import, read-only (a missing file is
/// `false`, never created).
pub fn has_import_state(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let idx = Index::open_read_only(path, &IndexOptions::default())
        .with_context(|| format!("opening {} read-only", path.display()))?;
    Ok(cv_search::import_state(&idx)
        .map_err(|e| anyhow!("{e}"))?
        .is_some())
}

fn parse(rest: &[String]) -> Option<(PathBuf, Option<PathBuf>)> {
    let (mut from, mut to) = (None, None);
    let mut it = rest.iter();
    while let Some(k) = it.next() {
        let v = PathBuf::from(it.next()?);
        match k.as_str() {
            "--from" if from.is_none() => from = Some(v),
            "--to" if to.is_none() => to = Some(v),
            _ => return None,
        }
    }
    Some((from?, to))
}

/// The import: refuses while the daemon socket accepts connections (the daemon is the index's
/// only writer, D22), then imports `from` into `to` (default `CV_SEARCH_DB`).
pub fn import(config: &Config, from: &Path, to: Option<&Path>) -> Result<String> {
    if std::os::unix::net::UnixStream::connect(&config.socket).is_ok() {
        bail!(
            "something is accepting connections on {}; stop the daemon first — the search index has a single writer",
            config.socket.display()
        );
    }
    let c = import_into(from, to.unwrap_or(&config.search_db))?;
    Ok(format!(
        "sources {}, chunks {}, embedded {}, foreign {}, skipped {}, filled {}, written {}, seconds {:.1}",
        c.sources, c.chunks, c.embedded, c.foreign, c.skipped, c.filled, c.written, c.seconds
    ))
}

/// Opens (creating) the index at `to` and imports `from`.
pub fn import_into(from: &Path, to: &Path) -> Result<ImportCounts> {
    if !from.is_file() {
        bail!("--from {} is not a file", from.display());
    }
    if let Some(parent) = to.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut index = Index::open(to, &IndexOptions::default())
        .with_context(|| format!("opening {}", to.display()))?;
    cv_search::import_companion(from, &mut index).map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cv_search::{CompanionRow, write_companion_fixture};

    use super::*;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(tag: &str) -> Tmp {
        let base = if Path::new("/dev/shm").is_dir() {
            PathBuf::from("/dev/shm")
        } else {
            std::env::temp_dir()
        };
        let d = base.join(format!("cv-search-import-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }

    fn config(root: &Path) -> Config {
        let vars: HashMap<String, String> = [
            ("HOME", root.join("home")),
            ("CV_VAULT", root.join("prot").join("v")),
            ("CV_PROTECTED_VAULT", root.join("prot")),
            ("CV_STATE_DB", root.join("state/state.db")),
            ("CV_SEARCH_DB", root.join("state/search.db")),
            ("CV_INDEX_DB", root.join("cache/index.db")),
            ("CV_CONFIG_DIR", root.join("config")),
            ("CV_SOCKET", root.join("run/d.sock")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.display().to_string()))
        .collect();
        Config::resolve(move |k| vars.get(k).cloned()).unwrap()
    }

    #[test]
    fn import_refuses_while_a_daemon_socket_answers() {
        let t = tmp("refuse");
        let cfg = config(&t.0);
        assert_eq!(cfg.search_db, t.0.join("state/search.db"));
        let comp = t.0.join("companion.sqlite");
        let row = CompanionRow {
            id: "c1".into(),
            vault: "default".into(),
            path: "n.md".into(),
            content_hash: "h".into(),
            text: "synthetic words".into(),
            metadata_json: "{}".into(),
            ..CompanionRow::default()
        };
        write_companion_fixture(&comp, &[row]).unwrap();
        std::fs::create_dir_all(cfg.socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&cfg.socket).unwrap();
        let err = import(&cfg, &comp, None).unwrap_err().to_string();
        assert!(err.contains("stop the daemon first"), "{err}");
        assert!(!cfg.search_db.exists());
        drop(listener);
        std::fs::remove_file(&cfg.socket).unwrap();
        let ok = import(&cfg, &comp, None).unwrap();
        assert!(ok.starts_with("sources 1, chunks 1, embedded 0,"), "{ok}");
        assert!(ok.contains("written 1"), "{ok}");
        let again = import(&cfg, &comp, None).unwrap();
        assert!(again.contains("skipped 1, filled 0, written 0"), "{again}");
        let idx = Index::open_read_only(&cfg.search_db, &IndexOptions::default()).unwrap();
        let st = cv_search::import_state(&idx).unwrap().unwrap();
        assert_eq!((st.sources, st.chunks, st.embedded), (1, 1, 0));
    }

    #[test]
    fn status_probe_reports_import_state_read_only() {
        let t = tmp("status");
        let db = t.0.join("probe/search.db");
        let arg = |p: &Path| {
            vec![
                "import".into(),
                "--status".into(),
                "--to".into(),
                p.display().to_string(),
            ]
        };
        assert_eq!(run(&arg(&db)), 1, "missing file = absent");
        assert!(!db.exists(), "the probe never creates the index");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        drop(Index::open(&db, &IndexOptions::default()).unwrap());
        assert_eq!(
            run(&arg(&db)),
            1,
            "lexical-only index without import state = absent"
        );
        let comp = t.0.join("probe-companion.sqlite");
        let row = CompanionRow {
            id: "c1".into(),
            vault: "default".into(),
            path: "n.md".into(),
            content_hash: "h".into(),
            text: "synthetic words".into(),
            metadata_json: "{}".into(),
            ..CompanionRow::default()
        };
        write_companion_fixture(&comp, &[row]).unwrap();
        import_into(&comp, &db).unwrap();
        assert_eq!(run(&arg(&db)), 0);
        assert_eq!(run(&["import".into(), "--status".into(), "--x".into()]), 2);
    }

    #[test]
    fn usage_is_rejected_without_from() {
        assert_eq!(run(&["import".into()]), 2);
        assert_eq!(run(&["import".into(), "--to".into(), "x".into()]), 2);
        assert_eq!(run(&["nope".into()]), 2);
    }
}
