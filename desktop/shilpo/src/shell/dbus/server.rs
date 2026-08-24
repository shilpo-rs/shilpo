//! org.shilpo.Shell D-Bus service server implementation.

use std::sync::{Arc, Mutex};

use shilpo_services::CompositorCommandBroker;
use tokio::sync::mpsc;
use zbus::object_server::SignalEmitter;

use super::types::{CommandResult, ShellStatus, ShellTelemetry};

/// Internal command payload sent to GPUI thread mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellCommand {
    ReloadConfig,
    ShowBar,
    HideBar,
    ToggleBar,
    ShowOverview,
    HideOverview,
    ToggleOverview,
    SetBrightness(u8),
    SetDisplayBrightness {
        display_id: String,
        percentage: u8,
    },
    Capture(shilpo_services::capture::CaptureIntent),
    EmitTestNotification {
        title: String,
        body: String,
    },
    InvokeAction {
        action_id: String,
        payload_json: Option<String>,
    },
    NextWallpaper,
    ForgetSearchResult(String),
    ClearSearchLearning,
    ResetNotificationQuarantine,
    ResetDeviceQuarantine,
    Lock,
}

/// Active dev session record.
#[derive(Debug, Clone)]
pub struct DevSession {
    pub session_id: String,
    pub caller_unique_name: String,
    pub extension_id: shilpo_ext_api::ExtensionId,
    pub canonical_source_root: std::path::PathBuf,
    pub created_at: std::time::Instant,
    pub last_build_sequence: u64,
}

/// D-Bus interface implementation for `org.shilpo.Shell`.
#[derive(Clone)]
pub struct ShellDbusService {
    developer_mode: bool,
    mailbox_tx: mpsc::Sender<ShellCommand>,
    compositor_broker: Arc<Mutex<Option<Arc<CompositorCommandBroker>>>>,
    extension_coordinator: Arc<Mutex<Option<Arc<crate::extensions::ExtensionCoordinator>>>>,
    dev_sessions: Arc<Mutex<std::collections::HashMap<String, DevSession>>>,
    status: Arc<arc_swap::ArcSwap<ShellStatus>>,
    telemetry: Arc<arc_swap::ArcSwap<ShellTelemetry>>,
    last_workspace: Arc<Mutex<Option<(u64, u64, u64)>>>,
    last_theme: Arc<Mutex<Option<(String, String)>>>,
}

impl ShellDbusService {
    pub fn new(
        mailbox_tx: mpsc::Sender<ShellCommand>,
        compositor_broker: Arc<Mutex<Option<Arc<CompositorCommandBroker>>>>,
        status: Arc<arc_swap::ArcSwap<ShellStatus>>,
        telemetry: Arc<arc_swap::ArcSwap<ShellTelemetry>>,
    ) -> Self {
        Self::new_with_developer_mode(mailbox_tx, compositor_broker, status, telemetry, false)
    }

