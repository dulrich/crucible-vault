//! Plan usability-sprint-captures-links WP-3: open an external link in the host browser.
//!
//! The command is `editor.toml` `open_url_cmd` (default `["xdg-open"]`); the URL is appended as
//! the last argv element and never passes through a shell. Only `http`, `https` and `mailto`
//! open — anything else (`file:`, `javascript:`, `obsidian:`, …) is refused. The child is
//! spawned detached (null stdio) and reaped on a short-lived thread, so the GUI thread never
//! waits and no zombie is left behind.

use std::process::{Child, Command, Stdio};

/// Schemes that open.
pub const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// The URL's scheme (lower-cased) when it is on the allowlist, else the refusal message.
pub fn check_scheme(url: &str) -> Result<String, String> {
    let url = url.trim();
    let scheme = url
        .split_once(':')
        .map(|(s, _)| s)
        .filter(|s| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
        .map(str::to_ascii_lowercase);
    match scheme {
        Some(s) if ALLOWED_SCHEMES.contains(&s.as_str()) => Ok(s),
        Some(s) => Err(format!("refusing to open {s}: URL")),
        None => Err(format!("refusing to open {url}: not a URL")),
    }
}

/// The argv to run: `cmd` then the URL as the last element. `Err` on a refused scheme or an
/// empty command.
pub fn build_argv(cmd: &[String], url: &str) -> Result<Vec<String>, String> {
    check_scheme(url)?;
    if cmd.first().is_none_or(|c| c.trim().is_empty()) {
        return Err("open_url_cmd is empty".into());
    }
    let mut argv = cmd.to_vec();
    argv.push(url.trim().to_owned());
    Ok(argv)
}

/// Spawns `argv` detached; the child is reaped on a background thread.
fn spawn_detached(argv: &[String]) -> Result<(), String> {
    let (prog, args) = argv.split_first().ok_or("open_url_cmd is empty")?;
    let mut child: Child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("open link: {prog}: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Opens `url` with `cmd` (allowlist enforced). `Err` is a status-bar message.
pub fn open(cmd: &[String], url: &str) -> Result<(), String> {
    spawn_detached(&build_argv(cmd, url)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn allowlist_accepts_web_and_mail_only() {
        for u in [
            "http://a.b",
            "HTTPS://a.b/x#frag",
            "mailto:me@x.y",
            "Https://x",
        ] {
            assert!(check_scheme(u).is_ok(), "{u}");
        }
        assert_eq!(
            check_scheme("file:///etc/passwd"),
            Err("refusing to open file: URL".into())
        );
        assert!(check_scheme("javascript:alert(1)").is_err());
        assert!(check_scheme("obsidian://open?vault=x").is_err());
        assert!(check_scheme("notes/a.md").is_err());
    }

    #[test]
    fn argv_has_url_last_and_no_shell() {
        let url = "https://x.y/a?b=1;rm -rf ~&c=$(id)";
        let argv = build_argv(&cmd(&["firefox", "--new-tab"]), url).expect("argv");
        assert_eq!(argv, cmd(&["firefox", "--new-tab", url]));
        assert!(build_argv(&[], "https://x").is_err());
        assert!(build_argv(&cmd(&[" "]), "https://x").is_err());
        assert!(build_argv(&cmd(&["xdg-open"]), "file:///x").is_err());
    }

    #[test]
    fn open_spawns_and_reports_spawn_failure() {
        // `true` exits at once; nothing is launched.
        assert!(open(&cmd(&["true"]), "https://example.com").is_ok());
        let e = open(&cmd(&["/nonexistent/cv-open-url"]), "https://example.com").unwrap_err();
        assert!(e.starts_with("open link:"), "{e}");
    }
}
