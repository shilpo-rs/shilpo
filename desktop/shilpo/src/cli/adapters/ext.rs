use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use shilpo_ext_api::{Capability, EventKind, ExtensionId, Subscription};
use shilpo_ext_runtime::{
    ExtensionCatalog, ExtensionCli, ExtensionCliResult, LintOptions, LintReport, LintSeverity,
    PackageManager, ReleaseChannel, ScaffoldError, ScaffoldOptions, StarterContribution,
    StarterLanguage, UpdateState, ViewSyntax, default_extension_state_dir, derive_package_name,
    scaffold_extension, sign_package,
};

use super::ipc::IpcAdapter;

pub struct ExtAdapter {
    ipc: IpcAdapter,
    state_dir: PathBuf,
    catalog: ExtensionCatalog,
}

pub struct ExtOpResult {
    pub success: bool,
    pub data: serde_json::Value,
    pub human_message: String,
    pub warnings: Vec<String>,
    pub exit_code: i32,
}

impl Default for ExtAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtAdapter {
    pub fn new() -> Self {
        Self {
            ipc: IpcAdapter::new(),
            state_dir: default_extension_state_dir(),
            catalog: ExtensionCatalog::open_default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn scaffold_new(
        &self,
        name: &str,
        target: Option<PathBuf>,
        language: Option<StarterLanguage>,
        contribution: Option<StarterContribution>,
        package_manager: Option<PackageManager>,
        view_syntax: Option<ViewSyntax>,
        extension_id: Option<String>,
        package_name: Option<String>,
        description: Option<String>,
        capabilities_raw: &[String],
        subscriptions_raw: &[String],
        install: bool,
        build: bool,
        git: bool,
        yes: bool,
        is_interactive: bool,
        is_json: bool,
        is_quiet: bool,
    ) -> ExtOpResult {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: "extension name cannot be empty".into(),
                warnings: Vec::new(),
                exit_code: crate::cli::output::EXIT_INVALID_ARGS,
            };
        }

        // Parse capability JSONs
        let mut capabilities = Vec::new();
        for cap_json in capabilities_raw {
            match serde_json::from_str::<Capability>(cap_json) {
                Ok(cap) => capabilities.push(cap),
                Err(e) => {
                    return ExtOpResult {
                        success: false,
                        data: serde_json::Value::Null,
                        human_message: format!("invalid capability JSON '{cap_json}': {e}"),
                        warnings: Vec::new(),
                        exit_code: crate::cli::output::EXIT_INVALID_ARGS,
                    };
                }
            }
        }

        // Parse subscriptions
        let mut subscriptions = Vec::new();
        for sub_str in subscriptions_raw {
            let event_json = format!("\"{sub_str}\"");
            match serde_json::from_str::<EventKind>(&event_json) {
                Ok(event) => subscriptions.push(Subscription { event }),
                Err(_) => {
                    return ExtOpResult {
                        success: false,
                        data: serde_json::Value::Null,
                        human_message: format!(
                            "invalid event '{sub_str}': expected one of 'outputs_changed', 'theme_changed', 'palette_generated', 'wallpaper_changed', 'network_changed', 'media_changed', 'power_changed', 'timer_fired', 'workspace_changed'"
                        ),
                        warnings: Vec::new(),
                        exit_code: crate::cli::output::EXIT_INVALID_ARGS,
                    };
                }
            }
        }

        // Resolve target directory
        let target_dir = match target {
            Some(t) => t,
            None => {
                let default_dir_name =
                    derive_package_name(trimmed_name, StarterLanguage::Typescript, None)
                        .unwrap_or_else(|_| "my-extension".into());
                PathBuf::from(default_dir_name)
            }
        };

        if language == Some(StarterLanguage::Rust) && view_syntax.is_some() {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: "--view-syntax is only supported for TypeScript extensions".into(),
                warnings: Vec::new(),
                exit_code: crate::cli::output::EXIT_INVALID_ARGS,
            };
        }

        let mut chosen_language = language;
        let mut chosen_contribution = contribution;
        let mut chosen_pm = package_manager;
        let mut chosen_view_syntax = view_syntax;
        let mut chosen_git = git;
        let mut chosen_install = install;
        let mut chosen_description = description;

