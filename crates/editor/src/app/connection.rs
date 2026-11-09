//! Daemon connection: the notification handle, reconnect backoff and lifecycle, the RPC executor and the test sink seam.

use super::*;

/// The notification receiver, handed once to the iced subscription.
/// The connection epoch is part of its identity (plan editor-data-safety WP-1): a reconnect
/// bumps it, so iced drops the old subscription and runs a new one over the new receiver.
#[derive(Clone, Default)]
pub struct NotifHandle(Arc<Mutex<Option<Receiver<Notification>>>>, u64);

impl NotifHandle {
    /// Wraps a receiver (epoch 0).
    pub fn new(rx: Option<Receiver<Notification>>) -> Self {
        Self(Arc::new(Mutex::new(rx)), 0)
    }

    /// A reconnect's receiver at connection `epoch`.
    pub fn shared(rx: Arc<Mutex<Option<Receiver<Notification>>>>, epoch: u64) -> Self {
        Self(rx, epoch)
    }

    /// The connection epoch.
    pub fn epoch(&self) -> u64 {
        self.1
    }
}

impl Hash for NotifHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "daemon-notifications".hash(state);
        self.1.hash(state);
    }
}

/// Plan round-15 WP-2: failures of the system opener's reaper thread (a non-zero exit), as
/// status-bar messages. The sender is cloned into each `open_file`; the receiver is handed once
/// to the iced subscription ([`opener_stream`]).
#[derive(Clone)]
pub struct OpenerNotices {
    pub tx: std::sync::mpsc::Sender<String>,
    rx: Arc<Mutex<Option<Receiver<String>>>>,
}

impl Default for OpenerNotices {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            tx,
            rx: Arc::new(Mutex::new(Some(rx))),
        }
    }
}

impl Hash for OpenerNotices {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "opener-notices".hash(state);
    }
}

impl OpenerNotices {
    /// Tests: waits up to `timeout` for the next failure message (the subscription is not
    /// running headless, so this stands in for it).
    #[cfg(test)]
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<String> {
        let g = self.rx.lock().ok()?;
        g.as_ref()?.recv_timeout(timeout).ok()
    }
}

