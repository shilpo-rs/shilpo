use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "shilpo",
    author,
    version,
    about = "Shilpo Shell Control & Desktop Utility CLI",
    long_about = None
)]
pub struct Cli {
    /// Machine-readable JSON output
    #[arg(global = true, long)]
    pub json: bool,

    /// Suppress human output (errors still printed to stderr)
    #[arg(global = true, short, long)]
    pub quiet: bool,

    /// Command timeout duration (e.g. 10s, 500ms, 10)
    #[arg(global = true, long, value_name = "DURATION")]
    pub timeout: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Shell daemon lifecycle, status, and telemetry (durable GPUI process role)
    Daemon {
        /// Permit session-bus callers to load local extension code for the lifetime of this daemon
        #[arg(long)]
        developer_mode: bool,
    },
    /// Standalone settings window (user-launched GPUI process role)
    Settings,
    /// Supervised Wasmtime extension runtime (private worker process role)
    ExtensionHost,
    /// Device integration daemon (systemd/DBus activated process role)
    DeviceDaemon,
    /// Theme synchronization daemon (systemd/DBus activated process role)
    ThemeDaemon,
    /// Session lock screen (dedicated GPUI process role; owns ext-session-lock-v1 and PAM)
    Lock,
    /// Shell daemon lifecycle, status, and telemetry
    Shell {
        #[command(subcommand)]
        command: ShellCommands,
    },
    /// Shell action invocation
    Action {
        #[command(subcommand)]
        command: ActionCommands,
    },
    /// Workspace overview visibility
    Overview {
        #[command(subcommand)]
        action: VisibilityAction,
    },
    /// Status bar visibility
    Bar {
        #[command(subcommand)]
        action: VisibilityAction,
    },
    /// Workspace navigation and layout
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommands,
    },
    /// Window focus and layout
    Window {
        #[command(subcommand)]
        command: WindowCommands,
    },
    /// Shell configuration inspection and validation
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Theme mode, seed color, and wallpaper management
    Theme {
        #[command(subcommand)]
        command: ThemeCommands,
    },
    /// Environment and runtime diagnostic checks
    Doctor {
        /// Attempt automatic fixes for warnings/errors
        #[arg(long)]
        fix: bool,
        /// Run as first-login one-shot task: write reports, send desktop notification, mark complete
        #[arg(long)]
        first_login: bool,
        /// Read-only local profiling summary inventory
        #[arg(long)]
        telemetry: bool,
    },
    /// Profile trace discovery and export
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },
    /// Shell extension development, packaging, and catalog management
    Ext {
        #[command(subcommand)]
        command: ExtCommands,
    },
    /// Native screen capture and annotation
    Capture {
        #[command(subcommand)]
        action: CaptureAction,
    },
    /// Display brightness controls
    Brightness {
        #[command(subcommand)]
        command: BrightnessCommands,
    },
    /// Interactive post-install setup: choose a compositor, stage its config, install GPU
    /// drivers, and wire up the session (Arch Linux only)
    Setup,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureAction {
    /// Select a region and copy PNG to clipboard immediately
    Region,
    /// Select a region and open the annotation editor
    Edit,
    /// Select a region and run OCR, copying recognized text
    Ocr,
    /// Open the capture menu (selection shape chooser)
    Menu,
}

