//! `cli` provider kind (Codex): one completion = one subprocess.
//!
//! Carried from Crucible `https://github.com/dulrich/obsidian-crucible/blob/master/src/providers/cli.ts:34-200`
//! (`buildCodexInvocation`, `runProcess`, `CLI_DEFAULT_TIMEOUT_SECONDS`). Dropped
//! (Obsidian-induced): bin-dir command resolution for GUI-launched Electron, prompt artifacts
//! written under the vault, and the Claude/Gemini/OpenCode variants (only Codex is in v0 scope).
//! Execution mode is fixed to `--sandbox read-only`; writable modes are deferred to the agents WP.
//!
//! Deviation: the pinned tokio features exclude `process`, and manifests are frozen, so the
//! subprocess runs on `std::process` inside `tokio::task::spawn_blocking` with a polled
//! deadline; the request still settles only when the child exits (killed on timeout).

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::{CLI_DEFAULT_TIMEOUT_SECONDS, ProviderConfig};
use crate::error::{ProviderError, Result};

/// The argv (after `command`) and stdin for one Codex call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInvocation {
    pub command: String,
    pub args: Vec<String>,
    pub stdin: String,
}

/// `buildCodexInvocation`: `exec --skip-git-repo-check --sandbox read-only -m <model> <extra> -`
/// with the prompt on stdin. `args` from config are prepended (wrapper scripts, tests).
pub fn codex_invocation(
    p: &ProviderConfig,
    model: &str,
    system: &str,
    user: &str,
) -> Result<CliInvocation> {
    let command =
        p.command.clone().filter(|c| !c.is_empty()).ok_or_else(|| {
            ProviderError::Config(format!("cli provider {:?} needs `command`", p.id))
        })?;
    let mut args = p.args.clone();
    args.extend(
        [
            "exec",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "-m",
            model,
        ]
        .map(str::to_owned),
    );
    args.extend(p.extra_args.iter().cloned());
    args.push("-".to_owned());
    let stdin = if system.is_empty() {
        user.to_owned()
    } else {
        format!("{system}\n\n{user}")
    };
    Ok(CliInvocation {
        command,
        args,
        stdin,
    })
}

fn timeout_of(p: &ProviderConfig) -> Duration {
    Duration::from_secs(
        p.timeout_seconds
            .filter(|s| *s > 0)
            .unwrap_or(CLI_DEFAULT_TIMEOUT_SECONDS),
    )
}

fn run_blocking(inv: &CliInvocation, cwd: Option<&str>, timeout: Duration) -> Result<String> {
    let mut cmd = Command::new(&inv.command);
    cmd.args(&inv.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| ProviderError::Cli(format!("spawn {}: {e}", inv.command)))?;
    let stdin_data = inv.stdin.clone();
    let mut stdin = child.stdin.take();
    let writer = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(stdin_data.as_bytes());
        }
    });
    let mut out = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_end(&mut buf);
        }
        buf
    });
    let mut err = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProviderError::Cli(format!(
                    "{} timed out after {}ms",
                    inv.command,
                    timeout.as_millis()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(ProviderError::Cli(format!("wait: {e}"))),
        }
    };
    let _ = writer.join();
    let stdout = reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if !status.success() {
        let tail: String = String::from_utf8_lossy(&stderr)
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(ProviderError::Cli(format!(
            "{} exited with {status}: {}",
            inv.command,
            tail.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
}

/// Runs one Codex completion; returns trimmed stdout.
pub async fn run_cli_completion(
    p: &ProviderConfig,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String> {
    let inv = codex_invocation(p, model, system, user)?;
    let cwd = p.cwd.clone();
    let timeout = timeout_of(p);
    tokio::task::spawn_blocking(move || run_blocking(&inv, cwd.as_deref(), timeout))
        .await
        .map_err(|e| ProviderError::Task(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProvidersConfig;

    fn cli(extra: &str) -> ProviderConfig {
        ProvidersConfig::from_toml(&format!(
            "[[provider]]\nid = \"codex\"\nkind = \"cli\"\n{extra}"
        ))
        .unwrap()
        .providers
        .remove(0)
    }

    #[test]
    fn invocation_shape() {
        let inv = codex_invocation(
            &cli("command = \"codex\"\nextra_args = [\"--x\"]\n"),
            "gpt-5",
            "SYS",
            "USER",
        )
        .unwrap();
        assert_eq!(inv.command, "codex");
        assert_eq!(
            inv.args,
            [
                "exec",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "-m",
                "gpt-5",
                "--x",
                "-"
            ]
        );
        assert_eq!(inv.stdin, "SYS\n\nUSER");
    }

    #[tokio::test]
    async fn synthetic_script_receives_argv_and_stdin() {
        // `sh -c '<script>' sh <codex args...>`: echoes argv, then stdin.
        let p = cli("command = \"/bin/sh\"\nargs = [\"-c\", 'echo \"$@\"; cat', \"sh\"]\n");
        let out = run_cli_completion(&p, "m1", "S", "U").await.unwrap();
        assert_eq!(
            out,
            "exec --skip-git-repo-check --sandbox read-only -m m1 -\nS\n\nU"
        );
    }

    #[tokio::test]
    async fn nonzero_exit_and_timeout_are_errors() {
        let p = cli("command = \"/bin/sh\"\nargs = [\"-c\", 'echo boom >&2; exit 3', \"sh\"]\n");
        let e = run_cli_completion(&p, "m", "", "u")
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("boom"), "{e}");
        let p =
            cli("command = \"/bin/sh\"\nargs = [\"-c\", 'sleep 5', \"sh\"]\ntimeout_seconds = 1\n");
        let t = Instant::now();
        let e = run_cli_completion(&p, "m", "", "u")
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("timed out after 1000ms"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(4));
    }
}
