pub mod actions;
pub mod app_icons;
pub mod bar;
pub mod battery;
pub mod capture;
pub mod dbus;
pub mod error;
pub mod extension_http;
pub mod extension_surface;
pub mod extensions;
pub mod idle;
pub mod keybindings;
pub mod notification;
pub mod osd;
pub mod overview;
pub mod overview_search;
pub mod polkit;
pub mod runtime;
pub mod widgets;
pub mod workspace_miniature;

use std::sync::{Arc, Mutex};

pub use actions::{
    ActionCategory, ActionDescriptor, ActionId, ActionRegistry, KeybindingManager, Shortcut,
};
pub use bar::BarView;
use dbus::{DebugDbusService, ShellCommand, ShellDbusService, ShellStatus, ShellTelemetry};
pub use error::ShellError;
pub use extensions::{
    ContributionDescriptor, ContributionInstance, ContributionSurface, ExtensionCoordinator,
    ExtensionSnapshot,
};
pub use idle::IdleGraceOverlayView;
pub use keybindings::{GlobalShortcutBackend, NiriShortcutBackend};
pub use notification::NotificationToastView;
pub use osd::{OsdKind, OsdView};
pub use overview::WorkspaceOverview;
pub use polkit::PolkitDialogView;
pub use runtime::ShellRuntime;

