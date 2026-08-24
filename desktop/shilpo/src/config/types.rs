use std::{
    borrow::Cow,
    collections::HashMap,
    fmt, fs,
    ops::Range,
    path::{Path, PathBuf},
    str::FromStr,
};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use shilpo_ext_api::{CanonicalId, ExtensionId, IdError};

use super::migration::LATEST_CONFIG_VERSION;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShellConfig {
    pub version: u32,
    pub theme: ThemeConfig,
    pub bar: BarConfig,
    #[serde(default)]
    pub desktop: DesktopConfig,
    #[serde(default)]
    pub extensions: ExtensionsConfig,
    #[serde(default)]
    pub outputs: HashMap<String, OutputConfig>,
    #[serde(default)]
    pub clock_format: Option<String>,
    #[serde(default)]
    pub temperature_unit: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub startup: StartupConfig,
    #[serde(default)]
    pub capture: CaptureConfig,
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    #[serde(default)]
    pub idle: IdleConfig,
    #[serde(default)]
    pub lock: LockConfig,
    #[serde(default)]
    pub keybindings: Vec<KeybindingConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LockConfig {
    /// PAM service name under `/etc/pam.d/` used for the lock screen's authentication.
    #[serde(default = "default_lock_pam_service")]
    pub pam_service: String,
    /// Whether suspend should wait for the session to be locked first.
    #[serde(default = "default_true")]
    pub lock_on_suspend: bool,
    #[serde(default = "default_true")]
    pub show_clock: bool,
    /// Seconds of no input after which the typed response is cleared.
    #[serde(default = "default_lock_clear_input_after_seconds")]
    pub clear_input_after_seconds: u32,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            pam_service: default_lock_pam_service(),
            lock_on_suspend: default_true(),
            show_clock: default_true(),
            clear_input_after_seconds: default_lock_clear_input_after_seconds(),
        }
    }
}

fn default_lock_pam_service() -> String {
    "login".to_string()
}

fn default_lock_clear_input_after_seconds() -> u32 {
    10
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeybindingConfig {
    pub action: String,
    #[serde(default)]
    pub shortcut: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionsConfig {
    /// Extension-wide settings keyed by extension ID.
    ///
    /// Every contribution instance receives the same validated JSON object.
    /// Surface-specific placement and geometry remain owned by their surface.
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
    /// Configured canonical ID for the active wallpaper provider.
    #[serde(default)]
    pub wallpaper_provider: Option<CanonicalId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DesktopConfig {
    #[serde(default)]
    pub widgets: Vec<DesktopWidgetConfig>,
    #[serde(default = "default_wallpaper_dir")]
    pub wallpaper_dir: PathBuf,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            widgets: Vec::new(),
            wallpaper_dir: default_wallpaper_dir(),
        }
    }
}

fn default_wallpaper_dir() -> PathBuf {
    PathBuf::from("~/Pictures/Wallpapers")
}

pub fn expand_home_path(path: &Path) -> PathBuf {
    if let (Ok(strip), Some(home)) = (path.strip_prefix("~"), dirs::home_dir()) {
        return home.join(strip);
    }
    path.to_path_buf()
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    #[serde(default = "default_screenshot_dir")]
    pub screenshot_dir: PathBuf,
    #[serde(default = "default_true")]
    pub show_pointer: bool,
    #[serde(default = "default_selection")]
    pub default_selection: String,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            screenshot_dir: default_screenshot_dir(),
            show_pointer: true,
            default_selection: default_selection(),
        }
    }
}

impl CaptureConfig {
    pub fn resolved_screenshot_dir(&self) -> PathBuf {
        expand_home_path(&self.screenshot_dir)
    }