        // Interactive / Non-interactive requirements
        if chosen_language.is_none() || chosen_contribution.is_none() {
            if !is_interactive || is_json {
                return ExtOpResult {
                    success: false,
                    data: serde_json::Value::Null,
                    human_message: "missing required arguments: --language and --contribution are required in non-interactive/json mode".into(),
                    warnings: Vec::new(),
                    exit_code: crate::cli::output::EXIT_INVALID_ARGS,
                };
            }

            // Interactive prompts
            println!("Creating new extension '{trimmed_name}'...\n");

            if chosen_language.is_none() {
                println!("Select implementation language:");
                println!("  1) TypeScript (recommended)");
                println!("  2) Rust");
                print!("Selection [1]: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let choice = line.trim();
                chosen_language = match choice {
                    "2" | "rust" | "Rust" => Some(StarterLanguage::Rust),
                    _ => Some(StarterLanguage::Typescript),
                };
            }

            if chosen_contribution.is_none() {
                println!("\nSelect starter template:");
                println!("  1) bar-widget     - Status bar indicator widget with action");
                println!("  2) desktop-widget - Desktop canvas widget container");
                println!("  3) settings-page  - Settings configuration panel with schema");
                println!("  4) side-panel     - Side drawer panel with layout");
                println!("  5) action         - Custom desktop action handler");
                println!("  6) empty          - Minimal compilable component");
                print!("Selection [1]: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let choice = line.trim();
                chosen_contribution = match choice {
                    "2" | "desktop-widget" | "desktop" => Some(StarterContribution::DesktopWidget),
                    "3" | "settings-page" | "settings" => Some(StarterContribution::SettingsPage),
                    "4" | "side-panel" | "panel" => Some(StarterContribution::SidePanel),
                    "5" | "action" => Some(StarterContribution::Action),
                    "6" | "empty" => Some(StarterContribution::Empty),
                    _ => Some(StarterContribution::BarWidget),
                };
            }

            if chosen_language == Some(StarterLanguage::Typescript) && chosen_view_syntax.is_none()
            {
                println!("\nSelect view authoring syntax:");
                println!("  1) JSX (recommended)");
                println!("  2) Builder functions");
                print!("Selection [1]: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let choice = line.trim();
                chosen_view_syntax = match choice {
                    "2" | "builders" | "builder" => Some(ViewSyntax::Builders),
                    _ => Some(ViewSyntax::Jsx),
                };
            }

            if chosen_language == Some(StarterLanguage::Typescript) && chosen_pm.is_none() {
                println!("\nSelect package manager:");
                println!("  1) npm (default)");
                println!("  2) pnpm");
                println!("  3) yarn");
                println!("  4) bun");
                println!("  5) deno");
                print!("Selection [1]: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let choice = line.trim();
                chosen_pm = match choice {
                    "2" | "pnpm" => Some(PackageManager::Pnpm),
                    "3" | "yarn" => Some(PackageManager::Yarn),
                    "4" | "bun" => Some(PackageManager::Bun),
                    "5" | "deno" => Some(PackageManager::Deno),
                    _ => Some(PackageManager::Npm),
                };
            }

            if !git {
                print!("\nInitialize git repository? [y/N]: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                if line.trim().eq_ignore_ascii_case("y") || line.trim().eq_ignore_ascii_case("yes")
                {
                    chosen_git = true;
                }
            }

            if !install && !build && chosen_language == Some(StarterLanguage::Typescript) {
                print!("\nInstall dependencies now? [y/N]: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                if line.trim().eq_ignore_ascii_case("y") || line.trim().eq_ignore_ascii_case("yes")
                {
                    chosen_install = true;
                }
            }
        }

        if is_interactive && !is_json {
            if chosen_description.is_none() {
                print!("\nDescription (optional, press Enter to skip): ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let value = line.trim().to_string();
                if !value.is_empty() {
                    chosen_description = Some(value);
                }
            }

            let preview_language = chosen_language.unwrap_or(StarterLanguage::Typescript);
            let preview_contribution =
                chosen_contribution.unwrap_or(StarterContribution::BarWidget);
            let preview_pm = chosen_pm.unwrap_or(PackageManager::Npm);
            let preview_syntax = chosen_view_syntax.unwrap_or(ViewSyntax::Jsx);
            if !yes {
                let syntax_line = if preview_language == StarterLanguage::Typescript {
                    format!("\n  view syntax: {preview_syntax}")
                } else {
                    String::new()
                };
                println!(
                    "\nSummary:\n  name: {trimmed_name}\n  target: {}\n  language: {preview_language}\n  contribution: {preview_contribution}{syntax_line}\n  package manager: {preview_pm}\n  git: {}\n  install: {}",
                    target_dir.display(),
                    chosen_git,
                    chosen_install || build
                );
                print!("\nCreate this extension? [Y/n]: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let trimmed_resp = line.trim();
                if !trimmed_resp.is_empty()
                    && !trimmed_resp.eq_ignore_ascii_case("y")
                    && !trimmed_resp.eq_ignore_ascii_case("yes")
                {
                    return ExtOpResult {
                        success: false,
                        data: serde_json::Value::Null,
                        human_message: "operation cancelled by user".into(),
                        warnings: Vec::new(),
                        exit_code: 130,
                    };
                }
            }
        }

        let final_language = chosen_language.unwrap_or(StarterLanguage::Typescript);
        let final_contribution = chosen_contribution.unwrap_or(StarterContribution::BarWidget);
        let final_pm = if final_language == StarterLanguage::Typescript {
            chosen_pm.or(Some(PackageManager::Npm))
        } else {
            None
        };
        let final_view_syntax = if final_language == StarterLanguage::Typescript {
            chosen_view_syntax.or(Some(ViewSyntax::Jsx))
        } else {
            None
        };

        let options = ScaffoldOptions {
            name: trimmed_name.to_string(),
            target_dir,
            language: final_language,
            contribution: final_contribution,
            package_manager: final_pm,
            view_syntax: final_view_syntax,
            extension_id,
            package_name,
            description: chosen_description,
            capabilities,
            subscriptions,
            install: chosen_install,
            build,
            git: chosen_git,
        };

        match scaffold_extension(&options, &shilpo_ext_runtime::OsProcessRunner) {
            Ok(result) => {
                if is_quiet {
                    println!("{}", result.target_dir.display());
                }

                let mut human_lines = vec![format!(
                    "Created extension '{}' ({}) at {}",
                    result.name,
                    result.extension_id,
                    result.target_dir.display()
                )];
                human_lines.push(String::new());
                human_lines.push("Next steps:".into());
                for step in &result.next_steps {
                    human_lines.push(format!("  {step}"));
                }

                ExtOpResult {
                    success: true,
                    data: serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
                    human_message: human_lines.join("\n"),
                    warnings: Vec::new(),
                    exit_code: 0,
                }
            }
            Err(err) => {
                let exit_code = match &err {
                    ScaffoldError::InvalidTarget(_)
                    | ScaffoldError::InvalidExtensionId(_)
                    | ScaffoldError::InvalidPackageName(_)
                    | ScaffoldError::InvalidCapability(_)
                    | ScaffoldError::InvalidSubscription(_)
                    | ScaffoldError::CapabilityConflict(_)
                    | ScaffoldError::TargetExistsAndNotEmpty(_)
                    | ScaffoldError::TargetIsFile(_)
                    | ScaffoldError::TargetIsSymlink(_)
                    | ScaffoldError::PathTraversal(_) => crate::cli::output::EXIT_INVALID_ARGS,
                    ScaffoldError::Cancelled => 130,
                    _ => crate::cli::output::EXIT_FAILURE,
                };

                ExtOpResult {
                    success: false,
                    data: serde_json::Value::Null,
                    human_message: err.to_string(),
                    warnings: Vec::new(),
                    exit_code,
                }
            }
        }
    }

    fn notify_daemon_if_needed(&self, result: &mut ExtOpResult) {
        if result.success {
            match self.ipc.config_reload() {
                Ok(()) => {}
                Err((3, _)) => record_refresh_warning(result, "live shell daemon is not running"),
                Err((_, e)) => {
                    record_refresh_warning(result, format!("live daemon refresh failed: {e}"))
                }
            }
        }
    }

    pub fn status(&self) -> ExtOpResult {
        match self.ipc.telemetry() {
            Ok(telemetry) => {
                let diagnostics = telemetry.extension_host_diagnostics_json;
                let ext_status: serde_json::Value =
                    serde_json::from_str(&diagnostics).unwrap_or(serde_json::Value::Null);
                if ext_status.is_null() {
                    return ExtOpResult {
                        success: false,
                        data: serde_json::Value::Null,
                        human_message: "Extension host diagnostics are unavailable".into(),
                        warnings: Vec::new(),
                        exit_code: 1,
                    };
                }
                let state_str = ext_status
                    .get("lifecycle")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let host_gen = ext_status
                    .get("host_generation")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let engine_gen = ext_status
                    .get("engine_generation")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let mut human_lines = vec![
                    "Extension Host Status:".to_string(),
                    format!("  State: {state_str}"),
                    format!("  Host Generation: {host_gen}"),
                    format!("  Engine Generation: {engine_gen}"),
                ];

                let wasm_extensions = ext_status.get("wasm_extensions").and_then(|v| v.as_array());

                if let Some(wasms) = wasm_extensions
                    && !wasms.is_empty()
                {
                    human_lines.push(String::new());
                    human_lines.push(format!("WASM Extensions ({}):", wasms.len()));
                    for ext in wasms {
                        let id = ext.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
                        let state = ext
                            .get("state")
                            .and_then(|v| v.as_str())
                            .unwrap_or("closed");
                        let trip_count =
                            ext.get("trip_count").and_then(|v| v.as_u64()).unwrap_or(0);
                        let desc = match state {
                            "closed" => {
                                let failures = ext
                                    .get("consecutive_failures")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                if failures > 0 {
                                    format!("closed, {failures} failure(s)")
                                } else {
                                    "closed, healthy".to_string()
                                }
                            }
                            "open" => {
                                let retry_ms = ext
                                    .get("retry_after_ms")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let secs = retry_ms.div_ceil(1000);
                                format!("open, retrying in {secs}s (trip {trip_count})")
                            }
                            "half_open" => {
                                let successes = ext
                                    .get("consecutive_successes")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                format!(
                                    "half_open, probing ({successes}/3 successes, trip {trip_count})"
                                )
                            }
                            "permanently_disabled" => {
                                format!(
                                    "permanently_disabled, failed after {trip_count} trip cycles"
                                )
                            }
                            other => other.to_string(),
                        };
                        human_lines.push(format!("  {id} [{desc}]"));
                    }
                }

                let script_extensions = ext_status
                    .get("script_extensions")
                    .and_then(|v| v.as_array());

                if let Some(scripts) = script_extensions
                    && !scripts.is_empty()
                {
                    human_lines.push(String::new());
                    human_lines.push(format!("Script Extensions ({}):", scripts.len()));
                    for script in scripts {
                        let id = script
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let status = script
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let version = script
                            .get("version")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        human_lines.push(format!("  {id} (v{version}) [{status}]"));
                    }
                }

                ExtOpResult {
                    success: true,
                    data: ext_status,
                    human_message: human_lines.join("\n"),
                    warnings: Vec::new(),
                    exit_code: 0,
                }
            }
            Err((3, _)) => ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message:
                    "shell daemon is unavailable; cannot query extension host diagnostics".into(),
                warnings: Vec::new(),
                exit_code: 3,
            },
            Err((code, msg)) => ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("Failed to query telemetry from daemon: {msg}"),
                warnings: Vec::new(),
                exit_code: code,
            },
        }
    }

    pub fn build(&self, path: Option<&Path>, release: bool) -> ExtOpResult {
        let target = path.unwrap_or_else(|| Path::new("."));
        let cli_res = ExtensionCli::build(target, release);
        ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "artifact": cli_res.artifact,
                "release": release,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        }
    }

    pub fn check(&self, path: Option<&Path>) -> ExtOpResult {
        let target = path.unwrap_or_else(|| Path::new("."));
        let cli_res = ExtensionCli::check(target);
        ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        }
    }

    pub fn lint(
        &self,
        path: Option<&Path>,
        deny_warnings: bool,
        _is_json: bool,
        is_quiet: bool,
        timeout: std::time::Duration,
    ) -> ExtOpResult {
        let target = path.unwrap_or_else(|| Path::new("."));
        let report = ExtensionCli::lint(
            target,
            LintOptions {
                deny_warnings,
                timeout,
            },
        );
        let human_message = format_human_lint_report(&report, is_quiet);
        let exit_code = if report.passed {
            crate::cli::output::EXIT_SUCCESS
        } else {
            crate::cli::output::EXIT_FAILURE
        };

        ExtOpResult {
            success: report.passed,
            data: serde_json::to_value(&report).unwrap_or(serde_json::Value::Null),
            human_message,
            warnings: Vec::new(),
            exit_code,
        }
    }

    pub fn pack(&self, path: Option<&Path>, output: Option<&Path>) -> ExtOpResult {
        let source = path.unwrap_or_else(|| Path::new("."));
        let out_dir = output
            .map(PathBuf::from)
            .unwrap_or_else(|| source.join("dist"));
        let cli_res = ExtensionCli::pack(source, &out_dir);
        ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "artifact": cli_res.artifact,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        }
    }

    pub fn dev(
        &self,
        path: Option<&Path>,
        json: bool,
        quiet: bool,
        timeout: std::time::Duration,
    ) -> ExtOpResult {
        let target_dir = path.unwrap_or_else(|| Path::new("."));
        let canonical_root = match target_dir.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                let err_msg = format!(
                    "failed to resolve extension path '{}': {e}",
                    target_dir.display()
                );
                return ExtOpResult {
                    success: false,
                    data: serde_json::json!({ "error": err_msg }),
                    human_message: err_msg,
                    warnings: Vec::new(),
                    exit_code: 1,
                };
            }
        };

        let manifest_path = canonical_root.join("extension.toml");
        let manifest_str = match std::fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                let err_msg = format!(
                    "failed to read extension.toml at '{}': {e}",
                    manifest_path.display()
                );
                return ExtOpResult {
                    success: false,
                    data: serde_json::json!({ "error": err_msg }),
                    human_message: err_msg,
                    warnings: Vec::new(),
                    exit_code: 1,
                };
            }
        };

        let manifest = match shilpo_ext_api::ExtensionManifest::from_toml(&manifest_str) {
            Ok(m) => m,
            Err(e) => {
                let err_msg = format!(
                    "invalid extension.toml at '{}': {e}",
                    manifest_path.display()
                );
                return ExtOpResult {
                    success: false,
                    data: serde_json::json!({ "error": err_msg }),
                    human_message: err_msg,
                    warnings: Vec::new(),
                    exit_code: 1,
                };
            }
        };

        // Step 1: Initial Build
        if !quiet && !json {
            eprintln!(
                "Building extension '{}' (v{})...",
                manifest.id, manifest.version
            );
        }

        let initial_build = ExtensionCli::build_with_timeout(&canonical_root, false, timeout);
        if !initial_build.success {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "build",
                        "status": "failed",
                        "build_sequence": 1,
                        "extension_id": manifest.id.to_string(),
                        "diagnostics": initial_build.diagnostics,
                    })
                );
            }
            return ExtOpResult {
                success: false,
                data: serde_json::json!({
                    "event": "build",
                    "status": "failed",
                    "extension_id": manifest.id.to_string(),
                    "diagnostics": initial_build.diagnostics,
                }),
                human_message: format!(
                    "Initial build failed for extension '{}':\n{}",
                    manifest.id,
                    initial_build.diagnostics.join("\n")
                ),
                warnings: Vec::new(),
                exit_code: 1,
            };
        }

        let initial_artifact = match initial_build.artifact.clone() {
            Some(path) => path,
            None => {
                let message = "build succeeded without an artifact".to_string();
                return ExtOpResult {
                    success: false,
                    data: serde_json::json!({ "error": message }),
                    human_message: message,
                    warnings: Vec::new(),
                    exit_code: 1,
                };
            }
        };
        let artifact_relative = match initial_artifact.strip_prefix(&canonical_root) {
            Ok(path) if !path.as_os_str().is_empty() => path.to_string_lossy().into_owned(),
            _ => {
                let message = format!(
                    "build artifact '{}' is outside the extension root",
                    initial_artifact.display()
                );
                return ExtOpResult {
                    success: false,
                    data: serde_json::json!({ "error": message }),
                    human_message: message,
                    warnings: Vec::new(),
                    exit_code: 1,
                };
            }
        };

        if json {
            println!(
                "{}",
                serde_json::json!({
                    "event": "build",
                    "status": "success",
                    "build_sequence": 1,
                    "extension_id": manifest.id.to_string(),
                })
            );
        }

        // Step 2: D-Bus connection & StartDevSession
        //
        // `dev()` is called synchronously from within `main`'s own multi-threaded Tokio
        // runtime (see `#[tokio::main] async fn main()`), so building and blocking on a
        // second, independent runtime here panics with "Cannot start a runtime from within
        // a runtime". `block_in_place` is the standard escape hatch for a long-running
        // blocking call made from inside an already-running multi-threaded runtime: it
        // hands this worker thread's other queued work to the remaining workers for the
        // duration of the (effectively unbounded, file-watching) loop below.
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
            let conn = match zbus::Connection::session().await {
                Ok(c) => c,
                Err(e) => {
                    return Err(format!(
                        "failed to connect to session D-Bus: {e}. Is the Shilpo shell running?"
                    ));
                }
            };

            let shell_proxy = match crate::shell::dbus::ShellProxy::new(&conn).await {
                Ok(p) => p,
                Err(e) => {
                    return Err(format!("failed to attach ShellProxy: {e}"));
                }
            };

            let session_id = match shell_proxy
                .start_dev_session(
                    manifest.id.to_string(),
                    canonical_root.to_string_lossy().to_string(),
                )
                .await
            {
                Ok(id) => id,
                Err(e) => {
                    return Err(format!("StartDevSession failed: {e}"));
                }
            };

            let mut build_sequence: u64 = 1;

            // Step 3: Initial Reload
            let reload_res = tokio::time::timeout(
                timeout,
                shell_proxy.reload_dev_session(
                    session_id.clone(),
                    build_sequence,
                    artifact_relative.clone(),
                    timeout.as_millis().min(u64::MAX as u128) as u64,
                ),
            )
            .await
            .map_err(|_| format!("reload timed out after {timeout:?}"))?;

            match reload_res {
                Ok(res) => {
                    if res.outcome == "applied" {
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "reload",
                                    "status": "applied",
                                    "build_sequence": build_sequence,
                                    "extension_id": manifest.id.to_string(),
                                    "session_id": session_id,
                                    "host_generation": res.host_generation,
                                    "engine_generation": res.engine_generation,
                                })
                            );
                        } else if !quiet {
                            eprintln!(
                                "Extension '{}' (v{}) loaded into dev session '{}' (build #{})",
                                manifest.id, manifest.version, session_id, build_sequence
                            );
                        }
                    } else {
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "reload",
                                    "status": res.outcome,
                                    "build_sequence": build_sequence,
                                    "extension_id": manifest.id.to_string(),
                                    "diagnostic_code": res.diagnostic_code,
                                    "message": res.message,
                                })
                            );
                        } else {
                            eprintln!(
                                "Reload rejected [{}]: {}",
                                res.diagnostic_code, res.message
                            );
                        }
                    }
                }
                Err(e) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "event": "reload",
                                "status": "error",
                                "build_sequence": build_sequence,
                                "extension_id": manifest.id.to_string(),
                                "error": e.to_string(),
                            })
                        );
                    } else {
                        eprintln!("Reload error: {e}");
                    }
                }
            }

            if !quiet && !json {
                eprintln!(
                    "Watching '{}' for changes... (Press Ctrl+C to stop)",
                    canonical_root.display()
                );
            }

            // Step 4: File watcher setup
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
            let root_for_watcher = canonical_root.clone();

            let mut watcher = match notify::RecommendedWatcher::new(
                move |res: Result<notify::Event, notify::Error>| {
                    if let Ok(event) = res
                        && (event.kind.is_modify()
                            || event.kind.is_create()
                            || event.kind.is_remove())
                    {
                        let relevant = event.paths.iter().any(|p| !should_ignore_path(p, &root_for_watcher));
                        if relevant {
                            let _ = event_tx.try_send(());
                        }
                    }
                },
                notify::Config::default(),
            ) {
                Ok(w) => w,
                Err(e) => return Err(format!("failed to initialize file watcher: {e}")),
            };

            use notify::Watcher;
            if let Err(e) = watcher.watch(&canonical_root, notify::RecursiveMode::Recursive) {
                return Err(format!("failed to watch directory: {e}"));
            }

            // Step 5: Event loop with debouncing and Ctrl-C handling
            let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|e| format!("failed to register SIGINT handler: {e}"))?;
            let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|e| format!("failed to register SIGTERM handler: {e}"))?;

            loop {
                tokio::select! {
                    _ = sigint.recv() => {
                        if !quiet && !json {
                            eprintln!("\nReceived interrupt signal, ending dev session...");
                        }
                        let _ = shell_proxy.end_dev_session(session_id.clone()).await;
                        break;
                    }
                    _ = sigterm.recv() => {
                        if !quiet && !json {
                            eprintln!("\nReceived terminate signal, ending dev session...");
                        }
                        let _ = shell_proxy.end_dev_session(session_id.clone()).await;
                        break;
                    }
                    Some(()) = event_rx.recv() => {
                        // Debounce window (150ms)
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        // Drain any coalesced changes that arrived during debounce window
                        while event_rx.try_recv().is_ok() {}

                        build_sequence += 1;
                        if !quiet && !json {
                            eprintln!("File changes detected. Rebuilding #{}...", build_sequence);
                        }

                        let root_clone = canonical_root.clone();
                        let build_res = tokio::task::spawn_blocking(move || {
                            ExtensionCli::build_with_timeout(&root_clone, false, timeout)
                        }).await.map_err(|e| format!("build task join error: {e}"))?;

                        if !build_res.success {
                            if json {
                                println!(
                                    "{}",
                                    serde_json::json!({
                                        "event": "build",
                                        "status": "failed",
                                        "build_sequence": build_sequence,
                                        "extension_id": manifest.id.to_string(),
                                        "diagnostics": build_res.diagnostics,
                                    })
                                );
                            } else {
                                eprintln!(
                                    "Build #{} failed:\n{}",
                                    build_sequence,
                                    build_res.diagnostics.join("\n")
                                );
                            }
                            continue;
                        }

                        let artifact = match build_res.artifact.clone() {
                            Some(path) => path,
                            None => {
                                eprintln!("Build #{} succeeded without an artifact", build_sequence);
                                continue;
                            }
                        };
                        let artifact_relative = match artifact.strip_prefix(&canonical_root) {
                            Ok(path) if !path.as_os_str().is_empty() => {
                                path.to_string_lossy().into_owned()
                            }
                            _ => {
                                eprintln!("Build #{} produced an artifact outside the project root", build_sequence);
                                continue;
                            }
                        };

                        if json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "build",
                                    "status": "success",
                                    "build_sequence": build_sequence,
                                    "extension_id": manifest.id.to_string(),
                                })
                            );
                        }

                        let reload_res = tokio::time::timeout(
                            timeout,
                            shell_proxy.reload_dev_session(
                                session_id.clone(),
                                build_sequence,
                                artifact_relative,
                                timeout.as_millis().min(u64::MAX as u128) as u64,
                            ),
                        )
                        .await
                        .map_err(|_| format!("reload timed out after {timeout:?}"))?;

                        match reload_res {
                            Ok(res) => {
                                if res.outcome == "applied" {
                                    if json {
                                        println!(
                                            "{}",
                                            serde_json::json!({
                                                "event": "reload",
                                                "status": "applied",
                                                "build_sequence": build_sequence,
                                                "extension_id": manifest.id.to_string(),
                                                "session_id": session_id,
                                                "host_generation": res.host_generation,
                                                "engine_generation": res.engine_generation,
                                            })
                                        );
                                    } else if !quiet {
                                        eprintln!(
                                            "Dev reload applied (build #{}, host gen: {}, engine gen: {})",
                                            build_sequence, res.host_generation, res.engine_generation
                                        );
                                    }
                                } else {
                                    if json {
                                        println!(
                                            "{}",
                                            serde_json::json!({
                                                "event": "reload",
                                                "status": res.outcome,
                                                "build_sequence": build_sequence,
                                                "extension_id": manifest.id.to_string(),
                                                "diagnostic_code": res.diagnostic_code,
                                                "message": res.message,
                                            })
                                        );
                                    } else {
                                        eprintln!(
                                            "Dev reload rejected [{}]: {}",
                                            res.diagnostic_code, res.message
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                if json {
                                    println!(
                                        "{}",
                                        serde_json::json!({
                                            "event": "reload",
                                            "status": "error",
                                            "build_sequence": build_sequence,
                                            "extension_id": manifest.id.to_string(),
                                            "error": e.to_string(),
                                        })
                                    );
                                } else {
                                    eprintln!("Dev reload error: {e}");
                                }
                            }
                        }
                    }
                }
            }

                Ok(())
            })
        });

        match result {
            Ok(()) => ExtOpResult {
                success: true,
                data: serde_json::json!({ "status": "stopped" }),
                human_message: "Dev server stopped".into(),
                warnings: Vec::new(),
                exit_code: 0,
            },
            Err(e) => ExtOpResult {
                success: false,
                data: serde_json::json!({ "error": e }),
                human_message: format!("Error: {e}"),
                warnings: Vec::new(),
                exit_code: 1,
            },
        }
    }

    pub fn logs(&self, id_str: Option<&str>, follow: bool) -> ExtOpResult {
        let Some(id_raw) = id_str else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: "extension ID is required".into(),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let Ok(id) = ExtensionId::new(id_raw) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_raw}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let cli_res = ExtensionCli::logs(&id, &self.state_dir);
        let result = ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "log_file": cli_res.artifact,
                "logs": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        };
        if result.success && follow {
            shilpo_ext_runtime::follow_log(&id, &self.state_dir);
        }
        result
    }

    pub fn list(&self, dev_only: bool) -> ExtOpResult {
        let dev_regs = shilpo_ext_runtime::development_registrations(&self.state_dir).0;
        let installed = self.catalog.installed().unwrap_or_default();
        let installed_count = installed.len();
        let live_scripts = self
            .ipc
            .telemetry()
            .ok()
            .and_then(|telemetry| {
                serde_json::from_str::<serde_json::Value>(
                    &telemetry.extension_host_diagnostics_json,
                )
                .ok()
            })
            .and_then(|diagnostics| diagnostics.get("script_extensions").cloned())
            .and_then(|statuses| statuses.as_array().cloned());
        let script_items: Vec<serde_json::Value> = if let Some(statuses) = live_scripts {
            statuses
                .into_iter()
                .map(|status| script_item_from_status(&status))
                .collect()
        } else {
            let catalog_paths = shilpo_ext_runtime::CatalogPaths::platform_default();
            shilpo_ext_runtime::script::discover_script_bundles(&catalog_paths)
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "id": b.id,
                        "name": b.name,
                        "version": b.version,
                        "path": b.path,
                        "mode": b.mode,
                        "type": "script",
                        "runtime_kind": "trusted_local_script",
                        "source": "local",
                        "status": b.status,
                        "trusted": true,
                        "sandboxed": false,
                        "label": "Trusted local script (not sandboxed)",
                        "contributions_count": b.contributions_count,
                        "diagnostics": b.diagnostics,
                    })
                })
                .collect()
        };

        let mut human_lines = Vec::new();
        human_lines.push(format!("Development extensions: {}", dev_regs.len()));
        human_lines.push(format!(
            "Installed extensions: {}",
            if dev_only { 0 } else { installed_count }
        ));
        if !installed.is_empty() && !dev_only {
            for ext in &installed {
                human_lines.push(format!(
                    "  {} (v{}) [wasm, installed] - {} contributions",
                    ext.receipt.id,
                    ext.receipt.active.version,
                    ext.manifest.contributions.bar_widgets.len()
                        + ext.manifest.contributions.desktop_widgets.len()
                ));
            }
        }
        human_lines.push(format!(
            "Trusted local script extensions: {}",
            script_items.len()
        ));
        for script in &script_items {
            human_lines.push(format!(
                "  {} (v{}) [script, local, {}] - {} contributions (Trusted local script (not sandboxed))",
                script.get("id").and_then(|value| value.as_str()).unwrap_or("unknown"),
                script.get("version").and_then(|value| value.as_str()).unwrap_or("unknown"),
                script.get("status").and_then(|value| value.as_str()).unwrap_or("unknown"),
                script.get("contributions_count").and_then(|value| value.as_u64()).unwrap_or(0),
            ));
        }

        ExtOpResult {
            success: true,
            data: serde_json::json!({
                "development": dev_regs,
                "installed": if dev_only { Vec::new() } else { installed },
                "scripts": script_items,
            }),
            human_message: human_lines.join("\n"),
            warnings: Vec::new(),
            exit_code: 0,
        }
    }

    pub fn install(&self, target: &str, hash: Option<&str>) -> ExtOpResult {
        let catalog_paths = shilpo_ext_runtime::CatalogPaths::platform_default();
        let script_bundles = shilpo_ext_runtime::script::discover_script_bundles(&catalog_paths);
        if script_bundles.iter().any(|b| b.id.as_str() == target) {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!(
                    "script extension '{target}' is a trusted local script in $XDG_CONFIG_HOME/shilpo/scripts and is not managed via catalog install"
                ),
                warnings: Vec::new(),
                exit_code: 1,
            };
        }

        let cli_res = if target.starts_with("https://") {
            match self.catalog.install_url(target, hash) {
                Ok(receipt) => ExtensionCliResult {
                    success: true,
                    extension_id: Some(receipt.id.to_string()),
                    artifact: None,
                    diagnostics: vec![format!(
                        "installed '{}' {} disabled",
                        receipt.id, receipt.active.version
                    )],
                },
                Err(error) => ExtensionCliResult {
                    success: false,
                    extension_id: None,
                    artifact: None,
                    diagnostics: vec![format!("error[install.url]: {error}")],
                },
            }
        } else if let Ok(id) = ExtensionId::new(target) {
            if !Path::new(target).exists() {
                ExtensionCli::install_from_catalog(&id, &self.catalog)
            } else {
                ExtensionCli::install(Path::new(target), &self.catalog)
            }
        } else {
            ExtensionCli::install(Path::new(target), &self.catalog)
        };

        let mut op_res = ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        };
        self.notify_daemon_if_needed(&mut op_res);
        op_res
    }

    pub fn update(&self, id_str: Option<&str>, all: bool, dry_run: bool) -> ExtOpResult {
        if let Some(id) = id_str
            && self.is_script_id(id)
        {
            return script_catalog_operation_error(id, "update");
        }
        if all {
            let snapshot = self.catalog.snapshot();
            let available = snapshot
                .updates
                .into_iter()
                .filter(|u| u.state == UpdateState::Available)
                .map(|u| u.id)
                .collect::<Vec<_>>();

            if dry_run {
                return ExtOpResult {
                    success: true,
                    data: serde_json::json!({ "would_update": available }),
                    human_message: format!("Would update {} extension(s)", available.len()),
                    warnings: Vec::new(),
                    exit_code: 0,
                };
            }

            let results = available
                .iter()
                .map(|id| ExtensionCli::install_from_catalog(id, &self.catalog))
                .collect::<Vec<_>>();
            let failed = results.iter().any(|r| !r.success);

            let mut op_res = ExtOpResult {
                success: !failed,
                data: serde_json::json!({
                    "results": results.iter().map(|r| serde_json::json!({
                        "extension_id": r.extension_id,
                        "success": r.success,
                        "diagnostics": r.diagnostics,
                    })).collect::<Vec<_>>()
                }),
                human_message: format!("Updated {} extension(s)", available.len()),
                warnings: Vec::new(),
                exit_code: if !failed { 0 } else { 1 },
            };
            self.notify_daemon_if_needed(&mut op_res);
            return op_res;
        }

        let Some(id_raw) = id_str else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: "extension ID or --all required".into(),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let Ok(id) = ExtensionId::new(id_raw) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_raw}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };

        let cli_res = ExtensionCli::install_from_catalog(&id, &self.catalog);
        let mut op_res = ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        };
        self.notify_daemon_if_needed(&mut op_res);
        op_res
    }

    pub fn enable(&self, id_str: &str) -> ExtOpResult {
        if self.is_script_id(id_str) {
            return script_catalog_operation_error(id_str, "enable");
        }
        let Ok(id) = ExtensionId::new(id_str) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_str}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let cli_res = ExtensionCli::enable(&id, &self.catalog);
        let mut op_res = ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        };
        self.notify_daemon_if_needed(&mut op_res);
        op_res
    }

    pub fn disable(&self, id_str: &str) -> ExtOpResult {
        if self.is_script_id(id_str) {
            return script_catalog_operation_error(id_str, "disable");
        }
        let Ok(id) = ExtensionId::new(id_str) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_str}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let cli_res = ExtensionCli::disable(&id, &self.catalog);
        let mut op_res = ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        };
        self.notify_daemon_if_needed(&mut op_res);
        op_res
    }

    pub fn approve(&self, id_str: &str, grant_all: bool) -> ExtOpResult {
        if self.is_script_id(id_str) {
            return script_catalog_operation_error(id_str, "grant");
        }
        let Ok(id) = ExtensionId::new(id_str) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_str}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };

        let receipt = match self.catalog.receipt(&id) {
            Ok(receipt) => receipt,
            Err(error) => {
                return ExtOpResult {
                    success: false,
                    data: serde_json::Value::Null,
                    human_message: format!("failed to inspect extension permissions: {error}"),
                    warnings: Vec::new(),
                    exit_code: 1,
                };
            }
        };

        let pending = receipt.pending.is_some();
        let capabilities = if grant_all {
            let requested = if pending {
                self.catalog.pending_capabilities(&id)
            } else {
                self.catalog.requested_capabilities(&id)
            };
            match requested {
                Ok(caps) => caps,
                Err(error) => {
                    return ExtOpResult {
                        success: false,
                        data: serde_json::Value::Null,
                        human_message: format!("failed to inspect requested permissions: {error}"),
                        warnings: Vec::new(),
                        exit_code: 1,
                    };
                }
            }
        } else {
            Vec::new()
        };

        let result = if pending {
            self.catalog
                .approve_pending(&id, capabilities)
                .map(|r| r.active.version.to_string())
        } else {
            self.catalog
                .approve_capabilities(&id, capabilities)
                .map(|_| receipt.active.version.to_string())
        };

        let (success, message) = match result {
            Ok(version) => (
                true,
                format!("permission review completed; '{id}' {version} is active"),
            ),
            Err(error) => (false, format!("error[permissions.review]: {error}")),
        };

        let mut op_res = ExtOpResult {
            success,
            data: serde_json::json!({
                "extension_id": id.to_string(),
                "diagnostics": [message.clone()],
            }),
            human_message: message,
            warnings: Vec::new(),
            exit_code: if success { 0 } else { 1 },
        };
        self.notify_daemon_if_needed(&mut op_res);
        op_res
    }

    pub fn rollback(&self, id_str: &str) -> ExtOpResult {
        if self.is_script_id(id_str) {
            return script_catalog_operation_error(id_str, "rollback");
        }
        let Ok(id) = ExtensionId::new(id_str) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_str}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let cli_res = ExtensionCli::rollback(&id, &self.catalog);
        let mut op_res = ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        };
        self.notify_daemon_if_needed(&mut op_res);
        op_res
    }

    pub fn uninstall(&self, id_str: &str, delete_secrets: bool, delete_state: bool) -> ExtOpResult {
        if self.is_script_id(id_str) {
            return script_catalog_operation_error(id_str, "uninstall");
        }
        let Ok(id) = ExtensionId::new(id_str) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_str}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let secret_policy = if delete_secrets {
            shilpo_ext_runtime::SecretPolicy::Delete
        } else {
            shilpo_ext_runtime::SecretPolicy::Retain
        };
        let state_policy = if delete_state {
            shilpo_ext_runtime::StatePolicy::Delete
        } else {
            shilpo_ext_runtime::StatePolicy::Retain
        };
        let broker = Some(
            std::sync::Arc::new(shilpo_ext_runtime::Oo7SecretBroker::new())
                as std::sync::Arc<dyn shilpo_ext_runtime::SecretBroker>,
        );
        let store = if state_policy == shilpo_ext_runtime::StatePolicy::Delete {
            match shilpo_ext_runtime::HeedStateStore::open(&self.catalog.paths().state_store_dir())
            {
                Ok(store) => Some(std::sync::Arc::new(store)
                    as std::sync::Arc<dyn shilpo_ext_runtime::StateStore>),
                Err(error) => {
                    return ExtOpResult {
                        success: false,
                        data: serde_json::Value::Null,
                        human_message: format!("failed to open extension state store: {error}"),
                        warnings: Vec::new(),
                        exit_code: 1,
                    };
                }
            }
        } else {
            None
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

        let cli_res = ExtensionCli::uninstall_with_policies(
            &id,
            &self.catalog,
            secret_policy,
            broker.as_deref(),
            state_policy,
            store.as_deref(),
            deadline,
        );
        let mut op_res = ExtOpResult {
            success: cli_res.success,
            data: serde_json::json!({
                "extension_id": cli_res.extension_id,
                "diagnostics": cli_res.diagnostics,
            }),
            human_message: cli_res.diagnostics.join("\n"),
            warnings: Vec::new(),
            exit_code: if cli_res.success { 0 } else { 1 },
        };
        self.notify_daemon_if_needed(&mut op_res);
        op_res
    }

    pub fn check_updates(&self) -> ExtOpResult {
        let snapshot = self.catalog.snapshot();
        ExtOpResult {
            success: true,
            data: serde_json::json!({
                "updates": snapshot.updates,
            }),
            human_message: format!("Found {} available update(s)", snapshot.updates.len()),
            warnings: Vec::new(),
            exit_code: 0,
        }
    }

    pub fn channel(&self, id_str: &str, channel_str: Option<&str>) -> ExtOpResult {
        let Ok(id) = ExtensionId::new(id_str) else {
            return ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("invalid extension ID '{id_str}'"),
                warnings: Vec::new(),
                exit_code: 2,
            };
        };
        let channel = match channel_str {
            Some("stable") => ReleaseChannel::Stable,
            Some("beta") => ReleaseChannel::Beta,
            Some("development") => ReleaseChannel::Development,
            _ => {
                return ExtOpResult {
                    success: false,
                    data: serde_json::Value::Null,
                    human_message: "channel must be stable, beta, or development".into(),
                    warnings: Vec::new(),
                    exit_code: 2,
                };
            }
        };
        match self.catalog.set_channel(&id, channel) {
            Ok(()) => ExtOpResult {
                success: true,
                data: serde_json::json!({
                    "extension_id": id.to_string(),
                    "channel": format!("{channel:?}"),
                }),
                human_message: format!("selected {channel:?} channel for '{id}'"),
                warnings: Vec::new(),
                exit_code: 0,
            },
            Err(error) => ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("error[channel.failed]: {error}"),
                warnings: Vec::new(),
                exit_code: 1,
            },
        }
    }

    pub fn source(&self, args: &[String]) -> ExtOpResult {
        match shilpo_ext_runtime::source_command(args, &self.catalog) {
            Ok(message) => ExtOpResult {
                success: true,
                data: serde_json::json!({ "arguments": args, "message": message }),
                human_message: message,
                warnings: Vec::new(),
                exit_code: 0,
            },
            Err(error) => ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: error,
                warnings: Vec::new(),
                exit_code: 2,
            },
        }
    }

    pub fn refresh_sources(&self) -> ExtOpResult {
        match self.catalog.refresh_sources() {
            Ok(diagnostics) => ExtOpResult {
                success: true,
                data: serde_json::json!({ "diagnostics": diagnostics }),
                human_message: "Catalog sources refreshed".into(),
                warnings: Vec::new(),
                exit_code: 0,
            },
            Err(error) => ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: error.to_string(),
                warnings: Vec::new(),
                exit_code: 1,
            },
        }
    }

    pub fn sign(&self, package: &Path, key: &Path, publisher: &str) -> ExtOpResult {
        let key_str = match std::fs::read_to_string(key) {
            Ok(s) => s,
            Err(e) => {
                return ExtOpResult {
                    success: false,
                    data: serde_json::Value::Null,
                    human_message: format!(
                        "error[sign.key_read]: failed to read key file {}: {e}",
                        key.display()
                    ),
                    warnings: Vec::new(),
                    exit_code: 1,
                };
            }
        };
        match sign_package(package, publisher, &key_str) {
            Ok(signature_path) => ExtOpResult {
                success: true,
                data: serde_json::json!({
                    "signature_path": signature_path,
                }),
                human_message: format!(
                    "signed package {}; signature at {}",
                    package.display(),
                    signature_path.display()
                ),
                warnings: Vec::new(),
                exit_code: 0,
            },
            Err(error) => ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("error[sign.failed]: {error}"),
                warnings: Vec::new(),
                exit_code: 1,
            },
        }
    }

    pub fn keygen(&self, output: &Path) -> ExtOpResult {
        match shilpo_ext_runtime::write_signing_key(output) {
            Ok(public_path) => ExtOpResult {
                success: true,
                data: serde_json::json!({
                    "private_key": output,
                    "public_key": public_path,
                }),
                human_message: format!(
                    "generated signing key at {} (public: {})",
                    output.display(),
                    public_path.display()
                ),
                warnings: Vec::new(),
                exit_code: 0,
            },
            Err(error) => ExtOpResult {
                success: false,
                data: serde_json::Value::Null,
                human_message: format!("error[keygen.failed]: {error}"),
                warnings: Vec::new(),
                exit_code: 1,
            },
        }
    }

    fn is_script_id(&self, id: &str) -> bool {
        shilpo_ext_runtime::script::discover_script_bundles(
            &shilpo_ext_runtime::CatalogPaths::platform_default(),
        )
        .iter()
        .any(|bundle| bundle.id.as_str() == id)
    }
}