pub(super) fn opener_stream(
    handle: &OpenerNotices,
) -> impl iced::futures::Stream<Item = Message> + use<> {
    use iced::futures::{SinkExt, StreamExt};
    let rx = handle.rx.lock().ok().and_then(|mut g| g.take());
    iced::stream::channel(8, async move |mut out| {
        let Some(rx) = rx else { return };
        let (tx, mut arx) = iced::futures::channel::mpsc::unbounded();
        let spawned = std::thread::Builder::new()
            .name("cv-opener-bridge".into())
            .spawn(move || {
                while let Ok(m) = rx.recv() {
                    if tx.unbounded_send(m).is_err() {
                        break;
                    }
                }
            });
        if spawned.is_ok() {
            while let Some(m) = arx.next().await {
                if out
                    .send(Message::Status(StatusMsg::OpenerFailed(m)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    })
}

/// Reconnect backoff (plan editor-data-safety WP-1): 0.5 s doubling, capped at 5 s.
pub fn reconnect_delay(attempt: u32) -> std::time::Duration {
    let ms = 500u64.saturating_mul(1u64 << attempt.min(4));
    std::time::Duration::from_millis(ms.min(5_000))
}

/// SR13: how [`App::rpc`] runs daemon calls.
pub enum RpcExec {
    /// Production: the call runs on the blocking pool; its completion arrives as a `Task`.
    Pool,
    /// Tests: the call runs synchronously and its completion `Message` is queued, never fed
    /// back re-entrantly; tests drain the queue with `App::pump`.
    Queue(std::collections::VecDeque<Message>),
}

/// SR13: observation points tests assert on (closed tabs, clipboard copies). Production
/// installs [`NoopSink`]; tests a recording sink.
pub trait TestSink: Send + Sync {
    /// A tab showing `path` was removed.
    fn closed(&self, _path: &str) {}
    /// Text was written to the clipboard.
    fn copied(&self, _text: &str) {}
    /// A vault image was copied to the clipboard.
    fn copied_image(&self, _path: &str) {}
    /// Plan round-14 WP-3: the system opener was asked to open this absolute path.
    fn opened_file(&self, _abs: &str) {}
    /// Downcast hook for test accessors.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// The production sink: records nothing.
pub struct NoopSink;

impl TestSink for NoopSink {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Tests: records every sink event, in order.
#[cfg(test)]
#[derive(Default)]
pub struct RecordingSink {
    pub closed: Mutex<Vec<String>>,
    pub copied: Mutex<Vec<String>>,
    pub copied_images: Mutex<Vec<String>>,
    pub opened_files: Mutex<Vec<String>>,
}

#[cfg(test)]
impl TestSink for RecordingSink {
    fn closed(&self, path: &str) {
        self.closed.lock().unwrap().push(path.to_owned());
    }
    fn copied(&self, text: &str) {
        self.copied.lock().unwrap().push(text.to_owned());
    }
    fn copied_image(&self, path: &str) {
        self.copied_images.lock().unwrap().push(path.to_owned());
    }
    fn opened_file(&self, abs: &str) {
        self.opened_files.lock().unwrap().push(abs.to_owned());
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl App {
    /// A disconnected app (used if boot cannot take the prepared one).
    pub fn disconnected() -> Self {
        Self::new(None, None)
    }

    /// plan editor-data-safety WP-1: reconnect through `connector` whenever the connection is
    /// lost (or was never made).
    #[must_use]
    pub fn with_connector(mut self, connector: crate::daemon::Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    /// The connection epoch (bumped by every successful reconnect).
    pub fn connection_epoch(&self) -> u64 {
        self.notifications.epoch()
    }

    /// Starts the reconnect loop unless it is running or there is no connector.
    pub(super) fn start_reconnect(&mut self) -> Task<Message> {
        if self.connector.is_none() || self.status.reconnecting {
            return Task::none();
        }
        self.status.reconnecting = true;
        self.reconnect_attempt = 0;
        self.schedule_reconnect()
    }

    pub(super) fn schedule_reconnect(&mut self) -> Task<Message> {
        if matches!(self.rpc_exec, RpcExec::Queue(_)) {
            // Tests drive attempts with `StatusMsg::ReconnectDue`.
            return Task::none();
        }
        let delay = reconnect_delay(self.reconnect_attempt);
        Task::perform(async move { tokio::time::sleep(delay).await }, |()| {
            Message::Status(StatusMsg::ReconnectDue)
        })
    }

    pub(super) fn try_reconnect(&mut self) -> Task<Message> {
        let Some(c) = self.connector.clone() else {
            return Task::none();
        };
        if !self.status.reconnecting {
            return Task::none();
        }
        if let RpcExec::Queue(q) = &mut self.rpc_exec {
            let r = crate::daemon::connect(&c).map_err(|e| e.to_string());
            q.push_back(Message::Status(StatusMsg::Connected(r)));
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    crate::daemon::connect(&c).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            |r| Message::Status(StatusMsg::Connected(r)),
        )
    }

    /// Installs a fresh connection: new client, epoch bump (restarts the notification
    /// subscription), daemon state refetch, view/panel invalidation and open-document resync.
    /// Tasks still holding the old client fail with `Disconnected` and are not resurrected.
    pub(super) fn install_connection(&mut self, conn: crate::daemon::Connection) -> Task<Message> {
        let h = conn.client.hello();
        tracing::info!(generation = h.generation, "reconnected to daemon");
        self.status.generation = h.generation;
        self.status.connected = true;
        self.status.reconnecting = false;
        self.status.notice = None;
        // Plan round-17 WP-2: the subscribe snapshot re-fills the holds.
        let released = self.clear_holds();
        self.status
            .record(Severity::Info, "reconnected to daemon", None);
        self.reconnect_attempt = 0;
        let epoch = self.notifications.epoch() + 1;
        self.notifications = NotifHandle::shared(conn.rx, epoch);
        self.daemon = Some(conn.client);
        self.refresh_files();
        self.panels.invalidate();
        self.views.invalidate(&[]);
        self.palette.invalidate();
        self.palette.loading = false;
        let fetch = self.fetch_actions();
        let status = self.refresh_status_if_open();
        let paths: Vec<String> = self.docs.keys().cloned().collect();
        let saves: Vec<Task<Message>> = paths.iter().map(|p| self.resync_doc(p)).collect();
        let memory = self.send_memory_report();
        Task::batch([released, fetch, status, memory].into_iter().chain(saves))
    }

    /// Sends a `memory_report` to the daemon (plan memory-instrumentation-and-reduction WP-M2):
    /// the app-owned counts are captured here, the snapshot and the call run on the blocking
    /// pool, and every error (an old daemon's method-not-found included) is dropped with a
    /// debug line — never a status message.
    pub fn send_memory_report(&mut self) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        if !self.status.connected || !matches!(self.rpc_exec, RpcExec::Pool) {
            return Task::none();
        }
        let memory = Arc::clone(&self.memory);
        memory.capture(self);
        Task::future(async move {
            let sent = tokio::task::spawn_blocking(move || d.memory_report(&memory.report())).await;
            match sent {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::debug!(error = %e, "memory_report dropped"),
                Err(e) => tracing::debug!(error = %e, "memory_report task"),
            }
        })
        .discard()
    }

    /// Plan memory-instrumentation-and-reduction WP-M4: trims the glibc heap off the UI thread,
    /// only after a quiet period (no input, no finished decode), above the retained threshold,
    /// and at most once per quiet period.
    pub fn maybe_trim(&mut self) -> Task<Message> {
        let now = std::time::Instant::now();
        let retained = cv_malloc::info().retained();
        if !crate::memory::should_trim(
            now,
            crate::memory::last_activity(),
            self.last_trim,
            retained,
        ) {
            return Task::none();
        }
        self.last_trim = Some(now);
        Task::future(async move {
            match tokio::task::spawn_blocking(cv_malloc::trim).await {
                Ok(released) => tracing::debug!(
                    target: "cvault_editor::memory",
                    retained_bytes = retained,
                    released,
                    "quiet-period malloc_trim"
                ),
                Err(e) => tracing::debug!(target: "cvault_editor::memory", error = %e, "trim task"),
            }
        })
        .discard()
    }

    /// Reconnect resync of one open document against the disk (audit E1: a document with a
    /// save in flight on the old connection waits for that save's completion).
    pub(super) fn resync_doc(&mut self, path: &str) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let Some(doc) = buf_mut(&mut self.docs, path) else {
            return Task::none();
        };
        if doc.is_saving() {
            doc.defer_resync();
            return Task::none();
        }
        // Plan note-path-continuity D3: revalidate (path, cursor) before reading; a note that
        // moved while disconnected is followed and resynced at its new path.
        let path = &match self.resolve_open(path) {
            super::files::Resolved::Moved(to, head) => {
                self.follow_resolved(path, &to, head);
                to
            }
            super::files::Resolved::Live | super::files::Resolved::Gone => path.to_owned(),
        };
        let Some(doc) = buf_mut(&mut self.docs, path) else {
            return Task::none();
        };
        match d.read_file(path) {
            Ok((bytes, hash)) => {
                let job = doc.resync(&bytes, hash);
                crate::source::spawn_save(Some(&d), job)
            }
            // plan editor-data-safety WP-2: a vanished file under edits → missing-file state.
            Err(DaemonError::NotFound(_)) => {
                doc.mark_missing();
                Task::none()
            }
            Err(e) => {
                tracing::warn!(error = %e, "reconnect resync read failed");
                self.status.notify_from(
                    Severity::Warning,
                    format!("re-reading after reconnect failed: {e}"),
                    path.to_owned(),
                );
                Task::none()
            }
        }
    }

    /// Runs `call` against the daemon off the UI thread and feeds `done` its result.
    pub(super) fn rpc<X: Send + 'static>(
        &mut self,
        call: impl FnOnce(&dyn DaemonClient) -> Result<X, String> + Send + 'static,
        done: impl FnOnce(Result<X, String>) -> Message + Send + 'static,
    ) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            self.status.set_error("not connected".into());
            return Task::none();
        };
        if let RpcExec::Queue(q) = &mut self.rpc_exec {
            q.push_back(done(call(d.as_ref())));
            return Task::none();
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || call(d.as_ref()))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            done,
        )
    }
}

pub(super) fn notification_stream(
    handle: &NotifHandle,
) -> impl iced::futures::Stream<Item = Message> + use<> {
    use iced::futures::{SinkExt, StreamExt};
    let rx = handle.0.lock().ok().and_then(|mut g| g.take());
    iced::stream::channel(64, async move |mut out| {
        let Some(rx) = rx else {
            // No connection (boot without a daemon, or a failed subscribe): start the
            // reconnect loop (plan editor-data-safety WP-1).
            let _ = out.send(Message::Status(StatusMsg::Disconnected)).await;
            return;
        };
        // Bridge the blocking std receiver onto an async channel from a dedicated thread.
        let (tx, mut arx) = iced::futures::channel::mpsc::unbounded();
        let spawned = std::thread::Builder::new()
            .name("cv-notify-bridge".into())
            .spawn(move || {
                while let Ok(n) = rx.recv() {
                    if tx.unbounded_send(n).is_err() {
                        break;
                    }
                }
            });
        if spawned.is_ok() {
            while let Some(n) = arx.next().await {
                if out
                    .send(Message::Status(StatusMsg::Notification(n)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        let _ = out.send(Message::Status(StatusMsg::Disconnected)).await;
    })
}