    pub fn ensure_screenshot_dir(&self) -> std::io::Result<PathBuf> {
        let dir = self.resolved_screenshot_dir();
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

fn default_screenshot_dir() -> PathBuf {
    PathBuf::from("~/Pictures/Screenshots")
}

fn default_selection() -> String {
    "rectangle".to_string()
}

pub const DEFAULT_CLIPBOARD_HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClipboardConfig {
    #[serde(default = "default_clipboard_history_limit")]
    pub history_limit: usize,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            history_limit: default_clipboard_history_limit(),
        }
    }
}

pub fn default_clipboard_history_limit() -> usize {
    DEFAULT_CLIPBOARD_HISTORY_LIMIT
}

pub use shilpo_services::idle::{IdleAction, IdleBehaviorConfig};

pub fn default_grace_seconds() -> f64 {
    2.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdleConfig {
    #[serde(default = "default_grace_seconds")]
    pub grace_seconds: f64,
    #[serde(default = "default_idle_behaviors")]
    pub behaviors: std::collections::BTreeMap<String, IdleBehaviorConfig>,
}

pub fn default_idle_behaviors() -> std::collections::BTreeMap<String, IdleBehaviorConfig> {
    let mut behaviors = std::collections::BTreeMap::new();
    behaviors.insert(
        "lock".to_string(),
        IdleBehaviorConfig {
            enabled: true,
            timeout_seconds: 600.0,
            action: IdleAction::Lock,
            lock_before_suspend: false,
            resume_command: String::new(),
        },
    );
    behaviors.insert(
        "screen-off".to_string(),
        IdleBehaviorConfig {
            enabled: false,
            timeout_seconds: 660.0,
            action: IdleAction::ScreenOff,
            lock_before_suspend: false,
            resume_command: String::new(),
        },
    );
    behaviors.insert(
        "suspend".to_string(),
        IdleBehaviorConfig {
            enabled: false,
            timeout_seconds: 1800.0,
            action: IdleAction::Suspend,
            lock_before_suspend: true,
            resume_command: String::new(),
        },
    );
    behaviors
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            grace_seconds: default_grace_seconds(),
            behaviors: default_idle_behaviors(),
        }
    }
}

/// Resolve Shilpo XDG configuration directory (`$XDG_CONFIG_HOME/shilpo` or `$HOME/.config/shilpo`).
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("shilpo")
}

/// Resolve Shilpo primary configuration file path (`$XDG_CONFIG_HOME/shilpo/config.toml`).
pub fn default_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Resolve Shilpo XDG state directory (`$XDG_STATE_HOME/shilpo` or `$HOME/.local/state/shilpo`).
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("shilpo")
}

/// Resolve Shilpo XDG cache directory (`$XDG_CACHE_HOME/shilpo` or `$HOME/.cache/shilpo`).
pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("shilpo")
}

/// Resolve Shilpo XDG data directory (`$XDG_DATA_HOME/shilpo` or `$HOME/.local/share/shilpo`).
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"))
        .join("shilpo")
}

/// Path to doctor JSON report (`$XDG_STATE_HOME/shilpo/doctor-report.json`).
pub fn doctor_report_json_path() -> PathBuf {
    state_dir().join("doctor-report.json")
}

/// Path to doctor human text report (`$XDG_STATE_HOME/shilpo/doctor-report.txt`).
pub fn doctor_report_txt_path() -> PathBuf {
    state_dir().join("doctor-report.txt")
}

/// Marker file indicating one-shot first-login doctor check ran (`$XDG_STATE_HOME/shilpo/first-login-completed`).
pub fn doctor_first_login_marker_path() -> PathBuf {
    state_dir().join("first-login-completed")
}

/// Path to caffeine active state persistence file (`$XDG_STATE_HOME/shilpo/caffeine.state`).
pub fn caffeine_state_path() -> PathBuf {
    state_dir().join("caffeine.state")
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DesktopWidgetConfig {
    pub instance: String,
    pub contribution: ExtensionContributionRef,
    #[serde(default = "default_primary_output")]
    pub output: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_settings_value")]
    pub settings: serde_json::Value,
}

fn default_primary_output() -> String {
    "primary".into()
}

fn default_settings_value() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExtensionContributionRef(pub CanonicalId);

impl fmt::Display for ExtensionContributionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ext:{}", self.0)
    }
}

