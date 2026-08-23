pub mod applications;
pub mod audio;
pub mod auth;
pub mod bluetooth;
pub mod brightness;
pub mod caffeine;
pub mod capture;
pub mod clipboard;
pub mod compositor;
pub mod device_daemon;
pub use shilpo_domain as domain;
pub use shilpo_domain::*;
pub mod error;
pub mod idle;
pub mod lock_supervisor;
pub use shilpo_device as device;
pub use shilpo_device as device_protocol;
pub use shilpo_device as device_client;
pub use shilpo_device::*;
pub mod location;
pub mod media;
pub mod network;
pub mod night_light;
pub mod notifications;
pub mod polkit;
pub mod power_profile;
pub(crate) mod runtime;
pub mod secret;
pub mod session_store;
pub mod status;
pub mod tray;
pub mod upower;

pub use applications::{
    AppScanner, Application, find_terminal_emulator, parse_uri_list, percent_decode,
    resolve_handler_for_uri, validate_drag_drop_payload,
};
pub use audio::{AudioDevice, AudioInfo, AudioPort, AudioService, AudioStream};
pub use bluetooth::{BluetoothAddress, BluetoothDevice, BluetoothInfo, BluetoothService};
pub use brightness::{BrightnessInfo, BrightnessService};
pub use caffeine::{CaffeineInfo, CaffeineService};
pub use clipboard::{ClipboardPersistenceError, ClipboardService};
pub use compositor::{
    BackendFactory, BoundProtocols, BrokerOptions, CancellationReason, CandidateBackend,
    CommandCancellation, CommandExecutorFn, CommandOutcome, CommandTicket, CompositorAdapter,
    CompositorCapabilities, CompositorCommand, CompositorCommandBroker, CompositorCommandError,
    CompositorExtras, CompositorKind, CompositorOutput, CompositorRegistry, CompositorSnapshot,
    CompositorTarget, DomainLifecycle, DomainVersion, ExecutorAck, GenericWaylandCompositorBackend,
    HyprlandCompositorBackend, HyprlandExtras, HyprlandSpecialWorkspace, MailboxPolicy,
    NiriCompositorService, NiriExtras, NullCompositorBackend, RejectionReason, StaleUpdateError,
    SupervisorState, TestCompositorAdapter, WindowIdentity, WindowInfo, WorkspaceInfo, detect,
    detect_from, init_compositor, init_compositor_with,
};
pub use device_daemon::{
    DeviceAdapter, DeviceDaemonService, DeviceDbusService, InMemoryDeviceAdapter,
    SystemDeviceAdapter,
};
pub use error::ServiceError;
pub use idle::{
    ActionExecutionOutcome, ActiveGraceInfo, IdleAction, IdleActionSink, IdleBehaviorConfig,
    IdleCommand, IdleCommandOutcome, IdleDomainState, IdleNotifierBackend, IdlePort,
    IdleRejectionReason, IdleService, IdleSnapshot, InhibitSource, MockIdleActionSink,
    MockIdleNotifier, SystemIdleActionSink, WaylandIdleNotifier,
};
pub use location::{LocationInfo, LocationService};
pub use media::{MediaCommand, MediaInfo, MediaService, PlaybackState};
pub use network::{NetworkCommand, NetworkInfo, NetworkService, VpnConnection};
pub use night_light::{NightLightInfo, NightLightService, ThemeSchedule, should_use_dark_mode};
pub use notifications::{
    MonotonicTimeSource, Notification, NotificationCloseReason, NotificationCommand,
    NotificationCommandOutcome, NotificationDomainState, NotificationPort,
    NotificationRejectionReason, NotificationService, NotificationSnapshot, NotificationUrgency,
    TimeSource,
};
pub use polkit::{
    AuthorityClient, HelperEvent, MockPolkitHelper, POLKIT_AGENT_OBJECT_PATH,
    POLKIT_AUTHORITY_DESTINATION, POLKIT_AUTHORITY_INTERFACE, POLKIT_AUTHORITY_PATH,
    PolkitAgentServer, PolkitCommand, PolkitCommandOutcome, PolkitDomainState, PolkitHelper,
    PolkitHelperSession, PolkitIdentity, PolkitPort, PolkitPromptState, PolkitRejectionReason,
    PolkitRequest, PolkitService, PolkitSnapshot, SystemPolkitHelper,
};
pub use power_profile::{PowerProfile, PowerProfileInfo, PowerProfileService};
pub use secret::SecretString;
pub use session_store::*;
pub use status::{BarState, ReadinessState, ServiceHealth, ServiceLifecycle};

