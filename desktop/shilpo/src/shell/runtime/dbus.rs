//! D-Bus command drain loop and status publishing for ShellRuntime.

use gpui::App;

use super::{ShellRuntime, ShellSurfaces};
use crate::shell::{
    actions::ActionInvocation,
    bar::service_worker::{self, WorkerCommand},
    dbus::{ShellCommand, ShellStatus, ShellTelemetry},
    error::ShellError,
};

impl ShellRuntime {
    pub fn save_audio_preference(cx: &App, device: Option<String>, port: Option<String>) {
        if cx.has_global::<Self>()
            && let Some(store) = cx.global::<Self>().heed_store()
        {
            let mut pref = store.get_audio_preference().unwrap_or_default();
            if device.is_some() {
                pref.default_device = device;
            }
            if port.is_some() {
                pref.default_port = port;
            }
            let _ = store.save_audio_preference(&pref);
        }
    }

    pub(crate) fn reload_config(cx: &mut App) -> Result<(), ShellError> {
        let handle = Self::service_commands(cx)
            .ok_or_else(|| ShellError::ActionFailed("service worker unavailable".into()))?;
        service_worker::try_send_command(&handle, WorkerCommand::ReloadConfig)
            .map_err(|error| ShellError::ActionFailed(error.to_string()))?;
        Ok(())
    }

    pub fn save_output_bar(
        cx: &mut App,
        output_name: &str,
        state: &shilpo_services::OutputBarState,
    ) {
        if cx.has_global::<Self>()
            && let Some(store) = cx.global::<Self>().heed_store()
        {
            let _ = store.put_output_bar(output_name, state);
        }
    }

    pub fn load_output_bar(cx: &App, output_name: &str) -> Option<shilpo_services::OutputBarState> {
        if cx.has_global::<Self>()
            && let Some(store) = cx.global::<Self>().heed_store()
        {
            store.get_output_bar(output_name).ok().flatten()
        } else {
            None
        }
    }