impl FromStr for ExtensionContributionRef {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .strip_prefix("ext:")
            .ok_or_else(|| {
                format!("invalid extension contribution '{value}': expected 'ext:<extension>/<contribution>'")
            })?
            .parse()
            .map(Self)
            .map_err(|error: IdError| error.to_string())
    }
}

impl Serialize for ExtensionContributionRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ExtensionContributionRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for ExtensionContributionRef {
    fn schema_name() -> Cow<'static, str> {
        "ExtensionContributionRef".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = generator.subschema_for::<String>();
        if let Some(object) = schema.as_object_mut() {
            object.insert(
                "pattern".into(),
                serde_json::Value::String(
                    r"^ext:[a-z0-9][a-z0-9-]*(\.[a-z0-9][a-z0-9-]*){2,}/[a-z0-9][a-z0-9_-]*$"
                        .into(),
                ),
            );
        }
        schema
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StartupConfig {
    #[serde(default)]
    pub autostart_apps: Vec<String>,
    #[serde(default = "default_compositor_wait_timeout")]
    pub compositor_wait_timeout_ms: u64,
}

fn default_compositor_wait_timeout() -> u64 {
    3000
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            autostart_apps: Vec::new(),
            compositor_wait_timeout_ms: 3000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub scale: Option<f32>,
    pub position: Option<BarPosition>,
    pub style: Option<BarStyle>,
    pub height: Option<u32>,
    pub padding: Option<u32>,
    pub margin: Option<BarMargin>,
    pub widget_spacing: Option<u32>,
    pub opacity: Option<f32>,
    pub exclusive_zone: Option<u32>,
    pub widgets: Option<BarWidgets>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub font_family: String,
    #[serde(default)]
    pub heading_font_family: Option<String>,
    #[serde(default)]
    pub mono_font_family: Option<String>,
    pub corner_radius_scale: f32,
    #[serde(default)]
    pub high_contrast: bool,
    #[serde(default)]
    pub reduced_motion: bool,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub gtk_theme_light: Option<String>,
    #[serde(default)]
    pub gtk_theme_dark: Option<String>,
    #[serde(default)]
    pub custom_adapter_cmd: Option<Vec<String>>,
    #[serde(default = "default_transition_duration_ms")]
    #[schemars(range(max = 5000))]
    pub transition_duration_ms: u64,
    #[serde(default)]
    pub scheme_variant: Option<String>,
}

fn default_transition_duration_ms() -> u64 {
    300
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BarConfig {
    pub position: BarPosition,
    pub style: BarStyle,
    pub height: u32,
    pub padding: u32,
    pub margin: BarMargin,
    pub widget_spacing: u32,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default)]
    pub exclusive_zone: Option<u32>,
    pub widgets: BarWidgets,
}

fn default_opacity() -> f32 {
    0.92
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BarMargin {
    pub horizontal: u32,
    pub vertical: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BarWidgets {
    pub start: Vec<BarWidget>,
    pub center: Vec<BarWidget>,
    pub end: Vec<BarWidget>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "PascalCase")]
pub enum BarPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "PascalCase")]
pub enum BarStyle {
    #[default]
    Hug,
    Float,
    Rect,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BarWidget {
    Builtin(BuiltinBarWidget),
    Extension(CanonicalId),
}

impl BarWidget {
    pub fn is_builtin(&self) -> bool {
        matches!(self, Self::Builtin(_))
    }

    pub fn is_extension(&self) -> bool {
        matches!(self, Self::Extension(_))
    }
}

impl fmt::Display for BarWidget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Builtin(widget) => write!(f, "builtin:{widget}"),
            Self::Extension(id) => write!(f, "ext:{id}"),
        }
    }
}

