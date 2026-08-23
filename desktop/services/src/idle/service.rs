use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use tokio::sync::{mpsc, watch};
use zbus::fdo::DBusProxy;
use zbus::names::WellKnownName;
use zbus::{Connection, proxy};

use super::actions::{IdleActionSink, SystemIdleActionSink};
use super::backend::{
    IdleBackendEvent, IdleNotifierBackend, MockIdleNotifier, WaylandIdleNotifier,
};
use super::inhibits::ScreenSaverServer;
use super::state::IdleDomainState;
use super::types::{
    CommandTicket, DomainLifecycle, DomainPortTelemetry, IdleCommand, IdleCommandOutcome, IdlePort,
    IdleSnapshot, InhibitSource, SupervisorState, TimeSource,
};
use crate::lock_supervisor::LockSupervisor;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
pub trait LogindManager {
    #[zbus(property)]
    fn block_inhibited(&self) -> zbus::Result<String>;
}

/// Service wrapper managing the lifecycle, D-Bus interfaces, and event loop for the Idle domain.
pub struct IdleService {
    adapter: Arc<IdleDomainState>,
    _cmd_tx: mpsc::UnboundedSender<IdleCommand>,
    time_source: Arc<dyn TimeSource>,
}

impl Default for IdleService {
    fn default() -> Self {
        Self::new()
    }
}

impl IdleService {
    /// Creates and starts a production `IdleService` with real Wayland and D-Bus backends,
    /// and its own private `LockSupervisor`. Prefer [`Self::new_with_lock_supervisor`] in
    /// production so idle-triggered locks and telemetry/doctor share one supervisor
    /// instance with every other lock trigger.
    pub fn new() -> Self {
        Self::new_with_lock_supervisor(LockSupervisor::new())
    }

    /// Creates and starts a production `IdleService`, sharing `lock_supervisor` with the
    /// rest of the daemon (D-Bus `Lock`, `PrepareForSleep` watch) so telemetry reflects a
    /// single consistent view of the lock subsystem.
    pub fn new_with_lock_supervisor(lock_supervisor: Arc<LockSupervisor>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let backend: Arc<dyn IdleNotifierBackend> = match WaylandIdleNotifier::new(event_tx) {
            Ok(wayland) => Arc::new(wayland),
            Err(err) => {
                tracing::warn!(%err, "wayland idle notifier backend unavailable; using mock fallback");
                Arc::new(MockIdleNotifier::new())
            }
        };

        let action_sink: Arc<dyn IdleActionSink> =
            Arc::new(SystemIdleActionSink::new(None, lock_supervisor.clone()));
        let time_source: Arc<dyn TimeSource> = Arc::new(shilpo_domain::MonotonicTimeSource::new());

        Self::with_components_and_lock_supervisor(
            backend,
            action_sink,
            time_source,
            event_rx,
            lock_supervisor,
        )
    }

    /// Creates an offline `IdleService` for tests without live Wayland or D-Bus connections.
    pub fn new_offline(
        backend: Arc<dyn IdleNotifierBackend>,
        action_sink: Arc<dyn IdleActionSink>,
    ) -> Self {
        let (_event_tx, event_rx) = mpsc::unbounded_channel::<IdleBackendEvent>();
        let time_source: Arc<dyn TimeSource> = Arc::new(shilpo_domain::MonotonicTimeSource::new());
        Self::with_components(backend, action_sink, time_source, event_rx)
    }

    /// Creates a mock ready `IdleService` for test environments.
    pub fn new_mock() -> Self {
        Self::new_ready_for_test(
            Arc::new(crate::idle::backend::MockIdleNotifier::new()),
            Arc::new(crate::idle::actions::MockIdleActionSink::new()),
        )
    }

    /// Creates an offline ready `IdleService` for test environments.
    pub fn new_ready_for_test(
        backend: Arc<dyn IdleNotifierBackend>,
        action_sink: Arc<dyn IdleActionSink>,
    ) -> Self {
        let time_source: Arc<dyn TimeSource> = Arc::new(shilpo_domain::MonotonicTimeSource::new());
        let adapter = Arc::new(IdleDomainState::new(
            32,
            backend,
            action_sink,
            time_source.clone(),
        ));
        adapter.begin_start();
        adapter.mark_ready(time_source.now_ms());

        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        Self {
            adapter,
            _cmd_tx: cmd_tx,
            time_source,
        }
    }

    pub fn with_components(
        backend: Arc<dyn IdleNotifierBackend>,
        action_sink: Arc<dyn IdleActionSink>,
        time_source: Arc<dyn TimeSource>,
        event_rx: mpsc::UnboundedReceiver<IdleBackendEvent>,
    ) -> Self {
        Self::with_components_and_lock_supervisor(
            backend,
            action_sink,
            time_source,
            event_rx,
            LockSupervisor::new(),
        )
    }

