use clap::{CommandFactory, Parser};
use shilpo::cli::adapters::{
    self, DoctorChecker, ExtAdapter, IpcAdapter, SystemdAdapter, ThemeAdapter,
};
use shilpo::cli::args::{
    ActionCommands, BrightnessCommands, CaptureAction, Cli, Commands, ConfigCommands, ExtCommands,
    ModeValue, ProfileCommands, ShellCommands, ThemeCommands, ThemeModeAction, ThemeSeedAction,
    ThemeWallpaperAction, VisibilityAction, WindowCommands, WorkspaceCommands,
};
use shilpo::cli::output::{CliOutput, EXIT_FAILURE, EXIT_INVALID_ARGS, EXIT_SUCCESS};
use shilpo::cli::parse_duration;

#[tokio::main]
async fn main() {
    if let Ok(service) = std::env::var(shilpo_services::auth::PAM_HELPER_ENV_VAR) {
        // Runs the PAM conversation for the lock screen's auth domain and never returns.
        // See `shilpo_services::auth::pam_child` for why this must be a freshly `exec`'d
        // process rather than a raw `fork()` from the (multi-threaded) domain owner.
        shilpo_services::auth::pam_child::run(&service);
    }
    if let Ok(path) = std::env::var("SHILPO_WASM_VALIDATOR") {
        let result = std::fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                shilpo_ext_runtime::WasmRuntime::validate_module_unbounded(&bytes)
                    .map_err(|error| error.to_string())
            });
        let _ = std::fs::remove_file(path);
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(EXIT_FAILURE);
        }
        std::process::exit(EXIT_SUCCESS);
    }
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.len() <= 1 {
        let mut cmd = Cli::command();
        cmd.print_help().unwrap();
        println!();
        std::process::exit(EXIT_SUCCESS);
    }

    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let exit_code = if e.use_stderr() {
                EXIT_INVALID_ARGS
            } else {
                EXIT_SUCCESS
            };
            if raw_args.iter().any(|arg| arg == "--json") {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "ok": false,
                        "command": "usage",
                        "data": null,
                        "warnings": [],
                        "error": { "code": "usage.invalid_arguments", "message": e.to_string() }
                    })
                );
            } else {
                print!("{e}");
            }
            std::process::exit(exit_code);
        }
    };

    let output = match CliOutput::new(cli.json, cli.quiet) {
        Ok(out) => out,
        Err((code, msg)) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "ok": false,
                        "command": "usage",
                        "data": null,
                        "warnings": [],
                        "error": { "code": "usage.invalid_options", "message": msg }
                    })
                );
            } else {
                eprintln!("{msg}");
            }
            std::process::exit(code);
        }
    };

    let timeout = match parse_duration(cli.timeout.as_deref()) {
        Ok(timeout) => timeout,
        Err(message) => {
            let code = output.error(
                "usage",
                "usage.invalid_timeout",
                &message,
                None,
                Vec::new(),
                EXIT_INVALID_ARGS,
            );
            std::process::exit(code);
        }
    };

    let Some(command) = cli.command else {
        let mut cmd = Cli::command();
        cmd.print_help().unwrap();
        println!();
        std::process::exit(EXIT_SUCCESS);
    };

    let exit_code = match command {
        Commands::Daemon { developer_mode } => {
            shilpo::shell::run_daemon(developer_mode);
            std::process::exit(EXIT_SUCCESS);
        }
        Commands::Settings => {
            let _obs_guard = shilpo_observability::init(
                shilpo_observability::ProcessRole::Settings,
                "warn,shilpo=info",
            )
            .map_err(|e| eprintln!("observability warning: {e}"))
            .ok();
            shilpo::settings::run_settings().await;
            std::process::exit(EXIT_SUCCESS);
        }
        Commands::Lock => {
            let _obs_guard = shilpo_observability::init(
                shilpo_observability::ProcessRole::Lock,
                "warn,shilpo=info",
            )
            .map_err(|e| eprintln!("observability warning: {e}"))
            .ok();
            shilpo::lock::run_lock().await;
            std::process::exit(EXIT_SUCCESS);
        }
        Commands::ExtensionHost => {
            let _obs_guard = shilpo_observability::init(
                shilpo_observability::ProcessRole::ExtensionHost,
                "warn,shilpo_ext_runtime=info",
            )
            .map_err(|e| eprintln!("observability warning: {e}"))
            .ok();
            shilpo_ext_runtime::run_extension_host();
            std::process::exit(EXIT_SUCCESS);
        }
        Commands::DeviceDaemon => {
            if let Err(e) = shilpo_services::run_device_daemon().await {
                eprintln!("device daemon error: {e}");
                std::process::exit(EXIT_FAILURE);
            }
            std::process::exit(EXIT_SUCCESS);
        }
        Commands::ThemeDaemon => {
            let config_path = shilpo::config::default_config_path();
            let options = match shilpo::config::ShellConfig::load_or_create(&config_path) {
                Ok(cfg) => shilpo_theme_daemon::ThemeDaemonOptions {
                    provider: cfg.theme.provider,
                    gtk_theme_light: cfg.theme.gtk_theme_light,
                    gtk_theme_dark: cfg.theme.gtk_theme_dark,
                    custom_adapter_cmd: cfg.theme.custom_adapter_cmd,
                    wallpaper_dir: Some(cfg.desktop.wallpaper_dir.clone()),
                    scheme_variant: cfg
                        .theme
                        .scheme_variant
                        .as_deref()
                        .map(shilpo_m3e::theme::SchemeVariant::from_str),
                    config_path: Some(config_path),
                    state_path: Some(shilpo::config::state_dir().join("colors.json")),
                },
                Err(_) => shilpo_theme_daemon::ThemeDaemonOptions::default(),
            };
            if let Err(e) = shilpo_theme_daemon::run_theme_daemon(options).await {
                eprintln!("theme daemon error: {e}");
                std::process::exit(EXIT_FAILURE);
            }
            std::process::exit(EXIT_SUCCESS);
        }
        Commands::Shell { command } => match command {
            ShellCommands::Status => {
                let ipc = IpcAdapter::new();
                match ipc.status() {
                    Ok(status) if status.readiness == "ready" => {
                        let unit_active = SystemdAdapter::is_unit_active();
                        let data = serde_json::json!({ "unit_active": unit_active, "ipc": status });
                        output.success(
                                "shell.status", &data,
                                Some(&format!("Systemd active: {unit_active}\nInstance ID: {}\nPID: {}\nReadiness: {}\nBar State: {}\nOverview Visible: {}", status.instance_id, status.pid, status.readiness, status.bar_state, status.overview_visible)),
                                Vec::new(),
                            )
                    }
                    Ok(status) => output.error(
                        "shell.status",
                        "shell_degraded",
                        &format!("Shell readiness is {}", status.readiness),
                        Some(serde_json::to_value(&status).unwrap_or_default()),
                        Vec::new(),
                        EXIT_FAILURE,
                    ),
                    Err((code, msg)) => output.error(
                        "shell.status",
                        "daemon_unavailable",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                }
            }
            ShellCommands::Start => {
                let systemd = SystemdAdapter::new();
                match systemd.start(timeout) {
                    Ok(status) if status.readiness == "ready" => output.success(
                        "shell.start",
                        &status,
                        Some("Shell daemon started successfully"),
                        Vec::new(),
                    ),
                    Ok(status) => output.error(
                        "shell.start",
                        "shell_degraded",
                        &format!("Shell started with readiness {}", status.readiness),
                        Some(serde_json::to_value(&status).unwrap_or_default()),
                        Vec::new(),
                        EXIT_FAILURE,
                    ),
                    Err((code, msg)) => {
                        output.error("shell.start", "start_failed", &msg, None, Vec::new(), code)
                    }
                }
            }
            ShellCommands::Stop => {
                let systemd = SystemdAdapter::new();
                match systemd.stop(timeout) {
                    Ok(()) => output.success(
                        "shell.stop",
                        &serde_json::json!({ "stopped": true }),
                        Some("Shell daemon stopped"),
                        Vec::new(),
                    ),
                    Err((code, msg)) => {
                        output.error("shell.stop", "stop_failed", &msg, None, Vec::new(), code)
                    }
                }
            }
            ShellCommands::Restart => {
                let systemd = SystemdAdapter::new();
                match systemd.restart(timeout) {
                    Ok(status) if status.readiness == "ready" => output.success(
                        "shell.restart",
                        &status,
                        Some("Shell daemon restarted successfully"),
                        Vec::new(),
                    ),
                    Ok(status) => output.error(
                        "shell.restart",
                        "shell_degraded",
                        &format!("Shell restarted with readiness {}", status.readiness),
                        Some(serde_json::to_value(&status).unwrap_or_default()),
                        Vec::new(),
                        EXIT_FAILURE,
                    ),
                    Err((code, msg)) => output.error(
                        "shell.restart",
                        "restart_failed",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                }
            }
            ShellCommands::Logs {
                follow,
                since,
                lines,
            } => {
                let systemd = SystemdAdapter::new();
                if cli.json && follow {
                    output.error(
                        "shell.logs",
                        "json_stream_unsupported",
                        "--json cannot be combined with --follow",
                        None,
                        Vec::new(),
                        EXIT_INVALID_ARGS,
                    )
                } else if cli.json {
                    match systemd.logs_capture(since.as_deref(), lines) {
                        Ok(logs) => output.success(
                            "shell.logs",
                            &serde_json::json!({ "logs": logs }),
                            None,
                            Vec::new(),
                        ),
                        Err((code, msg)) => {
                            output.error("shell.logs", "logs_failed", &msg, None, Vec::new(), code)
                        }
                    }
                } else {
                    match systemd.logs(follow, since.as_deref(), lines) {
                        Ok(code) => code,
                        Err((code, msg)) => {
                            output.error("shell.logs", "logs_failed", &msg, None, Vec::new(), code)
                        }
                    }
                }
            }
            ShellCommands::Telemetry => {
                let ipc = IpcAdapter::new();
                match ipc.telemetry() {
                    Ok(health) => output.success("shell.telemetry", &health, Some(&format!("Service Health & Broker Telemetry:\n  Compositor Connected: {}\n  Compositor State: {}\n  Compositor Revision: {}\n  Uptime: {}s", health.compositor_connected, health.compositor_state, health.compositor_revision, health.uptime_seconds)), Vec::new()),
                    Err((code, msg)) => output.error("shell.telemetry", "telemetry_failed", &msg, None, Vec::new(), code),
                }
            }
        },
        Commands::Action { command } => match command {
            ActionCommands::Invoke { action_id, payload } => {
                let ipc = IpcAdapter::new();
                match ipc.action_invoke(action_id.clone(), payload) {
                    Ok(()) => output.success(
                        "action.invoke",
                        &serde_json::json!({ "action_id": action_id }),
                        Some(&format!("Action '{action_id}' invoked")),
                        Vec::new(),
                    ),
                    Err((code, msg)) => {
                        output.error("action.invoke", "ipc_failed", &msg, None, Vec::new(), code)
                    }
                }
            }
        },
        Commands::Overview { action } => {
            let ipc = IpcAdapter::new();
            let res = match action {
                VisibilityAction::Show => ipc.overview_show(),
                VisibilityAction::Hide => ipc.overview_hide(),
                VisibilityAction::Toggle => ipc.overview_toggle(),
            };
            match res {
                Ok(()) => output.success(
                    "overview",
                    &serde_json::json!({ "action": format!("{action:?}") }),
                    Some(&format!("Overview action {action:?} applied")),
                    Vec::new(),
                ),
                Err((code, msg)) => {
                    output.error("overview", "ipc_failed", &msg, None, Vec::new(), code)
                }
            }
        }
        Commands::Bar { action } => {
            let ipc = IpcAdapter::new();
            let res = match action {
                VisibilityAction::Show => ipc.bar_show(),
                VisibilityAction::Hide => ipc.bar_hide(),
                VisibilityAction::Toggle => ipc.bar_toggle(),
            };
            match res {
                Ok(()) => output.success(
                    "bar",
                    &serde_json::json!({ "action": format!("{action:?}") }),
                    Some(&format!("Bar action {action:?} applied")),
                    Vec::new(),
                ),
                Err((code, msg)) => output.error("bar", "ipc_failed", &msg, None, Vec::new(), code),
            }
        }
        Commands::Workspace { command } => {
            let ipc = IpcAdapter::new();
            let (cmd_name, res) = match command {
                WorkspaceCommands::Focus { id } => ("workspace.focus", ipc.workspace_focus(id)),
                WorkspaceCommands::Create => ("workspace.create", ipc.workspace_create()),
            };
            match res {
                Ok(()) => output.success(
                    cmd_name,
                    &serde_json::json!({ "ok": true }),
                    Some("Workspace command applied"),
                    Vec::new(),
                ),
                Err((code, msg)) => {
                    output.error(cmd_name, "ipc_failed", &msg, None, Vec::new(), code)
                }
            }
        }
        Commands::Window { command } => {
            let ipc = IpcAdapter::new();
            let (cmd_name, res) = match command {
                WindowCommands::Focus { id } => ("window.focus", ipc.window_focus(id)),
                WindowCommands::FocusPrevious => {
                    ("window.focus_previous", ipc.window_focus_previous())
                }
                WindowCommands::Move { id, workspace } => {
                    ("window.move", ipc.window_move(id, workspace))
                }
            };
            match res {
                Ok(()) => output.success(
                    cmd_name,
                    &serde_json::json!({ "ok": true }),
                    Some("Window command applied"),
                    Vec::new(),
                ),
                Err((code, msg)) => {
                    output.error(cmd_name, "ipc_failed", &msg, None, Vec::new(), code)
                }
            }
        }
        Commands::Config { command } => match command {
            ConfigCommands::Path => {
                let path = DoctorChecker::default_config_path();
                output.success(
                    "config.path",
                    &serde_json::json!({ "path": path }),
                    Some(&path.display().to_string()),
                    Vec::new(),
                )
            }
            ConfigCommands::Validate => {
                let path = DoctorChecker::default_config_path();
                let result = adapters::ConfigAdapter::validate(&path);
                if result.success {
                    output.success(
                        result.command,
                        &result.data,
                        Some(&result.human_message),
                        result.warnings,
                    )
                } else {
                    output.error(
                        result.command,
                        result.error_code,
                        &result.human_message,
                        result.error_details,
                        result.warnings,
                        result.exit_code,
                    )
                }
            }
            ConfigCommands::Effective { origins } => {
                let path = DoctorChecker::default_config_path();
                let result = adapters::ConfigAdapter::effective(&path, origins);
                if result.success {
                    output.success(
                        result.command,
                        &result.data,
                        Some(&result.human_message),
                        result.warnings,
                    )
                } else {
                    output.error(
                        result.command,
                        result.error_code,
                        &result.human_message,
                        result.error_details,
                        result.warnings,
                        result.exit_code,
                    )
                }
            }
            ConfigCommands::Reload => {
                let ipc = IpcAdapter::new();
                match ipc.config_reload() {
                    Ok(()) => output.success(
                        "config.reload",
                        &serde_json::json!({ "reloaded": true }),
                        Some("Config reload signal sent to shell daemon"),
                        Vec::new(),
                    ),
                    Err((code, msg)) => {
                        output.error("config.reload", "ipc_failed", &msg, None, Vec::new(), code)
                    }
                }
            }
            ConfigCommands::Migrate { dry_run } => {
                let path = DoctorChecker::default_config_path();
                let result = adapters::ConfigMigrateAdapter::run(&path, dry_run);
                if result.success {
                    output.success(
                        "config.migrate",
                        &result.data,
                        Some(&result.human_message),
                        result.warnings,
                    )
                } else {
                    output.error(
                        "config.migrate",
                        result.error_code,
                        &result.human_message,
                        Some(result.data),
                        result.warnings,
                        result.exit_code,
                    )
                }
            }
        },
        Commands::Theme { command } => match command {
            ThemeCommands::Mode { action } => match action {
                ThemeModeAction::Get => match ThemeAdapter::get_mode().await {
                    Ok(mode) => output.success(
                        "theme.mode.get",
                        &serde_json::json!({ "mode": mode }),
                        Some(&mode),
                        Vec::new(),
                    ),
                    Err((code, msg)) => output.error(
                        "theme.mode.get",
                        "theme_daemon_unavailable",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                },
                ThemeModeAction::Set { mode } => {
                    let theme_mode = match mode {
                        ModeValue::Light => shilpo_m3e::theme::ThemeMode::Light,
                        ModeValue::Dark => shilpo_m3e::theme::ThemeMode::Dark,
                        ModeValue::System => shilpo_m3e::theme::ThemeMode::System,
                    };
                    match ThemeAdapter::set_mode(theme_mode).await {
                        Ok(msg) => output.success(
                            "theme.mode.set",
                            &serde_json::json!({ "mode": format!("{mode:?}") }),
                            Some(&msg),
                            Vec::new(),
                        ),
                        Err((code, msg)) => output.error(
                            "theme.mode.set",
                            "theme_daemon_unavailable",
                            &msg,
                            None,
                            Vec::new(),
                            code,
                        ),
                    }
                }
                ThemeModeAction::Toggle => match ThemeAdapter::toggle_mode().await {
                    Ok(msg) => output.success(
                        "theme.mode.toggle",
                        &serde_json::json!({ "toggled": true }),
                        Some(&msg),
                        Vec::new(),
                    ),
                    Err((code, msg)) => output.error(
                        "theme.mode.toggle",
                        "theme_daemon_unavailable",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                },
            },
            ThemeCommands::Seed { action } => match action {
                ThemeSeedAction::Set { color } => match ThemeAdapter::set_seed(&color).await {
                    Ok(msg) => output.success(
                        "theme.seed.set",
                        &serde_json::json!({ "seed": color }),
                        Some(&msg),
                        Vec::new(),
                    ),
                    Err((code, msg)) => output.error(
                        "theme.seed.set",
                        "theme_daemon_unavailable",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                },
            },
            ThemeCommands::Wallpaper { action } => match action {
                ThemeWallpaperAction::Get => match ThemeAdapter::get_wallpaper().await {
                    Ok(state) => {
                        let path = state.wallpaper_path.as_deref().map_or_else(
                            || "<none>".to_string(),
                            |path| path.display().to_string(),
                        );
                        let directory = state.wallpaper_dir.display().to_string();
                        output.success(
                            "theme.wallpaper.get",
                            &serde_json::json!({
                                "path": state.wallpaper_path,
                                "directory": state.wallpaper_dir,
                            }),
                            Some(&format!(
                                "Current wallpaper: {path}\nWallpaper directory: {directory}"
                            )),
                            Vec::new(),
                        )
                    }
                    Err((code, msg)) => output.error(
                        "theme.wallpaper.get",
                        "theme_daemon_unavailable",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                },
                ThemeWallpaperAction::Set { path } => {
                    match ThemeAdapter::set_wallpaper(&path).await {
                        Ok(msg) => output.success(
                            "theme.wallpaper.set",
                            &serde_json::json!({ "path": path }),
                            Some(&msg),
                            Vec::new(),
                        ),
                        Err((code, msg)) => output.error(
                            "theme.wallpaper.set",
                            "theme_daemon_unavailable",
                            &msg,
                            None,
                            Vec::new(),
                            code,
                        ),
                    }
                }
                ThemeWallpaperAction::Random => match ThemeAdapter::random_wallpaper().await {
                    Ok(msg) => output.success(
                        "theme.wallpaper.random",
                        &serde_json::json!({ "random": true }),
                        Some(&msg),
                        Vec::new(),
                    ),
                    Err((code, msg)) => output.error(
                        "theme.wallpaper.random",
                        "theme_daemon_unavailable",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                },
            },
        },
        Commands::Brightness { command } => match command {
            BrightnessCommands::List => {
                let (ddc_displays, ddc_perms) =
                    shilpo_services::brightness::discover_ddc_displays();
                let data = serde_json::json!({
                    "displays": ddc_displays,
                    "permissions_ok": ddc_perms,
                });
                let mut report =
                    format!("Discovered DDC/CI Displays (Permissions OK: {ddc_perms}):\n");
                for d in &ddc_displays {
                    report.push_str(&format!(
                        " - [{}] {} (Connector: {}, Brightness: {}%)\n",
                        d.id,
                        d.name,
                        d.connector.as_deref().unwrap_or("<unknown>"),
                        d.percentage
                    ));
                }
                output.success("brightness.list", &data, Some(&report), Vec::new())
            }
            BrightnessCommands::Set { display, value } => {
                let ipc = IpcAdapter::new();
                let res = if let Some(disp_id) = display {
                    ipc.set_display_brightness(disp_id, value)
                } else {
                    ipc.set_brightness(value)
                };
                match res {
                    Ok(_) => output.success(
                        "brightness.set",
                        &serde_json::json!({ "value": value }),
                        Some(&format!("Set brightness to {value}%")),
                        Vec::new(),
                    ),
                    Err((code, msg)) => output.error(
                        "brightness.set",
                        "daemon_unavailable",
                        &msg,
                        None,
                        Vec::new(),
                        code,
                    ),
                }
            }
        },
        Commands::Setup => match shilpo::setup::run() {
            Ok(()) => std::process::exit(EXIT_SUCCESS),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(EXIT_FAILURE);
            }
        },
        Commands::Doctor {
            fix,
            first_login,
            telemetry,
        } => {
            if telemetry {
                if fix || first_login {
                    let code = output.error(
                        "doctor.telemetry",
                        "usage.invalid_arguments",
                        "cannot combine --telemetry with --fix or --first-login",
                        None,
                        Vec::new(),
                        EXIT_INVALID_ARGS,
                    );
                    std::process::exit(code);
                }
                let profile_dir = match shilpo_observability::resolve_profile_dir() {
                    Ok(d) => d,
                    Err(e) => {
                        let code = output.error(
                            "doctor.telemetry",
                            "doctor.telemetry.unavailable",
                            &e.to_string(),
                            None,
                            Vec::new(),
                            EXIT_FAILURE,
                        );
                        std::process::exit(code);
                    }
                };
                match shilpo_observability::summarize_profiles(&profile_dir) {
                    Ok(summary) => {
                        let human_msg = format!(
                            "Profile Directory: {}\nEnabled: {}\nCompleted Traces: {} ({} bytes)\nIncomplete Traces: {} ({} bytes)",
                            summary.profile_dir.display(),
                            summary.profile_enabled,
                            summary.completed_count,
                            summary.completed_bytes,
                            summary.incomplete_count,
                            summary.incomplete_bytes,
                        );
                        output.success(
                            "doctor.telemetry",
                            &summary,
                            Some(&human_msg),
                            summary.warnings.clone(),
                        )
                    }
                    Err(err) => {
                        let code = output.error(
                            "doctor.telemetry",
                            "doctor.telemetry.unavailable",
                            &err,
                            None,
                            Vec::new(),
                            EXIT_FAILURE,
                        );
                        std::process::exit(code);
                    }
                }
            } else {
                let doctor = DoctorChecker::new();
                let (items, has_fail) = if first_login {
                    doctor.run_first_login_report(fix)
                } else {
                    let items = doctor.run_diagnostics(fix);
                    let fail = items
                        .iter()
                        .any(|i| i.status == adapters::doctor::DiagnosticStatus::Fail);
                    (items, fail)
                };
                let report_str = doctor.format_report(&items);
                if has_fail {
                    output.error(
                        "doctor",
                        "diagnostics_failed",
                        &report_str,
                        Some(serde_json::to_value(&items).unwrap_or_default()),
                        Vec::new(),
                        EXIT_FAILURE,
                    )
                } else {
                    output.success("doctor", &items, Some(&report_str), Vec::new())
                }
            }
        }
        Commands::Profile { command } => match command {
            ProfileCommands::Export {
                output: out_path,
                source,
            } => {
                let profile_dir = match shilpo_observability::resolve_profile_dir() {
                    Ok(d) => d,
                    Err(e) => {
                        let code = output.error(
                            "profile.export",
                            "profile.export.dir_unavailable",
                            &e.to_string(),
                            None,
                            Vec::new(),
                            EXIT_FAILURE,
                        );
                        std::process::exit(code);
                    }
                };
                match shilpo_observability::export_trace(source.as_deref(), &out_path, &profile_dir)
                {
                    Ok(report) => {
                        let human_msg = format!(
                            "Exported {} trace ({} bytes) to {}",
                            report.process_role,
                            report.bytes,
                            report.output.display()
                        );
                        output.success("profile.export", &report, Some(&human_msg), Vec::new())
                    }
                    Err(err) => {
                        let code = output.error(
                            "profile.export",
                            err.stable_code(),
                            &err.to_string(),
                            None,
                            Vec::new(),
                            EXIT_FAILURE,
                        );
                        std::process::exit(code);
                    }
                }
            }
        },
        Commands::Ext { command } => {
            let ext = ExtAdapter::new();
            let op = match command {
                ExtCommands::New {
                    name,
                    target,
                    language,
                    contribution,
                    package_manager,
                    view_syntax,
                    extension_id,
                    package_name,
                    description,
                    capabilities,
                    subscriptions,
                    install,
                    build,
                    git,
                    yes,
                } => {
                    use std::io::IsTerminal;
                    let is_interactive =
                        std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
                    ext.scaffold_new(
                        &name,
                        target,
                        language.map(Into::into),
                        contribution.map(Into::into),
                        package_manager.map(Into::into),
                        view_syntax.map(Into::into),
                        extension_id,
                        package_name,
                        description,
                        &capabilities,
                        &subscriptions,
                        install,
                        build,
                        git,
                        yes,
                        is_interactive,
                        cli.json,
                        cli.quiet,
                    )
                }
                ExtCommands::Status => ext.status(),
                ExtCommands::Build { path, release } => ext.build(path.as_deref(), release),
                ExtCommands::Check { path } => ext.check(path.as_deref()),
                ExtCommands::Lint {
                    path,
                    deny_warnings,
                } => ext.lint(path.as_deref(), deny_warnings, cli.json, cli.quiet, timeout),
                ExtCommands::Pack { path, output } => ext.pack(path.as_deref(), output.as_deref()),
                ExtCommands::Dev { path } => ext.dev(path.as_deref(), cli.json, cli.quiet, timeout),
                ExtCommands::Logs { id: _, follow } if cli.json && follow => {
                    adapters::ext::ExtOpResult {
                        success: false,
                        data: serde_json::Value::Null,
                        human_message: "--json cannot be combined with --follow".into(),
                        warnings: Vec::new(),
                        exit_code: EXIT_INVALID_ARGS,
                    }
                }
                ExtCommands::Logs { id, follow } => ext.logs(id.as_deref(), follow),
                ExtCommands::List { dev } => ext.list(dev),
                ExtCommands::Search { query } => {
                    let snapshot = shilpo_ext_runtime::ExtensionCatalog::open_default().snapshot();
                    let q = query.unwrap_or_default().to_lowercase();
                    let matches = snapshot
                        .discover
                        .into_iter()
                        .filter(|e| {
                            e.release.id.to_string().to_lowercase().contains(&q)
                                || e.release.name.to_lowercase().contains(&q)
                        })
                        .collect::<Vec<_>>();
                    adapters::ext::ExtOpResult {
                        success: true,
                        data: serde_json::to_value(&matches).unwrap_or_default(),
                        human_message: format!("Found {} matching extension(s)", matches.len()),
                        warnings: Vec::new(),
                        exit_code: 0,
                    }
                }
                ExtCommands::Info { id } => {
                    let snapshot = shilpo_ext_runtime::ExtensionCatalog::open_default().snapshot();
                    if let Some(item) = snapshot
                        .discover
                        .into_iter()
                        .find(|e| e.release.id.to_string() == id)
                    {
                        adapters::ext::ExtOpResult {
                            success: true,
                            data: serde_json::to_value(&item).unwrap_or_default(),
                            human_message: format!(
                                "Name: {}\nID: {}\nVersion: {}",
                                item.release.name, item.release.id, item.release.version
                            ),
                            warnings: Vec::new(),
                            exit_code: 0,
                        }
                    } else {
                        adapters::ext::ExtOpResult {
                            success: false,
                            data: serde_json::Value::Null,
                            human_message: format!("Extension '{id}' not found in catalog"),
                            warnings: Vec::new(),
                            exit_code: 1,
                        }
                    }
                }
                ExtCommands::Install { target, hash } => ext.install(&target, hash.as_deref()),
                ExtCommands::Update { id, all, dry_run } => ext.update(id.as_deref(), all, dry_run),
                ExtCommands::Enable { id } => ext.enable(&id),
                ExtCommands::Disable { id } => ext.disable(&id),
                ExtCommands::Approve { id, grant_all } => ext.approve(&id, grant_all),
                ExtCommands::Rollback { id } => ext.rollback(&id),
                ExtCommands::Uninstall {
                    id,
                    delete_secrets,
                    delete_state,
                } => ext.uninstall(&id, delete_secrets, delete_state),
                ExtCommands::CheckUpdates => ext.check_updates(),
                ExtCommands::Channel { id, channel } => ext.channel(&id, channel.as_deref()),
                ExtCommands::Source { args } => ext.source(&args),
                ExtCommands::RefreshSources => ext.refresh_sources(),
                ExtCommands::Sign {
                    package,
                    key,
                    publisher,
                } => ext.sign(&package, &key, &publisher),
                ExtCommands::Keygen { output } => ext.keygen(&output),
            };

            if op.success {
                output.success("ext", &op.data, Some(&op.human_message), op.warnings)
            } else {
                output.error(
                    "ext",
                    "extension_operation_failed",
                    &op.human_message,
                    Some(op.data),
                    op.warnings,
                    op.exit_code,
                )
            }
        }
        Commands::Capture { action } => {
            let ipc = IpcAdapter::new();
            let intent = match action {
                CaptureAction::Region => shilpo_services::capture::CaptureIntent::Clipboard,
                CaptureAction::Edit => shilpo_services::capture::CaptureIntent::Annotation,
                CaptureAction::Ocr => shilpo_services::capture::CaptureIntent::Ocr,
                CaptureAction::Menu => shilpo_services::capture::CaptureIntent::Menu,
            };
            match ipc.capture(intent) {
                Ok(()) => {
                    let text = "Capture request accepted by shilpo-shell";
                    output.success(
                        "capture",
                        &serde_json::json!({ "accepted": true }),
                        Some(text),
                        Vec::new(),
                    )
                }
                Err((code, msg)) => {
                    output.error("capture", "ipc_failed", &msg, None, Vec::new(), code)
                }
            }
        }
    };

    std::process::exit(exit_code);
}