impl FromStr for BarWidget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(value) = value.strip_prefix("builtin:") {
            return BuiltinBarWidget::from_str(value).map(Self::Builtin);
        }
        if let Some(value) = value.strip_prefix("ext:") {
            return value
                .parse()
                .map(Self::Extension)
                .map_err(|error| error.to_string());
        }

        // Read the old built-in spelling so existing configurations can be
        // rewritten canonically without treating arbitrary strings as extensions.
        BuiltinBarWidget::from_legacy_str(value)
            .map(Self::Builtin)
            .ok_or_else(|| {
                format!(
                    "invalid bar widget reference '{value}': expected 'builtin:<name>' or 'ext:<extension>/<contribution>'"
                )
            })
    }
}

impl Serialize for BarWidget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BarWidget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

impl JsonSchema for BarWidget {
    fn schema_name() -> Cow<'static, str> {
        "BarWidget".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = generator.subschema_for::<String>();
        if let Some(object) = schema.as_object_mut() {
            object.insert(
                "pattern".into(),
                serde_json::Value::String(
                    r"^(builtin:(workspaces|running_apps|clock|date|media|sysinfo|network|bluetooth|caffeine|audio|battery|settings)|ext:[a-z0-9][a-z0-9-]*(\.[a-z0-9][a-z0-9-]*){2,}/[a-z0-9][a-z0-9_-]*)$"
                        .into(),
                ),
            );
        }
        schema
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "PascalCase")]
pub enum BuiltinBarWidget {
    Workspaces,
    RunningApps,
    Clock,
    Date,
    Media,
    Sysinfo,
    Network,
    Bluetooth,
    Caffeine,
    Audio,
    Battery,
    Settings,
}

impl BuiltinBarWidget {
    fn from_legacy_str(value: &str) -> Option<Self> {
        match value {
            "Workspaces" => Some(Self::Workspaces),
            "RunningApps" => Some(Self::RunningApps),
            "Clock" => Some(Self::Clock),
            "Date" => Some(Self::Date),
            "Media" => Some(Self::Media),
            "Sysinfo" => Some(Self::Sysinfo),
            "Network" => Some(Self::Network),
            "Bluetooth" => Some(Self::Bluetooth),
            "Caffeine" => Some(Self::Caffeine),
            "Audio" => Some(Self::Audio),
            "Battery" => Some(Self::Battery),
            "Settings" => Some(Self::Settings),
            _ => None,
        }
    }
}

impl fmt::Display for BuiltinBarWidget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Workspaces => "workspaces",
            Self::RunningApps => "running_apps",
            Self::Clock => "clock",
            Self::Date => "date",
            Self::Media => "media",
            Self::Sysinfo => "sysinfo",
            Self::Network => "network",
            Self::Bluetooth => "bluetooth",
            Self::Caffeine => "caffeine",
            Self::Audio => "audio",
            Self::Battery => "battery",
            Self::Settings => "settings",
        };
        value.fmt(f)
    }
}

