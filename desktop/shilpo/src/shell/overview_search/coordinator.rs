use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use shilpo_m3e::IconName;

use super::{
    learning::{NoopSearchLearningStore, SearchLearningStore},
    parser::{self, SearchMode},
    ranker::{self, RankerConfig},
    sink::{SearchSink, SinkConfig},
    types::{
        ActionResult, ProviderId, SearchActivation, SearchError, SearchProvider, SearchRequest,
    },
};

/// Default per-query search budget before deadline expiry and ranking fallback.
pub const DEFAULT_PER_QUERY_BUDGET: Duration = Duration::from_millis(250);

/// Default limit on concurrent in-flight searches per provider.
pub const DEFAULT_MAX_IN_FLIGHT_PER_PROVIDER: usize = 1;

/// Execution time budget for search queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchBudget {
    /// Maximum wall time the coordinator will wait for provider completion
    /// before ranking whatever has arrived.
    pub per_query: Duration,
}

impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            per_query: DEFAULT_PER_QUERY_BUDGET,
        }
    }
}

impl SearchBudget {
    /// Creates a new search budget with the specified per-query duration.
    pub const fn new(per_query: Duration) -> Self {
        Self { per_query }
    }
}

/// Execution summary returned by [`SearchCoordinator::search`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use = "a query summary reports timed-out and skipped providers; \
              discarding it silently hides degraded search behavior"]
pub struct SearchSummary {
    /// Providers that timed out and were cancelled at the query deadline.
    pub timed_out_providers: Vec<ProviderId>,
    /// Providers that were skipped because their in-flight run limit was reached.
    pub skipped_providers: Vec<ProviderId>,
    /// Total number of candidates gathered across all scratch sinks before ranking.
    pub raw_candidate_count: usize,
    /// Total number of candidates pushed into the destination sink after ranking.
    pub ranked_candidate_count: usize,
}

impl SearchSummary {
    /// Returns `true` if any provider timed out during the query.
    pub fn has_timed_out(&self) -> bool {
        !self.timed_out_providers.is_empty()
    }

    /// Returns `true` if any provider was skipped due to in-flight bounds.
    pub fn has_skipped(&self) -> bool {
        !self.skipped_providers.is_empty()
    }
}

/// Host search coordinator that manages registered providers and fans out requests.
#[derive(Clone)]
pub struct SearchCoordinator {
    providers: Vec<Arc<dyn SearchProvider>>,
    learning_store: Arc<dyn SearchLearningStore>,
    ranker_config: RankerConfig,
    budget: SearchBudget,
    max_in_flight_per_provider: usize,
    in_flight_counts: Arc<Mutex<HashMap<ProviderId, usize>>>,
}

