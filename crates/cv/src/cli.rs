//! Hand-rolled CLI dispatch (no clap is pinned): `cv daemon`, `cv uri <url>`, `cv status`,
//! `cv shadow …` (WP-14), `cv cutover …`, `cv import …`, `cv --help`.

use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use cv_protocol::{Request, StatusResult, UriParams, UriResult};

use crate::client::{Client, ClientError};
use crate::config::Config;
use crate::daemon::{Daemon, DaemonOptions};
use crate::{spool, uri};

/// Usage text.
pub const USAGE: &str = "usage: cv <command>\n\n  cv daemon        run the core daemon (CV_MODE=shadow|live)\n  cv uri <url>     hand an obsidian:// URL to the daemon (spooled if it is not running)\n  cv status        print mode, generation, queue counts, config errors, service health\n  cv shadow ...    soak tooling: report | parity | replay | import-descriptions\n  cv cutover ...   status | mark --vault <path> --yes [--cancel-unfinished]\n  cv import ...    link-records --from <jobs.sqlite> (daemon stopped)\n  cv --help        this text\n";

/// Runs the CLI with `args` (without argv[0]); returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            0
        }
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
        Some("shadow") => crate::shadow::run(&args[1..]),
        Some("cutover") => crate::cutover::run(&args[1..]),
        Some("import") => crate::import::run(&args[1..]),
        _ => {
            eprint!("{USAGE}");
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
    // Landlock (shadow) is applied here, before the runtime spawns any thread.
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
    let mut client = match Client::connect(&config.socket, "cv-uri", cv_index::SCHEMA_VERSION) {
        Ok(c) => c,
        Err(ClientError::Io(e)) => {
            tracing::info!(error = %e, "daemon unreachable; spooling");
            let f = spool::write(&config.spool_dir(), &resolved)?;
            tracing::info!(file = %f.display(), "spooled");
            return Ok(UriResult::Spooled);
        }
        Err(e) => return Err(anyhow!("{e}")),
    };
    let v = client
        .call(Request::Uri(UriParams { url: resolved }))
        .map_err(|e| anyhow!("{e}"))?;
    Ok(serde_json::from_value(v)?)
}

fn render(s: &StatusResult, daemon: &str) -> String {
    let mode = serde_json::to_value(s.mode)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut out = format!(
        "daemon: {daemon}\nmode: {mode}\ngeneration: {}\n",
        s.generation
    );
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
    if s.services.is_empty() {
        out.push_str("services: none recorded\n");
    } else {
        for h in &s.services {
            out.push_str(&format!("service {}: {}\n", h.service, h.state));
        }
    }
    out
}

/// `cv status`: via the daemon, else read-only from `state.db` + the index generation.
pub fn status_command(config: &Config) -> Result<String> {
    if let Ok(mut c) = Client::connect(&config.socket, "cv-status", cv_index::SCHEMA_VERSION) {
        let v = c.call(Request::Status).map_err(|e| anyhow!("{e}"))?;
        let s: StatusResult = serde_json::from_value(v)?;
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
        mode: config.mode,
        generation: generation.ok().flatten().unwrap_or(0),
        queue: Default::default(),
        config_errors: Vec::new(),
        services: Vec::new(),
    };
    if let Ok(r) = reader {
        for c in r.counts()? {
            *s.queue.entry(c.status.as_str().to_owned()).or_default() +=
                u64::try_from(c.count).unwrap_or(0);
        }
        for h in r.service_health()? {
            s.services.push(cv_protocol::ServiceHealth {
                service: h.service,
                state: serde_json::to_value(h.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            });
        }
    }
    Ok(render(&s, "not running"))
}
