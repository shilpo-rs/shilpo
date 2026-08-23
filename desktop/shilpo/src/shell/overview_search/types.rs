use std::{borrow::Cow, fmt, path::PathBuf};

use shilpo_m3e::IconName;
use shilpo_services::{Application, ClipboardItem};

use super::{parser::SearchMode, sink::SearchSink};
use crate::shell::actions::ActionDescriptor;

/// Default bound on candidates collected from a single trusted (built-in) provider's
/// scratch sink before ranking. Must comfortably exceed any realistic single provider's
/// output (hundreds of installed applications, dozens of actions, clipboard history,
/// keybindings) so a real candidate is never dropped before the ranker gets to see it.
pub const DEFAULT_SCRATCH_CAPACITY: usize = 4096;

/// Unique identifier for a registered search provider.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(Cow<'static, str>);

impl ProviderId {
    pub const fn from_static(id: &'static str) -> Self {
        Self(Cow::Borrowed(id))
    }

    pub fn new(id: impl Into<String>) -> Self {
        Self(Cow::Owned(id.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&'static str> for ProviderId {
    fn from(s: &'static str) -> Self {
        Self::from_static(s)
    }
}

impl From<String> for ProviderId {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Category classification for search candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultCategory {
    Window,
    Application,
    Action,
    Clipboard,
    Calculator,
    Command,
    WebSearch,
    FilePath,
    Uri,
    Keybinding,
    Custom,
}

impl ResultCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Window => "Window",
            Self::Application => "Application",
            Self::Action => "Action",
            Self::Clipboard => "Clipboard",
            Self::Calculator => "Calculator",
            Self::Command => "Command",
            Self::WebSearch => "Web Search",
            Self::FilePath => "Path",
            Self::Uri => "URI",
            Self::Keybinding => "Keybinding",
            Self::Custom => "Result",
        }
    }

    pub fn is_calculation(&self) -> bool {
        matches!(self, Self::Calculator)
    }

    pub fn is_suggestion(&self) -> bool {
        matches!(self, Self::Command | Self::WebSearch)
    }
}

/// Latency classification declared by search providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LatencyClass {
    Instant,
    Fast,
    Slow,
    Async,
}

/// Provider-declared completion state for query streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionState {
    Complete,
    Partial,
}

/// Icon representation for search candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchResultIcon {
    AppIcon(Option<PathBuf>),
    Named(IconName),
    Initial(char),
    ExtensionAsset {
        extension_id: shilpo_ext_api::ExtensionId,
        relative_path: PathBuf,
    },
}

/// Immutable request carrying monotonic query generation and parsed query text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    pub raw_query: String,
    pub query: String,
    pub mode: SearchMode,
    pub generation: u64,
}

impl SearchRequest {
    pub fn new(
        raw_query: impl Into<String>,
        mode: SearchMode,
        query: impl Into<String>,
        generation: u64,
    ) -> Self {
        Self {
            raw_query: raw_query.into(),
            query: query.into(),
            mode,
            generation,
        }
    }
}

/// Provider-owned activation payload passed to [`SearchProvider::activate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchActivation {
    pub payload: String,
}

impl SearchActivation {
    pub fn new(payload: impl Into<String>) -> Self {
        Self {
            payload: payload.into(),
        }
    }
}

/// The concrete result or execution effect produced by [`SearchProvider::activate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResult {
    LaunchApp(Application),
    InvokeAction(ActionDescriptor),
    CopyClipboard(ClipboardItem),
    CopyCalculation(String),
    ExecuteCommand(String),
    OpenWeb(String),
    OpenPath(PathBuf),
    OpenUri(String),
    CopyKeybinding(String),
    Handled {
        close_overview: bool,
    },
    InvokeExtension {
        canonical: shilpo_ext_api::CanonicalId,
        payload: String,
    },
}

/// Error type returned during search query execution or item activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    ProviderError(ProviderId, String),
    NotFound(String),
    ActivationFailed(String),
    Cancelled,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderError(id, msg) => write!(f, "Provider '{id}' error: {msg}"),
            Self::NotFound(item) => write!(f, "Item not found for activation: {item}"),
            Self::ActivationFailed(msg) => write!(f, "Activation failed: {msg}"),
            Self::Cancelled => write!(f, "Operation cancelled"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Search candidate emitted by a [`SearchProvider`] into a [`SearchSink`].
#[derive(Debug, Clone)]
pub struct SearchCandidate {
    pub provider_id: ProviderId,
    pub canonical_id: String,
    pub generation: u64,
    pub title: String,
    pub subtitle: Option<String>,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
    pub category: ResultCategory,
    pub latency: LatencyClass,
    pub completion: CompletionState,
    pub icon: SearchResultIcon,
    pub activation_verb: String,
    pub match_positions: Vec<usize>,
    pub activation: SearchActivation,
}

impl SearchCandidate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_id: ProviderId,
        canonical_id: impl Into<String>,
        generation: u64,
        title: impl Into<String>,
        subtitle: Option<String>,
        category: ResultCategory,
        icon: SearchResultIcon,
        activation_verb: impl Into<String>,
        activation: SearchActivation,
    ) -> Self {
        Self {
            provider_id,
            canonical_id: canonical_id.into(),
            generation,
            title: title.into(),
            subtitle,
            aliases: Vec::new(),
            keywords: Vec::new(),
            category,
            latency: LatencyClass::Instant,
            completion: CompletionState::Complete,
            icon,
            activation_verb: activation_verb.into(),
            match_positions: Vec::new(),
            activation,
        }
    }
}

/// Core search provider trait implemented by individual query sources.
pub trait SearchProvider: Send + Sync {
    /// Returns the unique identity of this provider.
    fn id(&self) -> ProviderId;

    /// Returns the search modes supported by this provider.
    ///
    /// The coordinator inspects declared modes before dispatching a query and
    /// only spawns search workers for providers declaring support for `request.mode`.
    fn declared_modes(&self) -> Cow<'static, [SearchMode]> {
        Cow::Borrowed(&[SearchMode::Default])
    }

    /// Returns the prefix icon for a specific search mode, if declared.
    fn prefix_icon(&self, _mode: SearchMode) -> Option<IconName> {
        None
    }

    /// Returns the maximum number of candidates this provider's scratch sink accepts
    /// before ranking. Untrusted providers (e.g. extensions) should override this with
    /// a much smaller bound than the default, so a hostile or buggy provider cannot
    /// force the ranker to score thousands of candidates per keystroke.
    fn scratch_capacity(&self) -> usize {
        DEFAULT_SCRATCH_CAPACITY
    }

    /// Executes search and streams candidates into the provided sink.
    fn search(&self, request: SearchRequest, sink: SearchSink);

    /// Activates a search candidate previously emitted by this provider.
    fn activate(&self, activation: SearchActivation) -> Result<ActionResult, SearchError>;
}
