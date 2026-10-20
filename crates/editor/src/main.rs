//! `cvault-editor [--fake <dir>]`: connects to the daemon at `$CV_SOCKET` (default
//! `$XDG_RUNTIME_DIR/crucible-vault/daemon.sock`), or with `--fake <dir>` (only when built with
//! the `fake` cargo feature) serves `<dir>` through the in-process `FakeDaemon` (an empty or
//! missing `<dir>` is seeded with a synthetic vault).

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use editor::app::App;
use editor::daemon::{Connector, DaemonClient, SocketDaemon};

const USAGE: &str = if cfg!(feature = "fake") {
    "usage: cvault-editor [--fake <dir>]"
} else {
    "usage: cvault-editor"
};

#[cfg(feature = "fake")]
fn fake(dir: &std::path::Path) -> anyhow::Result<editor::daemon::FakeDaemon> {
    let empty = std::fs::read_dir(dir).map_or(true, |mut d| d.next().is_none());
    if empty {
        cv_testkit::SyntheticVault::build(dir, &cv_testkit::VaultSpec::default())
            .context("seeding synthetic vault")?;
    }
    let data = std::env::temp_dir().join(format!("cvault-editor-fake-{}", std::process::id()));
    editor::daemon::FakeDaemon::open(dir, &data).context("starting FakeDaemon")
}

fn main() -> anyhow::Result<()> {
    // Arena cap + glibc FFI live in `cv-malloc` (plan memory-instrumentation-and-reduction WP-M1).
    cv_malloc::cap_arenas();
    gpu_defaults();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let connector: Option<Connector>;
    let daemon: Option<Arc<dyn DaemonClient>> = match args.as_slice() {
        [] => {
            let sock = editor::daemon::socket::default_socket()
                .context("no CV_SOCKET and no XDG_RUNTIME_DIR")?;
            // plan editor-data-safety WP-1: the same connect, reused by the reconnect loop.
            let c: Connector = Arc::new(move || {
                SocketDaemon::connect(&sock).map(|d| Arc::new(d) as Arc<dyn DaemonClient>)
            });
            let first = c();
            connector = Some(c);
            match first {
                Ok(d) => {
                    let h = d.hello();
                    tracing::info!(generation = h.generation, "connected to daemon");
                    Some(d)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "daemon not reachable; starting disconnected");
                    None
                }
            }
        }
        #[cfg(feature = "fake")]
        [flag, dir] if flag == "--fake" => {
            connector = None;
            Some(Arc::new(fake(std::path::Path::new(dir))?))
        }
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => return Err(anyhow::anyhow!(USAGE)),
    };
    let mut app = App::new(daemon, editor::ui_state::default_path());
    if let Some(c) = connector {
        app = app.with_connector(c);
    }
    let cell = Mutex::new(Some(app));
    let boot = move || {
        let mut app = cell
            .lock()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_else(App::disconnected);
        // plan memory-instrumentation-and-reduction WP-M2: the on-connect report.
        let first = app.send_memory_report();
        (app, first)
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

#[cfg(not(any(feature = "gpu", feature = "cpu")))]
compile_error!(
    "enable a renderer feature: `gpu` (default) or `cpu` (`--no-default-features --features cpu`)"
);

/// Plan memory-instrumentation-and-reduction WP-M5: the `gpu` build prefers Vulkan on the AMD
/// radeon ICD (measured 116.7 vs 146-152 MiB idle). iced exposes no backend/adapter setting (wgpu
/// reads `WGPU_BACKEND` and the Vulkan loader reads `VK_LOADER_DRIVERS_SELECT` itself), so these
/// are environment defaults, skipped when the user set any backend/driver variable. Before any thread.
#[cfg(all(feature = "gpu", target_os = "linux"))]
fn gpu_defaults() {
    const USER_SET: [&str; 5] = [
        "WGPU_BACKEND",
        "VK_LOADER_DRIVERS_SELECT",
        "VK_DRIVER_FILES",
        "VK_ICD_FILENAMES",
        "ICED_BACKEND",
    ];
    if USER_SET.iter().any(|k| std::env::var_os(k).is_some()) {
        return;
    }
    cv_malloc::set_env_default("WGPU_BACKEND", "vulkan");
    cv_malloc::set_env_default("VK_LOADER_DRIVERS_SELECT", "*radeon*");
}

#[cfg(not(all(feature = "gpu", target_os = "linux")))]
fn gpu_defaults() {}
