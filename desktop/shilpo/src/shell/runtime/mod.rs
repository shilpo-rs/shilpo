pub mod action_dispatcher;
pub mod dbus;
pub mod extension_host;
pub mod service_hub;
pub mod session;
pub mod shell_surfaces;
pub mod theme_manager;
pub(crate) mod wallpaper_coordinator;
pub(crate) mod wallpaper_preview;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub use action_dispatcher::ActionDispatcher;
pub use extension_host::ExtensionHost;
use gpui::{App, AppContext, Entity, Global, Subscription};
pub use service_hub::ServiceHub;
pub use session::SessionContext;
use shell_surfaces::WindowClosedOutcome;
pub use shell_surfaces::{ShellSurfaces, SurfaceRequest, SurfaceSnapshot};
use shilpo_services::{CompositorCommandBroker, CompositorSnapshot};
pub use wallpaper_coordinator::WallpaperCoordinator;
pub(crate) use wallpaper_preview::{WallpaperPreviewResource, WallpaperPreviewSnapshot};

#[cfg(test)]
use crate::extensions::ExtensionCommand;
use crate::{
    extensions::{ContributionSurface, ExtensionCoordinator},
    shell::dbus::{ShellCommand, ShellDbusService, ShellStatus, ShellTelemetry},
};

/// The shell runtime orchestrator: composes the deep service modules, watches
/// the compositor stream, and routes lifecycle events between them.
///
/// Every component is private; the shell reaches them through the narrow
/// `pub(super)` accessor surface below.
pub struct ShellRuntime {
    dbus_service: Arc<ShellDbusService>,
    _compositor_broker: Arc<Mutex<Option<Arc<CompositorCommandBroker>>>>,
    _status: Arc<arc_swap::ArcSwap<ShellStatus>>,
    _telemetry: Arc<arc_swap::ArcSwap<ShellTelemetry>>,
    dbus_connection: Option<zbus::Connection>,
    instance_id: String,
    active_config: crate::config::ShellConfig,
    shell_surfaces: ShellSurfaces,
    action_dispatcher: ActionDispatcher,
    extension_host: ExtensionHost,
    wallpaper_coordinator: WallpaperCoordinator,
    wallpaper_preview: Entity<WallpaperPreviewResource>,
    service_hub: Option<ServiceHub>,
    session_state: crate::config::ShellSessionState,
    session_path: PathBuf,
    heed_store: Option<Arc<shilpo_services::HeedSessionStore>>,
    _start_time: std::time::Instant,
    _window_closed: Option<Subscription>,
    _wallpaper_preview_changed: Option<Subscription>,
    _drain_task: gpui::Task<()>,
    _wallpaper_timer_task: gpui::Task<()>,
}

impl Global for ShellRuntime {}

impl ShellRuntime {
    #[cfg(test)]
    pub(crate) fn take_test_extension_inputs(
        cx: &App,
    ) -> std::sync::Arc<std::sync::Mutex<Vec<ExtensionCommand>>> {
        cx.global::<Self>().extension_host.test_inputs()
    }

    #[cfg(test)]
    pub(crate) fn install_for_test(cx: &mut App) -> tokio::sync::mpsc::Sender<ShellCommand> {
        let root = std::env::temp_dir().join(format!(
            "shilpo-shell-surface-test-{}",
            uuid::Uuid::new_v4()
        ));
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let compositor_broker = Arc::new(Mutex::new(None));
        let status = Arc::new(arc_swap::ArcSwap::from_pointee(ShellStatus::default()));
        let telemetry = Arc::new(arc_swap::ArcSwap::from_pointee(ShellTelemetry::default()));
        let dbus_service = Arc::new(ShellDbusService::new(
            tx.clone(),
            compositor_broker.clone(),
            status.clone(),
            telemetry.clone(),
        ));
        let wallpaper_preview = cx.new(WallpaperPreviewResource::new);
        let (_notif_tx, notif_rx) = tokio::sync::broadcast::channel(16);
        let (_device_tx, device_rx) = tokio::sync::broadcast::channel(16);
        let (_config_tx, config_rx) = tokio::sync::mpsc::channel(16);
        let (_polkit_tx, polkit_rx) =
            tokio::sync::watch::channel(shilpo_services::PolkitSnapshot::default());
        let (_idle_tx, idle_rx) =
            tokio::sync::watch::channel(shilpo_services::IdleSnapshot::default());
        let device_client = shilpo_services::DeviceClient::new();
        cx.set_global(Self {
            dbus_service,
            _compositor_broker: compositor_broker,
            _status: status,
            _telemetry: telemetry,
            dbus_connection: None,
            instance_id: uuid::Uuid::new_v4().to_string(),
            active_config: crate::config::ShellConfig::default(),
            shell_surfaces: ShellSurfaces::new(Arc::new(CompositorSnapshot::default())),
            action_dispatcher: ActionDispatcher::new(),
            extension_host: ExtensionHost::new(None),
            wallpaper_coordinator: WallpaperCoordinator::new(),
            wallpaper_preview,
            service_hub: None,
            session_state: crate::config::ShellSessionState::default(),
            session_path: root.join("session.json"),
            heed_store: None,
            _start_time: std::time::Instant::now(),
            _window_closed: None,
            _wallpaper_preview_changed: None,
            _drain_task: gpui::Task::ready(()),
            _wallpaper_timer_task: gpui::Task::ready(()),
        });
        Self::spawn_event_loop(
            cx,
            device_rx,
            notif_rx,
            polkit_rx,
            idle_rx,
            rx,
            config_rx,
            device_client,
        );
        tx
    }

    pub(crate) fn polkit(cx: &App) -> Arc<dyn shilpo_services::PolkitPort> {
        cx.global::<Self>()
            .service_hub
            .as_ref()
            .map(|s| s.polkit().clone())
            .unwrap_or_else(|| {
                Arc::new(shilpo_services::PolkitService::new_offline(Arc::new(
                    shilpo_services::SystemPolkitHelper::new(),
                )))
            })
    }

    #[allow(dead_code)]
    pub(crate) fn idle(cx: &App) -> Arc<dyn shilpo_services::IdlePort> {
        cx.global::<Self>()
            .service_hub
            .as_ref()
            .map(|s| s.idle().clone())
            .unwrap_or_else(|| Arc::new(shilpo_services::IdleService::new_mock()))
    }

    #[allow(dead_code)]
    pub(crate) fn polkit_snapshot(cx: &App) -> shilpo_services::PolkitSnapshot {
        Self::polkit(cx).snapshot()
    }

    #[cfg(test)]
    pub(crate) fn set_service_hub_for_test(cx: &mut App, service_hub: ServiceHub) {
        cx.global_mut::<Self>().service_hub = Some(service_hub);
    }

    pub(crate) fn readiness(&self) -> shilpo_services::ReadinessState {
        self.shell_surfaces.readiness()
    }

    pub(crate) fn session_state(&self) -> &crate::config::ShellSessionState {
        &self.session_state
    }

    pub(crate) fn session_state_mut(&mut self) -> &mut crate::config::ShellSessionState {
        &mut self.session_state
    }

    pub(crate) fn session_path(&self) -> &PathBuf {
        &self.session_path
    }

    pub(crate) fn heed_store(&self) -> Option<&Arc<shilpo_services::HeedSessionStore>> {
        self.heed_store.as_ref()
    }

    pub(crate) fn shell_surfaces(&self) -> &ShellSurfaces {
        &self.shell_surfaces
    }

    pub(crate) fn shell_surfaces_mut(&mut self) -> &mut ShellSurfaces {
        &mut self.shell_surfaces
    }

    pub(crate) fn action_dispatcher(&self) -> &ActionDispatcher {
        &self.action_dispatcher
    }

    pub(crate) fn action_dispatcher_mut(&mut self) -> &mut ActionDispatcher {
        &mut self.action_dispatcher
    }

    pub(crate) fn extension_host(&self) -> &ExtensionHost {
        &self.extension_host
    }