    pub(super) fn execute_dbus_command(cx: &mut App, cmd: ShellCommand) {
        if !cx.has_global::<Self>() {
            return;
        }
        match cmd {
            ShellCommand::ShowBar => {
                if !ShellSurfaces::has_bars(cx) {
                    let _ = Self::dispatch_action(cx, ActionInvocation::ToggleBar);
                }
            }
            ShellCommand::HideBar => {
                if ShellSurfaces::has_bars(cx) {
                    let _ = Self::dispatch_action(cx, ActionInvocation::ToggleBar);
                }
            }
            ShellCommand::ToggleBar => {
                let _ = Self::dispatch_action(cx, ActionInvocation::ToggleBar);
            }
            ShellCommand::ShowOverview => {
                if !ShellSurfaces::is_overview_open(cx) {
                    let _ = Self::dispatch_action(cx, ActionInvocation::ToggleOverview);
                }
            }
            ShellCommand::HideOverview => {
                if ShellSurfaces::is_overview_open(cx) {
                    let _ = Self::dispatch_action(cx, ActionInvocation::ToggleOverview);
                }
            }
            ShellCommand::ToggleOverview => {
                let _ = Self::dispatch_action(cx, ActionInvocation::ToggleOverview);
            }
            ShellCommand::ReloadConfig => {
                let _ = Self::dispatch_action(cx, ActionInvocation::ReloadConfig);
            }
            ShellCommand::SetBrightness(pct) => {
                Self::dispatch_device_command(
                    cx,
                    shilpo_services::DeviceCommand::Brightness(
                        shilpo_services::BrightnessAction::SetBrightness(pct),
                    ),
                );
            }
            ShellCommand::SetDisplayBrightness {
                display_id,
                percentage,
            } => {
                Self::dispatch_device_command(
                    cx,
                    shilpo_services::DeviceCommand::Brightness(
                        shilpo_services::BrightnessAction::SetDisplay {
                            id: display_id,
                            percentage,
                        },
                    ),
                );
            }
            ShellCommand::Capture(intent) => {
                ShellSurfaces::request(cx, super::SurfaceRequest::OpenCapture(intent));
            }
            ShellCommand::InvokeAction {
                action_id,
                payload_json,
            } => match action_id.parse::<crate::shell::actions::ActionId>() {
                Ok(id) => {
                    let payload = payload_json.and_then(|p| serde_json::from_str(&p).ok());
                    match crate::shell::actions::ActionInvocation::from_id_and_payload(id, payload)
                    {
                        Ok(invocation) => {
                            if let Err(error) = ShellRuntime::dispatch_action(cx, invocation) {
                                tracing::warn!(%error, "D-Bus action dispatch failed");
                            }
                        }
                        Err(error) => tracing::warn!(%error, "D-Bus action payload rejected"),
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, action = %action_id, "D-Bus action ID rejected")
                }
            },
            ShellCommand::NextWallpaper => {
                ShellRuntime::request_next_wallpaper(cx);
            }
            ShellCommand::EmitTestNotification { title, body } => {
                let notif = shilpo_services::Notification {
                    id: 0,
                    app_name: "Shilpo Debug".to_string(),
                    summary: title,
                    body,
                    app_icon: Some("dialog-information".to_string()),
                    desktop_entry: None,
                    image_path: None,
                    urgency: shilpo_services::NotificationUrgency::Normal,
                    actions: Vec::new(),
                    expire_timeout_ms: 5000,
                    timestamp: chrono::Local::now(),
                };
                if let Some(hub) = cx.global::<Self>().service_hub() {
                    hub.push_notification(notif);
                } else {
                    tracing::warn!("service hub unavailable for test notification");
                }
            }
            ShellCommand::ForgetSearchResult(canonical_id) => {
                if cx.has_global::<Self>()
                    && let Some(store) = cx.global::<Self>().heed_store()
                    && let Err(error) = store.forget_search_result(&canonical_id)
                {
                    tracing::warn!(%error, canonical_id = %canonical_id, "failed to forget search result");
                }
            }
            ShellCommand::ClearSearchLearning => {
                if cx.has_global::<Self>()
                    && let Some(store) = cx.global::<Self>().heed_store()
                    && let Err(error) = store.clear_search_learning()
                {
                    tracing::warn!(%error, "failed to clear search learning");
                }
            }
            ShellCommand::ResetNotificationQuarantine => {
                if let Some(hub) = cx.global::<Self>().service_hub() {
                    hub.reset_notification_quarantine();
                } else {
                    tracing::warn!("service hub unavailable for notification quarantine reset");
                }
            }
            ShellCommand::ResetDeviceQuarantine => {
                if let Some(hub) = cx.global::<Self>().service_hub() {
                    hub.reset_device_quarantine();
                } else {
                    tracing::warn!("service hub unavailable for device quarantine reset");
                }
            }
            ShellCommand::Lock => {
                // The locker is a dedicated process (ADR-0005, #135): a client that dies
                // while the session is locked may leave it locked permanently, so nothing
                // that can crash for unrelated reasons (this shell process included)
                // owns ext-session-lock-v1 directly. Goes through the shared
                // LockSupervisor so telemetry/doctor see the same state idle-triggered
                // and suspend-triggered locks do.
                if let Some(hub) = cx.global::<Self>().service_hub() {
                    hub.lock_supervisor()
                        .spawn("org.shilpo.Shell.Lock() D-Bus call");
                } else {
                    tracing::warn!("service hub unavailable for lock");
                }
            }
        }
        Self::publish_status(cx);
    }

