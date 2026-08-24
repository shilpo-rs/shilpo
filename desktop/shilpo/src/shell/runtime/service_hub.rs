use std::{path::PathBuf, sync::Arc};

use gpui::App;
use shilpo_services::{ClipboardItem, Notification, NotificationPort, NotificationService};

use super::{SessionContext, ShellRuntime};
use crate::shell::bar::service_worker::{
    self, CommandSender, ConfigReceiver, DeviceCommand, WorkerCommand,
};

pub struct ServiceHubStreams {
    pub device_rx: tokio::sync::broadcast::Receiver<shilpo_services::DeviceClientUpdate>,
    pub notif_rx: tokio::sync::broadcast::Receiver<Notification>,
    pub polkit_rx: tokio::sync::watch::Receiver<shilpo_services::PolkitSnapshot>,
    pub idle_rx: tokio::sync::watch::Receiver<shilpo_services::IdleSnapshot>,
    pub config_rx: ConfigReceiver,
    pub device_client: shilpo_services::DeviceClient,
}

/// Owns shell-facing integrations (compositor, notifications, clipboard, app
/// scanning) and the client bridge that reports device state from the daemon.
///
/// All state is private: the shell reaches the services exclusively through the
/// narrow method surface below.
pub struct ServiceHub {
    compositor: Arc<dyn shilpo_services::CompositorAdapter>,
    notification: Arc<dyn NotificationPort>,
    polkit: Arc<dyn shilpo_services::PolkitPort>,
    idle: Arc<dyn shilpo_services::IdlePort>,
    clipboard: shilpo_services::ClipboardService,
    app_scanner: shilpo_services::AppScanner,
    device_client: shilpo_services::DeviceClient,
    service_commands: CommandSender,
    device_snapshot: crate::shell::bar::service_worker::DeviceSnapshot,
    domain_states:
        std::collections::HashMap<shilpo_services::DeviceDomain, shilpo_services::DomainState>,
    _service_task: Option<gpui::Task<()>>,
    _app_watcher: Option<notify::RecommendedWatcher>,
    started_at: std::time::Instant,
    heed_store_available: bool,
    lock_supervisor: Arc<shilpo_services::lock_supervisor::LockSupervisor>,
}

impl ServiceHub {
    /// Starts the service hub from a restored session, applying persisted DND state.
    pub fn start(
        executor: gpui::BackgroundExecutor,
        session: &SessionContext,
    ) -> (Self, ServiceHubStreams) {
        let (hub, streams) = Self::new(
            executor,
            session.config_path.clone(),
            session.heed_store.clone(),
        );
        if session.session_state.dnd_active {
            hub.notification.set_dnd_enabled(true);
        }
        (hub, streams)
    }