    pub(crate) fn extension_host_mut(&mut self) -> &mut ExtensionHost {
        &mut self.extension_host
    }

    pub(crate) fn extension_coordinator(
        cx: &App,
    ) -> Option<Arc<crate::extensions::ExtensionCoordinator>> {
        if cx.has_global::<Self>() {
            cx.global::<Self>().extension_host().coordinator()
        } else {
            None
        }
    }

    pub(crate) fn extension_descriptors_for(
        cx: &App,
        surface: crate::extensions::ContributionSurface,
    ) -> Vec<crate::extensions::ContributionDescriptor> {
        if cx.has_global::<Self>() {
            cx.global::<Self>()
                .extension_host()
                .descriptors_for(surface)
        } else {
            Vec::new()
        }
    }

    pub(crate) fn wallpaper_coordinator(&self) -> &WallpaperCoordinator {
        &self.wallpaper_coordinator
    }

    pub(crate) fn wallpaper_coordinator_mut(&mut self) -> &mut WallpaperCoordinator {
        &mut self.wallpaper_coordinator
    }

    pub(crate) fn request_next_wallpaper(cx: &mut App) {
        if !cx.has_global::<Self>() {
            return;
        }
        let event_to_send = cx
            .global_mut::<Self>()
            .wallpaper_coordinator_mut()
            .request_next_wallpaper();
        if let Some((ext_id, event)) = event_to_send {
            cx.global::<Self>()
                .extension_host()
                .send_event_to_extension(&ext_id, event);
        }
    }

    pub(crate) fn wallpaper_preview(cx: &App) -> Entity<WallpaperPreviewResource> {
        cx.global::<Self>().wallpaper_preview.clone()
    }

    pub(crate) fn wallpaper_preview_snapshot(cx: &App) -> WallpaperPreviewSnapshot {
        let entity = Self::wallpaper_preview(cx);
        entity.read(cx).snapshot()
    }

    pub(crate) fn set_wallpaper_path(cx: &mut App, path: Option<PathBuf>) {
        let entity = Self::wallpaper_preview(cx);
        entity.update(cx, |wp, cx| {
            wp.set_wallpaper_path(path, cx);
        });
    }

    pub(crate) fn service_hub(&self) -> Option<&ServiceHub> {
        self.service_hub.as_ref()
    }

    pub(crate) fn service_hub_mut(&mut self) -> Option<&mut ServiceHub> {
        self.service_hub.as_mut()
    }

    pub fn active_config(cx: &App) -> crate::config::ShellConfig {
        if cx.has_global::<Self>() {
            cx.global::<Self>().active_config.clone()
        } else {
            crate::config::ShellConfig::default()
        }
    }

    pub fn session_heed_store(cx: &App) -> Option<Arc<shilpo_services::HeedSessionStore>> {
        if cx.has_global::<Self>() {
            cx.global::<Self>().heed_store.clone()
        } else {
            None
        }
    }

    pub(crate) fn set_active_config(cx: &mut App, config: &crate::config::ShellConfig) {
        if cx.has_global::<Self>() {
            cx.global_mut::<Self>().active_config = config.clone();
            if let Some(ref hub) = cx.global::<Self>().service_hub {
                let _ = hub.set_clipboard_history_limit(config.clipboard.history_limit);
                let _ =
                    hub.idle()
                        .submit_command(shilpo_services::IdleCommand::ConfigureBehaviors {
                            behaviors: config.idle.behaviors.clone(),
                            grace_seconds: config.idle.grace_seconds,
                        });
            }
            Self::sync_wallpaper_provider(cx);
            let settings_event = {
                let runtime = cx.global::<Self>();
                runtime
                    .wallpaper_coordinator()
                    .active_provider()
                    .and_then(|provider| {
                        runtime
                            .active_config
                            .extensions
                            .settings
                            .get(provider.extension_id.as_str())
                            .cloned()
                            .map(|settings| (provider.clone(), settings))
                    })
            };
            if let Some((provider, settings)) = settings_event {
                cx.global::<Self>()
                    .extension_host()
                    .send_event_to_extension(
                        &provider.extension_id,
                        shilpo_ext_api::ExtensionEvent::ContributionSettingsChanged {
                            contribution_id: provider.contribution_id.to_string(),
                            instance_id: None,
                            settings,
                        },
                    );
            }
        }
    }

