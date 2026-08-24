use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path};

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::effects::WallpaperSource;
use crate::events::EventKind;
use crate::id::{ContributionId, ExtensionId, IdError};

pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;
pub const SUPPORTED_API_VERSION: &str = "0.1.0";

#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    Id(IdError),
    ParseError(String),
    Validation(String),
}

impl From<IdError> for ManifestError {
    fn from(value: IdError) -> Self {
        Self::Id(value)
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(err) => write!(f, "{err}"),
            Self::ParseError(message) => write!(f, "failed to parse TOML manifest: {message}"),
            Self::Validation(message) => write!(f, "invalid extension manifest: {message}"),
        }
    }
}

impl std::error::Error for ManifestError {}

pub const OFFICIAL_AUTHOR: &str = "Sayeed Ahmed<sayeed205@gmail.com>";

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub id: ExtensionId,
    pub name: String,
    #[schemars(with = "String")]
    pub version: Version,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_api_version")]
    #[schemars(with = "String")]
    pub api_version: Version,
    #[serde(default = "default_api_version")]
    #[schemars(with = "String")]
    pub min_shilpo_version: Version,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub library: Option<LibraryConfig>,
    #[serde(default)]
    pub contributions: Contributions,
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

fn default_schema_version() -> u32 {
    SUPPORTED_SCHEMA_VERSION
}

fn default_api_version() -> Version {
    Version::parse(SUPPORTED_API_VERSION).expect("the supported API version is valid")
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LibraryConfig {
    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    #[serde(default)]
    pub bar_widgets: Vec<BarWidgetContribution>,
    #[serde(default)]
    pub bar_menus: Vec<BarMenuContribution>,
    #[serde(default)]
    pub desktop_widgets: Vec<DesktopWidgetContribution>,
    #[serde(default)]
    pub settings_pages: Vec<SettingsPageContribution>,
    #[serde(default)]
    pub side_panels: Vec<SidePanelContribution>,
    #[serde(default)]
    pub search_providers: Vec<SearchProviderContribution>,
    #[serde(default)]
    pub actions: Vec<ActionContribution>,
    #[serde(default)]
    pub keyboard_shortcuts: Vec<KeyboardShortcutContribution>,
    #[serde(default)]
    pub background_tasks: Vec<BackgroundTaskContribution>,
    #[serde(default)]
    pub wallpaper_providers: Vec<WallpaperProviderContribution>,
}

macro_rules! named_contribution {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub id: ContributionId,
            pub name: String,
            $(pub $field: $ty,)*
        }
    };
}