impl FromStr for BuiltinBarWidget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "workspaces" => Ok(Self::Workspaces),
            "running_apps" => Ok(Self::RunningApps),
            "clock" => Ok(Self::Clock),
            "date" => Ok(Self::Date),
            "media" => Ok(Self::Media),
            "sysinfo" => Ok(Self::Sysinfo),
            "network" => Ok(Self::Network),
            "bluetooth" => Ok(Self::Bluetooth),
            "caffeine" => Ok(Self::Caffeine),
            "audio" => Ok(Self::Audio),
            "battery" => Ok(Self::Battery),
            "settings" => Ok(Self::Settings),
            _ => Err(format!("unknown built-in bar widget '{value}'")),
        }
    }
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            version: LATEST_CONFIG_VERSION,
            theme: ThemeConfig::default(),
            bar: BarConfig::default(),
            desktop: DesktopConfig::default(),
            extensions: ExtensionsConfig::default(),
            outputs: HashMap::new(),
            clock_format: None,
            temperature_unit: None,
            locale: None,
            startup: StartupConfig::default(),
            capture: CaptureConfig::default(),
            clipboard: ClipboardConfig::default(),
            idle: IdleConfig::default(),
            lock: LockConfig::default(),
            keybindings: Vec::new(),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            font_family: "sans-serif".into(),
            heading_font_family: None,
            mono_font_family: None,
            corner_radius_scale: 1.0,
            high_contrast: false,
            reduced_motion: false,
            provider: None,
            gtk_theme_light: None,
            gtk_theme_dark: None,
            custom_adapter_cmd: None,
            transition_duration_ms: 300,
            scheme_variant: None,
        }
    }
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            position: BarPosition::Top,
            style: BarStyle::Hug,
            height: 48,
            padding: 8,
            margin: BarMargin {
                horizontal: 8,
                vertical: 8,
            },
            widget_spacing: 6,
            opacity: 0.92,
            exclusive_zone: None,
            widgets: BarWidgets {
                start: vec![
                    BarWidget::Builtin(BuiltinBarWidget::Workspaces),
                    BarWidget::Builtin(BuiltinBarWidget::RunningApps),
                ],
                center: vec![
                    BarWidget::Builtin(BuiltinBarWidget::Clock),
                    BarWidget::Builtin(BuiltinBarWidget::Date),
                    BarWidget::Builtin(BuiltinBarWidget::Media),
                ],
                end: vec![
                    BarWidget::Builtin(BuiltinBarWidget::Sysinfo),
                    BarWidget::Builtin(BuiltinBarWidget::Network),
                    BarWidget::Builtin(BuiltinBarWidget::Bluetooth),
                    BarWidget::Builtin(BuiltinBarWidget::Caffeine),
                    BarWidget::Builtin(BuiltinBarWidget::Audio),
                    BarWidget::Builtin(BuiltinBarWidget::Battery),
                    BarWidget::Builtin(BuiltinBarWidget::Settings),
                ],
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    pub path: String,
    pub message: String,
    pub span: Option<Range<usize>>,
}

impl ConfigDiagnostic {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            span: None,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        diagnostic: ConfigDiagnostic,
    },
    Validation {
        diagnostics: Vec<ConfigDiagnostic>,
    },
    Serialize {
        source: toml::ser::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "cannot access config {}: {source}", path.display())
            }
            Self::Parse { diagnostic } => {
                write!(f, "config {}: {}", diagnostic.path, diagnostic.message)
            }
            Self::Validation { diagnostics } => write!(
                f,
                "{}",
                diagnostics
                    .iter()
                    .map(|d| format!("{}: {}", d.path, d.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::Serialize { source } => write!(f, "cannot serialize config: {source}"),
        }
    }
}
impl std::error::Error for ConfigError {}