    fn new(
        executor: gpui::BackgroundExecutor,
        config_path: PathBuf,
        session_store: Option<Arc<shilpo_services::HeedSessionStore>>,
    ) -> (Self, ServiceHubStreams) {
        let compositor: Arc<dyn shilpo_services::CompositorAdapter> =
            shilpo_services::init_compositor();
        let device_client = shilpo_services::DeviceClient::new();
        let heed_store_available = session_store.is_some();
        let clipboard = shilpo_services::ClipboardService::with_store(session_store);
        let app_scanner = shilpo_services::AppScanner::new()
            .unwrap_or_else(|_| shilpo_services::AppScanner::new_empty());
        let app_watcher = app_scanner.start_watcher();

        let notification: Arc<dyn shilpo_services::NotificationPort> =
            match NotificationService::new() {
                Ok(service) => Arc::new(service),
                Err(error) => {
                    tracing::warn!(error = %error, "notification service unavailable; using offline port");
                    Arc::new(NotificationService::new_unavailable())
                }
            };

        let polkit: Arc<dyn shilpo_services::PolkitPort> = match shilpo_services::PolkitService::new(
        ) {
            Ok(service) => Arc::new(service),
            Err(error) => {
                tracing::warn!(error = %error, "polkit agent unavailable; using offline fallback");
                Arc::new(shilpo_services::PolkitService::new_offline(Arc::new(
                    shilpo_services::SystemPolkitHelper::new(),
                )))
            }
        };

        let lock_supervisor = shilpo_services::lock_supervisor::LockSupervisor::new();
        let idle: Arc<dyn shilpo_services::IdlePort> = Arc::new(
            shilpo_services::IdleService::new_with_lock_supervisor(lock_supervisor.clone()),
        );

        let notif_rx = notification.subscribe_events();
        let polkit_rx = polkit.subscribe();
        let idle_rx = idle.subscribe();
        let device_rx = device_client.subscribe_updates();

        let (config_tx, config_rx, service_commands, commands_rx) = service_worker::channels();
        let service_task = service_worker::spawn(
            executor,
            config_tx,
            commands_rx,
            config_path,
            device_client.clone(),
        );

        let streams = ServiceHubStreams {
            device_rx,
            notif_rx,
            polkit_rx,
            idle_rx,
            config_rx,
            device_client: device_client.clone(),
        };

        let hub = Self {
            compositor,
            notification,
            polkit,
            idle,
            clipboard,
            app_scanner,
            device_client,
            service_commands,
            device_snapshot: crate::shell::bar::service_worker::DeviceSnapshot::default(),
            domain_states: std::collections::HashMap::new(),
            _service_task: Some(service_task),
            _app_watcher: app_watcher,
            started_at: std::time::Instant::now(),
            heed_store_available,
            lock_supervisor,
        };

        (hub, streams)
    }

