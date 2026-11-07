//! `cvault-editor [--fake <dir>] [--version]`: connects to the daemon at `$CV_SOCKET` (default
//! `$XDG_RUNTIME_DIR/crucible-vault/daemon.sock`), or with `--fake <dir>` (only when built with
//! the `fake` cargo feature) serves `<dir>` through the in-process `FakeDaemon` (an empty or
//! missing `<dir>` is seeded with a synthetic vault).

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use editor::app::App;
use editor::daemon::{Connector, DaemonClient, SocketDaemon};

const USAGE: &str = if cfg!(feature = "fake") {
    "usage: cvault-editor [--fake <dir>] [--version] [--help]\n\nenvironment:\n  CV_SOCKET            daemon socket (default $XDG_RUNTIME_DIR/crucible-vault/daemon.sock)\n  XDG_RUNTIME_DIR      base for the default socket\n  RUST_LOG             tracing filter\n  WGPU_BACKEND, VK_LOADER_DRIVERS_SELECT, VK_DRIVER_FILES, VK_ICD_FILENAMES, ICED_BACKEND\n                       if any is set, the Vulkan/radeon defaults are skipped"
} else {
    "usage: cvault-editor [--version] [--help]\n\nenvironment:\n  CV_SOCKET            daemon socket (default $XDG_RUNTIME_DIR/crucible-vault/daemon.sock)\n  XDG_RUNTIME_DIR      base for the default socket\n  RUST_LOG             tracing filter\n  WGPU_BACKEND, VK_LOADER_DRIVERS_SELECT, VK_DRIVER_FILES, VK_ICD_FILENAMES, ICED_BACKEND\n                       if any is set, the Vulkan/radeon defaults are skipped"
};

/// Pure decision for the side-effect-free early exits: `Some(stdout text)` for help (wins) or
/// version, `None` for a normal start.
fn early_exit(args: &[String]) -> Option<String> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        Some(USAGE.to_owned())
    } else if args.iter().any(|a| a == "--build") {
        Some(env!("FLEET_BUILD_ID").to_owned())
    } else if args.iter().any(|a| a == "-V" || a == "--version") {
        Some(format!(
            "cvault-editor {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("FLEET_BUILD_ID")
        ))
    } else {
        None
    }
}

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
    // Help / version win over every other argument (`--fake --help` must not seed a vault in
    // `./--help`) and are answered before the arena cap, the GPU defaults or the tracing subscriber.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(text) = early_exit(&argv) {
        println!("{text}");
        return Ok(());
    }
    // Arena cap + glibc FFI live in `cv-malloc` (plan memory-instrumentation-and-reduction WP-M1).
    cv_malloc::cap_arenas();
    gpu_defaults();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = argv;
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
        // Always transparent-capable: surround changes are live, `transparent` is fixed at window
        // build. The alpha is only on `App::style`'s background (1.0 under the opaque surrounds).
        .transparent(true)
        .style(App::style)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn early_exit_help_version_normal() {
        assert_eq!(early_exit(&v(&["--help"])).as_deref(), Some(USAGE));
        assert_eq!(
            early_exit(&v(&["--fake", "--help"])).as_deref(),
            Some(USAGE)
        );
        assert!(USAGE.contains("--version") && USAGE.contains("CV_SOCKET"));
        let ver = format!(
            "cvault-editor {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("FLEET_BUILD_ID")
        );
        assert_eq!(
            early_exit(&v(&["--build"])).as_deref(),
            Some(env!("FLEET_BUILD_ID"))
        );
        assert_eq!(
            early_exit(&v(&["--build", "--help"])).as_deref(),
            Some(USAGE)
        );
        assert_eq!(early_exit(&v(&["--version"])), Some(ver.clone()));
        assert_eq!(early_exit(&v(&["-V"])), Some(ver));
        assert_eq!(
            early_exit(&v(&["--version", "--help"])).as_deref(),
            Some(USAGE)
        );
        assert_eq!(early_exit(&v(&[])), None);
        assert_eq!(early_exit(&v(&["--fake", "d"])), None);
    }
}
