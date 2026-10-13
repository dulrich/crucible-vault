//! Pure `obsidian://` parsing and path clamping (plan WP-8 "`cv uri`").
//!
//! Behaviour is ported from the S2 URI probe (`runs/spike/core-index/FINDINGS.md` "URI probe"):
//! params `vault`, `file`, `content` | `clipboard`, `silent`; `.md` appended when absent; the
//! `file` path is normalized and clamped to the vault root; UTF-8 kept intact; an unknown vault
//! is rejected. Crucible has no source for this — Obsidian core owns the scheme (inventory #5);
//! the Obsidian-induced part (startup event replay, stale metadata cache after the create) is
//! dropped because the daemon's own writer emits `created` immediately.
//!
//! Query decoding mirrors Obsidian's `decodeURIComponent` (a `+` stays a `+`), not form decoding.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

/// Characters left unescaped when re-encoding `content` (`encodeURIComponent`'s set).
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// A parsed, validated intake request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriAction {
    /// `obsidian://new`: create `path` (collision-suffixed by the caller).
    New {
        path: String,
        content: String,
        silent: bool,
    },
    /// `obsidian://open`.
    Open { path: String },
}

/// Why a URL was refused. Every variant is permanent (never retried).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UriError(pub String);

impl std::fmt::Display for UriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UriError {}

fn err<T>(msg: impl Into<String>) -> Result<T, UriError> {
    Err(UriError(msg.into()))
}

/// The raw pieces of an `obsidian://<action>?<query>` URL.
struct Raw<'a> {
    action: &'a str,
    /// `(raw key, raw value or None for a bare flag)`.
    pairs: Vec<(&'a str, Option<&'a str>)>,
}

fn split(url: &str) -> Result<Raw<'_>, UriError> {
    let Some(rest) = url.strip_prefix("obsidian://") else {
        return err(format!("unsupported scheme in {url:?}"));
    };
    let (action, query) = match rest.find('?') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let action = action.trim_end_matches('/');
    let pairs = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|s| match s.find('=') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        })
        .collect();
    Ok(Raw { action, pairs })
}

fn decode(raw: &str) -> Result<String, UriError> {
    percent_decode_str(raw)
        .decode_utf8()
        .map(|c| c.into_owned())
        .map_err(|_| UriError(format!("parameter is not valid UTF-8: {raw:?}")))
}

fn param(raw: &Raw<'_>, key: &str) -> Result<Option<String>, UriError> {
    for (k, v) in &raw.pairs {
        if decode(k)? == key {
            return Ok(Some(decode(v.unwrap_or(""))?));
        }
    }
    Ok(None)
}

fn has(raw: &Raw<'_>, key: &str) -> bool {
    raw.pairs
        .iter()
        .any(|(k, _)| decode(k).is_ok_and(|k| k == key))
}

/// Normalizes `file` into a vault-relative path clamped to the root: `.` and empty segments
/// dropped, `..` pops (never above the root).
pub fn clamp_path(file: &str) -> Result<String, UriError> {
    let mut out: Vec<&str> = Vec::new();
    for seg in file.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return err(format!("file {file:?} names no note"));
    }
    Ok(out.join("/"))
}

fn with_md(path: String) -> String {
    if path.ends_with(".md") {
        path
    } else {
        format!("{path}.md")
    }
}

/// Whether `url` still carries an unresolved `clipboard` flag.
pub fn needs_clipboard(url: &str) -> bool {
    split(url).is_ok_and(|r| has(&r, "clipboard"))
}

/// Rewrites `url` into its resolved form (E1): `clipboard` and any `content` removed, then
/// `content=<percent-encoded clip>` appended.
pub fn resolve_clipboard(url: &str, clip: &str) -> Result<String, UriError> {
    let raw = split(url)?;
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in &raw.pairs {
        let key = decode(k)?;
        if key == "clipboard" || key == "content" {
            continue;
        }
        parts.push(match v {
            Some(v) => format!("{k}={v}"),
            None => (*k).to_owned(),
        });
    }
    parts.push(format!("content={}", utf8_percent_encode(clip, COMPONENT)));
    Ok(format!("obsidian://{}?{}", raw.action, parts.join("&")))
}

/// Parses `url` for the vault whose root directory is named `vault_name`.
pub fn parse(url: &str, vault_name: &str) -> Result<UriAction, UriError> {
    let raw = split(url)?;
    if let Some(v) = param(&raw, "vault")?
        && v != vault_name
    {
        return err(format!("unknown vault {v:?}"));
    }
    match raw.action {
        "new" => {
            if has(&raw, "clipboard") {
                return err("clipboard must be resolved client-side");
            }
            // Obsidian names an unnamed new note "Untitled".
            let file = param(&raw, "file")?.unwrap_or_else(|| "Untitled".to_owned());
            Ok(UriAction::New {
                path: with_md(clamp_path(&file)?),
                content: param(&raw, "content")?.unwrap_or_default(),
                silent: has(&raw, "silent") && param(&raw, "silent")?.is_none_or(|v| v != "false"),
            })
        }
        "open" => {
            let Some(file) = param(&raw, "file")? else {
                return err("open needs a file");
            };
            let path = clamp_path(&file)?;
            let last = path.rsplit('/').next().unwrap_or(&path);
            let path = if last.contains('.') {
                path
            } else {
                format!("{path}.md")
            };
            Ok(UriAction::Open { path })
        }
        other => err(format!("unsupported action {other:?}")),
    }
}