    pub fn new_with_developer_mode(
        mailbox_tx: mpsc::Sender<ShellCommand>,
        compositor_broker: Arc<Mutex<Option<Arc<CompositorCommandBroker>>>>,
        status: Arc<arc_swap::ArcSwap<ShellStatus>>,
        telemetry: Arc<arc_swap::ArcSwap<ShellTelemetry>>,
        developer_mode: bool,
    ) -> Self {
        Self {
            developer_mode,
            mailbox_tx,
            compositor_broker,
            extension_coordinator: Arc::new(Mutex::new(None)),
            dev_sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
            status,
            telemetry,
            last_workspace: Arc::new(Mutex::new(None)),
            last_theme: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_extension_coordinator(
        &self,
        coordinator: Option<Arc<crate::extensions::ExtensionCoordinator>>,
    ) {
        *self.extension_coordinator.lock().unwrap() = coordinator;
    }

    pub fn dev_sessions(&self) -> Arc<Mutex<std::collections::HashMap<String, DevSession>>> {
        self.dev_sessions.clone()
    }

    pub fn handle_name_owner_changed(&self, name: &str, old_owner: &str, new_owner: &str) {
        if new_owner.is_empty() {
            let target = if !old_owner.is_empty() {
                old_owner
            } else {
                name
            };
            let to_unload: Vec<DevSession> = {
                let mut sessions = self.dev_sessions.lock().unwrap();
                let mut removed = Vec::new();
                sessions.retain(|_id, session| {
                    if session.caller_unique_name == target || session.caller_unique_name == name {
                        removed.push(session.clone());
                        false
                    } else {
                        true
                    }
                });
                removed
            };
            for session in to_unload {
                if let Some(coordinator) = self.extension_coordinator.lock().unwrap().clone() {
                    let _ = coordinator.unload_dev(session.session_id, session.extension_id);
                }
            }
        }
    }

    fn send_command(&self, cmd: ShellCommand) -> zbus::fdo::Result<()> {
        let result = match self.mailbox_tx.try_send(cmd) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(zbus::fdo::Error::LimitsExceeded(
                "command mailbox is full".into(),
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(zbus::fdo::Error::Failed("shell daemon is stopping".into()))
            }
        };
        tracing::Span::current().record(
            "outcome",
            if result.is_ok() { "accepted" } else { "failed" },
        );
        result
    }

    async fn execute_compositor_command(
        &self,
        cmd: shilpo_services::CompositorCommand,
    ) -> zbus::fdo::Result<CommandResult> {
        let broker = self
            .compositor_broker
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| {
                zbus::fdo::Error::Failed("compositor command broker is unavailable".into())
            })?;

        let outcome = tokio::task::spawn_blocking(move || match broker.submit(cmd) {
            Ok(ticket) => ticket.wait_timeout(std::time::Duration::from_secs(2)),
            Err(outcome) => outcome,
        })
        .await
        .map_err(|_| zbus::fdo::Error::Failed("compositor command task failed".into()))?;

        let result = CommandResult::from(outcome);
        tracing::Span::current().record("outcome", result.outcome.as_str());
        Ok(result)
    }

    pub fn update_status(&self, status: ShellStatus) {
        self.status.store(Arc::new(status));
    }

    pub fn update_telemetry(&self, telemetry: ShellTelemetry) {
        self.telemetry.store(Arc::new(telemetry));
    }

    pub async fn emit_workspace_changed_if_needed(
        &self,
        emitter: &SignalEmitter<'_>,
        workspace_id: u64,
        owner_generation: u64,
        revision: u64,
    ) {
        let key = (workspace_id, 0, 0);
        let should_emit = {
            let mut last = self.last_workspace.lock().unwrap();
            if *last != Some(key) {
                *last = Some(key);
                true
            } else {
                false
            }
        };
        if should_emit {
            let _ =
                Self::workspace_changed(emitter, workspace_id, owner_generation, revision).await;
        }
    }

    /// Seeds the deduplication state with the authoritative initial snapshot.
    pub fn prime_workspace(&self, workspace_id: u64) {
        *self.last_workspace.lock().unwrap() = Some((workspace_id, 0, 0));
    }

    pub async fn emit_theme_changed_if_needed(
        &self,
        emitter: &SignalEmitter<'_>,
        mode: &str,
        scheme_variant: &str,
    ) {
        let key = (mode.to_string(), scheme_variant.to_string());
        let should_emit = {
            let mut last = self.last_theme.lock().unwrap();
            if let Some(prev) = last.as_ref() {
                if prev != &key {
                    *last = Some(key.clone());
                    true
                } else {
                    false
                }
            } else {
                *last = Some(key);
                false
            }
        };
        if should_emit {
            let _ = Self::theme_changed(emitter, mode, scheme_variant).await;
        }
    }

    pub async fn emit_config_reloaded(
        &self,
        emitter: &SignalEmitter<'_>,
        success: bool,
        mut changed_components: Vec<String>,
        diagnostic_count: u32,
    ) {
        if !success {
            changed_components.clear();
        } else {
            changed_components.sort();
        }
        let _ = Self::config_reloaded(emitter, success, changed_components, diagnostic_count).await;
    }
}

#[zbus::interface(name = "org.shilpo.Shell")]
impl ShellDbusService {
    async fn reload_config(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "reload_config",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::ReloadConfig)
    }

