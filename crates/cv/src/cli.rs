//! Hand-rolled CLI dispatch (no clap is pinned): `cv daemon`, `cv uri <url>`, `cv status`,
//! `cv signals …`, `cv jobs|intake|run …`, `cv --help`, `cv --version`.

use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use cv_protocol::{StatusResult, UriParams, UriResult, methods};

use crate::client::{Client, ClientError};
use crate::config::Config;
use crate::daemon::{Daemon, DaemonOptions};
use crate::{spool, uri};

/// Usage text (`{JOBS}` is replaced by [`crate::jobs::USAGE`]).
const USAGE_TEMPLATE: &str = "usage: cv <command>\n\n  cv daemon        run the core daemon\n  cv uri <url>     hand an obsidian:// URL to the daemon (spooled if it is not running)\n  cv status        print generation, queue counts, config errors, service health\n  cv signals ...   export [--out <path>] [--weak] (source-eval training JSONL)\n  cv media audit   [--vault <path>] [--state-db <path>] [--strict] [--verbose] (read-only remote-media counts)\n{JOBS}  cv --version     print the version (also -V)\n  cv --help        this text\n";

/// Usage text.
#[must_use]
pub fn usage() -> String {
    USAGE_TEMPLATE.replace("{JOBS}", crate::jobs::USAGE)
}

/// Version line printed by `cv --version`: `cv <semver> (<build id>)`.
#[must_use]
pub fn version_line() -> String {
    format!(
        "cv {} ({})\n",
        env!("CARGO_PKG_VERSION"),
        env!("FLEET_BUILD_ID")
    )
}

/// Build-id line printed by `cv --build`.
#[must_use]
pub fn build_line() -> String {
    format!("{}\n", env!("FLEET_BUILD_ID"))
}

/// Pure decision for the side-effect-free early exits: `Some(stdout text)` for help (wins) or
/// version, `None` for a normal command.
#[must_use]
pub fn early_exit(args: &[String]) -> Option<String> {
    if args.first().map(String::as_str) == Some("help")
        || args.iter().any(|a| a == "--help" || a == "-h")
    {
        Some(usage())
    } else if args.iter().any(|a| a == "--build") {
        Some(build_line())
    } else if args.iter().any(|a| a == "--version" || a == "-V") {
        Some(version_line())
    } else {
        None
    }
}

/// Runs the CLI with `args` (without argv[0]); returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    // Help / version win over every command (`cv uri --help` must not spool "--help", `cv daemon
    // --version` must not start a daemon) and are answered before any config, socket or spool is
    // touched.
    if let Some(text) = early_exit(args) {
        print!("{text}");
        return 0;
    }
    match args.first().map(String::as_str) {
        Some("daemon") if args.len() == 1 => report(cmd_daemon()),
        Some("uri") if args.len() == 2 => report(Config::from_env().and_then(|c| {
            let r = uri_command(&c, &args[1])?;
            println!("{}", serde_json::to_string(&r)?);
            Ok(())
        })),
        Some("status") if args.len() == 1 => report(Config::from_env().and_then(|c| {
            print!("{}", status_command(&c)?);
            Ok(())
        })),
        Some("signals") => crate::signals::run(&args[1..]),
        Some("media") => crate::media_audit::run(&args[1..]),
        Some("jobs") => crate::jobs::run(args),
        Some("intake") => crate::jobs::run(args),
        Some("run") => crate::jobs::run(args),
        _ => {
            eprint!("{}", usage());
            2
        }
    }
}

fn report(r: Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

fn cmd_daemon() -> Result<()> {
    let config = Config::from_env()?;
    let daemon = Daemon::start(config, DaemonOptions::default())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?;
    rt.block_on(async move {
        let running = daemon.spawn().await?;
        wait_for_signal().await?;
        tracing::info!("shutdown signal received");
        running.shutdown().await
    })
}

async fn wait_for_signal() -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    Ok(())
}

/// Reads the clipboard via `CV_CLIPBOARD_CMD` (never a shell); UTF-8 required. Any failure is an
/// explicit error — never silent empty content (plan decision "wl-paste").
pub fn read_clipboard(cmd: &[String]) -> Result<String> {
    let (prog, rest) = cmd
        .split_first()
        .ok_or_else(|| anyhow!("empty clipboard command"))?;
    let mut child = Command::new(prog)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("running clipboard command {prog:?}"))?;
    let mut out = Vec::new();
    if let Some(mut o) = child.stdout.take() {
        o.read_to_end(&mut out)?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("clipboard command {prog:?} failed: {status}");
    }
    String::from_utf8(out).map_err(|_| anyhow!("clipboard content is not UTF-8"))
}