    fn with_components_and_lock_supervisor(
        backend: Arc<dyn IdleNotifierBackend>,
        action_sink: Arc<dyn IdleActionSink>,
        time_source: Arc<dyn TimeSource>,
        event_rx: mpsc::UnboundedReceiver<IdleBackendEvent>,
        lock_supervisor: Arc<LockSupervisor>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let adapter = Arc::new(IdleDomainState::new(
            32,
            backend,
            action_sink,
            time_source.clone(),
        ));

        let service = Self {
            adapter: adapter.clone(),
            _cmd_tx: cmd_tx.clone(),
            time_source: time_source.clone(),
        };

        Self::spawn_supervisor(
            adapter,
            cmd_tx,
            cmd_rx,
            event_rx,
            time_source,
            lock_supervisor,
        );
        service
    }

    pub fn adapter(&self) -> Arc<IdleDomainState> {
        self.adapter.clone()
    }

    pub fn time_source(&self) -> Arc<dyn TimeSource> {
        self.time_source.clone()
    }

    fn spawn_supervisor(
        adapter: Arc<IdleDomainState>,
        cmd_tx: mpsc::UnboundedSender<IdleCommand>,
        mut cmd_rx: mpsc::UnboundedReceiver<IdleCommand>,
        mut event_rx: mpsc::UnboundedReceiver<IdleBackendEvent>,
        time_source: Arc<dyn TimeSource>,
        lock_supervisor: Arc<LockSupervisor>,
    ) {
        tokio::spawn(async move {
            adapter.begin_start();

            // Attempt session D-Bus registration
            let session_conn = Connection::session().await.ok();
            if let Some(ref conn) = session_conn {
                let adapter_clone = adapter.clone();
                let server = ScreenSaverServer::new(
                    cmd_tx.clone(),
                    Arc::new(move || adapter_clone.snapshot().live_idle_seconds),
                    {
                        let lock_supervisor = lock_supervisor.clone();
                        Arc::new(move || lock_supervisor.is_active())
                    },
                    {
                        let lock_supervisor = lock_supervisor.clone();
                        Arc::new(move || lock_supervisor.spawn("org.freedesktop.ScreenSaver.Lock"))
                    },
                );

                let _ = conn
                    .object_server()
                    .at("/org/freedesktop/ScreenSaver", server)
                    .await;

                let adapter_clone2 = adapter.clone();
                let server2 = ScreenSaverServer::new(
                    cmd_tx.clone(),
                    Arc::new(move || adapter_clone2.snapshot().live_idle_seconds),
                    {
                        let lock_supervisor = lock_supervisor.clone();
                        Arc::new(move || lock_supervisor.is_active())
                    },
                    {
                        let lock_supervisor = lock_supervisor.clone();
                        Arc::new(move || lock_supervisor.spawn("org.freedesktop.ScreenSaver.Lock"))
                    },
                );
                let _ = conn.object_server().at("/ScreenSaver", server2).await;

                // Request name org.freedesktop.ScreenSaver
                match conn
                    .request_name(
                        WellKnownName::from_static_str("org.freedesktop.ScreenSaver").unwrap(),
                    )
                    .await
                {
                    Ok(reply) => {
                        tracing::info!(?reply, "registered org.freedesktop.ScreenSaver");
                    }
                    Err(err) => {
                        tracing::debug!(%err, "could not acquire org.freedesktop.ScreenSaver name; continuing degraded");
                    }
                }

                // Subscribe to NameOwnerChanged to release cookies when clients drop
                let conn_clone = conn.clone();
                let cmd_tx_clone = cmd_tx.clone();
                tokio::spawn(async move {
                    if let Ok(dbus_proxy) = DBusProxy::new(&conn_clone).await
                        && let Ok(mut stream) = dbus_proxy.receive_name_owner_changed().await
                    {
                        while let Some(sig) = stream.next().await {
                            if let Ok(args) = sig.args()
                                && (args.new_owner.is_none()
                                    || args.new_owner.as_deref() == Some(""))
                            {
                                let _ = cmd_tx_clone.send(IdleCommand::ClearInhibitsForSender {
                                    sender: args.name.to_string(),
                                });
                            }
                        }
                    }
                });
            }

            // Watch system logind BlockInhibited property
            let system_conn = Connection::system().await.ok();
            if let Some(ref sys_conn) = system_conn {
                let cmd_tx_clone = cmd_tx.clone();
                let sys_conn_clone = sys_conn.clone();
                tokio::spawn(async move {
                    if let Ok(manager_proxy) = LogindManagerProxy::new(&sys_conn_clone).await {
                        let mut prop_stream = manager_proxy.receive_block_inhibited_changed().await;
                        while let Some(prop) = prop_stream.next().await {
                            if let Ok(val) = prop.get().await {
                                if val.contains("idle") {
                                    let _ = cmd_tx_clone.send(IdleCommand::AddInhibit {
                                        source: InhibitSource::LogindBlockInhibited,
                                    });
                                } else {
                                    let _ = cmd_tx_clone.send(IdleCommand::RemoveInhibit {
                                        source: InhibitSource::LogindBlockInhibited,
                                    });
                                }
                            }
                        }
                    }
                });
            }

            adapter.mark_ready(time_source.now_ms());

            let mut tick_interval = tokio::time::interval(Duration::from_millis(500));

            loop {
                tokio::select! {
                    _ = tick_interval.tick() => {
                        let now = time_source.now_ms();

                        // Mirror the dedicated Wayland thread's live availability into the
                        // ADR-0006 supervisor so a lost connection actually enters
                        // Backoff/Quarantined, and a recovered connection re-registers
                        // behaviors instead of sitting in Reconnecting forever.
                        let backend_available = adapter.backend().is_available();
                        let lifecycle = adapter.snapshot().lifecycle;
                        let supervisor_state = adapter.supervisor_state();
                        match reconcile_backend_availability(backend_available, lifecycle, supervisor_state) {
                            BackendReconcileAction::ReportFailure => {
                                adapter.report_owner_failure(
                                    "wayland idle notifier connection lost".to_string(),
                                    now,
                                );
                            }
                            BackendReconcileAction::AttemptRecovery => {
                                adapter.begin_start();
                                adapter.mark_ready(now);
                            }
                            BackendReconcileAction::None => {}
                        }

                        adapter.tick(now);
                    }
                    Some(event) = event_rx.recv() => {
                        adapter.handle_backend_event(event);
                    }
                    Some(cmd) = cmd_rx.recv() => {
                        let _ = adapter.submit_command(cmd);
                        adapter.process_pending_commands();
                    }
                }
            }
        });
    }
}

