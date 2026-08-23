pub mod action_provider;
mod activation_cache;
pub mod app_provider;
pub mod calculator;
pub mod calculator_provider;
pub mod clipboard_provider;
pub mod coordinator;
pub mod extension_provider;
pub mod learning;
pub mod matcher;
pub mod parser;
pub mod quicklinks_provider;
pub mod ranker;
pub mod ranking;
pub mod sink;
pub mod types;
pub mod window_provider;

pub use action_provider::ActionSearchProvider;
pub use app_provider::AppSearchProvider;
pub use calculator_provider::CalculatorSearchProvider;
pub use clipboard_provider::ClipboardSearchProvider;
pub use coordinator::{
    DEFAULT_MAX_IN_FLIGHT_PER_PROVIDER, DEFAULT_PER_QUERY_BUDGET, SearchBudget, SearchCoordinator,
    SearchSummary,
};
pub use extension_provider::{
    DEFAULT_EXTENSION_SEARCH_BUDGET, EXTENSION_MAX_CANDIDATES, ExtensionSearchProvider,
    ExtensionSearchRunner, derive_guest_deadline, host_namespace_candidate_id, parse_named_icon,
    sanitize_relative_asset_path,
};
pub use learning::{
    DEFAULT_HALF_LIFE_SECS, HeedSearchLearningStore, LearningClock, MAX_INFLUENCE_BOOST,
    MAX_LEARNING_ENTRIES, NoopSearchLearningStore, SearchLearningStore, SystemLearningClock,
    TestLearningClock,
};
pub use matcher::{MatchResult, fuzzy_match, fuzzy_score};
pub use parser::SearchMode;
pub use quicklinks_provider::QuicklinksSearchProvider;
pub use ranker::{RankerConfig, rank};
pub use sink::{SearchSink, SinkConfig};
pub use types::{
    ActionResult, CompletionState, LatencyClass, ProviderId, ResultCategory, SearchActivation,
    SearchCandidate, SearchError, SearchProvider, SearchRequest, SearchResultIcon,
};
pub use window_provider::WindowSearchProvider;