/// `cv uri <url>`: resolves `clipboard` client-side (E1), then hands the resolved URL to the
/// daemon, or spools it durably when the daemon is unreachable.
pub fn uri_command(config: &Config, url: &str) -> Result<UriResult> {
    let resolved = if uri::needs_clipboard(url) {
        let clip = read_clipboard(&config.clipboard_cmd)?;
        uri::resolve_clipboard(url, &clip).map_err(|e| anyhow!("{e}"))?
    } else {
        url.to_owned()
    };
    let client = match Client::connect(&config.socket, "cv-uri", cv_index::SCHEMA_VERSION) {
        Ok(c) => c,
        Err(ClientError::Io(e)) => {
            tracing::info!(error = %e, "daemon unreachable; spooling");
            let f = spool::write(&config.spool_dir(), &resolved)?;
            tracing::info!(file = %f.display(), "spooled");
            return Ok(UriResult::Spooled);
        }
        Err(e) => return Err(anyhow!("{e}")),
    };
    client
        .call::<methods::Uri>(UriParams { url: resolved })
        .map_err(|e| anyhow!("{e}"))
}

fn render(s: &StatusResult, daemon: &str) -> String {
    let mut out = format!("daemon: {daemon}\ngeneration: {}\n", s.generation);
    let queue: Vec<String> = s.queue.iter().map(|(k, v)| format!("{k}={v}")).collect();
    out.push_str(&format!(
        "queue: {}\n",
        if queue.is_empty() {
            "empty".into()
        } else {
            queue.join(" ")
        }
    ));
    if s.config_errors.is_empty() {
        out.push_str("config errors: none\n");
    } else {
        out.push_str(&format!("config errors: {}\n", s.config_errors.len()));
        for e in &s.config_errors {
            out.push_str(&format!("  {e}\n"));
        }
    }
    if s.tracker_items.is_some() || s.link_records.is_some() {
        let n = |v: Option<u64>| v.map_or_else(|| "?".to_owned(), |n| n.to_string());
        out.push_str(&format!(
            "tracker items: {}\nlink records: {}\n",
            n(s.tracker_items),
            n(s.link_records)
        ));
    }
    if s.services.is_empty() {
        out.push_str("services: none recorded\n");
    } else {
        for h in &s.services {
            let mut detail = Vec::new();
            if let Some(s) = h.failure_score {
                detail.push(format!("score {s:.1}"));
            }
            if let Some(r) = h.retry_at_ms {
                let secs = (r - crate::now_ms()) / 1000;
                if secs > 0 {
                    detail.push(format!("retry in {secs}s"));
                }
            }
            if let Some(l) = &h.last_reason {
                let l: String = l.chars().take(80).collect();
                detail.push(format!("last: {l}"));
            }
            if detail.is_empty() {
                out.push_str(&format!("service {}: {}\n", h.service, h.state));
            } else {
                out.push_str(&format!(
                    "service {}: {} ({})\n",
                    h.service,
                    h.state,
                    detail.join(", ")
                ));
            }
        }
    }
    if let Some(m) = &s.memory {
        render_memory(&mut out, m);
    }
    out
}

/// MiB with one decimal, `?` when absent.
fn mib(v: Option<u64>) -> String {
    #[allow(clippy::cast_precision_loss)]
    v.map_or_else(
        || "?".to_owned(),
        |b| format!("{:.1} MiB", b as f64 / 1_048_576.0),
    )
}

/// The `cv status` memory block (plan memory-instrumentation-and-reduction WP-M1).
fn render_memory(out: &mut String, m: &cv_protocol::MemoryStatus) {
    let one = |out: &mut String, who: &str, r: &cv_protocol::MemoryReport, age: Option<u64>| {
        let rss_swap = match (r.rss, r.swap) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
        };
        let retained = r
            .malloc_system
            .zip(r.malloc_in_use)
            .map(|(s, u)| s.saturating_sub(u));
        let age = age.map_or_else(String::new, |a| format!(" ({a}s ago)"));
        out.push_str(&format!(
            "{who}: rss+swap {}, anon {}, malloc live {} / retained {}, hwm {}{age}\n",
            mib(rss_swap),
            mib(r.anon),
            mib(r.malloc_in_use),
            mib(retained),
            mib(r.hwm),
        ));
        let mut gauges: Vec<&cv_protocol::GaugeValue> = r.gauges.iter().collect();
        gauges.sort_by_key(|g| std::cmp::Reverse(g.value.unwrap_or(0)));
        let shown: Vec<String> = gauges
            .iter()
            .take(6)
            .map(|g| match (&g.value, &g.error) {
                (Some(v), _) => format!("{} {v}", g.name),
                (None, Some(e)) => {
                    format!("{} ({})", g.name, e.chars().take(40).collect::<String>())
                }
                (None, None) => format!("{} ?", g.name),
            })
            .collect();
        if !shown.is_empty() {
            out.push_str(&format!("  gauges: {}\n", shown.join(", ")));
        }
    };
    out.push_str("memory:\n");
    one(out, "  daemon", &m.daemon, None);
    match &m.editor {
        Some(e) => one(out, "  editor", &e.report, Some(e.age_secs)),
        None => out.push_str("  editor: no report\n"),
    }
}