    async fn show_bar(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "show_bar",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::ShowBar)
    }

    async fn hide_bar(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "hide_bar",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::HideBar)
    }

    async fn toggle_bar(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "toggle_bar",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::ToggleBar)
    }

    async fn show_overview(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "show_overview",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::ShowOverview)
    }

    async fn hide_overview(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "hide_overview",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::HideOverview)
    }

    async fn toggle_overview(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "toggle_overview",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::ToggleOverview)
    }

    async fn forget_search_result(&self, canonical_id: String) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "forget_search_result",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::ForgetSearchResult(canonical_id))
    }

    async fn clear_search_learning(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "clear_search_learning",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::ClearSearchLearning)
    }

    async fn focus_workspace(&self, workspace_id: u64) -> zbus::fdo::Result<CommandResult> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "focus_workspace",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.execute_compositor_command(shilpo_services::CompositorCommand::FocusWorkspace(
            workspace_id,
        ))
        .await
    }

    async fn create_workspace(&self) -> zbus::fdo::Result<CommandResult> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "create_workspace",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.execute_compositor_command(shilpo_services::CompositorCommand::CreateWorkspace)
            .await
    }

    async fn focus_window(&self, window_id: u64) -> zbus::fdo::Result<CommandResult> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "focus_window",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.execute_compositor_command(shilpo_services::CompositorCommand::FocusWindow(window_id))
            .await
    }

    async fn focus_previous_window(&self) -> zbus::fdo::Result<CommandResult> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "focus_previous_window",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.execute_compositor_command(shilpo_services::CompositorCommand::FocusPreviousWindow)
            .await
    }

    async fn close_window(&self, window_id: u64) -> zbus::fdo::Result<CommandResult> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "close_window",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.execute_compositor_command(shilpo_services::CompositorCommand::CloseWindow(window_id))
            .await
    }

    async fn move_window_to_workspace(
        &self,
        window_id: u64,
        workspace_id: u64,
    ) -> zbus::fdo::Result<CommandResult> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "move_window_to_workspace",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.execute_compositor_command(shilpo_services::CompositorCommand::MoveWindowToWorkspace {
            window_id,
            workspace_id,
        })
        .await
    }

    async fn set_brightness(&self, percentage: u8) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "set_brightness",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        if percentage > 100 {
            tracing::Span::current().record("outcome", "invalid_args");
            return Err(zbus::fdo::Error::InvalidArgs(
                "brightness percentage must be between 0 and 100".into(),
            ));
        }
        self.send_command(ShellCommand::SetBrightness(percentage))
    }

    async fn set_display_brightness(
        &self,
        display_id: String,
        percentage: u8,
    ) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "set_display_brightness",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        if display_id.trim().is_empty() {
            tracing::Span::current().record("outcome", "invalid_args");
            return Err(zbus::fdo::Error::InvalidArgs(
                "display_id cannot be empty".into(),
            ));
        }
        if percentage > 100 {
            tracing::Span::current().record("outcome", "invalid_args");
            return Err(zbus::fdo::Error::InvalidArgs(
                "brightness percentage must be between 0 and 100".into(),
            ));
        }
        self.send_command(ShellCommand::SetDisplayBrightness {
            display_id,
            percentage,
        })
    }

    async fn get_status(&self) -> ShellStatus {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "get_status",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        let status = self.status.load().as_ref().clone();
        tracing::Span::current().record("outcome", "success");
        status
    }

    async fn get_telemetry(&self) -> ShellTelemetry {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "get_telemetry",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        let telemetry = self.telemetry.load().as_ref().clone();
        tracing::Span::current().record("outcome", "success");
        telemetry
    }

    async fn capture(&self, intent: String) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "capture",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        let capture_intent = match intent.as_str() {
            "clipboard" => shilpo_services::capture::CaptureIntent::Clipboard,
            "annotation" => shilpo_services::capture::CaptureIntent::Annotation,
            "ocr" => shilpo_services::capture::CaptureIntent::Ocr,
            "menu" => shilpo_services::capture::CaptureIntent::Menu,
            _ => {
                tracing::Span::current().record("outcome", "invalid_args");
                return Err(zbus::fdo::Error::InvalidArgs(format!(
                    "unknown capture intent '{intent}'"
                )));
            }
        };
        self.send_command(ShellCommand::Capture(capture_intent))
    }

    async fn invoke_action(
        &self,
        action_id: String,
        payload_json: Option<String>,
    ) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "invoke_action",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::InvokeAction {
            action_id,
            payload_json,
        })
    }

    async fn next_wallpaper(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "next_wallpaper",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::NextWallpaper)
    }

    async fn lock(&self) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "lock",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();
        self.send_command(ShellCommand::Lock)
    }

    async fn start_dev_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        extension_id: String,
        source_root: String,
    ) -> zbus::fdo::Result<String> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "start_dev_session",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();

        if !self.developer_mode {
            tracing::Span::current().record("outcome", "denied");
            return Err(zbus::fdo::Error::AccessDenied(
                "developer mode is disabled; restart with `shilpo daemon --developer-mode` to allow loading local extension code for this daemon's lifetime"
                    .into(),
            ));
        }

        let sender = header
            .sender()
            .map(|s| s.as_str().to_owned())
            .unwrap_or_else(|| "p2p-caller".to_string());

        let ext_id = shilpo_ext_api::ExtensionId::new(&extension_id)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("invalid extension ID: {e}")))?;

        let path = std::path::PathBuf::from(&source_root);
        if !path.is_absolute() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "source_root must be an absolute path".into(),
            ));
        }

        let canonical_root = path
            .canonicalize()
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("invalid source_root: {e}")))?;

        if !canonical_root.is_dir() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "source_root must be a directory".into(),
            ));
        }

        let manifest_path = canonical_root.join("extension.toml");
        let manifest_str = std::fs::read_to_string(&manifest_path).map_err(|e| {
            zbus::fdo::Error::InvalidArgs(format!("failed to read extension.toml: {e}"))
        })?;

        let manifest =
            shilpo_ext_api::ExtensionManifest::from_toml(&manifest_str).map_err(|e| {
                zbus::fdo::Error::InvalidArgs(format!("invalid extension manifest: {e}"))
            })?;

        if manifest.id != ext_id {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "manifest declares ID '{}' but requested '{}'",
                manifest.id, ext_id
            )));
        }

        let mut sessions = self.dev_sessions.lock().unwrap();
        if sessions.len() >= 64 {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "maximum concurrent dev sessions reached (64)".into(),
            ));
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            session_id.clone(),
            DevSession {
                session_id: session_id.clone(),
                caller_unique_name: sender,
                extension_id: ext_id,
                canonical_source_root: canonical_root,
                created_at: std::time::Instant::now(),
                last_build_sequence: 0,
            },
        );

        tracing::Span::current().record("outcome", "success");
        Ok(session_id)
    }

    async fn reload_dev_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_id: String,
        build_sequence: u64,
        artifact_path: String,
        timeout_ms: u64,
    ) -> zbus::fdo::Result<super::types::DevReloadResult> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "reload_dev_session",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();

        let sender = header
            .sender()
            .map(|s| s.as_str().to_owned())
            .unwrap_or_else(|| "p2p-caller".to_string());

        let session = {
            let sessions = self.dev_sessions.lock().unwrap();
            sessions.get(&session_id).cloned().ok_or_else(|| {
                zbus::fdo::Error::InvalidArgs(format!("dev session '{session_id}' not found"))
            })?
        };

        if session.caller_unique_name != sender {
            return Err(zbus::fdo::Error::AccessDenied(
                "caller unique name does not match session owner".into(),
            ));
        }

        let path = std::path::Path::new(&artifact_path);
        if path.is_absolute() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "artifact_path must be relative to the session source root".into(),
            ));
        }
        let artifact_full = session.canonical_source_root.join(path);

        let canonical_artifact = match artifact_full.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return Ok(super::types::DevReloadResult {
                    outcome: "rejected".into(),
                    host_generation: 0,
                    engine_generation: 0,
                    diagnostic_code: "ARTIFACT_NOT_FOUND".into(),
                    message: format!("artifact not found: {e}"),
                });
            }
        };

        if !canonical_artifact.starts_with(&session.canonical_source_root) {
            return Ok(super::types::DevReloadResult {
                outcome: "rejected".into(),
                host_generation: 0,
                engine_generation: 0,
                diagnostic_code: "ARTIFACT_OUTSIDE_ROOT".into(),
                message: format!(
                    "artifact '{}' is outside source root '{}'",
                    canonical_artifact.display(),
                    session.canonical_source_root.display()
                ),
            });
        }

        if !canonical_artifact.is_file() {
            return Ok(super::types::DevReloadResult {
                outcome: "rejected".into(),
                host_generation: 0,
                engine_generation: 0,
                diagnostic_code: "ARTIFACT_NOT_FILE".into(),
                message: format!(
                    "artifact '{}' is not a regular file",
                    canonical_artifact.display()
                ),
            });
        }

        let coordinator = self
            .extension_coordinator
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| {
                zbus::fdo::Error::Failed("extension coordinator is unavailable".into())
            })?;

        let sess_id = session.session_id.clone();
        let ext_id = session.extension_id.clone();
        let src_root = session.canonical_source_root.clone();

        let outcome = coordinator
            .reload_dev(
                sess_id,
                ext_id,
                src_root,
                canonical_artifact,
                build_sequence,
                std::time::Duration::from_millis(timeout_ms.clamp(1, 86_400_000)),
            )
            .map_err(|error| {
                if error == "extension command queue full" {
                    zbus::fdo::Error::LimitsExceeded(error)
                } else {
                    zbus::fdo::Error::Failed(error)
                }
            })?;

        if outcome.outcome == "applied" {
            let mut sessions = self.dev_sessions.lock().unwrap();
            if let Some(s) = sessions.get_mut(&session.session_id) {
                s.last_build_sequence = build_sequence;
            }
        }

        let result = super::types::DevReloadResult::from(outcome);
        tracing::Span::current().record("outcome", result.outcome.as_str());
        Ok(result)
    }

    async fn end_dev_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session_id: String,
    ) -> zbus::fdo::Result<()> {
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "dbus_call",
            destination = "org.shilpo.Shell",
            operation = "end_dev_session",
            outcome = tracing::field::Empty
        );
        let _enter = _span.enter();

        let sender = header
            .sender()
            .map(|s| s.as_str().to_owned())
            .unwrap_or_else(|| "p2p-caller".to_string());

        let removed = {
            let mut sessions = self.dev_sessions.lock().unwrap();
            if let Some(s) = sessions.get(&session_id)
                && s.caller_unique_name != sender
            {
                return Err(zbus::fdo::Error::AccessDenied(
                    "caller does not match session owner".into(),
                ));
            }
            sessions.remove(&session_id)
        };

        if let Some(session) = removed
            && let Some(coordinator) = self.extension_coordinator.lock().unwrap().clone()
        {
            let _ = coordinator.unload_dev(session.session_id, session.extension_id);
        }

        tracing::Span::current().record("outcome", "success");
        Ok(())
    }

    #[zbus(signal)]
    pub async fn shell_started(
        signal_ctor: &SignalEmitter<'_>,
        instance_id: &str,
        pid: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn shell_stopping(
        signal_ctor: &SignalEmitter<'_>,
        instance_id: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn workspace_changed(
        signal_ctor: &SignalEmitter<'_>,
        workspace_id: u64,
        owner_generation: u64,
        revision: u64,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn theme_changed(
        signal_ctor: &SignalEmitter<'_>,
        mode: &str,
        scheme_variant: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub async fn config_reloaded(
        signal_ctor: &SignalEmitter<'_>,
        success: bool,
        changed_components: Vec<String>,
        diagnostic_count: u32,
    ) -> zbus::Result<()>;
}