named_contribution!(BarWidgetContribution {
    description: Option<String>
});
named_contribution!(BarMenuContribution {
    bar_widget: ContributionId
});
named_contribution!(DesktopWidgetContribution {
    description: Option<String>,
    default_width: Option<u32>,
    default_height: Option<u32>,
    min_width: Option<u32>,
    min_height: Option<u32>
});
named_contribution!(SettingsPageContribution { schema: String });
named_contribution!(SidePanelContribution {});

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchProviderMode {
    Default,
    Apps,
    Actions,
    Clipboard,
    Calculator,
    Command,
    WebSearch,
    Keybindings,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchProviderContribution {
    pub id: ContributionId,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub modes: Vec<SearchProviderMode>,
}

named_contribution!(ActionContribution {});
named_contribution!(BackgroundTaskContribution {});

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperMode {
    Manual,
    Slideshow,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperTargetKind {
    Global,
    Workspace,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WallpaperProviderContribution {
    pub id: ContributionId,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub modes: Vec<WallpaperMode>,
    pub targets: Vec<WallpaperTargetKind>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyboardShortcutContribution {
    pub id: ContributionId,
    pub name: String,
    pub action: ContributionId,
    #[serde(default)]
    pub default_binding: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub event: EventKind,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub enum CapabilityKind {
    #[serde(rename = "events:subscribe")]
    EventsSubscribe,
    #[serde(rename = "wallpaper:set")]
    WallpaperSet,
    #[serde(rename = "wallpaper:read")]
    WallpaperRead,
    #[serde(rename = "theme:read")]
    ThemeRead,
    #[serde(rename = "theme:set_source")]
    ThemeSetSource,
    #[serde(rename = "notifications:show")]
    NotificationsShow,
    #[serde(rename = "clipboard:read")]
    ClipboardRead,
    #[serde(rename = "clipboard:write")]
    ClipboardWrite,
    #[serde(rename = "actions:invoke")]
    ActionsInvoke,
    #[serde(rename = "network:http")]
    NetworkHttp,
    #[serde(rename = "filesystem:read")]
    FilesystemRead,
    #[serde(rename = "filesystem:write")]
    FilesystemWrite,
    #[serde(rename = "location:read")]
    LocationRead,
    #[serde(rename = "secrets")]
    Secrets,
    #[serde(rename = "search:provide")]
    SearchProvide,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Capability {
    #[serde(rename = "events:subscribe")]
    EventsSubscribe { events: Vec<EventKind> },
    #[serde(rename = "wallpaper:set")]
    WallpaperSet { sources: Vec<WallpaperSource> },
    #[serde(rename = "wallpaper:read")]
    WallpaperRead,
    #[serde(rename = "theme:read")]
    ThemeRead,
    #[serde(rename = "theme:set_source")]
    ThemeSetSource,
    #[serde(rename = "notifications:show")]
    NotificationsShow,
    #[serde(rename = "clipboard:read")]
    ClipboardRead,
    #[serde(rename = "clipboard:write")]
    ClipboardWrite,
    #[serde(rename = "actions:invoke")]
    ActionsInvoke { actions: Vec<String> },
    #[serde(rename = "network:http")]
    NetworkHttp {
        hosts: Vec<String>,
        #[serde(default)]
        paths: Vec<String>,
    },
    #[serde(rename = "filesystem:read")]
    FilesystemRead { paths: Vec<String> },
    #[serde(rename = "filesystem:write")]
    FilesystemWrite { paths: Vec<String> },
    #[serde(rename = "location:read")]
    LocationRead,
    #[serde(rename = "secrets")]
    Secrets { purposes: Vec<SecretPurpose> },
    #[serde(rename = "search:provide")]
    SearchProvide,
}

impl Capability {
    pub fn kind(&self) -> CapabilityKind {
        match self {
            Self::EventsSubscribe { .. } => CapabilityKind::EventsSubscribe,
            Self::WallpaperSet { .. } => CapabilityKind::WallpaperSet,
            Self::WallpaperRead => CapabilityKind::WallpaperRead,
            Self::ThemeRead => CapabilityKind::ThemeRead,
            Self::ThemeSetSource => CapabilityKind::ThemeSetSource,
            Self::NotificationsShow => CapabilityKind::NotificationsShow,
            Self::ClipboardRead => CapabilityKind::ClipboardRead,
            Self::ClipboardWrite => CapabilityKind::ClipboardWrite,
            Self::ActionsInvoke { .. } => CapabilityKind::ActionsInvoke,
            Self::NetworkHttp { .. } => CapabilityKind::NetworkHttp,
            Self::FilesystemRead { .. } => CapabilityKind::FilesystemRead,
            Self::FilesystemWrite { .. } => CapabilityKind::FilesystemWrite,
            Self::LocationRead => CapabilityKind::LocationRead,
            Self::Secrets { .. } => CapabilityKind::Secrets,
            Self::SearchProvide => CapabilityKind::SearchProvide,
        }
    }

    pub fn allows_event(&self, event: EventKind) -> bool {
        matches!(self, Self::EventsSubscribe { events } if events.contains(&event))
    }
}

pub fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let (mut pattern_index, mut value_index, mut star, mut retry) = (0, 0, None, 0);
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    while value_index < value.len() {
        if pattern_index < pattern.len() && pattern[pattern_index] == value[value_index] {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            retry = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            retry += 1;
            value_index = retry;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanonicalVirtualPath(String);

impl CanonicalVirtualPath {
    pub fn parse_guest(value: &str) -> Result<Self, ManifestError> {
        parse_virtual_path(value, false)
    }

    fn parse_scope(value: &str) -> Result<Self, ManifestError> {
        parse_virtual_path(value, true)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn parse_virtual_path(
    value: &str,
    allow_wildcards: bool,
) -> Result<CanonicalVirtualPath, ManifestError> {
    if value.is_empty() || value.starts_with('/') || value.ends_with('/') || value.contains("//") {
        return Err(ManifestError::Validation(
            "virtual path must be relative and contain no empty components".into(),
        ));
    }
    let mut segments = Vec::new();
    for (index, segment) in value.split('/').enumerate() {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(ManifestError::Validation(
                "virtual path contains an invalid component".into(),
            ));
        }
        if !allow_wildcards && segment.contains('*') {
            return Err(ManifestError::Validation(
                "guest virtual paths cannot contain wildcards".into(),
            ));
        }
        if segment.contains("**") {
            return Err(ManifestError::Validation(
                "recursive wildcards are not supported".into(),
            ));
        }
        if segment
            .chars()
            .any(|character| matches!(character, '?' | '[' | ']' | '\\'))
        {
            return Err(ManifestError::Validation(
                "virtual path contains unsupported glob syntax".into(),
            ));
        }
        if index == 0 && !matches!(segment, "assets" | "data" | "user") {
            return Err(ManifestError::Validation(
                "virtual path must begin with assets, data, or user".into(),
            ));
        }
        segments.push(segment);
    }
    Ok(CanonicalVirtualPath(segments.join("/")))
}

pub fn virtual_path_pattern_matches(pattern: &str, path: &CanonicalVirtualPath) -> bool {
    let Ok(pattern) = CanonicalVirtualPath::parse_scope(pattern) else {
        return false;
    };
    let pattern_segments = pattern.0.split('/').collect::<Vec<_>>();
    let path_segments = path.0.split('/').collect::<Vec<_>>();
    pattern_segments.len() == path_segments.len()
        && pattern_segments
            .iter()
            .zip(path_segments)
            .all(|(pattern, value)| wildcard_matches(pattern, value))
}

impl Contributions {
    fn entries(&self) -> impl Iterator<Item = (&ContributionId, &str)> {
        self.bar_widgets
            .iter()
            .map(|entry| (&entry.id, entry.name.as_str()))
            .chain(
                self.bar_menus
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.desktop_widgets
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.settings_pages
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.side_panels
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.search_providers
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.actions
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.keyboard_shortcuts
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.background_tasks
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
            .chain(
                self.wallpaper_providers
                    .iter()
                    .map(|entry| (&entry.id, entry.name.as_str())),
            )
    }

    pub fn contains(&self, id: &ContributionId) -> bool {
        self.entries().any(|(candidate, _)| candidate == id)
    }
}

pub fn validate_shortcut_spec(spec: &str) -> Result<String, String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("shortcut specification cannot be empty".into());
    }
    let parts: Vec<&str> = trimmed.split('+').map(|s| s.trim()).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err("shortcut contains an empty key or modifier".into());
    }

    let mut super_mod = false;
    let mut ctrl_mod = false;
    let mut alt_mod = false;
    let mut shift_mod = false;
    let mut non_modifiers = Vec::new();

    for part in parts {
        match part.to_lowercase().as_str() {
            "super" | "mod" | "meta" => {
                if super_mod {
                    return Err("duplicate modifier 'Super'".into());
                }
                super_mod = true;
            }
            "ctrl" | "control" => {
                if ctrl_mod {
                    return Err("duplicate modifier 'Ctrl'".into());
                }
                ctrl_mod = true;
            }
            "alt" => {
                if alt_mod {
                    return Err("duplicate modifier 'Alt'".into());
                }
                alt_mod = true;
            }
            "shift" => {
                if shift_mod {
                    return Err("duplicate modifier 'Shift'".into());
                }
                shift_mod = true;
            }
            _ => {
                non_modifiers.push(part);
            }
        }
    }

    if non_modifiers.len() != 1 {
        return Err(format!(
            "shortcut must have exactly one main key, found {}",
            non_modifiers.len()
        ));
    }

    let mut canonical_mods = Vec::new();
    if super_mod {
        canonical_mods.push("Super");
    }
    if ctrl_mod {
        canonical_mods.push("Ctrl");
    }
    if alt_mod {
        canonical_mods.push("Alt");
    }
    if shift_mod {
        canonical_mods.push("Shift");
    }

    let key = non_modifiers[0];
    if key
        .chars()
        .any(|c| matches!(c, '"' | '\\' | '\n' | '\r' | '{' | '}' | ';'))
    {
        return Err("shortcut key contains a character unsafe for compositor projection".into());
    }
    let canonical_key = if key.len() == 1 {
        key.to_uppercase()
    } else {
        let mut chars = key.chars();
        match chars.next() {
            None => String::new(),
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        }
    };

    if canonical_mods.is_empty() {
        Ok(canonical_key)
    } else {
        Ok(format!("{}+{}", canonical_mods.join("+"), canonical_key))
    }
}

pub fn validate_author(author: &str) -> Result<(), ManifestError> {
    let trimmed = author.trim();
    if trimmed.is_empty() {
        return Err(ManifestError::Validation(
            "author entry cannot be empty".into(),
        ));
    }
    if !trimmed.ends_with('>')
        || trimmed.matches('<').count() != 1
        || trimmed.matches('>').count() != 1
    {
        return Err(ManifestError::Validation(format!(
            "author '{author}' must be in mailbox format 'Display Name <local@domain>'"
        )));
    }
    let Some(open_idx) = trimmed.rfind('<') else {
        return Err(ManifestError::Validation(format!(
            "author '{author}' must be in mailbox format 'Display Name <local@domain>'"
        )));
    };
    let name = trimmed[..open_idx].trim();
    if name.is_empty() {
        return Err(ManifestError::Validation(format!(
            "author '{author}' is missing display name"
        )));
    }
    let email = trimmed[open_idx + 1..trimmed.len() - 1].trim();
    if email.is_empty() {
        return Err(ManifestError::Validation(format!(
            "author '{author}' has empty email address"
        )));
    }
    if email.contains(char::is_whitespace) {
        return Err(ManifestError::Validation(format!(
            "author '{author}' email address cannot contain whitespace"
        )));
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(ManifestError::Validation(format!(
            "author '{author}' email address is missing '@'"
        )));
    };
    if local.is_empty() || domain.is_empty() {
        return Err(ManifestError::Validation(format!(
            "author '{author}' email address must have both local and domain parts"
        )));
    }
    if local.contains('@') || domain.contains('@') {
        return Err(ManifestError::Validation(format!(
            "author '{author}' email address cannot contain multiple '@'"
        )));
    }
    if !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
        || domain.split('.').any(|part| part.is_empty())
    {
        return Err(ManifestError::Validation(format!(
            "author '{author}' email domain is invalid"
        )));
    }
    Ok(())
}

impl ExtensionManifest {
    pub fn from_toml(source: &str) -> Result<Self, ManifestError> {
        let manifest: Self =
            toml::from_str(source).map_err(|error| ManifestError::ParseError(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.name.trim().is_empty() {
            return Err(ManifestError::Validation(
                "the extension name cannot be empty".into(),
            ));
        }
        for author in &self.authors {
            validate_author(author)?;
        }
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(ManifestError::Validation(format!(
                "schema version {} is unsupported; expected {SUPPORTED_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if self.api_version != default_api_version() {
            return Err(ManifestError::Validation(format!(
                "API version {} is unsupported; expected {SUPPORTED_API_VERSION}",
                self.api_version
            )));
        }
        if let Some(library) = &self.library {
            validate_relative_path("library.path", &library.path)?;
        }

        let mut contribution_ids = HashSet::new();
        for (id, name) in self.contributions.entries() {
            if !contribution_ids.insert(id) {
                return Err(ManifestError::Validation(format!(
                    "duplicate contribution ID '{id}'"
                )));
            }
            if name.trim().is_empty() {
                return Err(ManifestError::Validation(format!(
                    "contribution '{id}' has an empty name"
                )));
            }
        }
        for page in &self.contributions.settings_pages {
            validate_relative_path("settings page schema", &page.schema)?;
        }
        let mut bar_widget_targets = HashSet::new();
        for menu in &self.contributions.bar_menus {
            let target_exists = self
                .contributions
                .bar_widgets
                .iter()
                .any(|bw| bw.id == menu.bar_widget);
            if !target_exists {
                return Err(ManifestError::Validation(format!(
                    "bar menu '{}' targets unknown bar widget '{}'",
                    menu.id, menu.bar_widget
                )));
            }
            if !bar_widget_targets.insert(&menu.bar_widget) {
                return Err(ManifestError::Validation(format!(
                    "multiple bar menus target bar widget '{}'",
                    menu.bar_widget
                )));
            }
        }
        for widget in &self.contributions.desktop_widgets {
            if let (Some(minimum), Some(default)) = (widget.min_width, widget.default_width)
                && default < minimum
            {
                return Err(ManifestError::Validation(format!(
                    "desktop widget '{}' default width is below its minimum",
                    widget.id
                )));
            }
            if let (Some(minimum), Some(default)) = (widget.min_height, widget.default_height)
                && default < minimum
            {
                return Err(ManifestError::Validation(format!(
                    "desktop widget '{}' default height is below its minimum",
                    widget.id
                )));
            }
        }
        for shortcut in &self.contributions.keyboard_shortcuts {
            let target_exists = self
                .contributions
                .actions
                .iter()
                .any(|action| action.id == shortcut.action);
            if !target_exists {
                return Err(ManifestError::Validation(format!(
                    "keyboard shortcut '{}' targets unknown action '{}'",
                    shortcut.id, shortcut.action
                )));
            }
            if let Some(binding) = &shortcut.default_binding {
                validate_shortcut_spec(binding).map_err(|err| {
                    ManifestError::Validation(format!(
                        "keyboard shortcut '{}' default binding is invalid: {err}",
                        shortcut.id
                    ))
                })?;
            }
        }
        for provider in &self.contributions.wallpaper_providers {
            if provider.modes.is_empty() {
                return Err(ManifestError::Validation(format!(
                    "wallpaper provider '{}' must declare at least one mode",
                    provider.id
                )));
            }
            let mut seen_modes = HashSet::new();
            for mode in &provider.modes {
                if !seen_modes.insert(*mode) {
                    return Err(ManifestError::Validation(format!(
                        "wallpaper provider '{}' has duplicate mode '{mode:?}'",
                        provider.id
                    )));
                }
            }
            if provider.targets.is_empty() {
                return Err(ManifestError::Validation(format!(
                    "wallpaper provider '{}' must declare at least one target",
                    provider.id
                )));
            }
            let mut seen_targets = HashSet::new();
            for target in &provider.targets {
                if !seen_targets.insert(*target) {
                    return Err(ManifestError::Validation(format!(
                        "wallpaper provider '{}' has duplicate target '{target:?}'",
                        provider.id
                    )));
                }
            }
        }
        for provider in &self.contributions.search_providers {
            if provider.modes.is_empty() {
                return Err(ManifestError::Validation(format!(
                    "search provider '{}' must declare at least one mode",
                    provider.id
                )));
            }
            let mut seen_modes = HashSet::new();
            for mode in &provider.modes {
                if !seen_modes.insert(*mode) {
                    return Err(ManifestError::Validation(format!(
                        "search provider '{}' has duplicate mode '{mode:?}'",
                        provider.id
                    )));
                }
            }
        }
        if !self.contributions.search_providers.is_empty()
            && !self
                .capabilities
                .iter()
                .any(|c| matches!(c, Capability::SearchProvide))
        {
            return Err(ManifestError::Validation(
                "search_providers contribution requires the 'search:provide' capability".into(),
            ));
        }

        let mut subscriptions = HashSet::new();
        for subscription in &self.subscriptions {
            if !subscriptions.insert(subscription.event) {
                return Err(ManifestError::Validation(format!(
                    "duplicate subscription for {:?}",
                    subscription.event
                )));
            }
            if !self
                .capabilities
                .iter()
                .any(|capability| capability.allows_event(subscription.event))
            {
                return Err(ManifestError::Validation(format!(
                    "subscription {:?} is not covered by events:subscribe",
                    subscription.event
                )));
            }
        }
        validate_capabilities(&self.capabilities)
    }

    pub fn schema_json() -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&schemars::schema_for!(Self))
    }
}

fn validate_relative_path(field: &str, value: &str) -> Result<(), ManifestError> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ManifestError::Validation(format!(
            "{field} must be a safe relative path"
        )));
    }
    Ok(())
}

fn validate_capabilities(capabilities: &[Capability]) -> Result<(), ManifestError> {
    let mut kinds = HashSet::new();
    for capability in capabilities {
        if !kinds.insert(capability.kind()) {
            return Err(ManifestError::Validation(format!(
                "duplicate capability {:?}",
                capability.kind()
            )));
        }
        let valid = match capability {
            Capability::EventsSubscribe { events } => !events.is_empty(),
            Capability::WallpaperSet { sources } => !sources.is_empty(),
            Capability::WallpaperRead
            | Capability::ThemeRead
            | Capability::ThemeSetSource
            | Capability::NotificationsShow
            | Capability::ClipboardRead
            | Capability::ClipboardWrite
            | Capability::LocationRead
            | Capability::SearchProvide => true,
            Capability::ActionsInvoke { actions } => {
                !actions.is_empty() && actions.iter().all(|action| !action.trim().is_empty())
            }
            Capability::NetworkHttp { hosts, paths } => {
                !hosts.is_empty()
                    && hosts.iter().all(|host| {
                        !host.trim().is_empty() && !host.contains('/') && !host.contains("://")
                    })
                    && paths.iter().all(|path| path.starts_with('/'))
            }
            Capability::FilesystemRead { paths } | Capability::FilesystemWrite { paths } => {
                !paths.is_empty() && paths.iter().all(|path| valid_virtual_path_pattern(path))
            }
            Capability::Secrets { purposes } => {
                if purposes.is_empty() {
                    false
                } else {
                    let mut seen = HashSet::new();
                    purposes.iter().all(|p| {
                        SecretPurpose::validate(p.as_str()).is_ok() && seen.insert(p.as_str())
                    })
                }
            }
        };
        if !valid {
            return Err(ManifestError::Validation(format!(
                "capability {:?} has an empty or invalid scope",
                capability.kind()
            )));
        }
    }
    Ok(())
}

pub fn valid_virtual_path_pattern(value: &str) -> bool {
    CanonicalVirtualPath::parse_scope(value).is_ok()
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct SecretPurpose(String);

impl SecretPurpose {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        Self::validate(&value)?;
        Ok(Self(value))
    }

    pub fn parse(value: &str) -> Result<Self, ManifestError> {
        Self::validate(value)?;
        Ok(Self(value.to_string()))
    }

    pub fn validate(value: &str) -> Result<(), ManifestError> {
        if value.is_empty() || value.len() > 64 {
            return Err(ManifestError::Validation(
                "secret purpose must be 1 to 64 bytes".into(),
            ));
        }
        let bytes = value.as_bytes();
        if !bytes[0].is_ascii_lowercase() {
            return Err(ManifestError::Validation(
                "secret purpose must start with a lowercase ASCII letter".into(),
            ));
        }
        for &b in &bytes[1..] {
            if !b.is_ascii_lowercase() && !b.is_ascii_digit() && b != b'-' {
                return Err(ManifestError::Validation(format!(
                    "invalid character '{:?}' in secret purpose",
                    b as char
                )));
            }
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for SecretPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretPurpose({})", self.0)
    }
}

impl std::str::FromStr for SecretPurpose {
    type Err = ManifestError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for SecretPurpose {
    type Error = ManifestError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<SecretPurpose> for String {
    fn from(p: SecretPurpose) -> Self {
        p.0
    }
}

impl AsRef<str> for SecretPurpose {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for SecretPurpose {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct SecretRef {
    #[serde(rename = "secret_ref")]
    pub handle: String,
}

impl SecretRef {
    pub fn new(handle: impl Into<String>) -> Self {
        Self {
            handle: handle.into(),
        }
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretRef")
            .field("handle", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretRef(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_bar_menu_manifest() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.bar_widgets]]
            id = "weather-widget"
            name = "Weather"

            [[contributions.bar_menus]]
            id = "weather-menu"
            name = "Weather Details"
            bar_widget = "weather-widget"
        "#;
        let manifest = ExtensionManifest::from_toml(toml).unwrap();
        assert_eq!(manifest.contributions.bar_menus.len(), 1);
        assert_eq!(
            manifest.contributions.bar_menus[0].id.as_str(),
            "weather-menu"
        );
        assert_eq!(
            manifest.contributions.bar_menus[0].bar_widget.as_str(),
            "weather-widget"
        );
    }

    #[test]
    fn test_bar_menu_missing_target_fails() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.bar_menus]]
            id = "weather-menu"
            name = "Weather Details"
            bar_widget = "nonexistent-widget"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(err.to_string().contains("unknown bar widget"));
    }

    #[test]
    fn test_bar_menu_duplicate_target_fails() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.bar_widgets]]
            id = "weather-widget"
            name = "Weather"

            [[contributions.bar_menus]]
            id = "weather-menu-1"
            name = "Weather Details 1"
            bar_widget = "weather-widget"

            [[contributions.bar_menus]]
            id = "weather-menu-2"
            name = "Weather Details 2"
            bar_widget = "weather-widget"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("multiple bar menus target bar widget")
        );
    }

    #[test]
    fn test_bar_menu_duplicate_contribution_id_fails() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.bar_widgets]]
            id = "weather-item"
            name = "Weather"

            [[contributions.bar_menus]]
            id = "weather-item"
            name = "Weather Details"
            bar_widget = "weather-item"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(err.to_string().contains("duplicate contribution ID"));
    }

    #[test]
    fn test_bar_menu_unknown_sizing_field_fails() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.bar_widgets]]
            id = "weather-widget"
            name = "Weather"

            [[contributions.bar_menus]]
            id = "weather-menu"
            name = "Weather Details"
            bar_widget = "weather-widget"
            width = 300
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(matches!(err, ManifestError::ParseError(_)));
    }

    #[test]
    fn test_valid_keyboard_shortcut_manifest() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.actions]]
            id = "toggle-weather"
            name = "Toggle Weather"

            [[contributions.keyboard_shortcuts]]
            id = "toggle-weather-shortcut"
            name = "Toggle Weather Shortcut"
            action = "toggle-weather"
            default_binding = "Super+Shift+W"

            [[contributions.keyboard_shortcuts]]
            id = "unbound-shortcut"
            name = "Unbound Shortcut"
            action = "toggle-weather"
        "#;
        let manifest = ExtensionManifest::from_toml(toml).unwrap();
        assert_eq!(manifest.contributions.keyboard_shortcuts.len(), 2);
        assert_eq!(
            manifest.contributions.keyboard_shortcuts[0].id.as_str(),
            "toggle-weather-shortcut"
        );
        assert_eq!(
            manifest.contributions.keyboard_shortcuts[0].action.as_str(),
            "toggle-weather"
        );
        assert_eq!(
            manifest.contributions.keyboard_shortcuts[0]
                .default_binding
                .as_deref(),
            Some("Super+Shift+W")
        );
        assert_eq!(
            manifest.contributions.keyboard_shortcuts[1].default_binding,
            None
        );
    }

    #[test]
    fn test_keyboard_shortcut_missing_target_action_fails() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.keyboard_shortcuts]]
            id = "shortcut-1"
            name = "Shortcut 1"
            action = "nonexistent-action"
            default_binding = "Super+W"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(err.to_string().contains("targets unknown action"));
    }

    #[test]
    fn test_keyboard_shortcut_duplicate_contribution_id_fails() {
        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"

            [[contributions.actions]]
            id = "toggle-weather"
            name = "Toggle Weather"

            [[contributions.keyboard_shortcuts]]
            id = "toggle-weather"
            name = "Shortcut with same ID as action"
            action = "toggle-weather"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(err.to_string().contains("duplicate contribution ID"));
    }

    #[test]
    fn test_shortcut_spec_validation_and_canonicalization() {
        assert_eq!(
            validate_shortcut_spec("shift+super+w").unwrap(),
            "Super+Shift+W"
        );
        assert_eq!(
            validate_shortcut_spec("ctrl+alt+space").unwrap(),
            "Ctrl+Alt+Space"
        );
        assert_eq!(validate_shortcut_spec("super+b").unwrap(), "Super+B");

        assert!(validate_shortcut_spec("").is_err());
        assert!(validate_shortcut_spec("Super+").is_err());
        assert!(validate_shortcut_spec("Super+Ctrl").is_err());
        assert!(validate_shortcut_spec("Super+Super+A").is_err());
        assert!(validate_shortcut_spec("Super+A+B").is_err());
        assert!(validate_shortcut_spec("Super+bad\"key").is_err());
    }

    #[test]
    fn test_valid_search_providers_manifest() {
        let toml = r#"
            id = "org.shilpo.search"
            name = "Search Extension"
            version = "1.0.0"
            schema_version = 1

            [[contributions.search_providers]]
            id = "web-search"
            name = "Web Search"
            modes = ["web_search", "default"]

            [[contributions.search_providers]]
            id = "docs-search"
            name = "Documentation Search"
            modes = ["default"]

            [[capabilities]]
            kind = "search:provide"
        "#;
        let manifest =
            ExtensionManifest::from_toml(toml).expect("search_providers manifest should parse");
        assert_eq!(manifest.contributions.search_providers.len(), 2);
        assert_eq!(
            manifest.contributions.search_providers[0].id.as_str(),
            "web-search"
        );
        assert_eq!(
            manifest.contributions.search_providers[0].name,
            "Web Search"
        );
        assert_eq!(
            manifest.contributions.search_providers[0].modes,
            vec![SearchProviderMode::WebSearch, SearchProviderMode::Default]
        );
        assert_eq!(
            manifest.contributions.search_providers[1].id.as_str(),
            "docs-search"
        );
        assert_eq!(
            manifest.contributions.search_providers[1].name,
            "Documentation Search"
        );
    }

    #[test]
    fn test_search_provider_missing_modes_fails() {
        let toml = r#"
            id = "org.shilpo.search"
            name = "Search Extension"
            version = "1.0.0"
            schema_version = 1

            [[contributions.search_providers]]
            id = "web-search"
            name = "Web Search"
            modes = []

            [[capabilities]]
            kind = "search:provide"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(matches!(err, ManifestError::Validation(_)));
        assert!(
            err.to_string().contains("must declare at least one mode"),
            "Error should reject empty modes: {err}"
        );
    }

    #[test]
    fn test_search_provider_duplicate_modes_fails() {
        let toml = r#"
            id = "org.shilpo.search"
            name = "Search Extension"
            version = "1.0.0"
            schema_version = 1

            [[contributions.search_providers]]
            id = "web-search"
            name = "Web Search"
            modes = ["default", "default"]

            [[capabilities]]
            kind = "search:provide"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(matches!(err, ManifestError::Validation(_)));
        assert!(
            err.to_string().contains("duplicate mode"),
            "Error should reject duplicate mode: {err}"
        );
    }

    #[test]
    fn test_search_provider_missing_capability_fails() {
        let toml = r#"
            id = "org.shilpo.search"
            name = "Search Extension"
            version = "1.0.0"
            schema_version = 1

            [[contributions.search_providers]]
            id = "web-search"
            name = "Web Search"
            modes = ["default"]
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(matches!(err, ManifestError::Validation(_)));
        assert!(
            err.to_string()
                .contains("requires the 'search:provide' capability"),
            "Error should reject missing search:provide capability: {err}"
        );
    }

    #[test]
    fn test_legacy_provider_field_rejected() {
        let legacy_field = ["launcher", "providers"].join("_");
        let toml = format!(
            r#"
            id = "org.shilpo.search"
            name = "Search Extension"
            version = "1.0.0"
            schema_version = 1

            [[contributions.{legacy_field}]]
            id = "web-search"
            name = "Web Search"
        "#
        );
        let err = ExtensionManifest::from_toml(&toml).unwrap_err();
        assert!(matches!(err, ManifestError::ParseError(_)));
        assert!(
            err.to_string()
                .contains(&format!("unknown field `{legacy_field}`")),
            "Error should reject the legacy provider field: {err}"
        );
    }

    #[test]
    fn test_search_provider_duplicate_contribution_id_fails() {
        let toml = r#"
            id = "org.shilpo.search"
            name = "Search Extension"
            version = "1.0.0"
            schema_version = 1

            [[contributions.bar_widgets]]
            id = "query-tool"
            name = "Query Tool Bar Widget"

            [[contributions.search_providers]]
            id = "query-tool"
            name = "Query Tool Search Provider"
            modes = ["default"]

            [[capabilities]]
            kind = "search:provide"
        "#;
        let err = ExtensionManifest::from_toml(toml).unwrap_err();
        assert!(matches!(err, ManifestError::Validation(_)));
        assert!(
            err.to_string().contains("duplicate contribution ID"),
            "Error should reject cross-kind duplicate ID: {err}"
        );
    }

    #[test]
    fn test_official_author_constant_is_valid() {
        assert_eq!(OFFICIAL_AUTHOR, "Sayeed Ahmed<sayeed205@gmail.com>");
        validate_author(OFFICIAL_AUTHOR).expect("OFFICIAL_AUTHOR must be valid mailbox format");
    }

    #[test]
    fn test_valid_author_entries_accepted() {
        let valid_authors = [
            "Sayeed Ahmed<sayeed205@gmail.com>",
            "Sayeed Ahmed <sayeed205@gmail.com>",
            "Alice Smith <alice@example.com>",
            "Bob <bob.vance@refrigeration.co.uk>",
            "Shilpo Contributors <contributors@shilpo.org>",
        ];
        for author in valid_authors {
            validate_author(author)
                .unwrap_or_else(|e| panic!("expected '{author}' to be valid: {e}"));
        }

        let toml = r#"
            id = "org.shilpo.weather"
            name = "Weather App"
            version = "1.0.0"
            authors = ["Sayeed Ahmed<sayeed205@gmail.com>", "Alice <alice@example.com>"]
        "#;
        let manifest = ExtensionManifest::from_toml(toml).unwrap();
        assert_eq!(manifest.authors.len(), 2);
    }

    #[test]
    fn test_malformed_author_entries_rejected_at_parse_time() {
        let malformed_authors = [
            "",
            "   ",
            "Sayeed Ahmed",
            "<sayeed205@gmail.com>",
            " <sayeed205@gmail.com>",
            "Sayeed Ahmed <>",
            "Sayeed Ahmed <notanemail>",
            "Sayeed Ahmed <user@>",
            "Sayeed Ahmed <@domain.com>",
            "Sayeed Ahmed <user@domain>",
            "Sayeed Ahmed <user@domain..com>",
            "Sayeed Ahmed <user@.domain.com>",
            "Sayeed Ahmed <user@domain.com.>",
            "Sayeed Ahmed <user @domain.com>",
            "Sayeed <Ahmed> <sayeed205@gmail.com>",
            "Sayeed Ahmed <sayeed205@gmail.com> trailing",
            "Sayeed Ahmed <sayeed205@gmail.com",
            "Sayeed Ahmed sayeed205@gmail.com>",
        ];
        for author in malformed_authors {
            assert!(
                validate_author(author).is_err(),
                "expected '{author}' to be rejected"
            );

            let toml = format!(
                r#"
                id = "org.shilpo.weather"
                name = "Weather App"
                version = "1.0.0"
                authors = [{:?}]
                "#,
                author
            );
            let err = ExtensionManifest::from_toml(&toml).unwrap_err();
            assert!(
                matches!(err, ManifestError::Validation(_)),
                "expected Validation error for author '{author}', got {err:?}"
            );
        }
    }

    #[test]
    fn canonical_virtual_paths_reject_traversal_and_cross_root_inputs() {
        for value in [
            "/user/file",
            "user/",
            "user//file",
            "user/./file",
            "user/../secrets",
            "tmp/file",
            "user/*/file",
        ] {
            assert!(
                CanonicalVirtualPath::parse_guest(value).is_err(),
                "guest path should be rejected: {value}"
            );
        }
    }

    #[test]
    fn virtual_path_scopes_match_segments_without_crossing_roots() {
        let path = CanonicalVirtualPath::parse_guest("user/notes/today.txt").unwrap();
        assert!(virtual_path_pattern_matches("user/notes/*", &path));
        assert!(!virtual_path_pattern_matches("user/*", &path));
        assert!(!virtual_path_pattern_matches("assets/*", &path));
        assert!(!virtual_path_pattern_matches("user/../*", &path));
        assert!(valid_virtual_path_pattern("assets/icons/*.svg"));
        assert!(!valid_virtual_path_pattern("assets/**/icons"));
    }
}
