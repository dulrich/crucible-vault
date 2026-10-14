//! Environment → [`Config`] resolution (plan `v0-core-daemon` → Public Interfaces "Env vars").
//!
//! Every lookup goes through a caller-supplied closure so tests never mutate the process env.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cv_protocol::Mode;

/// Default clipboard command (plan decision "wl-paste"); split on whitespace, never a shell.
pub const DEFAULT_CLIPBOARD_CMD: &str = "wl-paste --no-newline";

/// Resolved daemon/CLI configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `CV_VAULT`: the vault root (default `CV_PROTECTED_VAULT`).
    pub vault: PathBuf,
    /// `CV_MODE`: `shadow` (default) or `live`.
    pub mode: Mode,
    /// `CV_PROTECTED_VAULT` (default [`cv_vault::DEFAULT_PROTECTED_ROOT`]).
    pub protected_vault: PathBuf,
    /// `CV_STATE_DB`.
    pub state_db: PathBuf,
    /// `CV_INDEX_DB`: a base path that never exists as a file (cv-index generations).
    pub index_db: PathBuf,
    /// `CV_SCRATCH`: the shadow overlay root.
    pub scratch: PathBuf,
    /// `CV_CONFIG_DIR`.
    pub config_dir: PathBuf,
    /// `CV_SOCKET`.
    pub socket: PathBuf,
    /// `CV_OPEN_CMD`: optional program run with the absolute note path on an unclaimed
    /// `obsidian://open` (never through a shell).
    pub open_cmd: Option<PathBuf>,
    /// `CV_CLIPBOARD_CMD`: program + whitespace-separated args (default [`DEFAULT_CLIPBOARD_CMD`]).
    pub clipboard_cmd: Vec<String>,
}

impl Config {
    /// Resolves from the real process environment.
    pub fn from_env() -> Result<Self> {
        Self::resolve(|k| std::env::var(k).ok())
    }