/// `cv status`: via the daemon, else read-only from `state.db` + the index generation.
pub fn status_command(config: &Config) -> Result<String> {
    if let Ok(c) = Client::connect(&config.socket, "cv-status", cv_index::SCHEMA_VERSION) {
        let s = c.call::<methods::Status>(()).map_err(|e| anyhow!("{e}"))?;
        return Ok(render(&s, "running"));
    }
    let reader = cv_state::StateReader::open(&config.state_db);
    let generation = cv_index::current_generation(&config.index_db);
    if reader.is_err() && !matches!(generation, Ok(Some(_))) {
        bail!(
            "daemon not running and neither {} nor the index is readable",
            config.state_db.display()
        );
    }
    let mut s = StatusResult {
        generation: generation.ok().flatten().unwrap_or(0),
        queue: Default::default(),
        config_errors: Vec::new(),
        services: Vec::new(),
        tracker_items: None,
        link_records: None,
        memory: None,
    };
    if let Ok(r) = reader {
        for c in r.counts()? {
            *s.queue.entry(c.status.as_str().to_owned()).or_default() +=
                u64::try_from(c.count).unwrap_or(0);
        }
        for h in r.service_health()? {
            s.services.push(crate::jobs::service_health(h));
        }
        (s.tracker_items, s.link_records) = crate::jobs::store_counts(&r);
    }
    Ok(render(&s, "not running"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn early_exit_help_version_normal() {
        assert_eq!(early_exit(&v(&["--help"])), Some(usage()));
        assert_eq!(early_exit(&v(&["uri", "-h"])), Some(usage()));
        assert_eq!(early_exit(&v(&["help"])), Some(usage()));
        assert!(usage().contains("--version"));
        let ver = format!(
            "cv {} ({})\n",
            env!("CARGO_PKG_VERSION"),
            env!("FLEET_BUILD_ID")
        );
        assert_eq!(
            early_exit(&v(&["--build"])),
            Some(format!("{}\n", env!("FLEET_BUILD_ID")))
        );
        assert_eq!(early_exit(&v(&["--build", "--help"])), Some(usage()));
        assert_eq!(early_exit(&v(&["--version"])), Some(ver.clone()));
        assert_eq!(early_exit(&v(&["-V"])), Some(ver.clone()));
        assert_eq!(early_exit(&v(&["daemon", "--version"])), Some(ver));
        // help beats version
        assert_eq!(early_exit(&v(&["--version", "--help"])), Some(usage()));
        assert_eq!(early_exit(&v(&["status"])), None);
        assert_eq!(early_exit(&v(&[])), None);
    }

    #[test]
    fn status_fallback_reports_counts_and_breaker_detail() {
        let dir = std::env::temp_dir().join(format!("cv-wp11-status-{}", std::process::id()));
        let home = dir.to_string_lossy().into_owned();
        let sock = dir.join("none.sock").to_string_lossy().into_owned();
        let db = dir.join("state.db").to_string_lossy().into_owned();
        let config = Config::resolve(|k| match k {
            "HOME" => Some(home.clone()),
            "CV_SOCKET" => Some(sock.clone()),
            "CV_STATE_DB" => Some(db.clone()),
            _ => None,
        })
        .unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        drop(
            cv_state::StateDb::open(&config.state_db, 1, cv_state::QueueConfig::default()).unwrap(),
        );
        let out = status_command(&config).unwrap();
        assert!(out.contains("daemon: not running"), "{out}");
        assert!(out.contains("tracker items: 0\nlink records: 0\n"), "{out}");
        let mut s = StatusResult {
            generation: 0,
            queue: Default::default(),
            config_errors: vec![],
            services: vec![cv_protocol::ServiceHealth {
                service: "youtube-api".into(),
                state: cv_protocol::BreakerState::Open,
                failure_score: Some(3.0),
                retry_at_ms: None,
                last_reason: Some("transient Timeout".into()),
            }],
            tracker_items: None,
            link_records: None,
            memory: None,
        };
        assert!(
            render(&s, "running")
                .contains("service youtube-api: open (score 3.0, last: transient Timeout)")
        );
        s.services[0].failure_score = None;
        s.services[0].last_reason = None;
        assert!(render(&s, "running").contains("service youtube-api: open\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