/// `path` with Obsidian's ` N` collision suffix (`n == 0` → `path` itself).
pub fn suffixed(path: &str, n: u32) -> String {
    if n == 0 {
        return path.to_owned();
    }
    match path.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains('/') => format!("{stem} {n}.{ext}"),
        _ => format!("{path} {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_path(url: &str) -> String {
        match parse(url, "Synth").unwrap() {
            UriAction::New { path, .. } => path,
            UriAction::Open { .. } => panic!("not new"),
        }
    }

    #[test]
    fn s2_basic_new_decodes_content() {
        let a = parse(
            "obsidian://new?vault=Synth&file=Clipped%20Article&content=%23%20Hello%0A%0Atag%20%23clip%20see%20%5B%5Bseed%5D%5D",
            "Synth",
        )
        .unwrap();
        let UriAction::New {
            path,
            content,
            silent,
        } = a
        else {
            panic!()
        };
        assert_eq!(path, "Clipped Article.md");
        assert_eq!(content, "# Hello\n\ntag #clip see [[seed]]");
        assert_eq!(content.len(), 31);
        assert!(!silent);
    }

    #[test]
    fn s2_silent_parsed_and_suffixes() {
        let UriAction::New { silent, .. } = parse(
            "obsidian://new?vault=Synth&file=Clipped%20Article&content=x&silent=true",
            "Synth",
        )
        .unwrap() else {
            panic!()
        };
        assert!(silent);
        assert_eq!(suffixed("Clipped Article.md", 1), "Clipped Article 1.md");
        assert_eq!(suffixed("Clipped Article.md", 2), "Clipped Article 2.md");
        assert_eq!(suffixed("a/b.md", 0), "a/b.md");
    }

    #[test]
    fn s2_deep_path_normalized_utf8_intact() {
        let UriAction::New { path, content, .. } = parse(
            "obsidian://new?file=%2Fnotes%2F.%2F..%2Fnotes%2FDeep%20Note&content=deep%20%C3%A9%E2%9C%85",
            "Synth",
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(path, "notes/Deep Note.md");
        assert_eq!(content, "deep é✅");
    }

    #[test]
    fn s2_escape_clamped_and_dotdot_vault_rejected() {
        assert_eq!(
            new_path("obsidian://new?file=..%2F..%2Fescape"),
            "escape.md"
        );
        assert!(parse("obsidian://new?vault=..&file=x", "Synth").is_err());
        assert!(parse("obsidian://new?vault=Other&file=x", "Synth").is_err());
        assert_eq!(new_path("obsidian://new?file=x.md"), "x.md");
        assert!(parse("obsidian://new?file=..%2F..", "Synth").is_err());
    }

    #[test]
    fn plus_is_literal_and_untitled_default() {
        let UriAction::New { content, path, .. } =
            parse("obsidian://new?content=a+b", "Synth").unwrap()
        else {
            panic!()
        };
        assert_eq!(content, "a+b");
        assert_eq!(path, "Untitled.md");
    }

    #[test]
    fn open_and_unsupported() {
        assert_eq!(
            parse("obsidian://open?vault=Synth&file=notes%2FA", "Synth").unwrap(),
            UriAction::Open {
                path: "notes/A.md".into()
            }
        );
        assert_eq!(
            parse("obsidian://open?file=img.png", "Synth").unwrap(),
            UriAction::Open {
                path: "img.png".into()
            }
        );
        assert!(parse("obsidian://search?query=x", "Synth").is_err());
        assert!(parse("https://new?file=x", "Synth").is_err());
        assert!(parse("obsidian://open", "Synth").is_err());
    }

    #[test]
    fn clipboard_resolution_round_trips() {
        let url = "obsidian://new?vault=Synth&file=Clip%20A&clipboard&silent=true";
        assert!(needs_clipboard(url));
        assert!(parse(url, "Synth").is_err());
        let resolved = resolve_clipboard(url, "body é & 100%\n+x").unwrap();
        assert!(!needs_clipboard(&resolved));
        let UriAction::New {
            path,
            content,
            silent,
        } = parse(&resolved, "Synth").unwrap()
        else {
            panic!()
        };
        assert_eq!(path, "Clip A.md");
        assert_eq!(content, "body é & 100%\n+x");
        assert!(silent);
    }
}