/// Decision produced by [`reconcile_backend_availability`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendReconcileAction {
    /// The backend died while the domain believed it was operational; record an ADR-0006
    /// owner failure so the supervisor enters Backoff/Quarantined.
    ReportFailure,
    /// The backend is available again and the supervisor is ready for a new attempt;
    /// re-run start-up so behaviors are re-registered and the domain returns to Ready.
    AttemptRecovery,
    /// No action needed this tick.
    None,
}

/// Pure decision function mirroring `IdleNotifierBackend::is_available()` into the domain's
/// ADR-0006 supervisor/lifecycle state. Kept side-effect-free so it can be unit tested without
/// a Tokio runtime or wall-clock timing.
fn reconcile_backend_availability(
    backend_available: bool,
    lifecycle: DomainLifecycle,
    supervisor_state: SupervisorState,
) -> BackendReconcileAction {
    if !backend_available && lifecycle == DomainLifecycle::Ready {
        BackendReconcileAction::ReportFailure
    } else if backend_available
        && matches!(
            lifecycle,
            DomainLifecycle::Unavailable
                | DomainLifecycle::Reconnecting
                | DomainLifecycle::Connecting
        )
        && matches!(supervisor_state, SupervisorState::Starting)
    {
        BackendReconcileAction::AttemptRecovery
    } else {
        BackendReconcileAction::None
    }
}

#[cfg(test)]
mod reconcile_tests {
    use super::*;

    #[test]
    fn ready_and_available_takes_no_action() {
        let action =
            reconcile_backend_availability(true, DomainLifecycle::Ready, SupervisorState::Running);
        assert_eq!(action, BackendReconcileAction::None);
    }

    #[test]
    fn backend_dying_while_ready_reports_failure() {
        let action =
            reconcile_backend_availability(false, DomainLifecycle::Ready, SupervisorState::Running);
        assert_eq!(action, BackendReconcileAction::ReportFailure);
    }

    #[test]
    fn backend_unavailable_while_already_reconnecting_takes_no_action() {
        // Already reflected as failed; avoid re-reporting every tick.
        let action = reconcile_backend_availability(
            false,
            DomainLifecycle::Reconnecting,
            SupervisorState::Backoff {
                attempt: 1,
                retry_at_ms: 1_000,
            },
        );
        assert_eq!(action, BackendReconcileAction::None);
    }