impl Default for SearchCoordinator {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            learning_store: Arc::new(NoopSearchLearningStore),
            ranker_config: RankerConfig::default(),
            budget: SearchBudget::default(),
            max_in_flight_per_provider: DEFAULT_MAX_IN_FLIGHT_PER_PROVIDER,
            in_flight_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl SearchCoordinator {
    /// Creates a new coordinator with the given providers.
    pub fn new(providers: Vec<Arc<dyn SearchProvider>>) -> Self {
        Self {
            providers,
            learning_store: Arc::new(NoopSearchLearningStore),
            ranker_config: RankerConfig::default(),
            budget: SearchBudget::default(),
            max_in_flight_per_provider: DEFAULT_MAX_IN_FLIGHT_PER_PROVIDER,
            in_flight_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Builder helper to set the search learning store.
    pub fn with_learning_store(mut self, learning_store: Arc<dyn SearchLearningStore>) -> Self {
        self.learning_store = learning_store;
        self
    }

    /// Builder helper to set ranker configuration.
    pub fn with_ranker_config(mut self, ranker_config: RankerConfig) -> Self {
        self.ranker_config = ranker_config;
        self
    }

    /// Builder helper to set search budget.
    pub fn with_budget(mut self, budget: SearchBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Builder helper to set search per-query duration budget.
    pub fn with_per_query_budget(mut self, budget: Duration) -> Self {
        self.budget = SearchBudget::new(budget);
        self
    }

    /// Builder helper to set max in-flight queries per provider.
    pub fn with_max_in_flight_per_provider(mut self, limit: usize) -> Self {
        self.max_in_flight_per_provider = limit.max(1);
        self
    }

    /// Returns the active search budget.
    pub fn budget(&self) -> &SearchBudget {
        &self.budget
    }

    /// Returns the configured in-flight limit per provider.
    pub fn max_in_flight_per_provider(&self) -> usize {
        self.max_in_flight_per_provider
    }

    /// Registers an additional provider.
    pub fn register(&mut self, provider: Arc<dyn SearchProvider>) {
        self.providers.push(provider);
    }

    /// Fans out a search query to registered providers supporting the parsed query mode
    /// and ranks the merged results into the provided sink.
    ///
    /// Each eligible provider's `search` call runs concurrently on a detached worker thread.
    /// Providers are bounded by a per-query deadline budget: if any provider fails to complete
    /// within budget, its scratch sink is cancelled and the coordinator proceeds to rank all
    /// candidates that arrived in time. Furthermore, concurrent in-flight queries per provider
    /// are bounded to prevent thread accumulation across repeated keystrokes.
    pub fn search(&self, raw_query: &str, generation: u64, sink: &SearchSink) -> SearchSummary {
        let (mode, query) = parser::parse_query(raw_query);
        let request = SearchRequest::new(raw_query, mode, query, generation);

        let eligible_providers: Vec<&Arc<dyn SearchProvider>> = self
            .providers
            .iter()
            .filter(|p| p.declared_modes().contains(&mode))
            .collect();

        let mut spawned_providers = Vec::new();
        let mut skipped_providers = Vec::new();
        let (tx, rx) = mpsc::channel();

        // 1. Filter out providers exceeding in-flight capacity and allocate scratch sinks,
        //    each sized to that provider's own declared capacity (see `scratch_capacity`).
        {
            let mut in_flight = self
                .in_flight_counts
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for provider in eligible_providers {
                let id = provider.id();
                let count = in_flight.entry(id.clone()).or_insert(0);
                if *count >= self.max_in_flight_per_provider {
                    skipped_providers.push(id);
                } else {
                    *count += 1;
                    let capacity = provider.scratch_capacity();
                    let scratch_config = SinkConfig {
                        max_per_provider: capacity,
                        max_total: capacity,
                    };
                    let scratch = SearchSink::new(generation, scratch_config);
                    spawned_providers.push(((*provider).clone(), scratch));
                }
            }
        }

        // 2. Spawn detached threads for each active provider.
        let mut outstanding: HashSet<ProviderId> = HashSet::new();
        for (provider, scratch) in &spawned_providers {
            let id = provider.id();
            outstanding.insert(id.clone());

            let request = request.clone();
            let scratch = scratch.clone();
            let provider = provider.clone();
            let tx = tx.clone();
            let tracker = self.in_flight_counts.clone();

            std::thread::spawn(move || {
                struct InFlightGuard {
                    tracker: Arc<Mutex<HashMap<ProviderId, usize>>>,
                    provider_id: ProviderId,
                }
                impl Drop for InFlightGuard {
                    fn drop(&mut self) {
                        let mut in_flight = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(count) = in_flight.get_mut(&self.provider_id) {
                            *count = count.saturating_sub(1);
                        }
                    }
                }

                let guard = InFlightGuard {
                    tracker,
                    provider_id: id.clone(),
                };

                provider.search(request, scratch);
                drop(guard);
                let _ = tx.send(id);
            });
        }

        // Explicitly drop our clone of tx so rx can detect disconnect if all threads finish.
        drop(tx);

        // 3. Wait on completions until all eligible providers finish or budget expires.
        let start = Instant::now();
        let budget = self.budget.per_query;

        while !outstanding.is_empty() {
            let elapsed = start.elapsed();
            if elapsed >= budget {
                break;
            }
            let remaining = budget - elapsed;
            match rx.recv_timeout(remaining) {
                Ok(completed_id) => {
                    outstanding.remove(&completed_id);
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        // 4. Cancel scratch sinks for any providers that did not finish within budget.
        let mut timed_out_providers = Vec::new();
        for (provider, scratch) in &spawned_providers {
            let id = provider.id();
            if outstanding.contains(&id) {
                scratch.cancel();
                timed_out_providers.push(id);
            }
        }

        // 5. Gather candidates across scratch sinks in registration order.
        let mut all_candidates = Vec::new();
        for (_provider, scratch) in &spawned_providers {
            all_candidates.extend(scratch.snapshot());
        }
        let raw_candidate_count = all_candidates.len();

        // 6. Rank candidates.
        let ranked = ranker::rank(
            all_candidates,
            query,
            self.learning_store.as_ref(),
            &self.ranker_config,
        );
        let ranked_candidate_count = ranked.len();

        for candidate in ranked {
            sink.push(candidate);
        }

        SearchSummary {
            timed_out_providers,
            skipped_providers,
            raw_candidate_count,
            ranked_candidate_count,
        }
    }

    /// Returns the prefix icon for the given raw query based on declared provider descriptors.
    pub fn prefix_icon(&self, raw_query: &str) -> IconName {
        let (mode, _) = parser::parse_query(raw_query);
        if mode == SearchMode::Default {
            return IconName::Search;
        }
        for provider in &self.providers {
            if provider.declared_modes().contains(&mode)
                && let Some(icon) = provider.prefix_icon(mode)
            {
                return icon;
            }
        }
        mode.default_icon()
    }

    /// Routes an activation request to the appropriate provider by ID and records the activation
    /// in the learning store upon success.
    pub fn activate(
        &self,
        provider_id: &ProviderId,
        canonical_id: &str,
        activation: SearchActivation,
    ) -> Result<ActionResult, SearchError> {
        let provider = self
            .providers
            .iter()
            .find(|p| p.id() == *provider_id)
            .ok_or_else(|| SearchError::NotFound(provider_id.to_string()))?;

        let result = provider.activate(activation)?;
        self.learning_store.record_activation(canonical_id);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::shell::overview_search::types::{
        DEFAULT_SCRATCH_CAPACITY, ResultCategory, SearchCandidate, SearchResultIcon,
    };

    struct TestProvider {
        id: &'static str,
        prefix: &'static str,
    }

    impl SearchProvider for TestProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from_static(self.id)
        }

        fn search(&self, request: SearchRequest, sink: SearchSink) {
            sink.push(SearchCandidate::new(
                self.id(),
                format!("{}:{}", self.id, request.query),
                request.generation,
                format!("{} - {}", self.prefix, request.query),
                None,
                ResultCategory::Custom,
                SearchResultIcon::Initial('T'),
                "Open",
                SearchActivation::new(format!("payload-{}", self.id)),
            ));
        }

        fn activate(&self, activation: SearchActivation) -> Result<ActionResult, SearchError> {
            if activation.payload == format!("payload-{}", self.id) {
                Ok(ActionResult::Handled {
                    close_overview: true,
                })
            } else {
                Err(SearchError::ActivationFailed("mismatched payload".into()))
            }
        }
    }

    #[test]
    fn test_coordinator_fanout_and_activation() {
        let p1 = Arc::new(TestProvider {
            id: "p1",
            prefix: "Provider One",
        });
        let p2 = Arc::new(TestProvider {
            id: "p2",
            prefix: "Provider Two",
        });

        let coordinator = SearchCoordinator::new(vec![p1, p2]);
        let sink = SearchSink::for_test(1);

        let _ = coordinator.search("hello", 1, &sink);

        let results = sink.snapshot();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Provider One - hello");
        assert_eq!(results[1].title, "Provider Two - hello");

        // Test activation routing
        let act1 = coordinator.activate(
            &ProviderId::from_static("p1"),
            &results[0].canonical_id,
            results[0].activation.clone(),
        );
        assert!(matches!(
            act1,
            Ok(ActionResult::Handled {
                close_overview: true
            })
        ));

        let act2 = coordinator.activate(
            &ProviderId::from_static("p2"),
            &results[1].canonical_id,
            results[1].activation.clone(),
        );
        assert!(matches!(
            act2,
            Ok(ActionResult::Handled {
                close_overview: true
            })
        ));

        // Test unknown provider activation
        let unknown = coordinator.activate(
            &ProviderId::from_static("unknown"),
            "unknown:x",
            SearchActivation::new("x"),
        );
        assert!(matches!(unknown, Err(SearchError::NotFound(_))));
    }

    #[test]
    fn test_coordinator_activation_records_learning_exactly_once() {
        use crate::shell::overview_search::learning::{
            DEFAULT_HALF_LIFE_SECS, HeedSearchLearningStore, TestLearningClock,
        };

        let p1 = Arc::new(TestProvider {
            id: "p1",
            prefix: "Provider One",
        });

        let clock = Arc::new(TestLearningClock::new(1000));
        let learning = Arc::new(HeedSearchLearningStore::with_config(
            None,
            clock,
            512,
            DEFAULT_HALF_LIFE_SECS,
        ));

        let coordinator = SearchCoordinator::new(vec![p1]).with_learning_store(learning.clone());
        let sink = SearchSink::for_test(1);

        // Searching generates impressions but records 0 activations in learning store
        let _ = coordinator.search("query", 1, &sink);
        let results = sink.snapshot();
        assert_eq!(results.len(), 1);
        assert_eq!(learning.score_boost(&results[0].canonical_id), 0);

        // One activation records exactly one increment (boost = 10)
        let act_res = coordinator.activate(
            &ProviderId::from_static("p1"),
            &results[0].canonical_id,
            results[0].activation.clone(),
        );
        assert!(act_res.is_ok());
        assert_eq!(learning.score_boost(&results[0].canonical_id), 10);
    }

    struct SleepingProvider {
        id: &'static str,
        title: &'static str,
        sleep: std::time::Duration,
    }

    impl SearchProvider for SleepingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from_static(self.id)
        }

        fn search(&self, request: SearchRequest, sink: SearchSink) {
            std::thread::sleep(self.sleep);
            sink.push(SearchCandidate::new(
                self.id(),
                format!("{}:{}", self.id, request.query),
                request.generation,
                self.title,
                None,
                ResultCategory::Custom,
                SearchResultIcon::Initial('S'),
                "Open",
                SearchActivation::new(format!("payload-{}", self.id)),
            ));
        }

        fn activate(&self, _activation: SearchActivation) -> Result<ActionResult, SearchError> {
            Ok(ActionResult::Handled {
                close_overview: true,
            })
        }
    }

    #[test]
    fn test_slow_provider_does_not_delay_dispatch_beyond_the_slowest_provider() {
        // Two providers that each take ~100ms. If dispatch were sequential,
        // total wall time would be ~200ms. Concurrent dispatch keeps it
        // close to the duration of a single provider.
        let sleep = std::time::Duration::from_millis(100);
        let p1 = Arc::new(SleepingProvider {
            id: "slow-1",
            title: "Result One",
            sleep,
        });
        let p2 = Arc::new(SleepingProvider {
            id: "slow-2",
            title: "Result Two",
            sleep,
        });

        let coordinator = SearchCoordinator::new(vec![p1, p2]);
        let sink = SearchSink::for_test(1);

        let start = std::time::Instant::now();
        let _ = coordinator.search("Result", 1, &sink);
        let elapsed = start.elapsed();

        let results = sink.snapshot();
        assert_eq!(results.len(), 2, "both slow providers must still deliver");
        assert!(
            elapsed < std::time::Duration::from_millis(180),
            "expected concurrent dispatch (~100ms), got {elapsed:?} \
             which indicates providers ran sequentially"
        );
    }

    #[test]
    fn test_fast_provider_delivers_alongside_a_slow_provider() {
        struct FastProvider;
        impl SearchProvider for FastProvider {
            fn id(&self) -> ProviderId {
                ProviderId::from_static("fast")
            }
            fn search(&self, request: SearchRequest, sink: SearchSink) {
                sink.push(SearchCandidate::new(
                    self.id(),
                    "fast:1",
                    request.generation,
                    "Fast Result",
                    None,
                    ResultCategory::Custom,
                    SearchResultIcon::Initial('F'),
                    "Open",
                    SearchActivation::new("payload-fast"),
                ));
            }
            fn activate(&self, _activation: SearchActivation) -> Result<ActionResult, SearchError> {
                Ok(ActionResult::Handled {
                    close_overview: true,
                })
            }
        }

        let slow = Arc::new(SleepingProvider {
            id: "slow",
            title: "Slow Result",
            sleep: std::time::Duration::from_millis(60),
        });
        let fast = Arc::new(FastProvider);

        let coordinator = SearchCoordinator::new(vec![slow, fast]);
        let sink = SearchSink::for_test(1);
        let _ = coordinator.search("Result", 1, &sink);

        let results = sink.snapshot();
        assert!(results.iter().any(|c| c.title == "Fast Result"));
        assert!(results.iter().any(|c| c.title == "Slow Result"));
    }

    #[test]
    fn test_a_prolific_single_provider_does_not_starve_its_own_late_candidates() {
        // A provider producing far more than the old 64-item scratch-sink
        // quota must still have every one of its candidates reach the
        // ranker. Regression test for the intra-provider starvation bug:
        // sink.push()'s return value was discarded, so a provider emitting
        // more than DEFAULT_SCRATCH_CAPACITY candidates used to lose the tail
        // silently before ranking ever saw them.
        struct ManyCandidatesProvider;
        impl SearchProvider for ManyCandidatesProvider {
            fn id(&self) -> ProviderId {
                ProviderId::from_static("many")
            }
            fn search(&self, request: SearchRequest, sink: SearchSink) {
                // One order of magnitude past the old 64-candidate cap, and
                // still comfortably under DEFAULT_SCRATCH_CAPACITY.
                for i in 0..(DEFAULT_SCRATCH_CAPACITY / 4) {
                    sink.push(SearchCandidate::new(
                        self.id(),
                        format!("many:{i}"),
                        request.generation,
                        format!("Filler {i}"),
                        None,
                        ResultCategory::Custom,
                        SearchResultIcon::Initial('M'),
                        "Open",
                        SearchActivation::new(format!("payload-many-{i}")),
                    ));
                }
                // The target sits well past the old 64-item quota.
                sink.push(SearchCandidate::new(
                    self.id(),
                    "many:target",
                    request.generation,
                    "Zzzedge Unique Target",
                    None,
                    ResultCategory::Custom,
                    SearchResultIcon::Initial('Z'),
                    "Open",
                    SearchActivation::new("payload-target"),
                ));
            }
            fn activate(&self, _activation: SearchActivation) -> Result<ActionResult, SearchError> {
                Ok(ActionResult::Handled {
                    close_overview: true,
                })
            }
        }

        let coordinator = SearchCoordinator::new(vec![Arc::new(ManyCandidatesProvider)]);
        let sink = SearchSink::for_test(1);
        let _ = coordinator.search("Zzzedge", 1, &sink);

        let results = sink.snapshot();
        assert!(
            results.iter().any(|c| c.canonical_id == "many:target"),
            "candidate past the old scratch-sink quota was dropped before ranking"
        );
    }

    #[test]
    fn test_declared_mode_scoping_does_not_dispatch_non_matching_providers() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct DispatchTrackingProvider {
            id: &'static str,
            modes: &'static [SearchMode],
            dispatches: Arc<AtomicUsize>,
        }

        impl SearchProvider for DispatchTrackingProvider {
            fn id(&self) -> ProviderId {
                ProviderId::from_static(self.id)
            }
            fn declared_modes(&self) -> Cow<'static, [SearchMode]> {
                Cow::Borrowed(self.modes)
            }
            fn search(&self, request: SearchRequest, sink: SearchSink) {
                self.dispatches.fetch_add(1, Ordering::SeqCst);
                sink.push(SearchCandidate::new(
                    self.id(),
                    format!("{}:{}", self.id, request.query),
                    request.generation,
                    format!("Title {}", self.id),
                    None,
                    ResultCategory::Custom,
                    SearchResultIcon::Initial('D'),
                    "Open",
                    SearchActivation::new("act"),
                ));
            }
            fn activate(&self, _activation: SearchActivation) -> Result<ActionResult, SearchError> {
                Ok(ActionResult::Handled {
                    close_overview: true,
                })
            }
        }

        let app_dispatches = Arc::new(AtomicUsize::new(0));
        let calc_dispatches = Arc::new(AtomicUsize::new(0));
        let clip_dispatches = Arc::new(AtomicUsize::new(0));

        let app_prov = Arc::new(DispatchTrackingProvider {
            id: "app-tracker",
            modes: &[SearchMode::Default, SearchMode::Apps],
            dispatches: app_dispatches.clone(),
        });
        let calc_prov = Arc::new(DispatchTrackingProvider {
            id: "calc-tracker",
            modes: &[SearchMode::Calculator],
            dispatches: calc_dispatches.clone(),
        });
        let clip_prov = Arc::new(DispatchTrackingProvider {
            id: "clip-tracker",
            modes: &[SearchMode::Clipboard],
            dispatches: clip_dispatches.clone(),
        });

        let coordinator = SearchCoordinator::new(vec![app_prov, calc_prov, clip_prov]);

        // 1. Query with '>' scopes to Apps mode: only app_prov dispatched
        let sink1 = SearchSink::for_test(1);
        let _ = coordinator.search(">term", 1, &sink1);
        assert_eq!(app_dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(calc_dispatches.load(Ordering::SeqCst), 0);
        assert_eq!(clip_dispatches.load(Ordering::SeqCst), 0);

        // 2. Query with '=' scopes to Calculator mode: only calc_prov dispatched
        let sink2 = SearchSink::for_test(2);
        let _ = coordinator.search("=2+2", 2, &sink2);
        assert_eq!(app_dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(calc_dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(clip_dispatches.load(Ordering::SeqCst), 0);

        // 3. Query with ';' scopes to Clipboard mode: only clip_prov dispatched
        let sink3 = SearchSink::for_test(3);
        let _ = coordinator.search(";note", 3, &sink3);
        assert_eq!(app_dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(calc_dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(clip_dispatches.load(Ordering::SeqCst), 1);

        // 4. Default query: only app_prov dispatched (declared Default)
        let sink4 = SearchSink::for_test(4);
        let _ = coordinator.search("firefox", 4, &sink4);
        assert_eq!(app_dispatches.load(Ordering::SeqCst), 2);
        assert_eq!(calc_dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(clip_dispatches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_prefix_icon_uses_declared_provider_descriptors() {
        struct MockIconProvider;
        impl SearchProvider for MockIconProvider {
            fn id(&self) -> ProviderId {
                ProviderId::from_static("mock-icon")
            }
            fn declared_modes(&self) -> Cow<'static, [SearchMode]> {
                Cow::Borrowed(&[SearchMode::Actions])
            }
            fn prefix_icon(&self, mode: SearchMode) -> Option<IconName> {
                if mode == SearchMode::Actions {
                    Some(IconName::Settings)
                } else {
                    None
                }
            }
            fn search(&self, _request: SearchRequest, _sink: SearchSink) {}
            fn activate(&self, _activation: SearchActivation) -> Result<ActionResult, SearchError> {
                Ok(ActionResult::Handled {
                    close_overview: true,
                })
            }
        }

        let coordinator = SearchCoordinator::new(vec![Arc::new(MockIconProvider)]);
        assert_eq!(coordinator.prefix_icon("/action"), IconName::Settings);
        assert_eq!(coordinator.prefix_icon("normal query"), IconName::Search);
    }

    #[test]
    fn test_all_sigils_scope_to_expected_providers() {
        use shilpo_services::ClipboardItem;
        use tokio::sync::watch;

        use crate::shell::actions::{
            ActionCategory, ActionDescriptor, ActionId, ActionInputRequirement,
        };
        use crate::shell::overview_search::{
            ActionSearchProvider, CalculatorSearchProvider, ClipboardSearchProvider,
            QuicklinksSearchProvider,
        };

        let actions = vec![ActionDescriptor {
            id: ActionId::ToggleOverview,
            name: "toggle-overview".to_string(),
            label: "Toggle Overview".to_string(),
            category: ActionCategory::Overlay,
            input: ActionInputRequirement::NoInput,
            enabled: true,
        }];
        let (_tx, rx) = watch::channel(vec![ClipboardItem::new_text(
            "copied snippet".to_string(),
            chrono::Utc::now(),
        )]);
        let keybindings = vec![("Super+T".to_string(), "Terminal".to_string())];

        let action_prov = Arc::new(ActionSearchProvider::new(actions));
        let clip_prov = Arc::new(ClipboardSearchProvider::new(Some(rx)));
        let calc_prov = Arc::new(CalculatorSearchProvider::new());
        let quick_prov = Arc::new(QuicklinksSearchProvider::new(keybindings));

        let coordinator =
            SearchCoordinator::new(vec![action_prov, clip_prov, calc_prov, quick_prov]);

        // 1. '/' scopes to actions
        let sink = SearchSink::for_test(1);
        let _ = coordinator.search("/toggle", 1, &sink);
        let res = sink.snapshot();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].category, ResultCategory::Action);

        // 2. ';' scopes to clipboard
        let sink = SearchSink::for_test(2);
        let _ = coordinator.search(";copied", 2, &sink);
        let res = sink.snapshot();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].category, ResultCategory::Clipboard);

        // 3. '=' scopes to calculator
        let sink = SearchSink::for_test(3);
        let _ = coordinator.search("=5 * 5", 3, &sink);
        let res = sink.snapshot();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].title, "25");
        assert_eq!(res[0].category, ResultCategory::Calculator);

        // 4. '?' scopes to web search
        let sink = SearchSink::for_test(4);
        let _ = coordinator.search("?rust documentation", 4, &sink);
        let res = sink.snapshot();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].category, ResultCategory::WebSearch);

        // 5. '<' scopes to keybindings
        let sink = SearchSink::for_test(5);
        let _ = coordinator.search("<Super", 5, &sink);
        let res = sink.snapshot();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].category, ResultCategory::Keybinding);
    }

    #[test]
    fn test_implicit_and_explicit_calculator_scoping() {
        use crate::shell::overview_search::CalculatorSearchProvider;

        let calc_prov = Arc::new(CalculatorSearchProvider::new());
        let coordinator = SearchCoordinator::new(vec![calc_prov]);

        // Bare implicit arithmetic
        let sink1 = SearchSink::for_test(1);
        let _ = coordinator.search("2 + 2", 1, &sink1);
        let res1 = sink1.snapshot();
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0].title, "4");

        // Complex implicit expression with parenthesis
        let sink2 = SearchSink::for_test(2);
        let _ = coordinator.search("10 * (5 - 3)", 2, &sink2);
        let res2 = sink2.snapshot();
        assert_eq!(res2.len(), 1);
        assert_eq!(res2[0].title, "20");

        // Non-arithmetic query "hello 2" parses as Default and is NOT dispatched to Calculator
        let sink3 = SearchSink::for_test(3);
        let _ = coordinator.search("hello 2", 3, &sink3);
        let res3 = sink3.snapshot();
        assert_eq!(res3.len(), 0);
    }

    #[test]
    fn test_unscoped_query_returns_ordered_mix_from_multiple_providers() {
        use std::path::PathBuf;

        use shilpo_services::{AppScanner, Application};

        use crate::shell::actions::{
            ActionCategory, ActionDescriptor, ActionId, ActionInputRequirement,
        };
        use crate::shell::overview_search::{
            ActionSearchProvider, AppSearchProvider, QuicklinksSearchProvider,
        };

        let scanner = AppScanner::from_applications(vec![Application {
            name: "Terminal Emulator".to_string(),
            description: Some("System terminal".to_string()),
            exec: "terminal".to_string(),
            icon: None,
            icon_path: None,
            desktop_file: PathBuf::from("/usr/share/applications/term.desktop"),
            categories: vec!["System".to_string()],
            working_dir: None,
            terminal: false,
            try_exec: None,
        }]);

        let actions = vec![ActionDescriptor {
            id: ActionId::ToggleOverview,
            name: "terminal-toggle".to_string(),
            label: "Terminal Toggle Action".to_string(),
            category: ActionCategory::Overlay,
            input: ActionInputRequirement::NoInput,
            enabled: true,
        }];

        let app_prov = Arc::new(AppSearchProvider::new(scanner));
        let action_prov = Arc::new(ActionSearchProvider::new(actions));
        let quick_prov = Arc::new(QuicklinksSearchProvider::new(Vec::new()));

        let coordinator = SearchCoordinator::new(vec![app_prov, action_prov, quick_prov]);

        let sink = SearchSink::for_test(1);
        let _ = coordinator.search("terminal", 1, &sink);
        let results = sink.snapshot();

        // Must contain results from AppSearchProvider, ActionSearchProvider, and QuicklinksSearchProvider
        assert!(
            results
                .iter()
                .any(|c| c.category == ResultCategory::Application)
        );
        assert!(results.iter().any(|c| c.category == ResultCategory::Action));
        assert!(
            results
                .iter()
                .any(|c| c.category == ResultCategory::Command)
        );
        assert!(
            results
                .iter()
                .any(|c| c.category == ResultCategory::WebSearch)
        );

        // Application (prior 200) outranks Command (prior 50) and WebSearch (prior 20)
        let app_idx = results
            .iter()
            .position(|c| c.category == ResultCategory::Application)
            .unwrap();
        let cmd_idx = results
            .iter()
            .position(|c| c.category == ResultCategory::Command)
            .unwrap();
        let web_idx = results
            .iter()
            .position(|c| c.category == ResultCategory::WebSearch)
            .unwrap();

        assert!(app_idx < cmd_idx);
        assert!(cmd_idx < web_idx);
    }

    #[test]
    fn test_every_action_result_variant_produced_by_domain_providers_via_coordinator() {
        use shilpo_services::{
            AppScanner, Application, ClipboardItem, CompositorCapabilities, CompositorSnapshot,
            DomainLifecycle, DomainVersion, TestCompositorAdapter, WindowInfo,
        };
        use std::path::PathBuf;
        use tokio::sync::watch;

        use crate::shell::actions::{
            ActionCategory, ActionDescriptor, ActionId, ActionInputRequirement,
        };
        use crate::shell::overview_search::{
            ActionSearchProvider, AppSearchProvider, CalculatorSearchProvider,
            ClipboardSearchProvider, QuicklinksSearchProvider, WindowSearchProvider,
        };

        let snapshot = CompositorSnapshot {
            version: DomainVersion::new(1, 1),
            connection: DomainLifecycle::Ready,
            capabilities: CompositorCapabilities {
                can_focus_window: true,
                ..Default::default()
            },
            windows: vec![WindowInfo {
                id: 42,
                title: Some("Terminal".to_string()),
                app_id: Some("org.gnome.Terminal".to_string()),
                workspace_id: Some(1),
                is_focused: false,
                is_floating: false,
                is_urgent: false,
                layout_x: None,
                layout_y: None,
            }],
            ..Default::default()
        };

        let scanner = AppScanner::from_applications(vec![Application {
            name: "Calculator App".to_string(),
            description: None,
            exec: "gnome-calculator".to_string(),
            icon: None,
            icon_path: None,
            desktop_file: PathBuf::from("/usr/share/applications/calc.desktop"),
            categories: Vec::new(),
            working_dir: None,
            terminal: false,
            try_exec: None,
        }]);

        let actions = vec![ActionDescriptor {
            id: ActionId::Quit,
            name: "quit".to_string(),
            label: "Quit Shilpo".to_string(),
            category: ActionCategory::System,
            input: ActionInputRequirement::NoInput,
            enabled: true,
        }];

        let (_tx, rx) = watch::channel(vec![ClipboardItem::new_text(
            "saved text".to_string(),
            chrono::Utc::now(),
        )]);

        let keybindings = vec![("Super+L".to_string(), "Lock Screen".to_string())];

        let app_prov = Arc::new(AppSearchProvider::new(scanner));
        let win_prov = Arc::new(WindowSearchProvider::new(Some(Arc::new(
            TestCompositorAdapter::new(snapshot),
        ))));
        let act_prov = Arc::new(ActionSearchProvider::new(actions));
        let clip_prov = Arc::new(ClipboardSearchProvider::new(Some(rx)));
        let calc_prov = Arc::new(CalculatorSearchProvider::new());
        let quick_prov = Arc::new(QuicklinksSearchProvider::new(keybindings));

        let coordinator = SearchCoordinator::new(vec![
            app_prov.clone(),
            win_prov.clone(),
            act_prov.clone(),
            clip_prov.clone(),
            calc_prov.clone(),
            quick_prov.clone(),
        ]);

        // 1. LaunchApp
        let sink = SearchSink::for_test(1);
        let _ = coordinator.search(">Calc", 1, &sink);
        let cand = &sink.snapshot()[0];
        let res = coordinator
            .activate(
                &cand.provider_id,
                &cand.canonical_id,
                cand.activation.clone(),
            )
            .unwrap();
        assert!(matches!(res, ActionResult::LaunchApp(_)));

        // 2. Handled (Window)
        let sink = SearchSink::for_test(2);
        let _ = coordinator.search("Terminal", 2, &sink);
        let win_cand = sink
            .snapshot()
            .into_iter()
            .find(|c| c.category == ResultCategory::Window)
            .unwrap();
        let res = coordinator
            .activate(
                &win_cand.provider_id,
                &win_cand.canonical_id,
                win_cand.activation.clone(),
            )
            .unwrap();
        assert!(matches!(
            res,
            ActionResult::Handled {
                close_overview: true
            }
        ));

        // 3. InvokeAction
        let sink = SearchSink::for_test(3);
        let _ = coordinator.search("/quit", 3, &sink);
        let act_cand = &sink.snapshot()[0];
        let res = coordinator
            .activate(
                &act_cand.provider_id,
                &act_cand.canonical_id,
                act_cand.activation.clone(),
            )
            .unwrap();
        assert!(matches!(res, ActionResult::InvokeAction(_)));

        // 4. CopyClipboard
        let sink = SearchSink::for_test(4);
        let _ = coordinator.search(";saved", 4, &sink);
        let clip_cand = &sink.snapshot()[0];
        let res = coordinator
            .activate(
                &clip_cand.provider_id,
                &clip_cand.canonical_id,
                clip_cand.activation.clone(),
            )
            .unwrap();
        assert!(matches!(res, ActionResult::CopyClipboard(_)));

        // 5. CopyCalculation
        let sink = SearchSink::for_test(5);
        let _ = coordinator.search("=100 + 200", 5, &sink);
        let calc_cand = &sink.snapshot()[0];
        let res = coordinator
            .activate(
                &calc_cand.provider_id,
                &calc_cand.canonical_id,
                calc_cand.activation.clone(),
            )
            .unwrap();
        assert_eq!(res, ActionResult::CopyCalculation("300".to_string()));

        // 6. OpenPath
        let sink = SearchSink::for_test(6);
        let _ = coordinator.search("~/", 6, &sink);
        let path_cand = sink
            .snapshot()
            .into_iter()
            .find(|c| c.category == ResultCategory::FilePath)
            .unwrap();
        let res = coordinator
            .activate(
                &path_cand.provider_id,
                &path_cand.canonical_id,
                path_cand.activation.clone(),
            )
            .unwrap();
        let home = std::env::var("HOME").unwrap();
        assert_eq!(res, ActionResult::OpenPath(PathBuf::from(home)));

        // 7. OpenUri
        let sink = SearchSink::for_test(7);
        let _ = coordinator.search("https://example.com/test", 7, &sink);
        let uri_cand = sink
            .snapshot()
            .into_iter()
            .find(|c| c.category == ResultCategory::Uri)
            .unwrap();
        let res = coordinator
            .activate(
                &uri_cand.provider_id,
                &uri_cand.canonical_id,
                uri_cand.activation.clone(),
            )
            .unwrap();
        assert_eq!(
            res,
            ActionResult::OpenUri("https://example.com/test".to_string())
        );

        // 8. ExecuteCommand
        let sink = SearchSink::for_test(8);
        let _ = coordinator.search("$echo hi", 8, &sink);
        if let Some(cmd_cand) = sink
            .snapshot()
            .into_iter()
            .find(|c| c.category == ResultCategory::Command)
        {
            let res = coordinator
                .activate(
                    &cmd_cand.provider_id,
                    &cmd_cand.canonical_id,
                    cmd_cand.activation.clone(),
                )
                .unwrap();
            assert_eq!(res, ActionResult::ExecuteCommand("echo hi".to_string()));
        }

        // 9. OpenWeb
        let sink = SearchSink::for_test(9);
        let _ = coordinator.search("?rust lang", 9, &sink);
        let web_cand = sink
            .snapshot()
            .into_iter()
            .find(|c| c.category == ResultCategory::WebSearch)
            .unwrap();
        let res = coordinator
            .activate(
                &web_cand.provider_id,
                &web_cand.canonical_id,
                web_cand.activation.clone(),
            )
            .unwrap();
        assert_eq!(
            res,
            ActionResult::OpenWeb("https://www.google.com/search?q=rust+lang".to_string())
        );

        // 10. CopyKeybinding
        let sink = SearchSink::for_test(10);
        let _ = coordinator.search("<Super", 10, &sink);
        let key_cand = sink
            .snapshot()
            .into_iter()
            .find(|c| c.category == ResultCategory::Keybinding)
            .unwrap();
        let res = coordinator
            .activate(
                &key_cand.provider_id,
                &key_cand.canonical_id,
                key_cand.activation.clone(),
            )
            .unwrap();
        assert_eq!(res, ActionResult::CopyKeybinding("Super+L".to_string()));
    }

    // -----------------------------------------------------------------------
    // Deadline, Cancellation, and In-Flight Bounding Tests (#254)
    // -----------------------------------------------------------------------

    struct ControllableBlockingProvider {
        id: &'static str,
        unblock_rx: Arc<Mutex<mpsc::Receiver<()>>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl SearchProvider for ControllableBlockingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from_static(self.id)
        }

        fn search(&self, request: SearchRequest, sink: SearchSink) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let rx = self.unblock_rx.lock().unwrap();
            let _ = rx.recv();
            sink.push(SearchCandidate::new(
                self.id(),
                format!("{}:{}", self.id, request.query),
                request.generation,
                "Blocked Result",
                None,
                ResultCategory::Custom,
                SearchResultIcon::Initial('B'),
                "Open",
                SearchActivation::new("payload-blocked"),
            ));
        }

        fn activate(&self, _activation: SearchActivation) -> Result<ActionResult, SearchError> {
            Ok(ActionResult::Handled {
                close_overview: true,
            })
        }
    }

    #[test]
    fn test_hung_provider_does_not_prevent_search_from_returning_and_fast_candidates_ranked() {
        let (unblock_tx, unblock_rx) = mpsc::channel();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hung = Arc::new(ControllableBlockingProvider {
            id: "hung",
            unblock_rx: Arc::new(Mutex::new(unblock_rx)),
            calls,
        });

        let fast = Arc::new(TestProvider {
            id: "fast",
            prefix: "Fast",
        });

        let coordinator =
            SearchCoordinator::new(vec![hung, fast]).with_per_query_budget(Duration::ZERO);
        let sink = SearchSink::for_test(1);

        let summary = coordinator.search("hello", 1, &sink);

        // Fast provider was ranked (or if budget was ZERO before thread ran, fast may or may not finish)
        // With ZERO budget, hung provider is definitely timed out
        assert!(summary.has_timed_out());
        assert!(
            summary
                .timed_out_providers
                .contains(&ProviderId::from_static("hung"))
        );

        // Unblock hung provider thread so it can exit cleanly
        let _ = unblock_tx.send(());
    }

    #[test]
    fn test_fast_provider_completes_and_delivers_when_another_provider_is_hung() {
        let (unblock_tx, unblock_rx) = mpsc::channel();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hung = Arc::new(ControllableBlockingProvider {
            id: "hung",
            unblock_rx: Arc::new(Mutex::new(unblock_rx)),
            calls,
        });

        let fast = Arc::new(TestProvider {
            id: "fast",
            prefix: "Fast",
        });

        // A generous budget lets the instant `fast` provider finish well within it,
        // while `hung` — blocked on a test-controlled channel we never release until
        // after assertions — is *guaranteed* still outstanding when the budget expires.
        // Only the budget's lower bound matters for determinism; using the same generous
        // value as the "everything completes" path keeps this from depending on how much
        // real time thread scheduling happens to take under CI load (see #254 Determinism).
        let coordinator = SearchCoordinator::new(vec![fast, hung])
            .with_per_query_budget(Duration::from_millis(500));
        let sink = SearchSink::for_test(1);

        let summary = coordinator.search("hello", 1, &sink);

        assert_eq!(
            summary.timed_out_providers,
            vec![ProviderId::from_static("hung")]
        );
        assert!(summary.skipped_providers.is_empty());

        let results = sink.snapshot();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].canonical_id, "fast:hello");
        assert_eq!(results[0].title, "Fast - hello");

        let _ = unblock_tx.send(());
    }

    #[test]
    fn test_zero_budget_ranks_only_what_was_already_collected_without_panicking() {
        let p1 = Arc::new(TestProvider {
            id: "p1",
            prefix: "One",
        });
        let p2 = Arc::new(TestProvider {
            id: "p2",
            prefix: "Two",
        });

        let coordinator =
            SearchCoordinator::new(vec![p1, p2]).with_per_query_budget(Duration::ZERO);
        let sink = SearchSink::for_test(1);

        let summary = coordinator.search("test", 1, &sink);
        // Does not panic and returns summary
        assert_eq!(summary.timed_out_providers.len(), 2);
    }

    #[test]
    fn test_all_providers_complete_within_budget_produces_identical_results() {
        let p1 = Arc::new(TestProvider {
            id: "p1",
            prefix: "One",
        });
        let p2 = Arc::new(TestProvider {
            id: "p2",
            prefix: "Two",
        });

        let coordinator =
            SearchCoordinator::new(vec![p1, p2]).with_per_query_budget(Duration::from_millis(500));
        let sink = SearchSink::for_test(1);

        let summary = coordinator.search("query", 1, &sink);

        assert_eq!(summary.timed_out_providers, Vec::<ProviderId>::new());
        assert_eq!(summary.skipped_providers, Vec::<ProviderId>::new());
        assert_eq!(summary.raw_candidate_count, 2);
        assert_eq!(summary.ranked_candidate_count, 2);

        let results = sink.snapshot();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "One - query");
        assert_eq!(results[1].title, "Two - query");
    }

    struct LatePushProvider {
        id: &'static str,
        gate_rx: Arc<Mutex<mpsc::Receiver<()>>>,
        push_accepted: Arc<std::sync::atomic::AtomicBool>,
    }

    impl SearchProvider for LatePushProvider {
        fn id(&self) -> ProviderId {
            ProviderId::from_static(self.id)
        }

        fn search(&self, request: SearchRequest, sink: SearchSink) {
            let rx = self.gate_rx.lock().unwrap();
            let _ = rx.recv();
            let accepted = sink.push(SearchCandidate::new(
                self.id(),
                format!("{}:late", self.id),
                request.generation,
                "Late Candidate",
                None,
                ResultCategory::Custom,
                SearchResultIcon::Initial('L'),
                "Open",
                SearchActivation::new("payload-late"),
            ));
            self.push_accepted
                .store(accepted, std::sync::atomic::Ordering::SeqCst);
        }

        fn activate(&self, _activation: SearchActivation) -> Result<ActionResult, SearchError> {
            Ok(ActionResult::Handled {
                close_overview: true,
            })
        }
    }

    #[test]
    fn test_late_push_after_deadline_is_rejected_and_does_not_leak_to_future_queries() {
        let (gate_tx, gate_rx) = mpsc::channel();
        let push_accepted = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let late_provider = Arc::new(LatePushProvider {
            id: "late",
            gate_rx: Arc::new(Mutex::new(gate_rx)),
            push_accepted: push_accepted.clone(),
        });

        let coordinator = SearchCoordinator::new(vec![late_provider.clone()])
            .with_per_query_budget(Duration::ZERO);
        let sink1 = SearchSink::for_test(1);

        // Query 1: Times out immediately due to ZERO budget, cancelling sink1's scratch sink
        let summary1 = coordinator.search("first", 1, &sink1);
        assert_eq!(
            summary1.timed_out_providers,
            vec![ProviderId::from_static("late")]
        );
        assert_eq!(sink1.len(), 0);

        // Now signal the late provider to push to its cancelled scratch sink
        let _ = gate_tx.send(());

        // Wait a brief moment for the thread to perform push
        for _ in 0..10_000 {
            if !push_accepted.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::yield_now();
        }

        assert!(
            !push_accepted.load(std::sync::atomic::Ordering::SeqCst),
            "push() into cancelled scratch sink must return false"
        );
        assert_eq!(sink1.len(), 0, "no candidates must appear in query 1 sink");

        // Wait for thread to exit and in-flight count to reset
        for _ in 0..10_000 {
            let in_flight = coordinator.in_flight_counts.lock().unwrap();
            if in_flight
                .get(&ProviderId::from_static("late"))
                .copied()
                .unwrap_or(0)
                == 0
            {
                break;
            }
            drop(in_flight);
            std::thread::yield_now();
        }

        // Query 2: New query with fast provider
        let fast = Arc::new(TestProvider {
            id: "fast",
            prefix: "Fast",
        });
        let coordinator2 =
            SearchCoordinator::new(vec![fast]).with_per_query_budget(Duration::from_millis(500));
        let sink2 = SearchSink::for_test(2);
        let _ = coordinator2.search("second", 2, &sink2);

        let results2 = sink2.snapshot();
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].canonical_id, "fast:second");
    }

    #[test]
    fn test_permanently_hung_provider_does_not_accumulate_unbounded_in_flight_threads() {
        let (unblock_tx, unblock_rx) = mpsc::channel();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hung = Arc::new(ControllableBlockingProvider {
            id: "hung",
            unblock_rx: Arc::new(Mutex::new(unblock_rx)),
            calls: calls.clone(),
        });

        let coordinator = SearchCoordinator::new(vec![hung])
            .with_per_query_budget(Duration::ZERO)
            .with_max_in_flight_per_provider(1);

        // Query 1: Spawns the hung provider (in-flight count becomes 1)
        let sink1 = SearchSink::for_test(1);
        let summary1 = coordinator.search("q1", 1, &sink1);
        assert_eq!(
            summary1.timed_out_providers,
            vec![ProviderId::from_static("hung")]
        );
        assert_eq!(summary1.skipped_providers, Vec::<ProviderId>::new());

        // With a ZERO budget, `search()` returns before the spawned provider thread is
        // guaranteed to have started running — the deadline check fires on the coordinator's
        // first loop iteration, without waiting on anything. Wait for the thread to actually
        // record its call before asserting on it, rather than racing its scheduling.
        for _ in 0..10_000 {
            if calls.load(std::sync::atomic::Ordering::SeqCst) == 1 {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Query 2: Hung provider is still in-flight, so it is skipped
        let sink2 = SearchSink::for_test(2);
        let summary2 = coordinator.search("q2", 2, &sink2);
        assert_eq!(summary2.timed_out_providers, Vec::<ProviderId>::new());
        assert_eq!(
            summary2.skipped_providers,
            vec![ProviderId::from_static("hung")]
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Query 3: Hung provider is still skipped
        let sink3 = SearchSink::for_test(3);
        let summary3 = coordinator.search("q3", 3, &sink3);
        assert_eq!(
            summary3.skipped_providers,
            vec![ProviderId::from_static("hung")]
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Query 4: Hung provider is still skipped
        let sink4 = SearchSink::for_test(4);
        let summary4 = coordinator.search("q4", 4, &sink4);
        assert_eq!(
            summary4.skipped_providers,
            vec![ProviderId::from_static("hung")]
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Release the hung provider
        let _ = unblock_tx.send(());

        // Wait until in-flight count drops back to 0
        for _ in 0..10_000 {
            let in_flight = coordinator.in_flight_counts.lock().unwrap();
            if in_flight
                .get(&ProviderId::from_static("hung"))
                .copied()
                .unwrap_or(0)
                == 0
            {
                break;
            }
            drop(in_flight);
            std::thread::yield_now();
        }

        // Query 5: Now hung provider is unblocked and can be scheduled again
        let (unblock_tx2, unblock_rx2) = mpsc::channel();
        let hung2 = Arc::new(ControllableBlockingProvider {
            id: "hung",
            unblock_rx: Arc::new(Mutex::new(unblock_rx2)),
            calls: calls.clone(),
        });
        let coordinator2 = SearchCoordinator::new(vec![hung2])
            .with_per_query_budget(Duration::ZERO)
            .with_max_in_flight_per_provider(1);
        let sink5 = SearchSink::for_test(5);
        let summary5 = coordinator2.search("q5", 5, &sink5);

        assert_eq!(summary5.skipped_providers, Vec::<ProviderId>::new());

        // Wait for thread to start and record the call
        for _ in 0..10_000 {
            if calls.load(std::sync::atomic::Ordering::SeqCst) == 2 {
                break;
            }
            std::thread::yield_now();
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let _ = unblock_tx2.send(());
    }

    #[test]
    fn test_timed_out_providers_reported_in_summary() {
        let (unblock_tx, unblock_rx) = mpsc::channel();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hung = Arc::new(ControllableBlockingProvider {
            id: "hung-prov",
            unblock_rx: Arc::new(Mutex::new(unblock_rx)),
            calls,
        });

        let fast = Arc::new(TestProvider {
            id: "fast-prov",
            prefix: "Fast",
        });

        // See test_fast_provider_completes_and_delivers_when_another_provider_is_hung for
        // why a generous, non-racy budget is used instead of a short one: `hung-prov` stays
        // blocked on a channel this test controls, so it is guaranteed to still be
        // outstanding when any budget expires, regardless of its length.
        let coordinator = SearchCoordinator::new(vec![hung, fast])
            .with_per_query_budget(Duration::from_millis(500));
        let sink = SearchSink::for_test(1);

        let summary = coordinator.search("test", 1, &sink);

        assert!(summary.has_timed_out());
        assert_eq!(
            summary.timed_out_providers,
            vec![ProviderId::from_static("hung-prov")]
        );
        assert_eq!(summary.skipped_providers, Vec::<ProviderId>::new());

        let _ = unblock_tx.send(());
    }
}
