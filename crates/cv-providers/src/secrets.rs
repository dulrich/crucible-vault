//! API-key resolution: env var or a file under `~/.config/crucible-vault/`, never the vault (D4).
//!
//! Replaces Crucible's Obsidian `SecretRegistry` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/providers.ts:15`,
//! `providerSecretKey`) — the Obsidian secret store is dropped. The source is injectable
//! ([`SecretSource`]) so tests never read the real environment or `~/.config`.

use std::path::{Path, PathBuf};

use crate::config::ProviderConfig;
use crate::error::{ProviderError, Result};

/// Where secrets come from. Implementations must not read from the vault.
pub trait SecretSource: Send + Sync {
    fn env_var(&self, name: &str) -> Option<String>;
    /// Reads `name` (a bare file name) from the secrets directory.
    fn secret_file(&self, name: &str) -> Option<String>;
}

/// Real environment + a secrets directory (normally [`default_secrets_dir`]).
#[derive(Debug, Clone)]
pub struct SystemSecrets {
    pub dir: PathBuf,
}

impl SecretSource for SystemSecrets {
    fn env_var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    fn secret_file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(name)).ok()
    }
}

/// `$XDG_CONFIG_HOME/crucible-vault` or `$HOME/.config/crucible-vault`.
pub fn default_secrets_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(Path::new(&x).join("crucible-vault"));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".config").join("crucible-vault"))
}

/// Resolves a provider's API key: env first, then the secrets file. `Ok(None)` when the
/// provider references neither (local servers may run without auth). A referenced but missing
/// secret is an error rather than a silent unauthenticated call.
pub fn resolve_api_key(p: &ProviderConfig, src: &dyn SecretSource) -> Result<Option<String>> {
    if let Some(name) = &p.api_key_env {
        if let Some(v) = src.env_var(name).filter(|v| !v.trim().is_empty()) {
            return Ok(Some(v.trim().to_owned()));
        }
        if p.api_key_file.is_none() {
            return Err(ProviderError::Secret(format!(
                "provider {:?}: env var {name} is unset",
                p.id
            )));
        }
    }
    if let Some(file) = &p.api_key_file {
        let bare = !file.is_empty()
            && Path::new(file)
                .file_name()
                .is_some_and(|f| f == file.as_str())
            && file != ".."
            && file != ".";
        if !bare {
            return Err(ProviderError::Secret(format!(
                "provider {:?}: api_key_file must be a bare file name",
                p.id
            )));
        }
        return src
            .secret_file(file)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .map(Some)
            .ok_or_else(|| {
                ProviderError::Secret(format!("provider {:?}: secret file {file} missing", p.id))
            });
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// In-memory secret source for tests.
    #[derive(Default)]
    pub struct MapSecrets {
        pub env: HashMap<String, String>,
        pub files: HashMap<String, String>,
    }

    impl SecretSource for MapSecrets {
        fn env_var(&self, n: &str) -> Option<String> {
            self.env.get(n).cloned()
        }
        fn secret_file(&self, n: &str) -> Option<String> {
            self.files.get(n).cloned()
        }
    }

    fn provider(env: Option<&str>, file: Option<&str>) -> ProviderConfig {
        let mut toml = "[[provider]]\nid = \"p\"\nkind = \"anthropic\"\n".to_owned();
        if let Some(e) = env {
            toml.push_str(&format!("api_key_env = \"{e}\"\n"));
        }
        if let Some(f) = file {
            toml.push_str(&format!("api_key_file = \"{f}\"\n"));
        }
        crate::config::ProvidersConfig::from_toml(&toml)
            .unwrap()
            .providers
            .remove(0)
    }

    #[test]
    fn env_wins_then_file_then_none() {
        let mut s = MapSecrets::default();
        s.files.insert("k".into(), "from-file\n".into());
        assert_eq!(
            resolve_api_key(&provider(Some("K"), Some("k")), &s).unwrap(),
            Some("from-file".into())
        );
        s.env.insert("K".into(), "from-env".into());
        assert_eq!(
            resolve_api_key(&provider(Some("K"), Some("k")), &s).unwrap(),
            Some("from-env".into())
        );
        assert_eq!(resolve_api_key(&provider(None, None), &s).unwrap(), None);
    }

    #[test]
    fn missing_or_unsafe_secret_is_an_error() {
        let s = MapSecrets::default();
        assert!(resolve_api_key(&provider(Some("K"), None), &s).is_err());
        assert!(resolve_api_key(&provider(None, Some("k")), &s).is_err());
        for bad in ["../k", "a/b", "/etc/passwd", ".."] {
            assert!(
                resolve_api_key(&provider(None, Some(bad)), &s).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn system_secrets_reads_only_the_injected_dir() {
        let dir = std::env::temp_dir().join(format!("cv-providers-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("key"), "abc").unwrap();
        let s = SystemSecrets { dir: dir.clone() };
        assert_eq!(s.secret_file("key"), Some("abc".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