#[derive(Subcommand, Debug, Clone)]
pub enum BrightnessCommands {
    /// List detected displays and current brightness percentages
    List,
    /// Set brightness for a specific display or all displays
    Set {
        /// Target display connector or ID (e.g. "DP-1" or "ddc:i2c-3")
        #[arg(long)]
        display: Option<String>,
        /// Target brightness percentage (0..=100)
        value: u8,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ShellCommands {
    /// Print status of shell daemon and services
    Status,
    /// Start shell daemon via systemd user unit
    Start,
    /// Stop shell daemon via systemd user unit
    Stop,
    /// Restart shell daemon via systemd user unit
    Restart,
    /// Stream or view shell daemon logs via journald
    Logs {
        /// Stream new log output continuously
        #[arg(short, long)]
        follow: bool,
        /// Show logs since specified time (e.g. "10m ago")
        #[arg(long)]
        since: Option<String>,
        /// Number of recent log lines to display
        #[arg(short = 'n', long)]
        lines: Option<usize>,
    },
    /// Query service health and compositor broker telemetry
    Telemetry,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibilityAction {
    /// Show component idempotently
    Show,
    /// Hide component idempotently
    Hide,
    /// Toggle component visibility
    Toggle,
}

#[derive(Subcommand, Debug, Clone)]
pub enum WorkspaceCommands {
    /// Focus workspace by numeric ID
    Focus { id: u64 },
    /// Create and activate a new empty workspace
    Create,
}

#[derive(Subcommand, Debug, Clone)]
pub enum WindowCommands {
    /// Focus window by numeric ID
    Focus { id: u64 },
    /// Focus previously active window
    FocusPrevious,
    /// Move window to specified workspace
    Move {
        id: u64,
        #[arg(long)]
        workspace: u64,
    },
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCommands {
    /// Print active configuration file path
    Path,
    /// Validate configuration file syntax and values
    Validate,
    /// Print effective configuration (with optional provenance origins)
    Effective {
        /// Include winning source origins/provenance for every leaf value
        #[arg(long)]
        origins: bool,
    },
    /// Signal running daemon to reload configuration
    Reload,
    /// Migrate the primary configuration file to the latest schema version
    Migrate {
        /// Preview the migration without writing any files
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ThemeCommands {
    /// Theme mode management (get, set, toggle)
    Mode {
        #[command(subcommand)]
        action: ThemeModeAction,
    },
    /// Theme seed color management
    Seed {
        #[command(subcommand)]
        action: ThemeSeedAction,
    },
    /// Wallpaper management
    Wallpaper {
        #[command(subcommand)]
        action: ThemeWallpaperAction,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ThemeModeAction {
    /// Get current theme mode
    Get,
    /// Set theme mode
    Set { mode: ModeValue },
    /// Toggle theme mode
    Toggle,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeValue {
    Light,
    Dark,
    System,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ThemeSeedAction {
    /// Set custom color seed (HEX string or integer)
    Set { color: String },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ThemeWallpaperAction {
    /// Show the current wallpaper and wallpaper directory
    Get,
    /// Set wallpaper file path
    Set { path: PathBuf },
    /// Select random wallpaper from directory
    Random,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ExtCommands {
    /// Create a new extension project (alias: create)
    #[command(alias = "create")]
    New {
        /// Human display name of the extension
        name: String,
        /// Optional target directory path (defaults to kebab-cased name)
        target: Option<PathBuf>,
        /// Extension implementation language
        #[arg(long, value_enum)]
        language: Option<StarterLanguageValue>,
        /// Starter contribution kind
        #[arg(long, value_enum)]
        contribution: Option<StarterContributionValue>,
        /// TypeScript package manager (npm, pnpm, yarn, bun, deno)
        #[arg(long, value_enum)]
        package_manager: Option<PackageManagerValue>,
        /// View authoring syntax for TypeScript extensions (jsx, builders)
        #[arg(long, value_enum)]
        view_syntax: Option<ViewSyntaxValue>,
        /// Explicit extension ID override (reverse-domain format, e.g. dev.local.my-extension)
        #[arg(long)]
        extension_id: Option<String>,
        /// Explicit package/crate name override
        #[arg(long)]
        package_name: Option<String>,
        /// Description of the extension
        #[arg(long)]
        description: Option<String>,
        /// Canonical capability JSON object (repeatable)
        #[arg(long = "capability", action = clap::ArgAction::Append)]
        capabilities: Vec<String>,
        /// Canonical event subscription (repeatable)
        #[arg(long = "subscribe", action = clap::ArgAction::Append)]
        subscriptions: Vec<String>,
        /// Install dependencies after generation
        #[arg(long)]
        install: bool,
        /// Build component after generation (implies --install)
        #[arg(long)]
        build: bool,
        /// Initialize git repository
        #[arg(long)]
        git: bool,
        /// Skip confirmation prompts in interactive mode
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// Display extension host runtime status, supervisor state, and process diagnostics
    Status,
    /// Build extension WebAssembly component
    Build {
        /// Path to extension directory (defaults to current directory)
        path: Option<PathBuf>,
        /// Build component with release optimizations
        #[arg(long)]
        release: bool,
    },
    /// Inspect extension manifest, component, and assets
    Check { path: Option<PathBuf> },
    /// Lint extension manifest, capabilities, project configuration, and assets
    Lint {
        /// Path to extension directory (defaults to current directory)
        path: Option<PathBuf>,
        /// Treat warnings as errors
        #[arg(long)]
        deny_warnings: bool,
    },
    /// Pack extension directory into .shilpo-ext bundle
    Pack {
        path: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Watch extension directory, compile, and reload live session in running shell
    Dev {
        /// Path to extension directory (defaults to current directory)
        path: Option<PathBuf>,
    },
    /// View development extension logs
    Logs {
        id: Option<String>,
        #[arg(short, long)]
        follow: bool,
    },
    /// List active and development extensions
    List {
        #[arg(long)]
        dev: bool,
    },
    /// Search extension catalog
    Search { query: Option<String> },
    /// Show detailed info for extension ID
    Info { id: String },
    /// Install extension package or catalog ID
    Install {
        target: String,
        #[arg(long)]
        hash: Option<String>,
    },
    /// Update extensions
    Update {
        id: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Enable installed extension
    Enable { id: String },
    /// Disable installed extension
    Disable { id: String },
    /// Approve pending extension permissions
    Approve {
        id: String,
        #[arg(long)]
        grant_all: bool,
    },
    /// Rollback extension to previous version
    Rollback { id: String },
    /// Uninstall extension
    Uninstall {
        id: String,
        #[arg(long, default_value_t = false)]
        delete_secrets: bool,
        #[arg(long, default_value_t = false)]
        delete_state: bool,
    },
    /// Check for catalog extension updates
    CheckUpdates,
    /// Manage extension release channel
    Channel { id: String, channel: Option<String> },
    /// Manage extension registry sources
    Source {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Refresh extension registry sources
    RefreshSources,
    /// Sign extension package with key
    Sign {
        package: PathBuf,
        key: PathBuf,
        #[arg(long)]
        publisher: String,
    },
    /// Generate ed25519 signing keypair
    Keygen { output: PathBuf },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProfileCommands {
    /// Export a completed profile trace file
    Export {
        /// Destination path for exported Chrome Trace JSON
        #[arg(short, long)]
        output: PathBuf,

        /// Optional path to completed trace file (discovers newest completed trace if omitted)
        #[arg(short, long)]
        source: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ActionCommands {
    /// Invoke an action by ID
    Invoke {
        /// The action ID to invoke (e.g. builtin:toggle_overview or ext:io.github.alice.weather/toggle-weather)
        action_id: String,

        /// Optional JSON payload string
        #[arg(long)]
        payload: Option<String>,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarterLanguageValue {
    Rust,
    Typescript,
}

impl From<StarterLanguageValue> for shilpo_ext_runtime::StarterLanguage {
    fn from(v: StarterLanguageValue) -> Self {
        match v {
            StarterLanguageValue::Rust => Self::Rust,
            StarterLanguageValue::Typescript => Self::Typescript,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StarterContributionValue {
    BarWidget,
    DesktopWidget,
    SettingsPage,
    SidePanel,
    Action,
    Empty,
}

impl From<StarterContributionValue> for shilpo_ext_runtime::StarterContribution {
    fn from(v: StarterContributionValue) -> Self {
        match v {
            StarterContributionValue::BarWidget => Self::BarWidget,
            StarterContributionValue::DesktopWidget => Self::DesktopWidget,
            StarterContributionValue::SettingsPage => Self::SettingsPage,
            StarterContributionValue::SidePanel => Self::SidePanel,
            StarterContributionValue::Action => Self::Action,
            StarterContributionValue::Empty => Self::Empty,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManagerValue {
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Deno,
}

impl From<PackageManagerValue> for shilpo_ext_runtime::PackageManager {
    fn from(v: PackageManagerValue) -> Self {
        match v {
            PackageManagerValue::Npm => Self::Npm,
            PackageManagerValue::Pnpm => Self::Pnpm,
            PackageManagerValue::Yarn => Self::Yarn,
            PackageManagerValue::Bun => Self::Bun,
            PackageManagerValue::Deno => Self::Deno,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewSyntaxValue {
    Jsx,
    Builders,
}

impl From<ViewSyntaxValue> for shilpo_ext_runtime::ViewSyntax {
    fn from(v: ViewSyntaxValue) -> Self {
        match v {
            ViewSyntaxValue::Jsx => Self::Jsx,
            ViewSyntaxValue::Builders => Self::Builders,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn test_package_manager_value_deno_conversion() {
        let pm: shilpo_ext_runtime::PackageManager = PackageManagerValue::Deno.into();
        assert_eq!(pm, shilpo_ext_runtime::PackageManager::Deno);
    }

    #[test]
    fn test_cli_ext_new_package_manager_deno_parsed() {
        let args = Cli::try_parse_from([
            "shilpo",
            "ext",
            "new",
            "my-deno-ext",
            "--language",
            "typescript",
            "--package-manager",
            "deno",
        ])
        .unwrap();

        match args.command {
            Some(Commands::Ext {
                command:
                    ExtCommands::New {
                        name,
                        package_manager,
                        ..
                    },
            }) => {
                assert_eq!(name, "my-deno-ext");
                assert_eq!(package_manager, Some(PackageManagerValue::Deno));
            }
            _ => panic!("Expected ExtCommands::New"),
        }
    }
}