pub async fn run_device_daemon() -> anyhow::Result<()> {
    use std::sync::Arc;

    use zbus::object_server::SignalEmitter;

    let _obs_guard = shilpo_observability::init(
        shilpo_observability::ProcessRole::DeviceDaemon,
        "info,shilpo_services=info",
    )
    .map_err(|e| eprintln!("observability warning: {e}"))
    .ok();

    let adapter = Arc::new(SystemDeviceAdapter::new());
    let daemon = Arc::new(DeviceDaemonService::new(adapter));
    let mut outcomes = daemon.subscribe_outcomes();
    let connection = zbus::Connection::session().await?;
    let _dbus_span = tracing::info_span!(
        target: "shilpo_profile",
        "dbus_call",
        bus = "session",
        destination = "org.shilpo.Device",
        operation = "register",
        outcome = "registered",
    );
    let _dbus_enter = _dbus_span.enter();
    use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
    let dbus = DBusProxy::new(&connection).await?;
    let reply = dbus
        .request_name(
            "org.shilpo.Device".try_into()?,
            RequestNameFlags::DoNotQueue.into(),
        )
        .await?;
    if reply != RequestNameReply::PrimaryOwner {
        eprintln!("org.shilpo.Device is already owned by another process");
        std::process::exit(1);
    }

    let service = DeviceDbusService::new(daemon);
    connection
        .object_server()
        .at("/org/shilpo/Device", service.clone())
        .await?;
    let emitter = SignalEmitter::new(&connection, "/org/shilpo/Device")?.into_owned();

    tracing::info!("shilpo-device-daemon registered org.shilpo.Device");
    loop {
        tokio::select! {
            outcome = outcomes.recv() => {
                if let Ok(outcome) = outcome {
                    service.emit_outcome(&emitter, &outcome).await?;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                service.emit_updates(&emitter).await?;
            }
        }
    }
}

pub use tray::{TrayItem, TrayMenuItem, TrayService};
pub use upower::{BatteryInfo, BatteryService};

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    pub(crate) fn serial_guard() -> MutexGuard<'static, ()> {
        SERIAL_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Returns whether Niri Wayland compositor IPC is supported on this target platform.
pub fn is_niri_supported() -> bool {
    cfg!(target_os = "linux")
}

/// Returns the supported system service capability matrix on this platform.
pub fn platform_capabilities() -> Vec<&'static str> {
    if cfg!(target_os = "linux") {
        vec![
            "compositor",
            "brightness",
            "audio",
            "notifications",
            "tray",
            "network",
            "upower",
            "location",
        ]
    } else {
        vec!["offline_fallback"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_capability_matrix() {
        assert!(is_niri_supported());
        let caps = platform_capabilities();
        assert!(caps.contains(&"compositor"));
    }

    #[test]
    fn test_all_services_watch_subscribe_pattern() {
        let bluetooth = BluetoothService::new_offline();
        assert_eq!(
            bluetooth.subscribe().borrow().clone(),
            BluetoothInfo::default()
        );

        let caffeine = CaffeineService::new();
        assert_eq!(
            caffeine.subscribe().borrow().clone(),
            CaffeineInfo::default()
        );

        let location = LocationService::new();
        assert_eq!(location.subscribe().borrow().clone(), None);

        let media = MediaService::new_offline();
        assert_eq!(media.subscribe().borrow().clone(), MediaInfo::default());

        let network = NetworkService::new_offline();
        assert_eq!(network.subscribe().borrow().clone(), NetworkInfo::default());

        let night_light = NightLightService::new_offline();
        assert_eq!(
            night_light.subscribe().borrow().clone(),
            NightLightInfo::default()
        );

        let power_profile = PowerProfileService::new_offline();
        assert_eq!(
            power_profile.subscribe().borrow().clone(),
            PowerProfileInfo {
                active_profile: PowerProfile::Balanced,
                available: false,
            }
        );

        let tray = TrayService::new_offline();

        assert!(tray.subscribe().borrow().is_empty());

        let notif = NotificationService::new_offline();
        assert!(notif.subscribe().borrow().notifications.is_empty());

        let clipboard = ClipboardService::with_custom_store(None);
        assert!(clipboard.subscribe().borrow().is_empty());
    }

    #[test]
    fn test_service_clone_and_drop_ownership() {
        let original = PowerProfileService::new_offline();
        let clone = original.clone();

        assert_eq!(original.info(), clone.info());
        drop(original);

        assert_eq!(clone.info(), PowerProfileInfo::fallback());
    }

    #[test]
    fn test_offline_contract_guarantees() {
        let battery = BatteryService::new().ok();
        // BatteryService::new() in offline/unreachable env yields default unavailable info
        if let Some(svc) = battery {
            let info = svc.battery_info();
            assert!(!info.is_present);
        }

        let media = MediaService::new_offline();
        assert!(media.media_info().is_empty());

        let network = NetworkService::new_offline();
        assert!(!network.network_info().available);
    }
}
