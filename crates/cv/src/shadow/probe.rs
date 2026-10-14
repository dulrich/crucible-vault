//! The system probe behind P1 (daemon PSS), P6 (Landlock denials) and P7 (soak): the shadow
//! unit's systemd state and journal. Every failure is `None`, which makes the affected metric
//! `unknown` — never a guessed value. Journal lines are only ever counted, never echoed.

use std::process::{Command, Stdio};

/// The systemd user unit the soak runs as.
pub const UNIT: &str = "crucible-vault-shadow";

/// What parity reads about the running shadow daemon.
pub trait SystemProbe {
    /// The unit's `MainPID` (`None` when not running or unknown).
    fn main_pid(&self) -> Option<u32>;
    /// The unit's `NRestarts`.
    fn n_restarts(&self) -> Option<u64>;
    /// When the unit last became active (epoch ms).
    fn active_since_ms(&self) -> Option<i64>;
    /// The unit's journal lines since `since_ms` (message text only).
    fn journal_lines(&self, since_ms: i64) -> Option<Vec<String>>;
}

/// Production probe: `systemctl --user show` + `journalctl --user` (synchronous, never a shell).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemdProbe;

fn output(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl SystemdProbe {
    fn show(&self, prop: &str) -> Option<String> {
        let text = output(
            "systemctl",
            &["--user", "show", UNIT, "-p", prop, "--no-pager"],
        )?;
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{prop}=")).map(str::to_owned))
    }
}

/// Milliseconds since boot (`/proc/uptime`).
fn uptime_ms() -> Option<i64> {
    let s = std::fs::read_to_string("/proc/uptime").ok()?;
    let secs: f64 = s.split_whitespace().next()?.parse().ok()?;
    Some((secs * 1000.0) as i64)
}

impl SystemProbe for SystemdProbe {
    fn main_pid(&self) -> Option<u32> {
        self.show("MainPID")?.trim().parse().ok().filter(|&p| p > 0)
    }

    fn n_restarts(&self) -> Option<u64> {
        self.show("NRestarts")?.trim().parse().ok()
    }

    fn active_since_ms(&self) -> Option<i64> {
        let mono_us: i64 = self
            .show("ActiveEnterTimestampMonotonic")?
            .trim()
            .parse()
            .ok()
            .filter(|&v| v > 0)?;
        let boot_ms = crate::now_ms() - uptime_ms()?;
        Some(boot_ms + mono_us / 1000)
    }

    fn journal_lines(&self, since_ms: i64) -> Option<Vec<String>> {
        let since = format!("@{}", since_ms.max(0) / 1000);
        let text = output(
            "journalctl",
            &[
                "--user",
                "-u",
                UNIT,
                "-o",
                "cat",
                "--no-pager",
                "--since",
                &since,
            ],
        )?;
        Some(text.lines().map(str::to_owned).collect())
    }
}

/// Reads a `kB` field (`VmHWM`, `VmRSS`, `Pss`, …) from a `/proc` status-style file.
pub fn proc_kb(path: &str, field: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|l| {
        let rest = l.strip_prefix(field)?.strip_prefix(':')?;
        rest.split_whitespace().next()?.parse().ok()
    })
}