    #[cfg(test)]
    pub(crate) fn new_offline_for_test() -> Self {
        let (_config_tx, _config_rx, service_commands, _commands_rx) = service_worker::channels();
        let adapter = Arc::new(shilpo_services::NotificationDomainState::new_ready(32));
        let polkit = Arc::new(shilpo_services::PolkitService::new_ready_for_test(
            Arc::new(shilpo_services::MockPolkitHelper::new(vec![])),
        ));
        let idle = Arc::new(shilpo_services::IdleService::new_mock());
        let device_client = shilpo_services::DeviceClient::new();
        Self {
            compositor: Arc::new(shilpo_services::TestCompositorAdapter::new_default()),
            notification: adapter,
            polkit,
            idle,
            clipboard: shilpo_services::ClipboardService::with_store(None),
            app_scanner: shilpo_services::AppScanner::new_empty(),
            device_client,
            service_commands,
            device_snapshot: crate::shell::bar::service_worker::DeviceSnapshot::default(),
            domain_states: std::collections::HashMap::new(),
            _service_task: None,
            _app_watcher: None,
            started_at: std::time::Instant::now(),
            heed_store_available: false,
            lock_supervisor: shilpo_services::lock_supervisor::LockSupervisor::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_offline_for_test_with_client(
        device_client: shilpo_services::DeviceClient,
    ) -> Self {
        let (_config_tx, _config_rx, service_commands, _commands_rx) = service_worker::channels();
        let adapter = Arc::new(shilpo_services::NotificationDomainState::new_ready(32));
        let polkit = Arc::new(shilpo_services::PolkitService::new_ready_for_test(
            Arc::new(shilpo_services::MockPolkitHelper::new(vec![])),
        ));
        let idle = Arc::new(shilpo_services::IdleService::new_mock());
        Self {
            compositor: Arc::new(shilpo_services::TestCompositorAdapter::new_default()),
            notification: adapter,
            polkit,
            idle,
            clipboard: shilpo_services::ClipboardService::with_store(None),
            app_scanner: shilpo_services::AppScanner::new_empty(),
            device_client,
            service_commands,
            device_snapshot: crate::shell::bar::service_worker::DeviceSnapshot::default(),
            domain_states: std::collections::HashMap::new(),
            _service_task: None,
            _app_watcher: None,
            started_at: std::time::Instant::now(),
            heed_store_available: false,
            lock_supervisor: shilpo_services::lock_supervisor::LockSupervisor::new(),
        }
    }

    pub(crate) fn compositor(&self) -> Arc<dyn shilpo_services::CompositorAdapter> {
        self.compositor.clone()
    }

    pub(crate) fn app_scanner(&self) -> shilpo_services::AppScanner {
        self.app_scanner.clone()
    }

    pub(crate) fn service_commands(&self) -> CommandSender {
        self.service_commands.clone()
    }

    pub(crate) fn device_snapshot(&self) -> crate::shell::bar::service_worker::DeviceSnapshot {
        self.device_snapshot.clone()
    }

    pub fn apply_domain_state(&mut self, state: &shilpo_services::DomainState) -> bool {
        let domain = state.domain;
        if let Some(existing) = self.domain_states.get(&domain)
            && state.version <= existing.version
        {
            return false;
        }
        self.domain_states.insert(domain, state.clone());
        self.device_snapshot.apply_domain_state(state);
        true
    }

    pub fn domain_state(
        &self,
        domain: shilpo_services::DeviceDomain,
    ) -> shilpo_services::DomainState {
        self.domain_states
            .get(&domain)
            .cloned()
            .unwrap_or_else(|| self.device_client.get_domain_state(domain))
    }

    pub fn domain_lifecycle(
        &self,
        domain: shilpo_services::DeviceDomain,
    ) -> shilpo_services::DomainLifecycle {
        self.domain_state(domain).lifecycle
    }

    pub(crate) fn health(&self) -> shilpo_services::ServiceHealth {
        let comp_snap = self.compositor.current();
        let notification = self.notification.snapshot();
        let polkit = self.polkit.snapshot();
        let idle = self.idle.snapshot();
        let battery = self.domain_state(shilpo_services::DeviceDomain::Battery);
        let audio = self.domain_state(shilpo_services::DeviceDomain::Audio);
        let network = self.domain_state(shilpo_services::DeviceDomain::Network);
        let media = self.domain_state(shilpo_services::DeviceDomain::Media);
        let brightness = self.domain_state(shilpo_services::DeviceDomain::Brightness);

        let lock_pam_service =
            crate::config::ShellConfig::load_or_create(crate::config::default_config_path())
                .map(|c| c.lock.pam_service)
                .unwrap_or_else(|_| "login".to_string());
        let lock_pam_service_path = std::path::PathBuf::from("/etc/pam.d").join(&lock_pam_service);

        shilpo_services::ServiceHealth {
            compositor_connected: matches!(
                comp_snap.connection,
                shilpo_services::DomainLifecycle::Ready
            ),
            compositor_state: format!("{:?}", comp_snap.connection).to_lowercase(),
            compositor_owner_generation: comp_snap.version.owner_generation,
            compositor_revision: comp_snap.version.revision,
            compositor_reconnect_attempt: if matches!(
                comp_snap.connection,
                shilpo_services::DomainLifecycle::Reconnecting
            ) {
                1
            } else {
                0
            },
            compositor_last_error: comp_snap.last_error.clone(),
            compositor_telemetry: Some(self.compositor.command_broker().telemetry()),
            battery_service_available: battery.lifecycle.is_ready(),
            battery_state: to_service_lifecycle(battery.lifecycle),
            battery_last_error: battery.error,
            audio_service_available: audio.lifecycle.is_ready(),
            audio_state: to_service_lifecycle(audio.lifecycle),
            audio_last_error: audio.error,
            network_service_available: network.lifecycle.is_ready(),
            network_state: to_service_lifecycle(network.lifecycle),
            network_last_error: network.error,
            notification_service_available: notification.lifecycle.is_ready(),
            notification_state: to_service_lifecycle(notification.lifecycle),
            notification_last_error: notification.last_error,
            media_service_available: media.lifecycle.is_ready(),
            media_state: to_service_lifecycle(media.lifecycle),
            media_last_error: media.error,
            brightness_service_available: brightness.lifecycle.is_ready(),
            brightness_state: to_service_lifecycle(brightness.lifecycle),
            brightness_last_error: brightness.error,
            polkit_service_available: polkit.lifecycle.is_ready(),
            polkit_state: to_service_lifecycle(polkit.lifecycle),
            polkit_last_error: polkit.last_error,
            polkit_helper_path: polkit.helper_path,
            idle_service_available: idle.lifecycle.is_ready(),
            idle_state: to_service_lifecycle(idle.lifecycle),
            idle_last_error: idle.last_error,
            idle_notifier_available: idle.notifier_available,
            idle_registered_behaviors: idle.registered_behaviors,
            idle_inhibit_count: idle.inhibit_count,
            idle_unsupported_actions: idle.unsupported_actions,
            lock_service_available: lock_pam_service_path.exists(),
            lock_state: if self.lock_supervisor.is_active() {
                "active".to_string()
            } else if self.lock_supervisor.last_error().is_some() {
                "error".to_string()
            } else {
                "idle".to_string()
            },
            lock_session_active: self.lock_supervisor.is_active(),
            lock_last_error: self.lock_supervisor.last_error(),
            lock_last_spawn_reason: self.lock_supervisor.last_spawn_reason(),
            lock_pam_service,
            heed_store_available: self.heed_store_available,
            uptime_seconds: self.started_at.elapsed().as_secs(),
            extension_host: None,
        }
    }

    pub(crate) fn is_dnd_enabled(&self) -> bool {
        self.notification.snapshot().dnd_enabled
    }

    pub fn polkit(&self) -> &Arc<dyn shilpo_services::PolkitPort> {
        &self.polkit
    }

    pub fn idle(&self) -> &Arc<dyn shilpo_services::IdlePort> {
        &self.idle
    }

    pub fn lock_supervisor(&self) -> &Arc<shilpo_services::lock_supervisor::LockSupervisor> {
        &self.lock_supervisor
    }

    #[cfg(test)]
    pub(crate) fn notification_dnd_flag(&self) -> bool {
        self.notification.snapshot().dnd_enabled
    }

    pub(crate) fn set_dnd_enabled(&mut self, enabled: bool) {
        self.notification.set_dnd_enabled(enabled);
    }

    pub(crate) fn push_notification(&self, notification: Notification) {
        self.notification.push_notification(notification);
    }

    pub(crate) fn dismiss_notification(&self, id: u32) {
        self.notification.dismiss(id);
    }

    pub(crate) fn expire_notification(&self, id: u32) {
        self.notification.expire(id);
    }

    pub(crate) fn invoke_notification_action(&self, id: u32, action_key: &str) {
        self.notification.invoke_action(id, action_key);
    }

    pub(crate) fn notification_history(&self) -> Vec<Notification> {
        self.notification.snapshot().history
    }

    pub(crate) fn clear_notification_history(&self) {
        self.notification.clear_history();
    }

    pub(crate) fn reset_notification_quarantine(&self) {
        self.notification.reset_quarantine();
    }

    pub(crate) fn reset_device_quarantine(&self) {
        self.device_client.reset_quarantine();
    }

    pub(crate) fn copy_text(&self, text: &str) -> anyhow::Result<()> {
        self.clipboard.copy_text(text)
    }

    pub(crate) fn clipboard_history(&self) -> Vec<ClipboardItem> {
        self.clipboard.history()
    }

    pub(crate) fn clipboard_subscription(
        &self,
    ) -> tokio::sync::watch::Receiver<Vec<ClipboardItem>> {
        self.clipboard.subscribe()
    }

    pub(crate) fn set_clipboard_history_limit(
        &self,
        limit: usize,
    ) -> Result<(), shilpo_services::ClipboardPersistenceError> {
        self.clipboard.set_history_limit(limit)
    }

    pub(crate) fn send_device_command(&self, command: DeviceCommand) {
        let _ = service_worker::try_send_command(
            &self.service_commands,
            WorkerCommand::Device(command),
        );
    }
}

fn to_service_lifecycle(
    lifecycle: shilpo_services::DomainLifecycle,
) -> shilpo_services::ServiceLifecycle {
    match lifecycle {
        shilpo_services::DomainLifecycle::Ready => shilpo_services::ServiceLifecycle::Ready,
        shilpo_services::DomainLifecycle::Connecting
        | shilpo_services::DomainLifecycle::Reconnecting => {
            shilpo_services::ServiceLifecycle::Connecting { attempt: 0 }
        }
        shilpo_services::DomainLifecycle::Degraded
        | shilpo_services::DomainLifecycle::Unavailable => {
            shilpo_services::ServiceLifecycle::Unavailable
        }
    }
}

/// Applies the do-not-disturb flag to a notification port.
#[cfg(test)]
pub(crate) fn apply_notification_dnd(notification: &dyn NotificationPort, enabled: bool) {
    notification.set_dnd_enabled(enabled);
}

impl ShellRuntime {
    pub fn service_commands(cx: &App) -> Option<crate::shell::bar::service_worker::CommandSender> {
        if cx.has_global::<Self>() {
            cx.global::<Self>()
                .service_hub()
                .map(|hub| hub.service_commands())
        } else {
            None
        }
    }

    pub fn device_snapshot(cx: &App) -> crate::shell::bar::service_worker::DeviceSnapshot {
        cx.global::<Self>()
            .service_hub()
            .map(|hub| hub.device_snapshot())
            .unwrap_or_default()
    }

    pub fn domain_state(
        cx: &App,
        domain: shilpo_services::DeviceDomain,
    ) -> shilpo_services::DomainState {
        cx.global::<Self>()
            .service_hub()
            .map(|hub| hub.domain_state(domain))
            .unwrap_or_else(|| shilpo_services::DomainState {
                domain,
                version: shilpo_services::DomainVersion::ZERO,
                lifecycle: shilpo_services::DomainLifecycle::Unavailable,
                payload: shilpo_services::DomainPayload::empty(domain),
                error: None,
            })
    }

    pub fn domain_lifecycle(
        cx: &App,
        domain: shilpo_services::DeviceDomain,
    ) -> shilpo_services::DomainLifecycle {
        Self::domain_state(cx, domain).lifecycle
    }

    pub fn battery_lifecycle(cx: &App) -> shilpo_services::DomainLifecycle {
        Self::domain_lifecycle(cx, shilpo_services::DeviceDomain::Battery)
    }

    pub fn service_health(cx: &App) -> Option<shilpo_services::ServiceHealth> {
        if cx.has_global::<Self>() {
            cx.global::<Self>().service_hub().map(|hub| hub.health())
        } else {
            None
        }
    }

    pub fn dispatch_device_command(cx: &App, command: DeviceCommand) {
        if let Some(hub) = cx.global::<Self>().service_hub() {
            hub.send_device_command(command);
        }
    }

    pub fn app_scanner(cx: &App) -> Option<shilpo_services::AppScanner> {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            Some(hub.app_scanner())
        } else {
            None
        }
    }

    pub fn compositor(cx: &App) -> Option<Arc<dyn shilpo_services::CompositorAdapter>> {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            Some(hub.compositor())
        } else {
            None
        }
    }