fn script_catalog_operation_error(id: &str, operation: &str) -> ExtOpResult {
    ExtOpResult {
        success: false,
        data: serde_json::Value::Null,
        human_message: format!(
            "trusted local script '{id}' is not managed by the catalog; '{operation}' is unavailable"
        ),
        warnings: Vec::new(),
        exit_code: 1,
    }
}

fn script_item_from_status(status: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": status.get("id"),
        "name": status.get("name"),
        "version": status.get("version"),
        "type": "script",
        "runtime_kind": "trusted_local_script",
        "source": "local",
        "status": status.get("status"),
        "trusted": true,
        "sandboxed": false,
        "label": "Trusted local script (not sandboxed)",
        "contributions_count": status.get("contributions_count"),
        "diagnostics": status.get("diagnostics"),
    })
}

fn record_refresh_warning(result: &mut ExtOpResult, reason: impl Into<String>) {
    result
        .warnings
        .push(format!("Local mutation succeeded, but {}", reason.into()));
}

fn format_human_lint_report(report: &LintReport, is_quiet: bool) -> String {
    let mut out = String::new();
    let use_color = std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();

    for diag in &report.diagnostics {
        if is_quiet && diag.severity == LintSeverity::Info {
            continue;
        }
        if is_quiet && diag.severity == LintSeverity::Warning && report.passed {
            continue;
        }

        let sev_str = match diag.severity {
            LintSeverity::Error => {
                if use_color {
                    "\x1b[31;1merror\x1b[0m"
                } else {
                    "error"
                }
            }
            LintSeverity::Warning => {
                if use_color {
                    "\x1b[33;1mwarning\x1b[0m"
                } else {
                    "warning"
                }
            }
            LintSeverity::Info => {
                if use_color {
                    "\x1b[36minfo\x1b[0m"
                } else {
                    "info"
                }
            }
        };

        let prefix = if let Some(path) = &diag.path {
            if let (Some(line), Some(col)) = (diag.line, diag.column) {
                format!(
                    "{path}:{line}:{col}: {sev_str}: [{}] {}",
                    diag.rule_id, diag.message
                )
            } else if let Some(line) = diag.line {
                format!(
                    "{path}:{line}: {sev_str}: [{}] {}",
                    diag.rule_id, diag.message
                )
            } else {
                format!("{path}: {sev_str}: [{}] {}", diag.rule_id, diag.message)
            }
        } else {
            format!("{sev_str}: [{}] {}", diag.rule_id, diag.message)
        };

        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&prefix);

        if let Some(help) = &diag.remediation {
            out.push_str(&format!("\n  help: {help}"));
        }
    }

    if !is_quiet {
        let summary = if report.passed && report.warning_count == 0 {
            format!(
                "Lint passed with 0 errors, 0 warnings, {} info in {}.",
                report.info_count, report.project_path
            )
        } else {
            format!(
                "Lint found {} error(s), {} warning(s), {} info in {}.",
                report.error_count, report.warning_count, report.info_count, report.project_path
            )
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&summary);
    }

    out
}

