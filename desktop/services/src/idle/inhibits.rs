use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::zvariant::OwnedFd as ZbusOwnedFd;
use zbus::{Connection, interface};

use super::types::{IdleCommand, InhibitSource};

/// D-Bus interface provider for `org.freedesktop.ScreenSaver`.
pub struct ScreenSaverServer {
    next_cookie: AtomicU32,
    cmd_tx: mpsc::UnboundedSender<IdleCommand>,
    idle_seconds_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
    active_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    lock_fn: Arc<dyn Fn() + Send + Sync>,
}

impl ScreenSaverServer {
    pub fn new(
        cmd_tx: mpsc::UnboundedSender<IdleCommand>,
        idle_seconds_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
        active_fn: Arc<dyn Fn() -> bool + Send + Sync>,
        lock_fn: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            next_cookie: AtomicU32::new(1),
            cmd_tx,
            idle_seconds_fn,
            active_fn,
            lock_fn,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn get_active_reports_lock_state_instead_of_elapsed_idle_time() {
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let active = Arc::new(AtomicBool::new(false));
        let active_for_query = active.clone();
        let server = ScreenSaverServer::new(
            cmd_tx,
            Arc::new(|| 30),
            Arc::new(move || active_for_query.load(Ordering::SeqCst)),
            Arc::new(|| {}),
        );

        assert!(!server.get_active().await);
        active.store(true, Ordering::SeqCst);
        assert!(server.get_active().await);
    }

    #[tokio::test]
    async fn lock_requests_the_shared_lock_path() {
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let requested = Arc::new(AtomicBool::new(false));
        let requested_by_lock = requested.clone();
        let server = ScreenSaverServer::new(
            cmd_tx,
            Arc::new(|| 0),
            Arc::new(|| false),
            Arc::new(move || requested_by_lock.store(true, Ordering::SeqCst)),
        );

        server.lock().await;

        assert!(requested.load(Ordering::SeqCst));
    }
}

#[interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaverServer {
    /// Inhibit screen saver / idle actions.
    async fn inhibit(
        &self,
        #[zbus(header)] header: Header<'_>,
        application_name: String,
        reason_for_inhibit: String,
    ) -> u32 {
        let cookie = self.next_cookie.fetch_add(1, Ordering::SeqCst);
        let sender = header
            .sender()
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();

        let _ = self.cmd_tx.send(IdleCommand::AddInhibit {
            source: InhibitSource::ScreenSaver {
                cookie,
                app: application_name,
                reason: reason_for_inhibit,
                sender,
            },
        });

        cookie
    }

    /// UnInhibit screen saver / idle actions for a previously granted cookie.
    async fn un_inhibit(&self, #[zbus(header)] header: Header<'_>, cookie: u32) {
        let sender = header
            .sender()
            .map(|s| s.as_str().to_string())
            .unwrap_or_default();

        let _ = self.cmd_tx.send(IdleCommand::RemoveInhibit {
            source: InhibitSource::ScreenSaver {
                cookie,
                app: String::new(),
                reason: String::new(),
                sender,
            },
        });
    }

    /// Returns whether the screensaver is currently active (blanked/locked).
    async fn get_active(&self) -> bool {
        (self.active_fn)()
    }

    /// Returns the session idle time in milliseconds.
    async fn get_session_idle_time(&self) -> u32 {
        let secs = (self.idle_seconds_fn)();
        (secs.saturating_mul(1000)).min(u32::MAX as u64) as u32
    }

    /// Requests session locking through the daemon's shared lock supervisor.
    async fn lock(&self) {
        (self.lock_fn)();
    }

    /// Simulates user activity (resets idle counter).
    async fn simulate_user_activity(&self) {}
}

/// Helper that manages an in-process systemd-logind inhibit file descriptor.
pub struct LogindInhibitHolder {
    system_conn: Option<Connection>,
    held_fd: Arc<Mutex<Option<OwnedFd>>>,
}

impl Default for LogindInhibitHolder {
    fn default() -> Self {
        Self::new(None)
    }
}

impl LogindInhibitHolder {
    pub fn new(system_conn: Option<Connection>) -> Self {
        Self {
            system_conn,
            held_fd: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_active(&self) -> bool {
        self.held_fd.lock().unwrap().is_some()
    }

    pub async fn set_active(&self, active: bool) -> bool {
        if active {
            if self.is_active() {
                return true;
            }

            let conn = match self.system_conn.clone() {
                Some(c) => Some(c),
                None => Connection::system().await.ok(),
            };

            let Some(conn) = conn else {
                tracing::warn!(
                    "system D-Bus unavailable for logind inhibit; using in-process state only"
                );
                *self.held_fd.lock().unwrap() = None;
                return true;
            };

            let res: Result<ZbusOwnedFd, zbus::Error> = conn
                .call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    "Inhibit",
                    &(
                        "idle:sleep:handle-lid-switch",
                        "Shilpo",
                        "Caffeine active",
                        "block",
                    ),
                )
                .await
                .and_then(|reply| reply.body().deserialize());

            match res {
                Ok(zbus_fd) => {
                    let fd: OwnedFd = zbus_fd.into();
                    *self.held_fd.lock().unwrap() = Some(fd);
                    true
                }
                Err(err) => {
                    tracing::warn!(%err, "logind Manager Inhibit call failed; using in-process state fallback");
                    true
                }
            }
        } else {
            *self.held_fd.lock().unwrap() = None;
            false
        }
    }
}