    /// Resolves from `lookup` (empty values count as unset).
    pub fn resolve(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |k: &str| lookup(k).filter(|v| !v.is_empty());
        let home = || -> Result<PathBuf> {
            get("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("HOME is not set"))
        };
        let xdg = |var: &str, fallback: &str| -> Result<PathBuf> {
            match get(var) {
                Some(v) => Ok(PathBuf::from(v)),
                None => Ok(home()?.join(fallback)),
            }
        };
        let mode = match get("CV_MODE").as_deref() {
            None | Some("shadow") => Mode::Shadow,
            Some("live") => Mode::Live,
            Some(other) => bail!("CV_MODE must be `shadow` or `live`, got {other:?}"),
        };
        let protected_vault = get("CV_PROTECTED_VAULT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(cv_vault::DEFAULT_PROTECTED_ROOT));
        let vault = get("CV_VAULT")
            .map(PathBuf::from)
            .unwrap_or_else(|| protected_vault.clone());
        let state_dir = || -> Result<PathBuf> {
            Ok(xdg("XDG_STATE_HOME", ".local/state")?.join("crucible-vault"))
        };
        let state_db = match get("CV_STATE_DB") {
            Some(v) => PathBuf::from(v),
            None => state_dir()?.join("state.db"),
        };
        let index_db = match get("CV_INDEX_DB") {
            Some(v) => PathBuf::from(v),
            None => xdg("XDG_CACHE_HOME", ".cache")?
                .join("crucible-vault")
                .join("index.db"),
        };
        let scratch = match get("CV_SCRATCH") {
            Some(v) => PathBuf::from(v),
            None => state_dir()?.join("scratch"),
        };
        // D4: live mode reads `<vault>/_custom/crucible-vault/`. Shadow mode must never write
        // into the vault (I1), so it keeps `~/.config/crucible-vault` and refuses a config dir
        // inside the vault or the protected root (WP-9).
        let config_dir = match (get("CV_CONFIG_DIR"), mode) {
            (Some(v), _) => PathBuf::from(v),
            (None, Mode::Live) => vault.join("_custom").join("crucible-vault"),
            (None, Mode::Shadow) => home()?.join(".config").join("crucible-vault"),
        };
        if mode == Mode::Shadow
            && (is_within(&config_dir, &vault) || is_within(&config_dir, &protected_vault))
        {
            bail!(
                "CV_CONFIG_DIR {} is inside the vault; shadow mode needs it outside (I1)",
                config_dir.display()
            );
        }
        let socket = match (get("CV_SOCKET"), get("XDG_RUNTIME_DIR")) {
            (Some(v), _) => PathBuf::from(v),
            (None, Some(rt)) => PathBuf::from(rt).join("crucible-vault").join("daemon.sock"),
            (None, None) => bail!("neither CV_SOCKET nor XDG_RUNTIME_DIR is set"),
        };
        let clipboard_cmd: Vec<String> = get("CV_CLIPBOARD_CMD")
            .unwrap_or_else(|| DEFAULT_CLIPBOARD_CMD.to_owned())
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if clipboard_cmd.is_empty() {
            bail!("CV_CLIPBOARD_CMD is blank");
        }
        Ok(Self {
            vault,
            mode,
            protected_vault,
            state_db,
            index_db,
            scratch,
            config_dir,
            socket,
            open_cmd: get("CV_OPEN_CMD").map(PathBuf::from),
            clipboard_cmd,
        })
    }

    /// The `cv uri` spool directory: `<dir of CV_STATE_DB>/uri-spool`.
    pub fn spool_dir(&self) -> PathBuf {
        parent_dir(&self.state_db).join("uri-spool")
    }

    /// Parent directories the daemon writes in (Landlock writable set minus scratch).
    pub fn state_dir(&self) -> PathBuf {
        parent_dir(&self.state_db)
    }

    /// Directory holding the index generations.
    pub fn index_dir(&self) -> PathBuf {
        parent_dir(&self.index_db)
    }

    /// Directory holding the socket.
    pub fn socket_dir(&self) -> PathBuf {
        parent_dir(&self.socket)
    }

    /// Creates the state, index, socket (mode 0700) and config dirs.
    pub fn create_dirs(&self) -> Result<()> {
        for d in [self.state_dir(), self.index_dir(), self.config_dir.clone()] {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        let sock = self.socket_dir();
        if !sock.exists() {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&sock)
                .with_context(|| format!("creating {}", sock.display()))?;
        }
        Ok(())
    }
}

/// Whether `p` is `root` or below it — lexically, and again after resolving symlinks of the
/// deepest existing ancestors (fail closed on either).
fn is_within(p: &Path, root: &Path) -> bool {
    fn resolve(p: &Path) -> Option<PathBuf> {
        let mut tail = Vec::new();
        let mut cur = p;
        loop {
            if let Ok(c) = cur.canonicalize() {
                let mut out = c;
                for t in tail.iter().rev() {
                    out.push(t);
                }
                return Some(out);
            }
            tail.push(cur.file_name()?.to_owned());
            cur = cur.parent()?;
        }
    }
    if p.starts_with(root) {
        return true;
    }
    matches!((resolve(p), resolve(root)), (Some(a), Some(b)) if a.starts_with(&b))
}

fn parent_dir(p: &Path) -> PathBuf {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn defaults_follow_xdg_and_home() {
        let c = Config::resolve(env(&[("HOME", "/h"), ("XDG_RUNTIME_DIR", "/run/u")])).unwrap();
        assert_eq!(c.mode, Mode::Shadow);
        assert_eq!(c.vault, PathBuf::from(cv_vault::DEFAULT_PROTECTED_ROOT));
        assert_eq!(c.protected_vault, c.vault);
        assert_eq!(
            c.state_db,
            PathBuf::from("/h/.local/state/crucible-vault/state.db")
        );
        assert_eq!(
            c.index_db,
            PathBuf::from("/h/.cache/crucible-vault/index.db")
        );
        assert_eq!(
            c.scratch,
            PathBuf::from("/h/.local/state/crucible-vault/scratch")
        );
        assert_eq!(c.config_dir, PathBuf::from("/h/.config/crucible-vault"));
        assert_eq!(c.socket, PathBuf::from("/run/u/crucible-vault/daemon.sock"));
        assert_eq!(c.clipboard_cmd, vec!["wl-paste", "--no-newline"]);
        assert_eq!(
            c.spool_dir(),
            PathBuf::from("/h/.local/state/crucible-vault/uri-spool")
        );
        assert!(c.open_cmd.is_none());
    }

    #[test]
    fn xdg_overrides_and_explicit_vars() {
        let c = Config::resolve(env(&[
            ("HOME", "/h"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "/c"),
            ("CV_SOCKET", "/sock/d.sock"),
            ("CV_MODE", "live"),
            ("CV_VAULT", "/v"),
            ("CV_OPEN_CMD", "/bin/ed"),
        ]))
        .unwrap();
        assert_eq!(c.mode, Mode::Live);
        assert_eq!(c.vault, PathBuf::from("/v"));
        assert_eq!(c.state_db, PathBuf::from("/s/crucible-vault/state.db"));
        assert_eq!(c.index_db, PathBuf::from("/c/crucible-vault/index.db"));
        assert_eq!(c.socket, PathBuf::from("/sock/d.sock"));
        assert_eq!(c.open_cmd, Some(PathBuf::from("/bin/ed")));
    }

    #[test]
    fn config_dir_defaults_by_mode_and_shadow_refuses_vault() {
        let base = [("HOME", "/h"), ("CV_SOCKET", "/s"), ("CV_VAULT", "/v")];
        let live = Config::resolve(env(&[base[0], base[1], base[2], ("CV_MODE", "live")])).unwrap();
        assert_eq!(live.config_dir, PathBuf::from("/v/_custom/crucible-vault"));
        let shadow = Config::resolve(env(&base)).unwrap();
        assert_eq!(
            shadow.config_dir,
            PathBuf::from("/h/.config/crucible-vault")
        );
        let inside = Config::resolve(env(&[
            base[0],
            base[1],
            base[2],
            ("CV_CONFIG_DIR", "/v/_custom/crucible-vault"),
        ]));
        assert!(inside.unwrap_err().to_string().contains("inside the vault"));
        let protected = Config::resolve(env(&[
            base[0],
            base[1],
            base[2],
            ("CV_PROTECTED_VAULT", "/p"),
            ("CV_CONFIG_DIR", "/p/x"),
        ]));
        assert!(protected.is_err());
        // Live mode may keep config in the vault.
        assert!(
            Config::resolve(env(&[
                base[0],
                base[1],
                base[2],
                ("CV_MODE", "live"),
                ("CV_CONFIG_DIR", "/v/_custom/crucible-vault"),
            ]))
            .is_ok()
        );
    }

    #[test]
    fn bad_mode_and_missing_socket_are_errors() {
        assert!(
            Config::resolve(env(&[
                ("HOME", "/h"),
                ("CV_SOCKET", "/s"),
                ("CV_MODE", "x")
            ]))
            .is_err()
        );
        assert!(Config::resolve(env(&[("HOME", "/h")])).is_err());
    }
}