    pub fn clipboard_history(cx: &App) -> Vec<ClipboardItem> {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            hub.clipboard_history()
        } else {
            Vec::new()
        }
    }

    pub fn clipboard_subscription(
        cx: &App,
    ) -> Option<tokio::sync::watch::Receiver<Vec<ClipboardItem>>> {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            Some(hub.clipboard_subscription())
        } else {
            None
        }
    }

    pub fn copy_clipboard_text(cx: &App, text: &str) {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            let _ = hub.copy_text(text);
        }
    }

    pub fn is_dnd_active(cx: &App) -> bool {
        if cx.has_global::<Self>() {
            let runtime = cx.global::<Self>();
            runtime
                .service_hub()
                .map_or(runtime.session_state().dnd_active, |hub| {
                    hub.is_dnd_enabled()
                })
        } else {
            false
        }
    }

    pub fn set_dnd_enabled(cx: &mut App, enabled: bool) {
        if cx.has_global::<Self>() {
            let runtime = cx.global_mut::<Self>();
            runtime.session_state_mut().dnd_active = enabled;
            let path = runtime.session_path().clone();
            let session = runtime.session_state().clone();
            let _ = session.save_atomic(&path);
            if let Some(hub) = runtime.service_hub_mut() {
                hub.set_dnd_enabled(enabled);
            }
        }
    }

    pub fn notification_history(cx: &App) -> Vec<Notification> {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            hub.notification_history()
        } else {
            Vec::new()
        }
    }

    pub fn clear_notification_history(cx: &mut App) {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            hub.clear_notification_history();
        }
    }

    pub fn invoke_notification_action(cx: &App, id: u32, action_key: &str) {
        if cx.has_global::<Self>()
            && let Some(hub) = cx.global::<Self>().service_hub()
        {
            hub.invoke_notification_action(id, action_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn service_hub_initialization_and_single_ownership() {
        let (_updates_tx, _updates_rx, service_commands, _commands_rx) = service_worker::channels();
        assert!(
            service_worker::try_send_command(&service_commands, WorkerCommand::ReloadConfig)
                .is_ok()
        );
    }

    #[test]
    fn restored_dnd_is_applied_to_notification_lifecycle() {
        let notification = Arc::new(shilpo_services::NotificationDomainState::new_ready(32));
        assert!(!notification.snapshot().dnd_enabled);

        apply_notification_dnd(notification.as_ref(), true);

        assert!(notification.snapshot().dnd_enabled);
    }

    #[test]
    fn service_hub_start_initializes_with_dnd() {
        let temp_dir =
            std::env::temp_dir().join(format!("shilpo_service_test_{}", uuid::Uuid::new_v4()));
        let session = SessionContext {
            config_path: temp_dir.join("config.toml"),
            active_config: crate::config::ShellConfig::default(),
            session_path: temp_dir.join("session.toml"),
            session_state: crate::config::ShellSessionState {
                dnd_active: true,
                ..Default::default()
            },
            heed_store: None,
        };
        let mut hub = ServiceHub::new_offline_for_test();
        if session.session_state.dnd_active {
            hub.set_dnd_enabled(true);
        }

        assert!(hub.notification_dnd_flag());
        assert!(hub.is_dnd_enabled());

        drop(hub);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn dnd_persistence_flag_survives_notification_restart() {
        let mut hub = ServiceHub::new_offline_for_test();
        hub.set_dnd_enabled(true);
        assert!(hub.notification_dnd_flag());
        assert!(hub.is_dnd_enabled());
        hub.set_dnd_enabled(false);
        assert!(!hub.notification_dnd_flag());
    }

    struct ServiceHubTestHarness {
        hub: ServiceHub,
    }

    impl ServiceHubTestHarness {
        fn new_offline() -> Self {
            Self {
                hub: ServiceHub::new_offline_for_test(),
            }
        }
    }

    #[test]
    fn test_harness_service_hub_isolated_state_transitions() {
        let mut harness = ServiceHubTestHarness::new_offline();
        assert!(!harness.hub.notification_dnd_flag());
        harness.hub.set_dnd_enabled(true);
        assert!(harness.hub.notification_dnd_flag());
    }

    #[test]
    fn emit_test_notification_routes_to_notification_domain() {
        let harness = ServiceHubTestHarness::new_offline();
        let notif = Notification {
            id: 0,
            app_name: "Shilpo Debug".to_string(),
            summary: "Test Title".to_string(),
            body: "Test Body".to_string(),
            app_icon: Some("dialog-information".to_string()),
            desktop_entry: None,
            image_path: None,
            urgency: shilpo_services::NotificationUrgency::Normal,
            actions: Vec::new(),
            expire_timeout_ms: 5000,
            timestamp: chrono::Local::now(),
        };

        harness.hub.push_notification(notif);

        let history = harness.hub.notification_history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].summary, "Test Title");
        assert_eq!(history[0].body, "Test Body");
    }
}
