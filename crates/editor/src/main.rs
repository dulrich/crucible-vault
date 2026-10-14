//! `cvault-editor [--fake <dir>]`: connects to the daemon at `$CV_SOCKET` (default
//! `$XDG_RUNTIME_DIR/crucible-vault/daemon.sock`), or with `--fake <dir>` serves `<dir>` through
//! the in-process `FakeDaemon` (an empty or missing `<dir>` is seeded with a synthetic vault).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use editor::app::App;
use editor::daemon::{DaemonClient, FakeDaemon, SocketDaemon};

fn usage() -> anyhow::Error {
    anyhow::anyhow!("usage: cvault-editor [--fake <dir>]")
}

fn fake(dir: &Path) -> anyhow::Result<FakeDaemon> {
    let empty = std::fs::read_dir(dir).map_or(true, |mut d| d.next().is_none());
    if empty {
        cv_testkit::SyntheticVault::build(dir, &cv_testkit::VaultSpec::default())
            .context("seeding synthetic vault")?;
    }
    let data = std::env::temp_dir().join(format!("cvault-editor-fake-{}", std::process::id()));
    FakeDaemon::open(dir, &data).context("starting FakeDaemon")
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let daemon: Option<Arc<dyn DaemonClient>> = match args.as_slice() {
        [] => {
            let sock = editor::daemon::socket::default_socket()
                .context("no CV_SOCKET and no XDG_RUNTIME_DIR")?;
            match SocketDaemon::connect(&sock) {
                Ok(d) => {
                    let h = d.hello();
                    tracing::info!(mode = ?h.mode, generation = h.generation, "connected to daemon");
                    Some(Arc::new(d))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "daemon not reachable; starting disconnected");
                    None
                }
            }
        }
        [flag, dir] if flag == "--fake" => Some(Arc::new(fake(&PathBuf::from(dir))?)),
        [flag] if flag == "-h" || flag == "--help" => {
            println!("usage: cvault-editor [--fake <dir>]");
            return Ok(());
        }
        _ => return Err(usage()),
    };
    let app = App::new(daemon, editor::ui_state::default_path());
    let cell = Mutex::new(Some(app));
    let boot = move || {
        cell.lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_else(App::disconnected)
    };
    let res = iced::application(boot, App::update, App::view)
        .title("crucible-vault")
        .subscription(App::subscription)
        .theme(App::theme)
        .settings(iced::Settings {
            default_font: iced::Font::with_name(editor::theme::FONT_FAMILY),
            default_text_size: editor::theme::BASE_TEXT_SIZE.into(),
            ..iced::Settings::default()
        })
        .run();
    if let Err(e) = res {
        bail!("iced: {e}");
    }
    Ok(())
}