pub fn run_daemon(developer_mode: bool) {
    let obs_guard = init_tracing();
    let filter_controller = obs_guard.as_ref().and_then(|g| g.log_filter_controller());

    let (mailbox_tx, mailbox_rx) = tokio::sync::mpsc::channel::<ShellCommand>(128);
    let compositor_broker = Arc::new(Mutex::new(None));
    let status = Arc::new(arc_swap::ArcSwap::from_pointee(ShellStatus::default()));
    let telemetry = Arc::new(arc_swap::ArcSwap::from_pointee(ShellTelemetry::default()));

    let instance_id = uuid::Uuid::new_v4().to_string();
    status.rcu(|s| {
        let mut next = (**s).clone();
        next.instance_id = instance_id.clone();
        next.pid = std::process::id();
        Arc::new(next)
    });

    let dbus_service = Arc::new(ShellDbusService::new_with_developer_mode(
        mailbox_tx.clone(),
        compositor_broker.clone(),
        status.clone(),
        telemetry.clone(),
        developer_mode,
    ));

    let debug_service = Arc::new(DebugDbusService::new(filter_controller, mailbox_tx));

    let dbus_conn = futures_lite::future::block_on(async {
        use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
        let conn = zbus::Connection::session().await.unwrap_or_else(|error| {
            eprintln!("failed to connect to the session bus: {error}");
            std::process::exit(1);
        });
        let dbus = DBusProxy::new(&conn).await.unwrap_or_else(|error| {
            eprintln!("failed to access the session bus: {error}");
            std::process::exit(1);
        });
        let reply = dbus
            .request_name(
                "org.shilpo.Shell".try_into().unwrap(),
                RequestNameFlags::DoNotQueue.into(),
            )
            .await
            .unwrap_or_else(|error| {
                eprintln!("failed to acquire org.shilpo.Shell: {error}");
                std::process::exit(1);
            });
        if reply != RequestNameReply::PrimaryOwner {
            eprintln!("org.shilpo.Shell is already owned by another process");
            std::process::exit(1);
        }
        if let Err(error) = conn
            .object_server()
            .at("/org/shilpo/Shell", (*dbus_service).clone())
            .await
        {
            eprintln!("failed to serve org.shilpo.Shell interface: {error}");
            std::process::exit(1);
        }
        if let Err(error) = conn
            .object_server()
            .at("/org/shilpo/Shell", (*debug_service).clone())
            .await
        {
            eprintln!("failed to serve org.shilpo.Debug interface: {error}");
            std::process::exit(1);
        }

        let dbus_service_for_names = dbus_service.clone();
        if let Ok(dbus_fdo) = DBusProxy::new(&conn).await
            && let Ok(mut stream) = dbus_fdo.receive_name_owner_changed().await
        {
            tokio::spawn(async move {
                use futures_lite::StreamExt;
                while let Some(signal) = stream.next().await {
                    if let Ok(args) = signal.args() {
                        let name = args.name.as_str();
                        let old_owner = args.old_owner.as_deref().unwrap_or("");
                        let new_owner = args.new_owner.as_deref().unwrap_or("");
                        dbus_service_for_names
                            .handle_name_owner_changed(name, old_owner, new_owner);
                    }
                }
            });
        }

        conn
    });

    let app = gpui_platform::application()
        .with_assets(crate::Assets)
        .with_quit_mode(gpui::QuitMode::Explicit);

    app.run(move |cx| {
        shilpo_m3e::init(cx);
        let config_path = std::env::var("HOME")
            .map(|home| std::path::PathBuf::from(home).join(".config/shilpo/config.toml"))
            .unwrap_or_else(|_| std::path::PathBuf::from(".config/shilpo/config.toml"));
        let resolver = crate::config::ConfigResolver::from_primary_path(&config_path);
        let config = match crate::config::migrate_primary_for_startup(&config_path) {
            Ok(outcome) => {
                crate::config::unknown_keys::log_unknown_key_warnings(&outcome.warnings);
                match resolver.resolve_initial() {
                    Ok((snapshot, report)) => {
                        crate::config::unknown_keys::log_unknown_key_warnings(&report.unknown_keys);
                        snapshot.config
                    }
                    Err(error) => {
                        if !config_path.exists() {
                            let default_config = crate::config::ShellConfig::default();
                            match default_config.save(&config_path) {
                                Ok(()) => default_config,
                                Err(save_error) => {
                                    tracing::error!(
                                        error = %save_error,
                                        "failed to write default config; using defaults"
                                    );
                                    crate::config::ShellConfig::default()
                                }
                            }
                        } else {
                            tracing::error!(
                                error = %error,
                                "failed to load shell config; using defaults"
                            );
                            crate::config::ShellConfig::default()
                        }
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    error = %error,
                    path = %config_path.display(),
                    "config migration failed; using defaults without rewriting the source",
                );
                crate::config::ShellConfig::default()
            }
        };
        crate::locale::ApplicationLocale::install(config.locale.as_deref(), cx);
        bar::view::apply_config_theme(&config, None, cx);
        cx.activate(true);
        ShellRuntime::install(
            cx,
            dbus_service,
            compositor_broker,
            status,
            telemetry,
            mailbox_rx,
            dbus_conn,
            instance_id,
        );
        cx.spawn(async move |cx| {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = signal(SignalKind::interrupt()).ok();
            let mut sigterm = signal(SignalKind::terminate()).ok();
            tokio::select! {
                _ = async {
                    if let Some(s) = sigint.as_mut() {
                        s.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {},
                _ = async {
                    if let Some(s) = sigterm.as_mut() {
                        s.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {},
            }
            tracing::info!("shutdown signal received; stopping shell");
            cx.update(ShellRuntime::shutdown);
        })
        .detach();

        let displays = cx.displays();
        if !displays.is_empty() {
            runtime::ShellSurfaces::request(cx, runtime::SurfaceRequest::SyncDisplays);
        } else {
            schedule_bar_retry(cx);
        }
    });
}

fn schedule_bar_retry(cx: &gpui::App) {
    cx.spawn(async move |cx| {
        for _ in 0..20 {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(50))
                .await;

            let opened = cx.update(|cx| {
                let displays = cx.displays();
                if displays.is_empty() {
                    return false;
                }
                runtime::ShellSurfaces::request(cx, runtime::SurfaceRequest::SyncDisplays);
                true
            });
            if opened {
                return;
            }
        }

        tracing::warn!("primary display unavailable after 1s; opening degraded bar");
        cx.update(|cx| {
            runtime::ShellSurfaces::request(cx, runtime::SurfaceRequest::OpenFallbackBar);
        });
    })
    .detach();
}

fn init_tracing() -> Option<shilpo_observability::ObservabilityGuard> {
    shilpo_observability::init(
        shilpo_observability::ProcessRole::Shell,
        "warn,shilpo_shell=info,shilpo_services=info",
    )
    .map_err(|e| eprintln!("observability warning: {e}"))
    .ok()
}