    #[test]
    fn recovered_backend_with_supervisor_starting_attempts_recovery() {
        let action = reconcile_backend_availability(
            true,
            DomainLifecycle::Reconnecting,
            SupervisorState::Starting,
        );
        assert_eq!(action, BackendReconcileAction::AttemptRecovery);
    }

    #[test]
    fn recovered_backend_without_supervisor_starting_takes_no_action() {
        // Backend flickered available before the supervisor's own backoff elapsed; wait for
        // the supervisor rather than racing it.
        let action = reconcile_backend_availability(
            true,
            DomainLifecycle::Reconnecting,
            SupervisorState::Backoff {
                attempt: 1,
                retry_at_ms: 1_000,
            },
        );
        assert_eq!(action, BackendReconcileAction::None);
    }

    #[test]
    fn unavailable_during_initial_connect_takes_no_action() {
        // Startup: backend hasn't connected yet, lifecycle is Connecting, not Ready. Must not
        // be treated as a failure of a previously-working connection.
        let action = reconcile_backend_availability(
            false,
            DomainLifecycle::Connecting,
            SupervisorState::Starting,
        );
        assert_eq!(action, BackendReconcileAction::None);
    }
}

impl IdlePort for IdleService {
    fn snapshot(&self) -> IdleSnapshot {
        self.adapter.snapshot()
    }

    fn subscribe(&self) -> watch::Receiver<IdleSnapshot> {
        self.adapter.subscribe()
    }

    fn submit_command(&self, command: IdleCommand) -> Result<CommandTicket, IdleCommandOutcome> {
        // `IdleDomainState::submit_command` only enqueues (kept split from
        // `process_pending_commands` so the ADR-0006 conformance harness can drive the two
        // steps independently). This is the production entry point external callers use
        // (config reload, the grace overlay's completion signal), so it must drain the
        // mailbox itself rather than leaving the command queued indefinitely.
        let ticket = self.adapter.submit_command(command)?;
        self.adapter.process_pending_commands();
        Ok(ticket)
    }

    fn supervisor_state(&self) -> SupervisorState {
        self.adapter.supervisor_state()
    }

    fn telemetry(&self) -> DomainPortTelemetry {
        self.adapter.telemetry()
    }

    fn reset_quarantine(&self) {
        self.adapter.reset_quarantine();
    }
}

#[cfg(test)]
mod submit_command_tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::idle::actions::MockIdleActionSink;
    use crate::idle::backend::MockIdleNotifier;
    use crate::idle::types::{IdleAction, IdleBehaviorConfig};

    /// Regression test for a production bug: `IdleDomainState::submit_command` only enqueues
    /// (by design, so the ADR-0006 conformance harness can drive enqueue/apply separately),
    /// but `IdleService`'s `IdlePort::submit_command` is the entry point every real caller
    /// (config reload, the grace overlay) uses, and previously called the adapter directly
    /// without ever draining the queue — so submitted commands were silently never applied.
    #[test]
    fn submit_command_applies_immediately_without_a_running_supervisor_task() {
        let service = IdleService::new_ready_for_test(
            Arc::new(MockIdleNotifier::new()),
            Arc::new(MockIdleActionSink::new()),
        );

        // The domain constructor registers one enabled default behavior ("lock"); replacing
        // the behavior map with two distinctly-named, enabled entries makes the post-apply
        // count unambiguous regardless of what the defaults happen to be.
        let before = service.snapshot();
        assert_eq!(before.registered_behaviors, 1);

        let mut behaviors = BTreeMap::new();
        behaviors.insert(
            "custom-one".to_string(),
            IdleBehaviorConfig {
                enabled: true,
                timeout_seconds: 120.0,
                action: IdleAction::Command {
                    command: "true".to_string(),
                },
                lock_before_suspend: false,
                resume_command: String::new(),
            },
        );
        behaviors.insert(
            "custom-two".to_string(),
            IdleBehaviorConfig {
                enabled: true,
                timeout_seconds: 240.0,
                action: IdleAction::Suspend,
                lock_before_suspend: false,
                resume_command: String::new(),
            },
        );

        let ticket = service
            .submit_command(IdleCommand::ConfigureBehaviors {
                behaviors,
                grace_seconds: 5.0,
            })
            .expect("command accepted");

        // No supervisor task is running in this test constructor (`_cmd_rx` is dropped), so
        // if `submit_command` only enqueued, this would stay `None` forever.
        assert!(
            ticket.outcome().is_some(),
            "command must resolve synchronously via submit_command, not require a background drain loop"
        );

        let after = service.snapshot();
        assert_eq!(
            after.registered_behaviors, 2,
            "configured behaviors must be registered with the backend immediately"
        );
    }
}
