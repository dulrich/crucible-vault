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

/// Caps glibc malloc arenas (plan editor-cutover-blockers WP-2): off-thread image decodes each
/// grow their own arena and glibc keeps the freed pages, measured at 100–130 MiB of RSS.
/// Equivalent to `MALLOC_ARENA_MAX=2 MALLOC_TRIM_THRESHOLD_=131072`. Linux/glibc only.
#[allow(unsafe_code)]
fn cap_malloc_arenas() {
    // SAFETY: `mallopt` only sets allocator tunables and is called first thing in `main`,
    // before any other thread exists, so no allocation races the change; both parameters are
    // valid glibc options with in-range values. A zero return (rejected) is harmless.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 131_072);
    }
}

fn main() -> anyhow::Result<()> {
    cap_malloc_arenas();
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
        // `editor.toml` `ui_scale`, live (plan editor-settings-tab WP-2).
        .scale_factor(App::scale_factor)
        // Window close is a Quit: dirty buffers flush first (editor-usability-round WP-4).
        .exit_on_close_request(false)
        .settings(iced::Settings {
            default_font: iced::Font::with_name(editor::theme::FONT_FAMILY),
            // Boot default only: the source / Reading text sizes follow `editor.toml`
            // `font_size` from App state.
            default_text_size: editor::theme::BASE_TEXT_SIZE.into(),
            ..iced::Settings::default()
        })
        // After `.settings` (which replaces the font list): the vendored lucide icon font.
        .font(editor::icons::FONT_BYTES)
        .run();
    if let Err(e) = res {
        bail!("iced: {e}");
    }
    Ok(())
}