    pub(crate) fn sync_wallpaper_provider(cx: &mut App) {
        if !cx.has_global::<Self>() {
            return;
        }
        let (config, descriptors, generation) = {
            let runtime = cx.global::<Self>();
            (
                runtime.active_config.extensions.clone(),
                runtime
                    .extension_host()
                    .descriptors_for(ContributionSurface::Wallpaper),
                runtime.extension_host().generation().unwrap_or_default(),
            )
        };
        let event = cx
            .global_mut::<Self>()
            .wallpaper_coordinator_mut()
            .sync_active_provider(&config, &descriptors, generation);
        if let Some((extension_id, event)) = event {
            cx.global::<Self>()
                .extension_host()
                .send_event_to_extension(&extension_id, event);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn install(
        cx: &mut App,
        dbus_service: Arc<ShellDbusService>,
        compositor_broker: Arc<Mutex<Option<Arc<CompositorCommandBroker>>>>,
        status: Arc<arc_swap::ArcSwap<ShellStatus>>,
        telemetry: Arc<arc_swap::ArcSwap<ShellTelemetry>>,
        mailbox_rx: tokio::sync::mpsc::Receiver<ShellCommand>,
        dbus_connection: zbus::Connection,
        instance_id: String,
    ) {
        let initial_wallpaper_path = theme_manager::init(cx);
        let session = session::SessionContext::init();
        let (hub, streams) = ServiceHub::start(cx.background_executor().clone(), &session);
        let extensions = ExtensionCoordinator::init(cx.background_executor().clone()).map(Arc::new);
        if let Some(ref ext) = extensions {
            dbus_service.set_extension_coordinator(Some(ext.clone()));
        }

        let compositor = hub.compositor();
        let latest_snapshot =
            shell_surfaces::attach_compositor_stream(&compositor_broker, &compositor);
        let shell_surfaces = ShellSurfaces::new(latest_snapshot.clone());
        let action_dispatcher = ActionDispatcher::new();
        let extension_host = ExtensionHost::new(extensions);
        let wallpaper_preview = cx.new(WallpaperPreviewResource::new);
        wallpaper_preview.update(cx, |wp, cx| {
            wp.set_wallpaper_path(initial_wallpaper_path.clone(), cx);
        });

        let _ = hub
            .idle()
            .submit_command(shilpo_services::IdleCommand::ConfigureBehaviors {
                behaviors: session.active_config.idle.behaviors.clone(),
                grace_seconds: session.active_config.idle.grace_seconds,
            });

        if session.active_config.lock.lock_on_suspend {
            spawn_prepare_for_sleep_lock_watch(hub.lock_supervisor().clone());
        }

        cx.set_global(Self {
            dbus_service,
            _compositor_broker: compositor_broker,
            _status: status,
            _telemetry: telemetry,
            dbus_connection: Some(dbus_connection),
            instance_id,
            active_config: session.active_config,
            shell_surfaces,
            action_dispatcher,
            extension_host,
            wallpaper_coordinator: WallpaperCoordinator::new(),
            wallpaper_preview: wallpaper_preview.clone(),
            service_hub: Some(hub),
            session_state: session.session_state,
            session_path: session.session_path,
            heed_store: session.heed_store,
            _start_time: std::time::Instant::now(),
            _window_closed: None,
            _wallpaper_preview_changed: None,
            _drain_task: gpui::Task::ready(()),
            _wallpaper_timer_task: gpui::Task::ready(()),
        });

        let wallpaper_preview_changed = cx.observe(&wallpaper_preview, |_, cx| {
            crate::bar::cards::adapter::CardCoordinator::refresh_owner(
                cx,
                &crate::bar::cards::workspace_card::workspace_owner_id(),
            );
        });
        cx.global_mut::<Self>()._wallpaper_preview_changed = Some(wallpaper_preview_changed);

        // Initial compositor state is population, not a workspace transition.
        cx.global::<Self>()
            .dbus_service
            .prime_workspace(latest_snapshot.focused_workspace_id.unwrap_or(0));

        shell_surfaces::spawn_compositor_stream_loop(cx, &compositor);
        theme_manager::sync_wallpaper(cx, initial_wallpaper_path);
        Self::on_compositor_snapshot_changed(cx, latest_snapshot);
        Self::spawn_window_closed_watch(cx);
        ExtensionHost::sync_extension_actions(cx);
        Self::spawn_event_loop(
            cx,
            streams.device_rx,
            streams.notif_rx,
            streams.polkit_rx,
            streams.idle_rx,
            mailbox_rx,
            streams.config_rx,
            streams.device_client,
        );
        Self::spawn_wallpaper_timer(cx);
        Self::publish_status(cx);

        let Some(conn) = cx.global::<Self>().dbus_connection.clone() else {
            return;
        };
        let inst_id = cx.global::<Self>().instance_id.clone();
        let pid = std::process::id();
        cx.spawn(async move |_| {
            if let Ok(iface) = conn
                .object_server()
                .interface::<_, ShellDbusService>("/org/shilpo/Shell")
                .await
            {
                let emitter = iface.signal_emitter();
                let _ = ShellDbusService::shell_started(emitter, &inst_id, pid).await;
            }
        })
        .detach();
    }

    fn spawn_window_closed_watch(cx: &mut App) {
        let subscription = cx.on_window_closed(|cx, window_id| {
            if !cx.has_global::<ShellRuntime>() {
                return;
            }
            let outcome = cx
                .global_mut::<ShellRuntime>()
                .shell_surfaces
                .handle_window_closed(window_id);
            crate::bar::cards::adapter::CardCoordinator::forget_window(cx, window_id);
            Self::publish_status(cx);
            match outcome {
                WindowClosedOutcome::Nothing => {}
                WindowClosedOutcome::Capture => ShellSurfaces::restore_prior_focus(cx),
                WindowClosedOutcome::ExtensionPanel(contribution) => {
                    Self::dispatch_extension_event(
                        cx,
                        shilpo_ext_api::ExtensionEvent::ContributionUnmounted {
                            contribution_id: contribution.contribution_id.to_string(),
                            instance_id: None,
                        },
                    );
                }
            }
        });
        cx.global_mut::<Self>()._window_closed = Some(subscription);
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_event_loop(
        cx: &mut App,
        mut device_rx: tokio::sync::broadcast::Receiver<shilpo_services::DeviceClientUpdate>,
        mut notif_rx: tokio::sync::broadcast::Receiver<shilpo_services::Notification>,
        mut polkit_rx: tokio::sync::watch::Receiver<shilpo_services::PolkitSnapshot>,
        mut idle_rx: tokio::sync::watch::Receiver<shilpo_services::IdleSnapshot>,
        mut mailbox_rx: tokio::sync::mpsc::Receiver<ShellCommand>,
        mut config_rx: crate::bar::service_worker::ConfigReceiver,
        device_client: shilpo_services::DeviceClient,
    ) {
        let task = cx.spawn(async move |cx| {
            let mut device_closed = false;
            let mut notif_closed = false;
            let mut polkit_closed = false;
            let mut idle_closed = false;
            let mut mailbox_closed = false;
            let mut config_closed = false;

            loop {
                if device_closed
                    && notif_closed
                    && polkit_closed
                    && idle_closed
                    && mailbox_closed
                    && config_closed
                {
                    break;
                }

                let mut device_updates = Vec::new();
                let mut notifications = Vec::new();
                let mut polkit_snapshot = None;
                let mut idle_snapshot = None;
                let mut shell_commands = Vec::new();
                let mut config_updates = Vec::new();
                let mut resync_device = false;

                tokio::select! {
                    res = device_rx.recv(), if !device_closed => {
                        match res {
                            Ok(upd) => device_updates.push(upd),
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                resync_device = true;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                device_closed = true;
                            }
                        }
                    }
                    res = notif_rx.recv(), if !notif_closed => {
                        match res {
                            Ok(notif) => notifications.push(notif),
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                notif_closed = true;
                            }
                        }
                    }
                    res = polkit_rx.changed(), if !polkit_closed => {
                        match res {
                            Ok(_) => {
                                polkit_snapshot = Some(polkit_rx.borrow_and_update().clone());
                            }
                            Err(_) => {
                                polkit_closed = true;
                            }
                        }
                    }
                    res = idle_rx.changed(), if !idle_closed => {
                        match res {
                            Ok(_) => {
                                idle_snapshot = Some(idle_rx.borrow_and_update().clone());
                            }
                            Err(_) => {
                                idle_closed = true;
                            }
                        }
                    }
                    cmd = mailbox_rx.recv(), if !mailbox_closed => {
                        match cmd {
                            Some(cmd) => shell_commands.push(cmd),
                            None => mailbox_closed = true,
                        }
                    }
                    config_upd = config_rx.recv(), if !config_closed => {
                        match config_upd {
                            Some(config_upd) => config_updates.push(config_upd),
                            None => config_closed = true,
                        }
                    }
                }

                if !device_closed {
                    loop {
                        match device_rx.try_recv() {
                            Ok(upd) => device_updates.push(upd),
                            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                                resync_device = true;
                            }
                            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                                device_closed = true;
                                break;
                            }
                            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                        }
                    }
                }

                if !notif_closed {
                    loop {
                        match notif_rx.try_recv() {
                            Ok(notif) => notifications.push(notif),
                            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                                notif_closed = true;
                                break;
                            }
                            _ => break,
                        }
                    }
                }

                if !mailbox_closed {
                    while let Ok(cmd) = mailbox_rx.try_recv() {
                        shell_commands.push(cmd);
                    }
                }

                if !config_closed {
                    while let Ok(config_upd) = config_rx.try_recv() {
                        config_updates.push(config_upd);
                    }
                }

                if resync_device {
                    for domain in shilpo_services::DeviceDomain::ALL {
                        let state = device_client.get_domain_state(domain);
                        device_updates.push(shilpo_services::DeviceClientUpdate { domain, state });
                    }
                }

                if device_updates.is_empty()
                    && notifications.is_empty()
                    && polkit_snapshot.is_none()
                    && idle_snapshot.is_none()
                    && shell_commands.is_empty()
                    && config_updates.is_empty()
                {
                    continue;
                }

                cx.update(|cx| {
                    if !cx.has_global::<ShellRuntime>() {
                        return;
                    }
                    Self::process_runtime_events(
                        cx,
                        device_updates,
                        notifications,
                        polkit_snapshot,
                        idle_snapshot,
                        shell_commands,
                        config_updates,
                    );
                });
            }
        });
        cx.global_mut::<Self>()._drain_task = task;
    }

    fn spawn_wallpaper_timer(cx: &mut App) {
        let task = cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                cx.update(|cx| {
                    if !cx.has_global::<ShellRuntime>() {
                        return;
                    }
                    let event_to_send = cx
                        .global_mut::<ShellRuntime>()
                        .wallpaper_coordinator_mut()
                        .on_wallpaper_tick(std::time::Instant::now());
                    if let Some((extension_id, event)) = event_to_send {
                        cx.global::<ShellRuntime>()
                            .extension_host()
                            .send_event_to_extension(&extension_id, event);
                    }
                });
            }
        });
        cx.global_mut::<Self>()._wallpaper_timer_task = task;
    }

    pub(super) fn process_runtime_events(
        cx: &mut App,
        device_updates: Vec<shilpo_services::DeviceClientUpdate>,
        notifications: Vec<shilpo_services::Notification>,
        polkit_snapshot: Option<shilpo_services::PolkitSnapshot>,
        idle_snapshot: Option<shilpo_services::IdleSnapshot>,
        shell_commands: Vec<ShellCommand>,
        config_updates: Vec<crate::bar::service_worker::ConfigUpdate>,
    ) {
        if !cx.has_global::<Self>() {
            return;
        }

        if let Some(snap) = polkit_snapshot {
            ShellSurfaces::sync_polkit_dialog(cx, snap.request, snap.prompt_state);
        }

        if let Some(snap) = idle_snapshot {
            ShellSurfaces::sync_idle_grace(cx, snap.active_grace);
        }

        for notif in notifications {
            ShellSurfaces::request(cx, SurfaceRequest::ShowNotification(notif));
        }

        let mut applied_device_updates = Vec::new();
        if !device_updates.is_empty()
            && let Some(hub) = cx.global_mut::<Self>().service_hub_mut()
        {
            for upd in device_updates {
                if hub.apply_domain_state(&upd.state) {
                    applied_device_updates.push(upd);
                }
            }
        }

        if !applied_device_updates.is_empty() {
            for upd in &applied_device_updates {
                match upd.domain {
                    shilpo_services::DeviceDomain::Battery => {
                        if let shilpo_services::DomainPayload::Battery(ref info) = upd.state.payload
                        {
                            if info.available && !info.is_present && upd.state.lifecycle.is_ready()
                            {
                                crate::bar::cards::adapter::CardCoordinator::dispatch(
                                    cx,
                                    crate::bar::cards::model::CardRequest::AnchorRemoved {
                                        source: crate::bar::cards::model::CardSourceId::singleton(
                                            "battery",
                                        ),
                                    },
                                );
                            }
                            crate::bar::cards::adapter::CardCoordinator::refresh_owner(
                                cx,
                                &crate::bar::cards::model::CardOwnerId::new("battery"),
                            );
                            Self::dispatch_extension_event(
                                cx,
                                shilpo_ext_api::ExtensionEvent::PowerChanged {
                                    percentage: info.is_present.then_some(info.percentage as f32),
                                    charging: info.is_charging(),
                                },
                            );
                        }
                    }
                    shilpo_services::DeviceDomain::Audio => {
                        crate::bar::cards::adapter::CardCoordinator::refresh_owner(
                            cx,
                            &crate::bar::cards::model::CardOwnerId::new("audio"),
                        );
                    }
                    shilpo_services::DeviceDomain::Network => {
                        if let shilpo_services::DomainPayload::Network(ref info) = upd.state.payload
                        {
                            crate::bar::cards::adapter::CardCoordinator::refresh_owner(
                                cx,
                                &crate::bar::cards::model::CardOwnerId::new("network"),
                            );
                            Self::dispatch_extension_event(
                                cx,
                                shilpo_ext_api::ExtensionEvent::NetworkChanged {
                                    connected: info.available && info.is_connected,
                                },
                            );
                        }
                    }
                    shilpo_services::DeviceDomain::Media => {
                        if let shilpo_services::DomainPayload::Media(ref info) = upd.state.payload {
                            crate::bar::cards::adapter::CardCoordinator::refresh_owner(
                                cx,
                                &crate::bar::cards::model::CardOwnerId::new("media"),
                            );
                            Self::dispatch_extension_event(
                                cx,
                                shilpo_ext_api::ExtensionEvent::MediaChanged {
                                    title: (!info.title.is_empty()).then_some(info.title.clone()),
                                    artist: (!info.artist.is_empty())
                                        .then_some(info.artist.clone()),
                                    playing: info.playback_state == "playing",
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }

            let handles = cx.global::<Self>().shell_surfaces().bar_handles();
            for handle in handles {
                let updates_clone = applied_device_updates.clone();
                if let Err(error) = handle.update(cx, |bar_view, _window, cx| {
                    for upd in &updates_clone {
                        bar_view.apply_domain_update(upd.domain, &upd.state, cx);
                    }
                }) {
                    tracing::debug!(
                        ?error,
                        window_id = ?handle.window_id(),
                        surface = "bar",
                        "stale window handle on device update"
                    );
                }
            }
        }

        for config_upd in config_updates {
            match config_upd {
                crate::bar::service_worker::ConfigUpdate::Loaded { config, changeset } => {
                    Self::emit_config_signal(cx, true, changeset.clone(), 0);
                    if changeset.locale {
                        crate::locale::ApplicationLocale::apply_config(
                            config.locale.as_deref(),
                            cx,
                        );
                    }
                    Self::set_active_config(cx, &config);
                    if changeset.outputs || changeset.desktop {
                        ShellSurfaces::request(cx, SurfaceRequest::SyncDisplays);
                    }
                    if changeset.extensions {
                        ShellSurfaces::reconcile_bar_extension_instances(cx);
                    }
                    let handles = cx.global::<Self>().shell_surfaces().bar_handles();
                    for handle in handles {
                        let config_clone = config.clone();
                        let changeset_clone = changeset.clone();
                        let _ = handle.update(cx, |bar_view, _window, cx| {
                            bar_view.apply_config_loaded(&config_clone, &changeset_clone, cx);
                        });
                    }
                }
                crate::bar::service_worker::ConfigUpdate::Failed { error, changeset } => {
                    Self::emit_config_signal(cx, false, changeset, 1);
                    let handles = cx.global::<Self>().shell_surfaces().bar_handles();
                    for handle in handles {
                        let err = error.clone();
                        let _ = handle.update(cx, |bar_view, _window, cx| {
                            bar_view.apply_config_failed(&err, cx);
                        });
                    }
                }
            }
        }

        for cmd in shell_commands {
            Self::execute_dbus_command(cx, cmd);
        }

        ExtensionHost::drain(cx);
    }

    pub fn on_compositor_snapshot_changed(cx: &mut App, snapshot: Arc<CompositorSnapshot>) {
        if !cx.has_global::<Self>() {
            return;
        }
        let outputs_changed = {
            let runtime = cx.global_mut::<Self>();
            let changed = runtime.shell_surfaces.latest_snapshot().outputs != snapshot.outputs;
            runtime.shell_surfaces.set_latest_snapshot(snapshot.clone());
            changed
        };
        cx.global_mut::<Self>()
            .action_dispatcher
            .update_enabled_for_snapshot(&snapshot);
        cx.global_mut::<Self>().shell_surfaces.update_readiness();
        Self::publish_status(cx);
        ShellSurfaces::refresh_bars(cx);
        crate::bar::cards::adapter::CardCoordinator::refresh_owner(
            cx,
            &crate::bar::cards::workspace_card::workspace_owner_id(),
        );
        if outputs_changed {
            ShellSurfaces::reconcile_bars(cx);
        }

        let Some(conn) = cx.global::<Self>().dbus_connection.clone() else {
            return;
        };
        let service = cx.global::<Self>().dbus_service.clone();
        let workspace_id = snapshot.focused_workspace_id.unwrap_or(0);
        let ws_str = workspace_id.to_string();
        cx.global::<Self>().extension_host().send_event(
            shilpo_ext_api::ExtensionEvent::WorkspaceChanged {
                workspace_id: ws_str.clone(),
                workspace_name: None,
                output_name: None,
            },
        );
        let event_to_send = cx
            .global_mut::<Self>()
            .wallpaper_coordinator_mut()
            .on_workspace_changed(&ws_str, None);
        if let Some((ext_id, event)) = event_to_send {
            cx.global::<Self>()
                .extension_host()
                .send_event_to_extension(&ext_id, event);
        }

        let owner_gen = snapshot.version.owner_generation;
        let rev = snapshot.version.revision;
        cx.spawn(async move |_| {
            if let Ok(iface) = conn
                .object_server()
                .interface::<_, ShellDbusService>("/org/shilpo/Shell")
                .await
            {
                let emitter = iface.signal_emitter();
                service
                    .emit_workspace_changed_if_needed(emitter, workspace_id, owner_gen, rev)
                    .await;
            }
        })
        .detach();
    }

    pub(crate) fn emit_config_signal(
        cx: &mut App,
        success: bool,
        changeset: crate::config::ConfigChangeSet,
        diagnostic_count: u32,
    ) {
        if !cx.has_global::<Self>() {
            return;
        }
        let Some(conn) = cx.global::<Self>().dbus_connection.clone() else {
            return;
        };
        let service = cx.global::<Self>().dbus_service.clone();
        cx.spawn(async move |_| {
            if let Ok(iface) = conn
                .object_server()
                .interface::<_, ShellDbusService>("/org/shilpo/Shell")
                .await
            {
                let emitter = iface.signal_emitter();
                let mut components = Vec::new();
                if changeset.theme {
                    components.push("theme".into());
                }
                if changeset.bar {
                    components.push("bar".into());
                }
                if changeset.desktop {
                    components.push("desktop".into());
                }
                if changeset.extensions {
                    components.push("extensions".into());
                }
                if changeset.outputs {
                    components.push("outputs".into());
                }
                if changeset.startup {
                    components.push("startup".into());
                }
                if changeset.capture {
                    components.push("capture".into());
                }
                if changeset.clock_format {
                    components.push("clock_format".into());
                }
                if changeset.temperature_unit {
                    components.push("temperature_unit".into());
                }
                if changeset.locale {
                    components.push("locale".into());
                }
                service
                    .emit_config_reloaded(emitter, success, components, diagnostic_count)
                    .await;
            }
        })
        .detach();
    }

    pub(crate) fn emit_theme_signal(cx: &mut App, mode: String, variant: String) {
        if !cx.has_global::<Self>() {
            return;
        }
        let Some(conn) = cx.global::<Self>().dbus_connection.clone() else {
            return;
        };
        let service = cx.global::<Self>().dbus_service.clone();
        cx.spawn(async move |_| {
            if let Ok(iface) = conn
                .object_server()
                .interface::<_, ShellDbusService>("/org/shilpo/Shell")
                .await
            {
                let emitter = iface.signal_emitter();
                service
                    .emit_theme_changed_if_needed(emitter, &mode, &variant)
                    .await;
            }
        })
        .detach();
    }

    pub fn shutdown(cx: &mut App) {
        if !cx.has_global::<Self>() {
            return;
        }
        let Some(conn) = cx.global::<Self>().dbus_connection.clone() else {
            return;
        };
        let inst_id = cx.global::<Self>().instance_id.clone();
        let shutdown_task = cx.global::<Self>().extension_host.shutdown_task(
            cx.background_executor().clone(),
            std::time::Duration::from_millis(300),
        );

        cx.spawn(async move |cx| {
            if let Ok(iface) = conn
                .object_server()
                .interface::<_, ShellDbusService>("/org/shilpo/Shell")
                .await
            {
                let emitter = iface.signal_emitter();
                let _ = ShellDbusService::shell_stopping(emitter, &inst_id).await;
            }
            if let Some(task) = shutdown_task {
                let _ = task.await;
            }
            cx.update(|cx| {
                crate::bar::cards::adapter::CardCoordinator::dispatch(
                    cx,
                    crate::bar::cards::model::CardRequest::Shutdown,
                );
                crate::bar::cards::adapter::CardCoordinator::destroy_all_bands(cx);
                let windows = cx
                    .global_mut::<Self>()
                    .shell_surfaces
                    .take_windows_for_shutdown();
                for (_, (handle, _)) in windows.bars {
                    let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
                }
                for (_, (handle, _)) in windows.extension_surfaces {
                    let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
                }
                if let Some((handle, _)) = windows.extension_panel {
                    let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
                }
                if let Some((_, _, handle)) = windows.notification {
                    let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
                }
                if let Some((_, handle, _)) = windows.polkit {
                    let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
                }
                if let Some((_, handle, _)) = windows.idle_grace {
                    let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
                }
                if let Some((_, handle)) = windows.capture {
                    let _ = cx.update_window(*handle, |_, window, _| window.remove_window());
                }
                cx.global_mut::<Self>()._drain_task = gpui::Task::ready(());
                cx.global_mut::<Self>()._wallpaper_timer_task = gpui::Task::ready(());
                cx.global_mut::<Self>().service_hub = None;
                Self::publish_status(cx);
                cx.quit();
            });
        })
        .detach();
    }

    pub(crate) fn report_idle_grace_completed(cx: &mut App, grace_generation: u64) {
        if !cx.has_global::<Self>() {
            return;
        }
        if let Some(hub) = cx.global::<Self>().service_hub.as_ref() {
            hub.idle().report_grace_completed(grace_generation);
        }
    }

    #[cfg(test)]
    pub(crate) fn install_for_event_test(
        cx: &mut App,
    ) -> (
        tokio::sync::broadcast::Sender<shilpo_services::DeviceClientUpdate>,
        tokio::sync::broadcast::Sender<shilpo_services::Notification>,
        tokio::sync::mpsc::Sender<ShellCommand>,
        crate::bar::service_worker::ConfigSender,
    ) {
        let root =
            std::env::temp_dir().join(format!("shilpo-shell-event-test-{}", uuid::Uuid::new_v4()));
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let compositor_broker = Arc::new(Mutex::new(None));
        let status = Arc::new(arc_swap::ArcSwap::from_pointee(ShellStatus::default()));
        let telemetry = Arc::new(arc_swap::ArcSwap::from_pointee(ShellTelemetry::default()));
        let dbus_service = Arc::new(ShellDbusService::new(
            tx.clone(),
            compositor_broker.clone(),
            status.clone(),
            telemetry.clone(),
        ));
        let wallpaper_preview = cx.new(WallpaperPreviewResource::new);
        let (notif_tx, notif_rx) = tokio::sync::broadcast::channel(16);
        let (device_tx, device_rx) = tokio::sync::broadcast::channel(16);
        let (config_tx, config_rx) = tokio::sync::mpsc::channel(16);
        let device_client = shilpo_services::DeviceClient::new();
        let hub = ServiceHub::new_offline_for_test();
        let polkit_rx = hub.polkit().subscribe();
        let idle_rx = hub.idle().subscribe();
        cx.set_global(Self {
            dbus_service,
            _compositor_broker: compositor_broker,
            _status: status,
            _telemetry: telemetry,
            dbus_connection: None,
            instance_id: uuid::Uuid::new_v4().to_string(),
            active_config: crate::config::ShellConfig::default(),
            shell_surfaces: ShellSurfaces::new(Arc::new(CompositorSnapshot::default())),
            action_dispatcher: ActionDispatcher::new(),
            extension_host: ExtensionHost::new(None),
            wallpaper_coordinator: WallpaperCoordinator::new(),
            wallpaper_preview,
            service_hub: Some(hub),
            session_state: crate::config::ShellSessionState::default(),
            session_path: root.join("session.json"),
            heed_store: None,
            _start_time: std::time::Instant::now(),
            _window_closed: None,
            _wallpaper_preview_changed: None,
            _drain_task: gpui::Task::ready(()),
            _wallpaper_timer_task: gpui::Task::ready(()),
        });
        Self::spawn_event_loop(
            cx,
            device_rx,
            notif_rx,
            polkit_rx,
            idle_rx,
            rx,
            config_rx,
            device_client.clone(),
        );
        (device_tx, notif_tx, tx, config_tx)
    }

    /// Like [`install_for_event_test`] but returns the [`DeviceClient`] directly so that tests
    /// can drive state through [`DeviceClient::update_local_domain_state`] and verify the
    /// full end-to-end path (client gate → broadcast → runtime → `ServiceHub`).
    #[cfg(test)]
    pub(crate) fn install_for_event_test_with_client(
        cx: &mut App,
    ) -> (
        std::sync::Arc<shilpo_services::DeviceClient>,
        tokio::sync::broadcast::Sender<shilpo_services::Notification>,
        tokio::sync::mpsc::Sender<ShellCommand>,
        crate::bar::service_worker::ConfigSender,
    ) {
        let root =
            std::env::temp_dir().join(format!("shilpo-shell-event-test-{}", uuid::Uuid::new_v4()));
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let compositor_broker = Arc::new(Mutex::new(None));
        let status = Arc::new(arc_swap::ArcSwap::from_pointee(ShellStatus::default()));
        let telemetry = Arc::new(arc_swap::ArcSwap::from_pointee(ShellTelemetry::default()));
        let dbus_service = Arc::new(ShellDbusService::new(
            tx.clone(),
            compositor_broker.clone(),
            status.clone(),
            telemetry.clone(),
        ));
        let wallpaper_preview = cx.new(WallpaperPreviewResource::new);
        let (notif_tx, notif_rx) = tokio::sync::broadcast::channel(16);
        let (config_tx, config_rx) = tokio::sync::mpsc::channel(16);
        let device_client = shilpo_services::DeviceClient::new();
        let device_rx = device_client.subscribe_updates();
        let hub = ServiceHub::new_offline_for_test_with_client(device_client.clone());
        let polkit_rx = hub.polkit().subscribe();
        let idle_rx = hub.idle().subscribe();
        cx.set_global(Self {
            dbus_service,
            _compositor_broker: compositor_broker,
            _status: status,
            _telemetry: telemetry,
            dbus_connection: None,
            instance_id: uuid::Uuid::new_v4().to_string(),
            active_config: crate::config::ShellConfig::default(),
            shell_surfaces: ShellSurfaces::new(Arc::new(CompositorSnapshot::default())),
            action_dispatcher: ActionDispatcher::new(),
            extension_host: ExtensionHost::new(None),
            wallpaper_coordinator: WallpaperCoordinator::new(),
            wallpaper_preview,
            service_hub: Some(hub),
            session_state: crate::config::ShellSessionState::default(),
            session_path: root.join("session.json"),
            heed_store: None,
            _start_time: std::time::Instant::now(),
            _window_closed: None,
            _wallpaper_preview_changed: None,
            _drain_task: gpui::Task::ready(()),
            _wallpaper_timer_task: gpui::Task::ready(()),
        });
        Self::spawn_event_loop(
            cx,
            device_rx,
            notif_rx,
            polkit_rx,
            idle_rx,
            rx,
            config_rx,
            device_client.clone(),
        );
        (std::sync::Arc::new(device_client), notif_tx, tx, config_tx)
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManagerSleepSignal {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// How long the watch will hold suspend for the locker to report readiness before giving
/// up and releasing the inhibitor anyway. Suspend is not blocked indefinitely by a locker
/// that never starts or never locks.
const LOCK_ON_SUSPEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Acquires a logind "delay" sleep inhibitor, returning the held fd. Held fds are what
/// keep the inhibitor active; dropping (closing) the fd releases it and lets suspend
/// proceed. See `logind`'s `Inhibit` documentation: a "delay" lock postpones sleep for a
/// bounded time (systemd's own `InhibitDelayMaxSec`, typically a few seconds) rather than
/// blocking it outright.
async fn acquire_sleep_delay_inhibitor(conn: &zbus::Connection) -> Option<std::os::fd::OwnedFd> {
    let result: Result<zbus::zvariant::OwnedFd, zbus::Error> = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            &(
                "sleep",
                "Shilpo",
                "Ensure the session is locked before suspend",
                "delay",
            ),
        )
        .await
        .and_then(|reply| reply.body().deserialize());

    match result {
        Ok(fd) => Some(fd.into()),
        Err(err) => {
            tracing::warn!(%err, "failed to acquire logind sleep delay inhibitor; lock-on-suspend will not block suspend");
            None
        }
    }
}

/// Watches logind's `PrepareForSleep` signal and locks the session before the system
/// suspends, holding a logind delay inhibitor so suspend actually waits (up to
/// [`LOCK_ON_SUSPEND_TIMEOUT`]) for the locker to report every output's surface is
/// committed before releasing it. Suspend triggered by `IdleAction::LockAndSuspend` gets a
/// similar (best-effort, non-blocking) head start independently, in `shilpo-services`'
/// idle action sink.
fn spawn_prepare_for_sleep_lock_watch(
    lock_supervisor: Arc<shilpo_services::lock_supervisor::LockSupervisor>,
) {
    tokio::spawn(async move {
        let Ok(conn) = zbus::Connection::system().await else {
            tracing::warn!("system D-Bus unavailable; lock-on-suspend watch disabled");
            return;
        };
        let Ok(proxy) = LoginManagerSleepSignalProxy::new(&conn).await else {
            tracing::warn!("failed to build logind Manager proxy for lock-on-suspend watch");
            return;
        };
        let Ok(mut stream) = proxy.receive_prepare_for_sleep().await else {
            tracing::warn!("failed to subscribe to PrepareForSleep for lock-on-suspend watch");
            return;
        };

        let mut held_inhibitor = acquire_sleep_delay_inhibitor(&conn).await;

        use futures_lite::StreamExt;
        while let Some(signal) = stream.next().await {
            let Ok(args) = signal.args() else { continue };

            if args.start {
                // Block the locker's readiness wait on a thread: this is real blocking
                // I/O (a FIFO open), not something with an async equivalent here.
                let supervisor = lock_supervisor.clone();
                let locked = tokio::task::spawn_blocking(move || {
                    supervisor
                        .spawn_and_wait_until_locked("PrepareForSleep", LOCK_ON_SUSPEND_TIMEOUT)
                })
                .await
                .unwrap_or(false);

                if !locked {
                    tracing::warn!(
                        "lock-on-suspend timed out waiting for the session to lock; suspending anyway"
                    );
                }

                // Release the inhibitor: this is what actually allows suspend to proceed.
                held_inhibitor = None;
            } else {
                // Resumed: re-acquire a fresh inhibitor for the next sleep cycle.
                held_inhibitor = acquire_sleep_delay_inhibitor(&conn).await;
            }
        }

        drop(held_inhibitor);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use shilpo_services::{
        AudioPayload, BatteryPayload, BrightnessPayload, DeviceClientUpdate, DeviceDomain,
        DomainLifecycle, DomainPayload, DomainState, DomainVersion, MediaPayload, NetworkPayload,
        ServiceLifecycle,
    };

    #[gpui::test]
    fn test_event_driven_domain_update_without_advancing_timer(cx: &mut gpui::TestAppContext) {
        let (device_tx, _notif_tx, _cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test(app)
        });

        let update = DeviceClientUpdate {
            domain: DeviceDomain::Battery,
            state: DomainState {
                domain: DeviceDomain::Battery,
                version: DomainVersion::new(1, 1),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 92,
                    ..Default::default()
                }),
                error: None,
            },
        };

        device_tx.send(update).unwrap();

        // Exactly one drain cycle driven purely by event arrival (no cx.advance_clock!)
        cx.run_until_parked();

        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 92);
            assert!(snap.battery.available);
            let domain_state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(domain_state.version, DomainVersion::new(1, 1));
            assert_eq!(domain_state.lifecycle, DomainLifecycle::Ready);
        });
    }

    #[gpui::test]
    fn test_stale_and_equal_domain_version_rejected(cx: &mut gpui::TestAppContext) {
        let (device_tx, _notif_tx, _cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test(app)
        });

        // 1. Send version (1, 2) -> should apply
        device_tx
            .send(DeviceClientUpdate {
                domain: DeviceDomain::Battery,
                state: DomainState {
                    domain: DeviceDomain::Battery,
                    version: DomainVersion::new(1, 2),
                    lifecycle: DomainLifecycle::Ready,
                    payload: DomainPayload::Battery(BatteryPayload {
                        available: true,
                        is_present: true,
                        percentage: 95,
                        ..Default::default()
                    }),
                    error: None,
                },
            })
            .unwrap();
        cx.run_until_parked();

        cx.update(|app| {
            assert_eq!(ShellRuntime::device_snapshot(app).battery.percentage, 95);
        });

        // 2. Send equal version (1, 2) with different percentage -> should NOT apply
        device_tx
            .send(DeviceClientUpdate {
                domain: DeviceDomain::Battery,
                state: DomainState {
                    domain: DeviceDomain::Battery,
                    version: DomainVersion::new(1, 2),
                    lifecycle: DomainLifecycle::Ready,
                    payload: DomainPayload::Battery(BatteryPayload {
                        available: true,
                        is_present: true,
                        percentage: 50,
                        ..Default::default()
                    }),
                    error: None,
                },
            })
            .unwrap();
        cx.run_until_parked();

        cx.update(|app| {
            assert_eq!(ShellRuntime::device_snapshot(app).battery.percentage, 95);
        });

        // 3. Send stale version (1, 1) with different percentage -> should NOT apply
        device_tx
            .send(DeviceClientUpdate {
                domain: DeviceDomain::Battery,
                state: DomainState {
                    domain: DeviceDomain::Battery,
                    version: DomainVersion::new(1, 1),
                    lifecycle: DomainLifecycle::Ready,
                    payload: DomainPayload::Battery(BatteryPayload {
                        available: true,
                        is_present: true,
                        percentage: 20,
                        ..Default::default()
                    }),
                    error: None,
                },
            })
            .unwrap();
        cx.run_until_parked();

        cx.update(|app| {
            assert_eq!(ShellRuntime::device_snapshot(app).battery.percentage, 95);
        });
    }

    #[gpui::test]
    fn test_burst_coalescing_multiple_streams(cx: &mut gpui::TestAppContext) {
        let (device_tx, notif_tx, cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test(app)
        });

        // Burst send across multiple streams
        device_tx
            .send(DeviceClientUpdate {
                domain: DeviceDomain::Battery,
                state: DomainState {
                    domain: DeviceDomain::Battery,
                    version: DomainVersion::new(1, 1),
                    lifecycle: DomainLifecycle::Ready,
                    payload: DomainPayload::Battery(BatteryPayload {
                        available: true,
                        is_present: true,
                        percentage: 88,
                        ..Default::default()
                    }),
                    error: None,
                },
            })
            .unwrap();

        notif_tx
            .send(shilpo_services::Notification {
                id: 1,
                app_name: "TestApp".into(),
                summary: "Burst Notif".into(),
                body: "Body".into(),
                app_icon: None,
                desktop_entry: None,
                image_path: None,
                urgency: shilpo_services::NotificationUrgency::Normal,
                actions: Vec::new(),
                expire_timeout_ms: 5000,
                timestamp: chrono::Local::now(),
            })
            .unwrap();

        cmd_tx
            .try_send(ShellCommand::EmitTestNotification {
                title: "Cmd Notif".into(),
                body: "Cmd Body".into(),
            })
            .unwrap();

        cx.run_until_parked();

        cx.update(|app| {
            assert_eq!(ShellRuntime::device_snapshot(app).battery.percentage, 88);
            let notif_hist = ShellRuntime::notification_history(app);
            assert!(
                notif_hist
                    .iter()
                    .any(|n| n.summary == "Cmd Notif" || n.summary == "Burst Notif")
            );
        });
    }

    #[gpui::test]
    fn test_lagged_broadcast_resync(cx: &mut gpui::TestAppContext) {
        let (device_tx, _notif_tx, _cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test(app)
        });

        // Overflow the channel (capacity is 16) to trigger Lagged
        for i in 1..=25 {
            let _ = device_tx.send(DeviceClientUpdate {
                domain: DeviceDomain::Battery,
                state: DomainState {
                    domain: DeviceDomain::Battery,
                    version: DomainVersion::new(1, i),
                    lifecycle: DomainLifecycle::Ready,
                    payload: DomainPayload::Battery(BatteryPayload {
                        available: true,
                        is_present: true,
                        percentage: i as u8,
                        ..Default::default()
                    }),
                    error: None,
                },
            });
        }

        // Draining should recover from Lagged by resyncing from device_client.
        //
        // All 25 sends land before the task is ever polled, so the broadcast
        // channel's overflow behavior is deterministic: with capacity 16, the
        // oldest 9 (versions 1..=9) are dropped and versions 10..=25 remain
        // buffered. The task observes exactly one `Lagged(9)`, then drains the
        // 16 surviving messages in order, ending on version (1, 25).
        cx.run_until_parked();

        cx.update(|app| {
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(
                state.version,
                DomainVersion::new(1, 25),
                "post-lag drain must end on the newest surviving message, not a stale or resync-clobbered version"
            );
            assert_eq!(
                state.payload,
                DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 25,
                    ..Default::default()
                })
            );
            assert_eq!(state.lifecycle, DomainLifecycle::Ready);
        });
    }

    #[gpui::test]
    fn test_shutdown_cancels_event_loop_and_stops_publishing(cx: &mut gpui::TestAppContext) {
        let (device_tx, _notif_tx, _cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test(app)
        });

        let update = DeviceClientUpdate {
            domain: DeviceDomain::Battery,
            state: DomainState {
                domain: DeviceDomain::Battery,
                version: DomainVersion::new(1, 1),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 50,
                    ..Default::default()
                }),
                error: None,
            },
        };
        let _ = device_tx.send(update);
        cx.run_until_parked();
        cx.update(|app| {
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Battery).payload,
                DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 50,
                    ..Default::default()
                })
            );
        });

        // Simulate the cancellation `ShellRuntime::shutdown` performs: dropping the
        // task handle cancels the spawned event loop.
        cx.update(|app| {
            app.global_mut::<ShellRuntime>()._drain_task = gpui::Task::ready(());
            app.global_mut::<ShellRuntime>()._wallpaper_timer_task = gpui::Task::ready(());
        });
        cx.run_until_parked();

        let post_shutdown_update = DeviceClientUpdate {
            domain: DeviceDomain::Battery,
            state: DomainState {
                domain: DeviceDomain::Battery,
                version: DomainVersion::new(1, 2),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 99,
                    ..Default::default()
                }),
                error: None,
            },
        };
        let _ = device_tx.send(post_shutdown_update);
        cx.run_until_parked();

        cx.update(|app| {
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Battery).payload,
                DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 50,
                    ..Default::default()
                }),
                "no update should be applied after the event loop task is cancelled"
            );
        });
    }

    #[gpui::test]
    fn test_degraded_projection_and_recovery(cx: &mut gpui::TestAppContext) {
        let (device_tx, _notif_tx, _cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test(app)
        });

        // 1. Initial healthy state (Ready)
        let healthy_update = DeviceClientUpdate {
            domain: DeviceDomain::Battery,
            state: DomainState {
                domain: DeviceDomain::Battery,
                version: DomainVersion::new(1, 1),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 90,
                    ..Default::default()
                }),
                error: None,
            },
        };
        device_tx.send(healthy_update).unwrap();
        cx.run_until_parked();

        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 90);
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(state.lifecycle, DomainLifecycle::Ready);
        });

        // 2. Degraded projection with error
        let degraded_update = DeviceClientUpdate {
            domain: DeviceDomain::Battery,
            state: DomainState {
                domain: DeviceDomain::Battery,
                version: DomainVersion::new(1, 2),
                lifecycle: DomainLifecycle::Degraded,
                payload: DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 90,
                    ..Default::default()
                }),
                error: Some("upower dbus timeout".into()),
            },
        };
        device_tx.send(degraded_update).unwrap();
        cx.run_until_parked();

        cx.update(|app| {
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(state.lifecycle, DomainLifecycle::Degraded);
            assert_eq!(state.error.as_deref(), Some("upower dbus timeout"));
            assert_eq!(state.version, DomainVersion::new(1, 2));

            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 90);

            let health = ShellRuntime::service_health(app).unwrap();
            assert!(!health.battery_service_available);
            assert_eq!(
                health.battery_last_error.as_deref(),
                Some("upower dbus timeout")
            );

            // Known gap: `to_service_lifecycle` (service_hub.rs:307) folds both
            // `DomainLifecycle::Degraded` and `DomainLifecycle::Unavailable` into
            // `ServiceLifecycle::Unavailable`, so `battery_state` alone cannot
            // distinguish a degraded domain from an absent one — `battery_last_error`
            // is the only field that does. Asserting the current (lossy) behavior
            // here; the fold is not fixed in this change.
            assert_eq!(health.battery_state, ServiceLifecycle::Unavailable);
        });

        // 3. Recovery to Ready
        let recovery_update = DeviceClientUpdate {
            domain: DeviceDomain::Battery,
            state: DomainState {
                domain: DeviceDomain::Battery,
                version: DomainVersion::new(1, 3),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 95,
                    ..Default::default()
                }),
                error: None,
            },
        };
        device_tx.send(recovery_update).unwrap();
        cx.run_until_parked();

        cx.update(|app| {
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(state.lifecycle, DomainLifecycle::Ready);
            assert_eq!(state.error, None);

            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 95);

            let health = ShellRuntime::service_health(app).unwrap();
            assert!(health.battery_service_available);
            assert_eq!(health.battery_state, ServiceLifecycle::Ready);
        });
    }

    #[gpui::test]
    fn test_end_to_end_delivery_through_device_client(cx: &mut gpui::TestAppContext) {
        let (client, _notif_tx, _cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test_with_client(app)
        });

        // 1. Initial state: stale_updates is 0
        assert_eq!(client.stale_updates(), 0);

        // 2. Deliver valid update via DeviceClient::update_local_domain_state at new(0, 1)
        client.update_local_domain_state(DomainState {
            domain: DeviceDomain::Battery,
            version: DomainVersion::new(0, 1),
            lifecycle: DomainLifecycle::Ready,
            payload: DomainPayload::Battery(BatteryPayload {
                available: true,
                is_present: true,
                percentage: 77,
                ..Default::default()
            }),
            error: None,
        });
        cx.run_until_parked();

        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 77);
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(state.version, DomainVersion::new(0, 1));
            assert_eq!(state.lifecycle, DomainLifecycle::Ready);
        });
        assert_eq!(client.stale_updates(), 0);

        // 3. Push equal-version conflicting update at new(0, 1) with different lifecycle/error
        client.update_local_domain_state(DomainState {
            domain: DeviceDomain::Battery,
            version: DomainVersion::new(0, 1),
            lifecycle: DomainLifecycle::Degraded,
            payload: DomainPayload::Battery(BatteryPayload {
                available: true,
                is_present: true,
                percentage: 10,
                ..Default::default()
            }),
            error: Some("stale conflict".into()),
        });
        cx.run_until_parked();

        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 77);
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(state.version, DomainVersion::new(0, 1));
        });
        assert_eq!(client.stale_updates(), 1);

        // 4. Push future-generation update at new(9, 1)
        client.update_local_domain_state(DomainState {
            domain: DeviceDomain::Battery,
            version: DomainVersion::new(9, 1),
            lifecycle: DomainLifecycle::Ready,
            payload: DomainPayload::Battery(BatteryPayload {
                available: true,
                is_present: true,
                percentage: 50,
                ..Default::default()
            }),
            error: None,
        });
        cx.run_until_parked();

        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 77);
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(state.version, DomainVersion::new(0, 1));
        });
        assert_eq!(client.stale_updates(), 2);

        // 5. Push valid newer revision at new(0, 2)
        client.update_local_domain_state(DomainState {
            domain: DeviceDomain::Battery,
            version: DomainVersion::new(0, 2),
            lifecycle: DomainLifecycle::Ready,
            payload: DomainPayload::Battery(BatteryPayload {
                available: true,
                is_present: true,
                percentage: 81,
                ..Default::default()
            }),
            error: None,
        });
        cx.run_until_parked();

        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.battery.percentage, 81);
            let state = ShellRuntime::domain_state(app, DeviceDomain::Battery);
            assert_eq!(state.version, DomainVersion::new(0, 2));
        });
        assert_eq!(client.stale_updates(), 2);
    }

    #[gpui::test]
    fn test_multi_domain_versions_advance_independently(cx: &mut gpui::TestAppContext) {
        let (device_tx, _notif_tx, _cmd_tx, _cfg_tx) = cx.update(|app| {
            shilpo_m3e::init(app);
            ShellRuntime::install_for_event_test(app)
        });

        // 1. Send all five domains with different, non-monotonic revisions in a single burst:
        // Battery at (1, 5), Audio at (1, 1), Network at (1, 9), Media at (1, 2), Brightness at (1, 7).
        // Send order is intentionally non-monotonic across domains.
        let audio_update = DeviceClientUpdate {
            domain: DeviceDomain::Audio,
            state: DomainState {
                domain: DeviceDomain::Audio,
                version: DomainVersion::new(1, 1),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Audio(AudioPayload {
                    available: true,
                    volume: 60,
                    ..Default::default()
                }),
                error: None,
            },
        };
        let network_update = DeviceClientUpdate {
            domain: DeviceDomain::Network,
            state: DomainState {
                domain: DeviceDomain::Network,
                version: DomainVersion::new(1, 9),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Network(NetworkPayload {
                    available: true,
                    ssid: "Wifi-Net".into(),
                    ..Default::default()
                }),
                error: None,
            },
        };
        let battery_update = DeviceClientUpdate {
            domain: DeviceDomain::Battery,
            state: DomainState {
                domain: DeviceDomain::Battery,
                version: DomainVersion::new(1, 5),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Battery(BatteryPayload {
                    available: true,
                    is_present: true,
                    percentage: 85,
                    ..Default::default()
                }),
                error: None,
            },
        };
        let brightness_update = DeviceClientUpdate {
            domain: DeviceDomain::Brightness,
            state: DomainState {
                domain: DeviceDomain::Brightness,
                version: DomainVersion::new(1, 7),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Brightness(BrightnessPayload {
                    available: true,
                    percentage: 75,
                    ..Default::default()
                }),
                error: None,
            },
        };
        let media_update = DeviceClientUpdate {
            domain: DeviceDomain::Media,
            state: DomainState {
                domain: DeviceDomain::Media,
                version: DomainVersion::new(1, 2),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Media(MediaPayload {
                    available: true,
                    title: "Song Title".into(),
                    playback_state: "playing".into(),
                    ..Default::default()
                }),
                error: None,
            },
        };

        device_tx.send(audio_update).unwrap();
        device_tx.send(network_update).unwrap();
        device_tx.send(battery_update).unwrap();
        device_tx.send(brightness_update).unwrap();
        device_tx.send(media_update).unwrap();

        cx.run_until_parked();

        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.audio.volume, 60);
            assert_eq!(snap.network.ssid.as_deref(), Some("Wifi-Net"));
            assert_eq!(snap.battery.percentage, 85);
            assert_eq!(snap.brightness.percentage, 75);
            assert_eq!(snap.media.title, "Song Title");

            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Audio).version,
                DomainVersion::new(1, 1)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Network).version,
                DomainVersion::new(1, 9)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Battery).version,
                DomainVersion::new(1, 5)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Brightness).version,
                DomainVersion::new(1, 7)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Media).version,
                DomainVersion::new(1, 2)
            );
        });

        // 2. Send stale update for Network only at (1, 3) with a different payload
        let stale_network_update = DeviceClientUpdate {
            domain: DeviceDomain::Network,
            state: DomainState {
                domain: DeviceDomain::Network,
                version: DomainVersion::new(1, 3),
                lifecycle: DomainLifecycle::Ready,
                payload: DomainPayload::Network(NetworkPayload {
                    available: true,
                    ssid: "Stale-Net".into(),
                    ..Default::default()
                }),
                error: None,
            },
        };
        device_tx.send(stale_network_update).unwrap();
        cx.run_until_parked();

        // 3. Verify Network is untouched and all other domains are also untouched
        cx.update(|app| {
            let snap = ShellRuntime::device_snapshot(app);
            assert_eq!(snap.network.ssid.as_deref(), Some("Wifi-Net"));
            assert_eq!(snap.audio.volume, 60);
            assert_eq!(snap.battery.percentage, 85);
            assert_eq!(snap.brightness.percentage, 75);
            assert_eq!(snap.media.title, "Song Title");

            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Network).version,
                DomainVersion::new(1, 9)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Audio).version,
                DomainVersion::new(1, 1)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Battery).version,
                DomainVersion::new(1, 5)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Brightness).version,
                DomainVersion::new(1, 7)
            );
            assert_eq!(
                ShellRuntime::domain_state(app, DeviceDomain::Media).version,
                DomainVersion::new(1, 2)
            );
        });
    }
}