impl BarConfig {
    pub fn validate(&self, prefix: &str, d: &mut Vec<ConfigDiagnostic>) {
        if !(16..=128).contains(&self.height) {
            d.push(ConfigDiagnostic::new(
                format!("{prefix}.height"),
                "must be between 16 and 128",
            ));
        }
        if self.padding > 64 {
            d.push(ConfigDiagnostic::new(
                format!("{prefix}.padding"),
                "must be at most 64",
            ));
        }
        if self.widget_spacing > 64 {
            d.push(ConfigDiagnostic::new(
                format!("{prefix}.widget_spacing"),
                "must be at most 64",
            ));
        }
        if self.margin.horizontal > 512 {
            d.push(ConfigDiagnostic::new(
                format!("{prefix}.margin.horizontal"),
                "must be at most 512",
            ));
        }
        if self.margin.vertical > 512 {
            d.push(ConfigDiagnostic::new(
                format!("{prefix}.margin.vertical"),
                "must be at most 512",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for (section, widgets) in [
            ("start", &self.widgets.start),
            ("center", &self.widgets.center),
            ("end", &self.widgets.end),
        ] {
            for widget in widgets {
                if !seen.insert(widget.clone()) {
                    d.push(ConfigDiagnostic::new(
                        format!("{prefix}.widgets.{section}"),
                        format!("duplicate widget {widget:?}"),
                    ));
                }
            }
        }
    }
}

impl ShellConfig {
    /// Resolves the effective BarConfig for a specific monitor output name or primary display.
    /// Returns None if the output is explicitly disabled (`enabled = false`).
    pub fn bar_for_output(&self, output_name: Option<&str>, is_primary: bool) -> Option<BarConfig> {
        let override_config = output_name
            .and_then(|name| self.outputs.get(name))
            .or_else(|| {
                if is_primary {
                    self.outputs.get("primary")
                } else {
                    None
                }
            });

        let Some(overrides) = override_config else {
            return Some(self.bar.clone());
        };

        if !overrides.enabled {
            return None;
        }

        let mut bar = self.bar.clone();
        if let Some(pos) = overrides.position {
            bar.position = pos;
        }
        if let Some(style) = overrides.style {
            bar.style = style;
        }
        if let Some(h) = overrides.height {
            bar.height = h;
        }
        if let Some(p) = overrides.padding {
            bar.padding = p;
        }
        if let Some(m) = &overrides.margin {
            bar.margin = m.clone();
        }
        if let Some(s) = overrides.widget_spacing {
            bar.widget_spacing = s;
        }
        if let Some(op) = overrides.opacity {
            bar.opacity = op;
        }
        if overrides.exclusive_zone.is_some() {
            bar.exclusive_zone = overrides.exclusive_zone;
        }
        if let Some(w) = &overrides.widgets {
            bar.widgets = w.clone();
        }

        Some(bar)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut d = Vec::new();
        if self.version != LATEST_CONFIG_VERSION {
            d.push(ConfigDiagnostic::new(
                "version",
                format!("must be {LATEST_CONFIG_VERSION}"),
            ));
        }
        if self.theme.font_family.trim().is_empty() {
            d.push(ConfigDiagnostic::new(
                "theme.font_family",
                "must not be empty",
            ));
        }
        if !self.theme.corner_radius_scale.is_finite()
            || !(0.0..=4.0).contains(&self.theme.corner_radius_scale)
        {
            d.push(ConfigDiagnostic::new(
                "theme.corner_radius_scale",
                "must be finite and between 0.0 and 4.0",
            ));
        }
        if self.theme.transition_duration_ms > 5000 {
            d.push(ConfigDiagnostic::new(
                "theme.transition_duration_ms",
                "must be at most 5000",
            ));
        }
        self.bar.validate("bar", &mut d);

        for output_name in self.outputs.keys() {
            if let Some(resolved_bar) = self.bar_for_output(Some(output_name), false) {
                resolved_bar.validate(&format!("outputs.\"{output_name}\""), &mut d);
            }
        }
        let mut desktop_instances = std::collections::HashSet::new();
        for (index, widget) in self.desktop.widgets.iter().enumerate() {
            let prefix = format!("desktop.widgets[{index}]");
            if widget.instance.trim().is_empty() {
                d.push(ConfigDiagnostic::new(
                    format!("{prefix}.instance"),
                    "must not be empty",
                ));
            } else if !desktop_instances.insert(widget.instance.as_str()) {
                d.push(ConfigDiagnostic::new(
                    format!("{prefix}.instance"),
                    "must be unique",
                ));
            }
            if widget.output.trim().is_empty() {
                d.push(ConfigDiagnostic::new(
                    format!("{prefix}.output"),
                    "must not be empty",
                ));
            }
            if widget.width == 0 || widget.height == 0 {
                d.push(ConfigDiagnostic::new(
                    prefix,
                    "width and height must be greater than zero",
                ));
            }
        }
        for (extension_id, settings) in &self.extensions.settings {
            if ExtensionId::new(extension_id.clone()).is_err() {
                d.push(ConfigDiagnostic::new(
                    format!("extensions.settings.\"{extension_id}\""),
                    "key must be a lowercase reverse-domain extension ID",
                ));
            }
            if !settings.is_object() {
                d.push(ConfigDiagnostic::new(
                    format!("extensions.settings.\"{extension_id}\""),
                    "must be an object",
                ));
            }
        }

        match self.capture.default_selection.as_str() {
            "rectangle" | "ellipse" => {}
            _ => {
                d.push(ConfigDiagnostic::new(
                    "capture.default_selection",
                    "must be 'rectangle' or 'ellipse'",
                ));
            }
        }

        if self.clipboard.history_limit == 0 {
            d.push(ConfigDiagnostic::new(
                "clipboard.history_limit",
                "must be greater than zero",
            ));
        }

        if !self.idle.grace_seconds.is_finite() || self.idle.grace_seconds < 0.0 {
            d.push(ConfigDiagnostic::new(
                "idle.grace_seconds",
                "must be a finite non-negative number",
            ));
        }

        for (name, behavior) in &self.idle.behaviors {
            if !behavior.timeout_seconds.is_finite() || behavior.timeout_seconds < 0.0 {
                d.push(ConfigDiagnostic::new(
                    format!("idle.behaviors.\"{name}\".timeout_seconds"),
                    "must be a finite non-negative number",
                ));
            }
            if let IdleAction::Command { ref command } = behavior.action
                && command.trim().is_empty()
            {
                d.push(ConfigDiagnostic::new(
                    format!("idle.behaviors.\"{name}\".action.command"),
                    "command must not be empty",
                ));
            }
        }

        let mut seen_keybinding_actions = std::collections::HashSet::new();
        let mut seen_keybinding_shortcuts = std::collections::HashMap::new();
        for (index, kb) in self.keybindings.iter().enumerate() {
            let prefix = format!("keybindings[{index}]");
            if kb.action.trim().is_empty() {
                d.push(ConfigDiagnostic::new(
                    format!("{prefix}.action"),
                    "action ID must not be empty",
                ));
            } else if !seen_keybinding_actions.insert(kb.action.as_str()) {
                d.push(ConfigDiagnostic::new(
                    format!("{prefix}.action"),
                    format!("duplicate keybinding entry for action '{}'", kb.action),
                ));
            }

            if kb.enabled {
                match &kb.shortcut {
                    Some(spec) if !spec.trim().is_empty() => {
                        match shilpo_ext_api::manifest::validate_shortcut_spec(spec) {
                            Ok(canonical) => {
                                if let Some(prev_action) = seen_keybinding_shortcuts
                                    .insert(canonical.clone(), kb.action.as_str())
                                {
                                    d.push(ConfigDiagnostic::new(
                                        format!("{prefix}.shortcut"),
                                        format!(
                                            "duplicate shortcut binding '{canonical}' for actions '{prev_action}' and '{}'",
                                            kb.action
                                        ),
                                    ));
                                }
                            }
                            Err(err) => {
                                d.push(ConfigDiagnostic::new(
                                    format!("{prefix}.shortcut"),
                                    format!("invalid shortcut specification '{spec}': {err}"),
                                ));
                            }
                        }
                    }
                    _ => {
                        d.push(ConfigDiagnostic::new(
                            format!("{prefix}.shortcut"),
                            "enabled keybinding must specify a non-empty shortcut",
                        ));
                    }
                }
            }
        }

        if d.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Validation { diagnostics: d })
        }
    }

    /// Persist this configuration atomically at `path`.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref().to_path_buf();
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.into(),
                source,
            })?;
        }
        let text =
            toml::to_string_pretty(self).map_err(|source| ConfigError::Serialize { source })?;
        let tmp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        fs::write(&tmp, text).map_err(|source| ConfigError::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, &path).map_err(|source| ConfigError::Io { path, source })?;
        Ok(())
    }

    /// Load and validate an existing configuration using ConfigResolver.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let resolver = crate::config::ConfigResolver::from_primary_path(path);
        resolver.load()
    }

    /// Load or create default configuration using ConfigResolver.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let resolver = crate::config::ConfigResolver::from_primary_path(path);
        resolver.load_or_create()
    }

    pub fn schema() -> schemars::Schema {
        schemars::schema_for!(Self)
    }
    pub fn schema_json() -> String {
        serde_json::to_string_pretty(&Self::schema()).expect("schema serialization") + "\n"
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShellSessionState {
    pub version: u32,
    #[serde(default)]
    pub pinned_apps: Vec<String>,
    #[serde(default)]
    pub launch_counts: HashMap<String, u32>,
    #[serde(default)]
    pub dnd_active: bool,
    #[serde(default)]
    pub night_light_active: bool,
    #[serde(default)]
    pub last_workspace_id: Option<u64>,
    #[serde(default)]
    pub visible_output_bars: Vec<u64>,
}

impl Default for ShellSessionState {
    fn default() -> Self {
        Self {
            version: 1,
            pinned_apps: Vec::new(),
            launch_counts: HashMap::new(),
            dnd_active: false,
            night_light_active: false,
            last_workspace_id: None,
            visible_output_bars: Vec::new(),
        }
    }
}

impl ShellSessionState {
    pub fn default_session_path() -> PathBuf {
        config_dir().join("session.json")
    }

    pub fn migrate_to_latest(raw_json: &str) -> Self {
        if let Ok(mut session) = serde_json::from_str::<Self>(raw_json)
            && (session.version == 0 || session.version == 1)
        {
            session.version = 1;
            return session;
        }
        Self::default()
    }

    pub fn restore_with_fallback(path: &Path) -> (Self, bool) {
        if !path.exists() {
            return (Self::default(), false);
        }
        let backup_path = path.with_extension("json.bak");
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(session) = serde_json::from_str::<Self>(&text)
        {
            return (session, true);
        }
        if backup_path.exists()
            && let Ok(text) = fs::read_to_string(&backup_path)
            && let Ok(session) = serde_json::from_str::<Self>(&text)
        {
            return (session, true);
        }
        (Self::default(), false)
    }

    pub fn load_or_default(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        Self::migrate_to_latest(&text)
    }

    pub fn sanitize_sensitive_state(&mut self) {
        self.pinned_apps
            .retain(|app| !app.contains("secret") && !app.contains("token"));
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), ConfigError> {
        let mut clone = self.clone();
        clone.sanitize_sensitive_state();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let text = serde_json::to_string_pretty(&clone).map_err(|e| ConfigError::Parse {
            diagnostic: ConfigDiagnostic {
                path: path.display().to_string(),
                message: e.to_string(),
                span: None,
            },
        })?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        fs::write(&tmp, text).map_err(|source| ConfigError::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    pub fn record_app_launch(&mut self, app_id: impl Into<String>) {
        let app_id = app_id.into();
        *self.launch_counts.entry(app_id).or_insert(0) += 1;
    }

    pub fn app_launch_count(&self, app_id: &str) -> u32 {
        self.launch_counts.get(app_id).copied().unwrap_or(0)
    }

    pub fn purge_usage_history(&mut self) {
        self.launch_counts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transition_duration_ms_default_and_validation() {
        let mut config = ShellConfig::default();
        assert_eq!(config.theme.transition_duration_ms, 300);
        assert!(config.validate().is_ok());

        config.theme.transition_duration_ms = 0;
        assert!(config.validate().is_ok());

        config.theme.transition_duration_ms = 5000;
        assert!(config.validate().is_ok());

        config.theme.transition_duration_ms = 5001;
        let err = config.validate().unwrap_err();
        if let ConfigError::Validation { diagnostics } = err {
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.path == "theme.transition_duration_ms")
            );
        } else {
            panic!("expected validation error");
        }
    }
}
