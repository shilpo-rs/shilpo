use gpui::App;
use shilpo_ext_api::CanonicalId;
use shilpo_services::{CompositorCommand, CompositorSnapshot, Notification};

use super::{ShellRuntime, ShellSurfaces, shell_surfaces::SurfaceRequest};
use crate::shell::{
    actions::{ActionDescriptor, ActionId, ActionInvocation, ActionRegistry},
    error::ShellError,
    extensions::ContributionDescriptor,
};

/// Owns the shell action registry and the keybinding table, plus the logic that
/// maps `ActionInvocation`s onto shell behavior and compositor commands.
///
/// Registry and keybinding state are private; the shell interacts with the
/// dispatcher exclusively through the method surface below.
pub struct ActionDispatcher {
    actions: ActionRegistry,
    keybindings: crate::shell::actions::KeybindingManager,
}

impl ActionDispatcher {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            actions: ActionRegistry::default(),
            keybindings: crate::shell::actions::KeybindingManager::with_defaults(),
        }
    }

    pub(crate) fn keybinding_manager(&self) -> &crate::shell::actions::KeybindingManager {
        &self.keybindings
    }

    pub(crate) fn reconcile_keybindings(
        &mut self,
        user_bindings: &[crate::config::KeybindingConfig],
        extension_shortcuts: &[ContributionDescriptor],
    ) -> crate::shell::actions::KeybindingReconciliationReport {
        let builtin_actions = self.actions.all();
        self.keybindings
            .reconcile(user_bindings, &builtin_actions, extension_shortcuts)
    }

    pub(crate) fn reset_shortcuts_to_defaults(&mut self) {
        self.keybindings.reset_to_defaults();
    }

    pub(crate) fn register_extension_action(
        &mut self,
        id: CanonicalId,
        name: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<ActionId, String> {
        self.actions.register_extension(id, name, label)
    }

    pub(crate) fn action_descriptors(&self) -> Vec<ActionDescriptor> {
        self.actions.all()
    }

    pub(crate) fn keybinding_descriptors(&self) -> Vec<(String, String)> {
        self.keybindings.keybinding_descriptors()
    }

    /// Reconciles the extension actions with the currently loaded extensions.
    pub(crate) fn sync_extension_actions(&mut self, desired: Vec<ContributionDescriptor>) {
        let existing = self
            .actions
            .all()
            .into_iter()
            .filter_map(|descriptor| descriptor.id.extension_id())
            .collect::<Vec<_>>();
        for id in existing {
            self.actions.unregister_extension(&id);
        }
        for descriptor in desired {
            if let Err(error) = self.actions.register_extension(
                descriptor.id,
                descriptor.extension_name.clone(),
                descriptor.name,
            ) {
                tracing::warn!(error = %error, "extension action registration failed");
            }
        }
    }

    /// Reflects compositor readiness and capabilities in the enabled flags of
    /// the compositor-backed actions.
    pub(crate) fn update_enabled_for_snapshot(&mut self, snapshot: &CompositorSnapshot) {
        let is_ready = snapshot.connection.is_ready();
        let set = |actions: &mut ActionRegistry, id: &ActionId, enabled: bool| {
            if let Some(desc) = actions.descriptor_mut(id) {
                desc.enabled = enabled;
            }
        };
        set(
            &mut self.actions,
            &ActionId::FocusWorkspace,
            is_ready && snapshot.capabilities.can_focus_workspace,
        );
        set(
            &mut self.actions,
            &ActionId::CreateWorkspace,
            is_ready && snapshot.capabilities.can_create_workspace,
        );
        set(
            &mut self.actions,
            &ActionId::MoveWindowToWorkspace,
            is_ready && snapshot.capabilities.can_move_window,
        );
        set(
            &mut self.actions,
            &ActionId::FocusWindow,
            is_ready && snapshot.capabilities.can_focus_window,
        );
        set(
            &mut self.actions,
            &ActionId::CloseWindow,
            is_ready && snapshot.capabilities.can_close_window,
        );
    }

    pub(crate) fn dispatch_action(
        cx: &mut App,
        action: ActionInvocation,
    ) -> Result<(), ShellError> {
        match Self::dispatch_invocation(cx, action) {
            Ok(crate::shell::actions::ActionResult::Immediate) => Ok(()),
            Ok(crate::shell::actions::ActionResult::Compositor(ticket)) => {
                cx.spawn(async move |cx| match ticket.await {
                    shilpo_services::CommandOutcome::Applied { version }
                    | shilpo_services::CommandOutcome::ReconciledApplied { version } => {
                        tracing::trace!(?version, "compositor action applied");
                    }
                    shilpo_services::CommandOutcome::Rejected { reason } => {
                        cx.update(|cx: &mut gpui::App| {
                            tracing::warn!(error = %reason, "compositor action rejected");
                            Self::show_compositor_error_message(cx, &reason.to_string());
                        });
                    }
                    shilpo_services::CommandOutcome::TimedOut {
                        last_observed_version,
                    } => {
                        cx.update(|cx: &mut gpui::App| {
                            tracing::warn!(?last_observed_version, "compositor action timed out");
                            Self::show_compositor_error_message(cx, "compositor action timed out");
                        });
                    }
                    shilpo_services::CommandOutcome::Cancelled { reason } => {
                        tracing::debug!(?reason, "compositor action cancelled");
                    }
                })
                .detach();
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, "action invocation failed");
                Self::show_compositor_error_message(cx, &err.to_string());
                Err(err)
            }
        }
    }

    pub(crate) fn show_compositor_error_toast(
        cx: &mut App,
        error: &shilpo_services::CompositorCommandError,
    ) {
        Self::show_compositor_error_message(cx, &error.to_string());
    }

    fn show_compositor_error_message(cx: &mut App, concise: &str) {
        if cx.has_global::<ShellRuntime>()
            && let Some(hub) = cx.global::<ShellRuntime>().service_hub()
        {
            hub.push_notification(Notification::new("Compositor command failed", concise));
        }
    }

    pub(crate) fn dispatch_invocation(
        cx: &mut App,
        invocation: ActionInvocation,
    ) -> Result<crate::shell::actions::ActionResult, ShellError> {
        let action_id = invocation.id();
        let (enabled, name) = {
            let dispatcher = cx.global::<ShellRuntime>().action_dispatcher();
            let descriptor = dispatcher
                .actions
                .descriptor(&action_id)
                .cloned()
                .ok_or_else(|| ShellError::ActionFailed("unknown action id".into()))?;
            if !invocation.matches_descriptor(&descriptor) {
                return Err(ShellError::ActionFailed("invocation mismatch".into()));
            }
            (descriptor.enabled, descriptor.name)
        };

        if !enabled {
            return Err(ShellError::ActionFailed(format!(
                "action '{}' is currently disabled",
                name
            )));
        }

        match invocation {
            ActionInvocation::ToggleBar => {
                ShellSurfaces::request(cx, SurfaceRequest::ToggleBars);
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::ToggleOverview => {
                ShellSurfaces::request(cx, SurfaceRequest::ToggleOverview);
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::ReloadConfig => {
                ShellRuntime::reload_config(cx)?;
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::Quit => {
                ShellRuntime::shutdown(cx);
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::FocusWorkspace(id) => {
                let comp = ShellRuntime::compositor(cx)
                    .ok_or_else(|| ShellError::ActionFailed("compositor unavailable".into()))?;
                let ticket = comp
                    .command_broker()
                    .submit(CompositorCommand::FocusWorkspace(id))
                    .map_err(|error| ShellError::ActionFailed(error.to_string()))?;
                Ok(crate::shell::actions::ActionResult::Compositor(ticket))
            }
            ActionInvocation::FocusWindow(id) => {
                let comp = ShellRuntime::compositor(cx)
                    .ok_or_else(|| ShellError::ActionFailed("compositor unavailable".into()))?;
                let ticket = comp
                    .command_broker()
                    .submit(CompositorCommand::FocusWindow(id))
                    .map_err(|error| ShellError::ActionFailed(error.to_string()))?;
                Ok(crate::shell::actions::ActionResult::Compositor(ticket))
            }
            ActionInvocation::CloseWindow(id) => {
                let comp = ShellRuntime::compositor(cx)
                    .ok_or_else(|| ShellError::ActionFailed("compositor unavailable".into()))?;
                let ticket = comp
                    .command_broker()
                    .submit(CompositorCommand::CloseWindow(id))
                    .map_err(|error| ShellError::ActionFailed(error.to_string()))?;
                Ok(crate::shell::actions::ActionResult::Compositor(ticket))
            }
            ActionInvocation::CreateWorkspace => {
                let comp = ShellRuntime::compositor(cx)
                    .ok_or_else(|| ShellError::ActionFailed("compositor unavailable".into()))?;
                let ticket = comp
                    .command_broker()
                    .submit(CompositorCommand::CreateWorkspace)
                    .map_err(|error| ShellError::ActionFailed(error.to_string()))?;
                Ok(crate::shell::actions::ActionResult::Compositor(ticket))
            }
            ActionInvocation::MoveWindowToWorkspace {
                window_id,
                workspace_id,
            } => {
                let comp = ShellRuntime::compositor(cx)
                    .ok_or_else(|| ShellError::ActionFailed("compositor unavailable".into()))?;
                let ticket = comp
                    .command_broker()
                    .submit(CompositorCommand::MoveWindowToWorkspace {
                        window_id,
                        workspace_id,
                    })
                    .map_err(|error| ShellError::ActionFailed(error.to_string()))?;
                Ok(crate::shell::actions::ActionResult::Compositor(ticket))
            }
            ActionInvocation::VolumeUp => {
                ShellRuntime::dispatch_device_command(
                    cx,
                    shilpo_services::DeviceCommand::Audio(shilpo_services::AudioAction::SetVolume(
                        (ShellRuntime::device_snapshot(cx).audio.volume + 5).min(100),
                    )),
                );
                let info = ShellRuntime::device_snapshot(cx).audio;
                let target_vol = (info.volume + 5).min(100);
                ShellSurfaces::request(
                    cx,
                    SurfaceRequest::ShowOsd(crate::shell::osd::OsdKind::Volume {
                        level: target_vol as u32,
                        muted: info.is_muted,
                    }),
                );
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::VolumeDown => {
                ShellRuntime::dispatch_device_command(
                    cx,
                    shilpo_services::DeviceCommand::Audio(shilpo_services::AudioAction::SetVolume(
                        ShellRuntime::device_snapshot(cx)
                            .audio
                            .volume
                            .saturating_sub(5),
                    )),
                );
                let info = ShellRuntime::device_snapshot(cx).audio;
                let target_vol = info.volume.saturating_sub(5);
                ShellSurfaces::request(
                    cx,
                    SurfaceRequest::ShowOsd(crate::shell::osd::OsdKind::Volume {
                        level: target_vol as u32,
                        muted: info.is_muted,
                    }),
                );
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::VolumeMute => {
                ShellRuntime::dispatch_device_command(
                    cx,
                    shilpo_services::DeviceCommand::Audio(shilpo_services::AudioAction::ToggleMute),
                );
                let info = ShellRuntime::device_snapshot(cx).audio;
                ShellSurfaces::request(
                    cx,
                    SurfaceRequest::ShowOsd(crate::shell::osd::OsdKind::Volume {
                        level: info.volume as u32,
                        muted: !info.is_muted,
                    }),
                );
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::BrightnessUp => {
                let info = ShellRuntime::device_snapshot(cx).brightness;
                let connector = ShellRuntime::compositor(cx)
                    .and_then(|c| c.current().focused_output.clone())
                    .unwrap_or_else(|| "eDP-1".to_string());
                ShellRuntime::dispatch_device_command(
                    cx,
                    shilpo_services::DeviceCommand::Brightness(
                        shilpo_services::BrightnessAction::StepUp,
                    ),
                );
                let target_display = info
                    .displays
                    .iter()
                    .find(|d| d.connector.as_deref() == Some(&connector))
                    .or_else(|| info.displays.iter().find(|d| d.is_primary))
                    .or_else(|| info.displays.first());

                let (target_pct, display_name, connector_opt) = match target_display {
                    Some(d) => (
                        (d.percentage as i16 + 5).clamp(0, 100) as u32,
                        Some(d.name.clone()),
                        d.connector.clone(),
                    ),
                    None => ((info.percentage + 5).min(100) as u32, None, Some(connector)),
                };

                ShellSurfaces::request(
                    cx,
                    SurfaceRequest::ShowOsd(crate::shell::osd::OsdKind::Brightness {
                        level: target_pct,
                        display_name,
                        connector: connector_opt,
                    }),
                );
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::BrightnessDown => {
                let info = ShellRuntime::device_snapshot(cx).brightness;
                let connector = ShellRuntime::compositor(cx)
                    .and_then(|c| c.current().focused_output.clone())
                    .unwrap_or_else(|| "eDP-1".to_string());
                ShellRuntime::dispatch_device_command(
                    cx,
                    shilpo_services::DeviceCommand::Brightness(
                        shilpo_services::BrightnessAction::StepDown,
                    ),
                );
                let target_display = info
                    .displays
                    .iter()
                    .find(|d| d.connector.as_deref() == Some(&connector))
                    .or_else(|| info.displays.iter().find(|d| d.is_primary))
                    .or_else(|| info.displays.first());

                let (target_pct, display_name, connector_opt) = match target_display {
                    Some(d) => (
                        d.percentage.saturating_sub(5) as u32,
                        Some(d.name.clone()),
                        d.connector.clone(),
                    ),
                    None => (
                        info.percentage.saturating_sub(5) as u32,
                        None,
                        Some(connector),
                    ),
                };

                ShellSurfaces::request(
                    cx,
                    SurfaceRequest::ShowOsd(crate::shell::osd::OsdKind::Brightness {
                        level: target_pct,
                        display_name,
                        connector: connector_opt,
                    }),
                );
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
            ActionInvocation::TakeScreenshot => {
                ShellSurfaces::request(
                    cx,
                    SurfaceRequest::OpenCapture(shilpo_services::capture::CaptureIntent::Clipboard),
                );
                Ok(crate::shell::actions::ActionResult::Immediate)
            }

            ActionInvocation::Extension { id, payload } => {
                if !cx.global::<ShellRuntime>().extension_host().is_loaded() {
                    return Err(ShellError::ActionFailed(format!(
                        "extension action 'ext:{id}' has no loaded runtime"
                    )));
                }
                ShellRuntime::dispatch_extension_input(cx, &id, None, "invoke", payload);
                Ok(crate::shell::actions::ActionResult::Immediate)
            }
        }
    }
}

impl ShellRuntime {
    pub fn reset_shortcuts_to_defaults(cx: &mut App) {
        if cx.has_global::<Self>() {
            cx.global_mut::<Self>()
                .action_dispatcher_mut()
                .reset_shortcuts_to_defaults();
        }
    }

    pub fn register_extension_action(
        cx: &mut App,
        id: CanonicalId,
        name: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<ActionId, String> {
        cx.global_mut::<Self>()
            .action_dispatcher_mut()
            .register_extension_action(id, name, label)
    }

    pub fn action_descriptors(cx: &App) -> Vec<ActionDescriptor> {
        if cx.has_global::<Self>() {
            cx.global::<Self>().action_dispatcher().action_descriptors()
        } else {
            ActionRegistry::default().all()
        }
    }

    pub fn keybinding_descriptors(cx: &App) -> Vec<(String, String)> {
        cx.global::<Self>()
            .action_dispatcher()
            .keybinding_descriptors()
    }

    pub fn resolved_shortcuts(cx: &App) -> Vec<crate::shell::actions::ResolvedShortcut> {
        cx.global::<Self>()
            .action_dispatcher()
            .keybinding_manager()
            .resolved_shortcuts()
            .to_vec()
    }

    pub fn focus_workspace(cx: &mut App, ws_id: u64) -> Result<(), ShellError> {
        ActionDispatcher::dispatch_action(cx, ActionInvocation::FocusWorkspace(ws_id))
    }

    pub fn dispatch_action(cx: &mut App, action: ActionInvocation) -> Result<(), ShellError> {
        ActionDispatcher::dispatch_action(cx, action)
    }

    pub fn dispatch_invocation(
        cx: &mut App,
        invocation: ActionInvocation,
    ) -> Result<crate::shell::actions::ActionResult, ShellError> {
        ActionDispatcher::dispatch_invocation(cx, invocation)
    }

    #[allow(dead_code)]
    pub(super) fn show_compositor_error_toast(
        cx: &mut App,
        error: &shilpo_services::CompositorCommandError,
    ) {
        ActionDispatcher::show_compositor_error_toast(cx, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_dispatcher_initialization_and_shortcuts() {
        let mut dispatcher = ActionDispatcher::new();
        assert!(!dispatcher.action_descriptors().is_empty());

        let user_bindings = vec![crate::config::KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: Some("Ctrl+Shift+T".into()),
            enabled: true,
        }];

        let report = dispatcher.reconcile_keybindings(&user_bindings, &[]);
        assert!(report.diagnostics.is_empty());

        dispatcher.reset_shortcuts_to_defaults();
        assert!(!dispatcher.keybinding_descriptors().is_empty());
    }

    #[test]
    fn test_action_dispatcher_extension_action_registration() {
        use shilpo_ext_api::{ContributionId, ExtensionId};
        let mut dispatcher = ActionDispatcher::new();
        let ext_id = ExtensionId::new("org.shilpo.test").unwrap();
        let contrib_id = ContributionId::new("test-action").unwrap();
        let cid = CanonicalId::new(ext_id, contrib_id);
        let res = dispatcher.register_extension_action(cid, "test-action", "Test Action Label");
        assert!(res.is_ok());
        let action_id = res.unwrap();
        assert!(
            dispatcher
                .action_descriptors()
                .into_iter()
                .any(|d| d.id == action_id)
        );
    }

    #[test]
    fn shortcut_override_reports_the_displaced_action() {
        let mut dispatcher = ActionDispatcher::new();
        let user_bindings = vec![crate::config::KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: Some("Super+Space".into()),
            enabled: true,
        }];
        let report = dispatcher.reconcile_keybindings(&user_bindings, &[]);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("collides"))
        );
    }

    #[test]
    fn snapshot_enables_only_actions_the_compositor_supports() {
        let mut dispatcher = ActionDispatcher::new();
        let snapshot = CompositorSnapshot {
            connection: shilpo_services::DomainLifecycle::Ready,
            capabilities: shilpo_services::CompositorCapabilities {
                can_create_workspace: true,
                can_focus_workspace: false,
                ..Default::default()
            },
            ..Default::default()
        };

        dispatcher.update_enabled_for_snapshot(&snapshot);

        let descriptors = dispatcher.action_descriptors();
        let focus_ws = descriptors
            .iter()
            .find(|d| d.id == ActionId::FocusWorkspace)
            .unwrap();
        assert!(!focus_ws.enabled);
        let create_ws = descriptors
            .iter()
            .find(|d| d.id == ActionId::CreateWorkspace)
            .unwrap();
        assert!(create_ws.enabled);
    }

    #[test]
    fn extension_actions_are_replaced_during_sync() {
        use shilpo_ext_api::{ContributionId, ExtensionId};
        let mut dispatcher = ActionDispatcher::new();
        let ext_id = ExtensionId::new("org.shilpo.test").unwrap();
        let cid = CanonicalId::new(ext_id.clone(), ContributionId::new("first").unwrap());
        dispatcher
            .register_extension_action(cid, "first", "First")
            .unwrap();

        let next = CanonicalId::new(ext_id, ContributionId::new("second").unwrap());
        dispatcher.sync_extension_actions(vec![ContributionDescriptor {
            id: next.clone(),
            extension_name: "org.shilpo.test".into(),
            name: "second".into(),
            surface: crate::shell::extensions::ContributionSurface::Action,
            runtime_kind: shilpo_ext_runtime::worker::protocol::ExtensionRuntimeKind::Wasm,
            settings_schema: None,
            default_size: None,
            minimum_size: None,
            bar_widget: None,
            action: None,
            default_binding: None,
            wallpaper_modes: None,
            wallpaper_targets: None,
            search_modes: None,
        }]);

        let ids = dispatcher
            .action_descriptors()
            .into_iter()
            .filter_map(|d| d.id.extension_id())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![next]);
    }

    struct ActionDispatcherTestHarness {
        dispatcher: ActionDispatcher,
    }

    impl ActionDispatcherTestHarness {
        fn new_offline() -> Self {
            Self {
                dispatcher: ActionDispatcher::new(),
            }
        }
    }

    #[test]
    fn test_harness_action_dispatcher_isolated_state_transitions() {
        let mut harness = ActionDispatcherTestHarness::new_offline();
        assert!(!harness.dispatcher.action_descriptors().is_empty());
        let user_bindings = vec![crate::config::KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: Some("Ctrl+Shift+U".into()),
            enabled: true,
        }];
        let report = harness
            .dispatcher
            .reconcile_keybindings(&user_bindings, &[]);
        assert!(report.diagnostics.is_empty());
        harness.dispatcher.reset_shortcuts_to_defaults();
        assert!(!harness.dispatcher.keybinding_descriptors().is_empty());
    }
}
