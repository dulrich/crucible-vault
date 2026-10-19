//! Environment → [`Config`] resolution (plan `v0-core-daemon` → Public Interfaces "Env vars").
//!
//! Every lookup goes through a caller-supplied closure so tests never mutate the process env.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

/// Default clipboard command in a Wayland session (`WAYLAND_DISPLAY` set; plan decision
/// "wl-paste"); split on whitespace, never a shell.
pub const DEFAULT_CLIPBOARD_CMD: &str = "wl-paste --no-newline";

/// Default clipboard command outside Wayland (an X11 session; plan cut-over-now WP-2 found
/// the host on X11, where `wl-paste` cannot read the clipboard).
pub const DEFAULT_CLIPBOARD_CMD_X11: &str = "xclip -o -selection clipboard";

/// The vault root when `CV_VAULT` is unset (the configured vault is the live vault, D24).
pub const DEFAULT_VAULT: &str = "~/vault";

/// Resolved daemon/CLI configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `CV_VAULT`: the vault root (default [`DEFAULT_VAULT`]).
    pub vault: PathBuf,
    /// `CV_STATE_DB`.
    pub state_db: PathBuf,
    /// `CV_SEARCH_DB`: the `semsearch` index (default `search.db` beside `state.db`; plan
    /// semsearch-crate WP-3).
    pub search_db: PathBuf,
    /// `CV_INDEX_DB`: a base path that never exists as a file (cv-index generations).
    pub index_db: PathBuf,
    /// `CV_CONFIG_DIR`.
    pub config_dir: PathBuf,
    /// `CV_SOCKET`.
    pub socket: PathBuf,
    /// `CV_OPEN_CMD`: optional program run with the absolute note path on an unclaimed
    /// `obsidian://open` (never through a shell).
    pub open_cmd: Option<PathBuf>,
    /// `CV_CLIPBOARD_CMD`: program + whitespace-separated args (default [`DEFAULT_CLIPBOARD_CMD`]
    /// when `WAYLAND_DISPLAY` is set, else [`DEFAULT_CLIPBOARD_CMD_X11`]).
    pub clipboard_cmd: Vec<String>,
    /// `CV_LINK_WRITE_ROOTS`: `:`-separated absolute roots outside the vault that a live write
    /// through a symlinked note may land under (plan editor-cutover-blockers WP-1, audit B1).
    /// Unset or empty means none.
    pub link_write_roots: Vec<PathBuf>,
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
        let vault = get("CV_VAULT").map_or_else(|| PathBuf::from(DEFAULT_VAULT), PathBuf::from);
        let state_dir = || -> Result<PathBuf> {
            Ok(xdg("XDG_STATE_HOME", ".local/state")?.join("crucible-vault"))
        };
        let state_db = match get("CV_STATE_DB") {
            Some(v) => PathBuf::from(v),
            None => state_dir()?.join("state.db"),
        };
        let search_db = match get("CV_SEARCH_DB") {
            Some(v) => PathBuf::from(v),
            None => state_dir()?.join("search.db"),
        };
        let index_db = match get("CV_INDEX_DB") {
            Some(v) => PathBuf::from(v),
            None => xdg("XDG_CACHE_HOME", ".cache")?
                .join("crucible-vault")
                .join("index.db"),
        };
        // D4: the config dir is `<vault>/_custom/crucible-vault/` unless overridden.
        let config_dir = get("CV_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| vault.join("_custom").join("crucible-vault"));
        let socket = match (get("CV_SOCKET"), get("XDG_RUNTIME_DIR")) {
            (Some(v), _) => PathBuf::from(v),
            (None, Some(rt)) => PathBuf::from(rt).join("crucible-vault").join("daemon.sock"),
            (None, None) => bail!("neither CV_SOCKET nor XDG_RUNTIME_DIR is set"),
        };
        let clipboard_cmd: Vec<String> = get("CV_CLIPBOARD_CMD")
            .unwrap_or_else(|| {
                if get("WAYLAND_DISPLAY").is_some() {
                    DEFAULT_CLIPBOARD_CMD
                } else {
                    DEFAULT_CLIPBOARD_CMD_X11
                }
                .to_owned()
            })
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if clipboard_cmd.is_empty() {
            bail!("CV_CLIPBOARD_CMD is blank");
        }
        let mut link_write_roots = Vec::new();
        for part in get("CV_LINK_WRITE_ROOTS").unwrap_or_default().split(':') {
            if part.is_empty() {
                continue;
            }
            let p = PathBuf::from(part);
            if !p.is_absolute() {
                bail!("CV_LINK_WRITE_ROOTS entries must be absolute paths, got {part:?}");
            }
            link_write_roots.push(p);
        }
        Ok(Self {
            vault,
            state_db,
            search_db,
            index_db,
            config_dir,
            socket,
            open_cmd: get("CV_OPEN_CMD").map(PathBuf::from),
            clipboard_cmd,
            link_write_roots,
        })
    }

    /// The `cv uri` spool directory: `<dir of CV_STATE_DB>/uri-spool`.
    pub fn spool_dir(&self) -> PathBuf {
        parent_dir(&self.state_db).join("uri-spool")
    }

    /// Directory holding `state.db`.
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
    fn clipboard_default_follows_session_type() {
        let base = [("HOME", "/h"), ("XDG_RUNTIME_DIR", "/run/u")];
        let wayland =
            Config::resolve(env(&[base[0], base[1], ("WAYLAND_DISPLAY", "wayland-0")])).unwrap();
        assert_eq!(wayland.clipboard_cmd, vec!["wl-paste", "--no-newline"]);
        let x11 = Config::resolve(env(&[base[0], base[1], ("DISPLAY", ":0")])).unwrap();
        assert_eq!(
            x11.clipboard_cmd,
            vec!["xclip", "-o", "-selection", "clipboard"]
        );
        let explicit = Config::resolve(env(&[
            base[0],
            base[1],
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("CV_CLIPBOARD_CMD", "xsel -ob"),
        ]))
        .unwrap();
        assert_eq!(explicit.clipboard_cmd, vec!["xsel", "-ob"]);
    }

    #[test]
    fn defaults_follow_xdg_and_home() {
        let c = Config::resolve(env(&[("HOME", "/h"), ("XDG_RUNTIME_DIR", "/run/u")])).unwrap();
        assert_eq!(c.vault, PathBuf::from(DEFAULT_VAULT));
        assert_eq!(
            c.state_db,
            PathBuf::from("/h/.local/state/crucible-vault/state.db")
        );
        assert_eq!(
            c.index_db,
            PathBuf::from("/h/.cache/crucible-vault/index.db")
        );
        assert_eq!(
            c.config_dir,
            PathBuf::from(DEFAULT_VAULT).join("_custom/crucible-vault")
        );
        assert_eq!(c.socket, PathBuf::from("/run/u/crucible-vault/daemon.sock"));
        assert_eq!(
            c.clipboard_cmd,
            vec!["xclip", "-o", "-selection", "clipboard"]
        );
        assert_eq!(
            c.spool_dir(),
            PathBuf::from("/h/.local/state/crucible-vault/uri-spool")
        );
        assert!(c.open_cmd.is_none());
        assert!(c.link_write_roots.is_empty());
    }

    #[test]
    fn link_write_roots_parse_and_refuse_relative() {
        let base = [("HOME", "/h"), ("CV_SOCKET", "/s")];
        let c = Config::resolve(env(&[
            base[0],
            base[1],
            ("CV_LINK_WRITE_ROOTS", "/a/b::/c"),
        ]))
        .unwrap();
        assert_eq!(
            c.link_write_roots,
            vec![PathBuf::from("/a/b"), PathBuf::from("/c")]
        );
        let empty = Config::resolve(env(&[base[0], base[1], ("CV_LINK_WRITE_ROOTS", "")])).unwrap();
        assert!(empty.link_write_roots.is_empty());
        let rel = Config::resolve(env(&[
            base[0],
            base[1],
            ("CV_LINK_WRITE_ROOTS", "/a:rel/x"),
        ]));
        assert!(rel.unwrap_err().to_string().contains("absolute"));
    }

    #[test]
    fn xdg_overrides_and_explicit_vars() {
        let c = Config::resolve(env(&[
            ("HOME", "/h"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_CACHE_HOME", "/c"),
            ("CV_SOCKET", "/sock/d.sock"),
            ("CV_VAULT", "/v"),
            ("CV_OPEN_CMD", "/bin/ed"),
        ]))
        .unwrap();
        assert_eq!(c.vault, PathBuf::from("/v"));
        assert_eq!(c.state_db, PathBuf::from("/s/crucible-vault/state.db"));
        assert_eq!(c.search_db, PathBuf::from("/s/crucible-vault/search.db"));
        assert_eq!(c.index_db, PathBuf::from("/c/crucible-vault/index.db"));
        assert_eq!(c.socket, PathBuf::from("/sock/d.sock"));
        assert_eq!(c.open_cmd, Some(PathBuf::from("/bin/ed")));
    }

    #[test]
    fn config_dir_defaults_into_the_vault_unless_overridden() {
        let base = [("HOME", "/h"), ("CV_SOCKET", "/s"), ("CV_VAULT", "/v")];
        let c = Config::resolve(env(&base)).unwrap();
        assert_eq!(c.config_dir, PathBuf::from("/v/_custom/crucible-vault"));
        let o =
            Config::resolve(env(&[base[0], base[1], base[2], ("CV_CONFIG_DIR", "/o")])).unwrap();
        assert_eq!(o.config_dir, PathBuf::from("/o"));
    }

    #[test]
    fn missing_socket_is_an_error() {
        assert!(Config::resolve(env(&[("HOME", "/h")])).is_err());
    }
}
