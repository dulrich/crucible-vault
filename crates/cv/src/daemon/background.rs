//! Background work: the URI spool drain and the config poller.

use super::*;

/// Drains the URI spool in filename order (I4: intake, not replay). A file is deleted only after
/// its intake succeeded; a permanent refusal moves it to `failed/`; a transient failure leaves it
/// for the next start.
pub(super) async fn drain_spool(shared: &Shared) {
    let dir = shared.config.spool_dir();
    let files = match spool::pending(&dir) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "listing the URI spool");
            return;
        }
    };
    for f in files {
        let url = match std::fs::read_to_string(&f) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(error = %e, file = %f.display(), "reading spool file");
                continue;
            }
        };
        match shared.intake(url.trim_end()).await {
            Ok(r) => {
                tracing::info!(?r, "spooled URI ingested");
                if let Err(e) = std::fs::remove_file(&f) {
                    tracing::error!(error = %e, file = %f.display(), "removing spool file");
                }
            }
            Err(e) if e.code == error_codes::INVALID_PARAMS => {
                tracing::error!(error = %e.message, file = %f.display(), "spooled URI refused; quarantined");
                if let Err(e) = spool::quarantine(&f) {
                    tracing::error!(error = %e, "quarantining spool file");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e.message, file = %f.display(), "spooled URI intake failed; kept");
            }
        }
    }
}

/// A cheap change fingerprint of `CV_CONFIG_DIR`: (max mtime ns, entry count) over top-level
/// files and `views/`.
pub(super) fn config_fingerprint(dir: &Path) -> (u128, usize) {
    let mut max = 0u128;
    let mut count = 0usize;
    for d in [dir.to_path_buf(), dir.join("views")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            count += 1;
            if let Ok(m) = ent.metadata().and_then(|m| m.modified())
                && let Ok(t) = m.duration_since(std::time::UNIX_EPOCH)
            {
                max = max.max(t.as_nanos());
            }
        }
    }
    (max, count)
}

/// Polls `CV_CONFIG_DIR`; on change notifies view services, reloads the action config (WP-9) and
/// broadcasts `config_reloaded{errors}` with the loader's errors.
pub(super) async fn config_poller(
    shared: Arc<Shared>,
    every: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let dir = shared.config.config_dir.clone();
    let mut last = config_fingerprint(&dir);
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = tick.tick() => {
                let fp = config_fingerprint(&dir);
                if fp != last {
                    last = fp;
                    shared.lookups.invalidate_models();
                    let errors = shared.runner().map(Runner::reload_config_observed).unwrap_or_default();
                    // After the reload: the view host takes the reloaded typed settings (WP-R6).
                    let config = shared.runner().map(Runner::config).unwrap_or_default();
                    shared.views.notify_config(&dir, &config);
                    super::trash::apply_trash_config(&shared);
                    shared.publish(Notification::ConfigReloaded { errors });
                }
            }
        }
    }
}

/// Ensures nothing is listening at `socket`; removes a stale socket file.
pub(crate) fn clear_stale_socket(socket: &Path) -> Result<()> {
    if socket.symlink_metadata().is_err() {
        return Ok(());
    }
    if std::os::unix::net::UnixStream::connect(socket).is_ok() {
        bail!("daemon already running at {}", socket.display());
    }
    std::fs::remove_file(socket).with_context(|| format!("removing stale {}", socket.display()))
}