    pub(super) fn publish_status(cx: &mut App) {
        if !cx.has_global::<Self>() {
            return;
        }
        let runtime = cx.global::<Self>();
        let is_running = runtime.readiness() != shilpo_services::ReadinessState::Failed;
        let readiness_str = runtime.readiness().as_str().to_string();
        let bar_state_str = if runtime.shell_surfaces.bars_are_open() {
            "visible".to_string()
        } else {
            "hidden".to_string()
        };
        let overview_visible = runtime.shell_surfaces.overview_is_open();

        let status = ShellStatus {
            running: is_running,
            instance_id: runtime.instance_id.clone(),
            pid: std::process::id(),
            readiness: readiness_str,
            bar_state: bar_state_str,
            overview_visible,
        };

        let service_health = runtime
            .service_hub
            .as_ref()
            .map(|h| h.health())
            .unwrap_or_default();

        let ext_diagnostics_json = runtime
            .extension_host
            .diagnostics()
            .and_then(|d| serde_json::to_string(&d).ok())
            .unwrap_or_else(|| "null".to_string());

        let telemetry = ShellTelemetry {
            compositor_connected: service_health.compositor_connected,
            compositor_state: service_health.compositor_state,
            compositor_owner_generation: service_health.compositor_owner_generation,
            compositor_revision: service_health.compositor_revision,
            compositor_reconnect_attempt: service_health.compositor_reconnect_attempt,
            compositor_last_error: service_health.compositor_last_error.unwrap_or_default(),
            battery_service_available: service_health.battery_service_available,
            battery_state: service_health.battery_state.as_str().to_string(),
            battery_last_error: service_health.battery_last_error.unwrap_or_default(),
            audio_service_available: service_health.audio_service_available,
            audio_state: service_health.audio_state.as_str().to_string(),
            audio_last_error: service_health.audio_last_error.unwrap_or_default(),
            network_service_available: service_health.network_service_available,
            network_state: service_health.network_state.as_str().to_string(),
            network_last_error: service_health.network_last_error.unwrap_or_default(),
            notification_service_available: service_health.notification_service_available,
            notification_state: service_health.notification_state.as_str().to_string(),
            notification_last_error: service_health.notification_last_error.unwrap_or_default(),
            media_service_available: service_health.media_service_available,
            media_state: service_health.media_state.as_str().to_string(),
            media_last_error: service_health.media_last_error.unwrap_or_default(),
            brightness_service_available: service_health.brightness_service_available,
            brightness_state: service_health.brightness_state.as_str().to_string(),
            brightness_last_error: service_health.brightness_last_error.unwrap_or_default(),
            polkit_service_available: service_health.polkit_service_available,
            polkit_state: service_health.polkit_state.as_str().to_string(),
            polkit_last_error: service_health.polkit_last_error.unwrap_or_default(),
            polkit_helper_path: service_health.polkit_helper_path.unwrap_or_default(),
            idle_service_available: service_health.idle_service_available,
            idle_state: service_health.idle_state.as_str().to_string(),
            idle_last_error: service_health.idle_last_error.unwrap_or_default(),
            idle_notifier_available: service_health.idle_notifier_available,
            idle_registered_behaviors: service_health.idle_registered_behaviors,
            idle_inhibit_count: service_health.idle_inhibit_count,
            idle_unsupported_actions: service_health.idle_unsupported_actions,
            lock_service_available: service_health.lock_service_available,
            lock_state: service_health.lock_state,
            lock_session_active: service_health.lock_session_active,
            lock_last_error: service_health.lock_last_error.unwrap_or_default(),
            lock_last_spawn_reason: service_health.lock_last_spawn_reason.unwrap_or_default(),
            lock_pam_service: service_health.lock_pam_service,
            heed_store_available: service_health.heed_store_available,
            uptime_seconds: service_health.uptime_seconds,
            extension_host_diagnostics_json: ext_diagnostics_json,
        };

        runtime.dbus_service.update_status(status);
        runtime.dbus_service.update_telemetry(telemetry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn emit_test_notification_command_reaches_notification_domain(cx: &mut gpui::TestAppContext) {
        let tx = cx.update(|app| {
            shilpo_m3e::init(app);
            let tx = ShellRuntime::install_for_test(app);
            ShellRuntime::set_service_hub_for_test(
                app,
                super::super::ServiceHub::new_offline_for_test(),
            );
            tx
        });

        tx.try_send(ShellCommand::EmitTestNotification {
            title: "Test Title".into(),
            body: "Test Body".into(),
        })
        .unwrap();

        cx.run_until_parked();

        let history = cx.update(|app| {
            app.global::<ShellRuntime>()
                .service_hub()
                .expect("test service hub installed")
                .notification_history()
        });
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].summary, "Test Title");
        assert_eq!(history[0].body, "Test Body");
    }
}