fn should_ignore_path(path: &Path, root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    for component in rel.components() {
        let name = component.as_os_str().to_string_lossy();
        if name.starts_with('.') || name == "node_modules" || name == "target" || name == "dist" {
            return true;
        }
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && (ext == "tmp" || ext == "wasm")
    {
        return true;
    }
    if let Some(file_name) = path.file_name().and_then(|f| f.to_str())
        && (file_name == "extension.wasm" || file_name.starts_with('.'))
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        ExtOpResult, record_refresh_warning, script_catalog_operation_error,
        script_item_from_status,
    };

    #[test]
    fn daemon_refresh_failure_keeps_local_mutation_successful() {
        let mut result = ExtOpResult {
            success: true,
            data: serde_json::Value::Null,
            human_message: "extension enabled".into(),
            warnings: Vec::new(),
            exit_code: 0,
        };

        record_refresh_warning(&mut result, "live shell daemon is not running");

        assert!(result.success);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("Local mutation succeeded"));
    }

    #[test]
    fn live_script_status_has_stable_human_and_structured_fields() {
        let item = script_item_from_status(&serde_json::json!({
            "id": "local.script.test",
            "name": "Test",
            "version": "0.1.0",
            "status": "ready",
            "contributions_count": 2,
            "diagnostics": ["example"],
        }));
        assert_eq!(item["type"], "script");
        assert_eq!(item["source"], "local");
        assert_eq!(item["status"], "ready");
        assert_eq!(item["runtime_kind"], "trusted_local_script");
        assert_eq!(item["sandboxed"], false);
        assert_eq!(item["contributions_count"], 2);
        assert_eq!(item["diagnostics"][0], "example");
    }

    #[test]
    fn scripts_reject_catalog_only_operations() {
        for operation in ["install", "update", "grant", "rollback", "uninstall"] {
            let result = script_catalog_operation_error("local.script.test", operation);
            assert!(!result.success);
            assert!(result.human_message.contains("not managed by the catalog"));
            assert!(result.human_message.contains(operation));
        }
    }

    #[test]
    fn test_status_output_formats_all_circuit_states() {
        let telemetry_json = serde_json::json!({
            "lifecycle": "ready",
            "host_generation": 1,
            "engine_generation": 5,
            "wasm_extensions": [
                {
                    "id": "io.github.test.healthy",
                    "runtime_kind": "wasm",
                    "state": "closed",
                    "consecutive_failures": 0,
                    "trip_count": 0
                },
                {
                    "id": "io.github.test.retrying",
                    "runtime_kind": "wasm",
                    "state": "open",
                    "trip_count": 2,
                    "retry_after_ms": 45000
                },
                {
                    "id": "io.github.test.probing",
                    "runtime_kind": "wasm",
                    "state": "half_open",
                    "consecutive_successes": 1,
                    "trip_count": 1
                },
                {
                    "id": "io.github.test.dead",
                    "runtime_kind": "wasm",
                    "state": "permanently_disabled",
                    "trip_count": 4
                }
            ],
            "script_extensions": [
                {
                    "id": "local.script.test",
                    "name": "Test Script",
                    "version": "1.0.0",
                    "status": "ready"
                }
            ]
        });

        let ext_status = telemetry_json;
        let state_str = ext_status.get("lifecycle").unwrap().as_str().unwrap();
        let host_gen = ext_status.get("host_generation").unwrap().as_u64().unwrap();
        let engine_gen = ext_status
            .get("engine_generation")
            .unwrap()
            .as_u64()
            .unwrap();

        let mut human_lines = vec![
            "Extension Host Status:".to_string(),
            format!("  State: {state_str}"),
            format!("  Host Generation: {host_gen}"),
            format!("  Engine Generation: {engine_gen}"),
        ];

        let wasm_extensions = ext_status
            .get("wasm_extensions")
            .unwrap()
            .as_array()
            .unwrap();
        human_lines.push(String::new());
        human_lines.push(format!("WASM Extensions ({}):", wasm_extensions.len()));
        for ext in wasm_extensions {
            let id = ext.get("id").unwrap().as_str().unwrap();
            let state = ext.get("state").unwrap().as_str().unwrap();
            let trip_count = ext.get("trip_count").unwrap().as_u64().unwrap();
            let desc = match state {
                "closed" => "closed, healthy".to_string(),
                "open" => {
                    let retry_ms = ext.get("retry_after_ms").unwrap().as_u64().unwrap();
                    let secs = retry_ms.div_ceil(1000);
                    format!("open, retrying in {secs}s (trip {trip_count})")
                }
                "half_open" => {
                    let successes = ext.get("consecutive_successes").unwrap().as_u64().unwrap();
                    format!("half_open, probing ({successes}/3 successes, trip {trip_count})")
                }
                "permanently_disabled" => {
                    format!("permanently_disabled, failed after {trip_count} trip cycles")
                }
                other => other.to_string(),
            };
            human_lines.push(format!("  {id} [{desc}]"));
        }

        let human = human_lines.join("\n");
        assert!(human.contains("io.github.test.healthy [closed, healthy]"));
        assert!(human.contains("io.github.test.retrying [open, retrying in 45s (trip 2)]"));
        assert!(
            human.contains("io.github.test.probing [half_open, probing (1/3 successes, trip 1)]")
        );
        assert!(
            human
                .contains("io.github.test.dead [permanently_disabled, failed after 4 trip cycles]")
        );
    }

    #[test]
    fn test_should_ignore_path() {
        let root = PathBuf::from("/home/user/project");
        assert!(super::should_ignore_path(&root.join(".git/HEAD"), &root));
        assert!(super::should_ignore_path(
            &root.join("node_modules/pkg/index.js"),
            &root
        ));
        assert!(super::should_ignore_path(
            &root.join("target/wasm32-wasip1/debug/app.wasm"),
            &root
        ));
        assert!(super::should_ignore_path(
            &root.join("dist/bundle.js"),
            &root
        ));
        assert!(super::should_ignore_path(
            &root.join("extension.wasm"),
            &root
        ));
        assert!(super::should_ignore_path(&root.join("build.tmp"), &root));
        assert!(super::should_ignore_path(
            &root.join(".shilpo-cache/data"),
            &root
        ));

        // Valid source files that should NOT be ignored
        assert!(!super::should_ignore_path(
            &root.join("extension.toml"),
            &root
        ));
        assert!(!super::should_ignore_path(&root.join("src/lib.rs"), &root));
        assert!(!super::should_ignore_path(
            &root.join("src/index.ts"),
            &root
        ));
    }
}
