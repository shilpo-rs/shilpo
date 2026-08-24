use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use shilpo_m3e::IconName;
use shilpo_services::{AppScanner, Application};

use super::{
    activation_cache::ActivationCache,
    matcher::fuzzy_match,
    parser::SearchMode,
    sink::SearchSink,
    types::{
        ActionResult, CompletionState, LatencyClass, ProviderId, ResultCategory, SearchActivation,
        SearchCandidate, SearchError, SearchProvider, SearchRequest, SearchResultIcon,
    },
};

/// Provider that searches desktop applications discovered by [`AppScanner`].
#[derive(Clone)]
pub struct AppSearchProvider {
    scanner: AppScanner,
    cached_apps: Arc<Mutex<ActivationCache<Application>>>,
}

impl AppSearchProvider {
    /// Creates a new application search provider over the given scanner.
    pub fn new(scanner: AppScanner) -> Self {
        Self {
            scanner,
            cached_apps: Arc::new(Mutex::new(ActivationCache::default())),
        }
    }
}

impl SearchProvider for AppSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from_static("app-search")
    }

    fn declared_modes(&self) -> std::borrow::Cow<'static, [SearchMode]> {
        std::borrow::Cow::Borrowed(&[SearchMode::Default, SearchMode::Apps])
    }

    fn prefix_icon(&self, mode: SearchMode) -> Option<IconName> {
        match mode {
            SearchMode::Apps => Some(IconName::Terminal),
            _ => None,
        }
    }

    fn search(&self, request: SearchRequest, sink: SearchSink) {
        let query_generation = request.generation;
        let provider_id = self.id();
        let mut next_cache = HashMap::new();

        // Fresh read from scanner on every search() invocation
        let applications = self.scanner.applications();
        let mut seen_entries = HashSet::new();

        for app in applications {
            // Dedup by desktop entry file path, fixing the exec-based collision bug
            if !seen_entries.insert(app.desktop_file.clone()) {
                continue;
            }

            let mut aliases = Vec::new();
            if !app.exec.is_empty() {
                aliases.push(app.exec.clone());
            }
            if let Some(stem) = app.desktop_file.file_stem().and_then(|s| s.to_str())
                && stem != app.name
                && !aliases.contains(&stem.to_string())
            {
                aliases.push(stem.to_string());
            }

            let query = request.query.trim();
            let matches_query = query.is_empty()
                || fuzzy_match(query, &app.name).is_some()
                || aliases
                    .iter()
                    .any(|alias| fuzzy_match(query, alias).is_some())
                || app
                    .categories
                    .iter()
                    .any(|category| fuzzy_match(query, category).is_some())
                || app
                    .description
                    .as_deref()
                    .is_some_and(|description| fuzzy_match(query, description).is_some());
            if !matches_query {
                continue;
            }

            let canonical_id = format!("app:{}", app.desktop_file.display());
            let act_key = canonical_id.clone();
            next_cache.insert(act_key.clone(), app.clone());

            let candidate = SearchCandidate {
                provider_id: provider_id.clone(),
                canonical_id,
                generation: query_generation,
                title: app.name.clone(),
                subtitle: app.description.clone().or_else(|| Some(app.exec.clone())),
                aliases,
                keywords: app.categories.clone(),
                category: ResultCategory::Application,
                latency: LatencyClass::Instant,
                completion: CompletionState::Complete,
                icon: SearchResultIcon::AppIcon(app.icon_path.clone()),
                activation_verb: "Launch".to_string(),
                match_positions: Vec::new(),
                activation: SearchActivation::new(act_key),
            };

            sink.push(candidate);
        }

        self.cached_apps
            .lock()
            .unwrap()
            .replace(query_generation, next_cache);
    }

    fn activate(&self, activation: SearchActivation) -> Result<ActionResult, SearchError> {
        let app = self
            .cached_apps
            .lock()
            .unwrap()
            .get_cloned(&activation.payload)
            .ok_or_else(|| SearchError::NotFound(activation.payload.clone()))?;

        Ok(ActionResult::LaunchApp(app))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn make_app(name: &str, exec: &str, desktop_path: &str, categories: Vec<&str>) -> Application {
        Application {
            name: name.to_string(),
            exec: exec.to_string(),
            icon: None,
            icon_path: None,
            description: Some(format!("{name} description")),
            categories: categories.into_iter().map(String::from).collect(),
            desktop_file: PathBuf::from(desktop_path),
            working_dir: None,
            terminal: false,
            try_exec: None,
        }
    }

    #[test]
    fn test_two_desktop_entries_sharing_one_exec_both_appear() {
        let app1 = make_app(
            "App One",
            "shared-binary",
            "/usr/share/applications/app1.desktop",
            vec![],
        );
        let app2 = make_app(
            "App Two",
            "shared-binary",
            "/usr/share/applications/app2.desktop",
            vec![],
        );

        let scanner = AppScanner::from_applications(vec![app1, app2]);
        let provider = AppSearchProvider::new(scanner);
        let sink = SearchSink::for_test(1);

        let request = SearchRequest::new("", SearchMode::Default, "", 1);
        provider.search(request, sink.clone());

        let results = sink.snapshot();
        assert_eq!(
            results.len(),
            2,
            "Both desktop entries sharing an exec must be emitted without collision"
        );
        let ids: Vec<&str> = results.iter().map(|r| r.canonical_id.as_str()).collect();
        assert!(ids.contains(&"app:/usr/share/applications/app1.desktop"));
        assert!(ids.contains(&"app:/usr/share/applications/app2.desktop"));
    }

    #[test]
    fn test_app_candidates_carry_aliases_and_keywords() {
        let app = make_app(
            "Visual Studio Code",
            "code",
            "/usr/share/applications/code.desktop",
            vec!["Development", "IDE"],
        );

        let scanner = AppScanner::from_applications(vec![app]);
        let provider = AppSearchProvider::new(scanner);
        let sink = SearchSink::for_test(1);

        let request = SearchRequest::new("code", SearchMode::Default, "code", 1);
        provider.search(request, sink.clone());

        let results = sink.snapshot();
        assert_eq!(results.len(), 1);
        assert!(results[0].aliases.contains(&"code".to_string()));
        assert!(results[0].keywords.contains(&"Development".to_string()));
        assert!(results[0].keywords.contains(&"IDE".to_string()));
    }

    #[test]
    fn test_consecutive_searches_read_fresh_applications() {
        let app1 = make_app(
            "App One",
            "app1",
            "/usr/share/applications/app1.desktop",
            vec![],
        );
        let scanner = AppScanner::from_applications(vec![app1.clone()]);
        let provider = AppSearchProvider::new(scanner.clone());

        // First search
        let sink1 = SearchSink::for_test(1);
        provider.search(
            SearchRequest::new("", SearchMode::Default, "", 1),
            sink1.clone(),
        );
        assert_eq!(sink1.snapshot().len(), 1);

        // Add a second application to the live scanner
        let app2 = make_app(
            "App Two",
            "app2",
            "/usr/share/applications/app2.desktop",
            vec![],
        );
        scanner.replace_applications(vec![app1, app2]);

        // Second search on same provider instance
        let sink2 = SearchSink::for_test(2);
        provider.search(
            SearchRequest::new("", SearchMode::Default, "", 2),
            sink2.clone(),
        );
        assert_eq!(
            sink2.snapshot().len(),
            2,
            "search() must reflect updated scanner state on every invocation"
        );
    }

    #[test]
    fn test_app_activation_round_trip() {
        let app = make_app(
            "Calculator",
            "gnome-calc",
            "/usr/share/applications/calc.desktop",
            vec![],
        );
        let scanner = AppScanner::from_applications(vec![app.clone()]);
        let provider = AppSearchProvider::new(scanner);

        let sink = SearchSink::for_test(1);
        provider.search(
            SearchRequest::new("calc", SearchMode::Default, "calc", 1),
            sink.clone(),
        );

        let results = sink.snapshot();
        assert_eq!(results.len(), 1);

        let activation_res = provider.activate(results[0].activation.clone()).unwrap();
        match activation_res {
            ActionResult::LaunchApp(launched) => {
                assert_eq!(launched.name, "Calculator");
                assert_eq!(launched.exec, "gnome-calc");
            }
            _ => panic!("Expected ActionResult::LaunchApp"),
        }

        // Unknown activation payload returns NotFound
        let err = provider.activate(SearchActivation::new("unknown-key"));
        assert!(matches!(err, Err(SearchError::NotFound(_))));
    }

    #[test]
    fn test_app_search_filters_before_caching_candidates() {
        let calculator = make_app(
            "Calculator",
            "gnome-calculator",
            "/usr/share/applications/calc.desktop",
            vec!["Utility"],
        );
        let browser = make_app(
            "Firefox",
            "firefox",
            "/usr/share/applications/firefox.desktop",
            vec!["Network"],
        );
        let provider =
            AppSearchProvider::new(AppScanner::from_applications(vec![calculator, browser]));
        let sink = SearchSink::for_test(1);

        provider.search(
            SearchRequest::new("calc", SearchMode::Default, "calc", 1),
            sink.clone(),
        );

        let results = sink.snapshot();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Calculator");
        assert_eq!(provider.cached_apps.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_app_activation_cache_replaces_previous_generation() {
        let calculator = make_app(
            "Calculator",
            "gnome-calculator",
            "/usr/share/applications/calc.desktop",
            vec![],
        );
        let browser = make_app(
            "Firefox",
            "firefox",
            "/usr/share/applications/firefox.desktop",
            vec![],
        );
        let provider =
            AppSearchProvider::new(AppScanner::from_applications(vec![calculator, browser]));

        let first_sink = SearchSink::for_test(1);
        provider.search(
            SearchRequest::new("", SearchMode::Default, "", 1),
            first_sink.clone(),
        );
        let stale_activation = first_sink
            .snapshot()
            .into_iter()
            .find(|candidate| candidate.title == "Firefox")
            .unwrap()
            .activation;

        let second_sink = SearchSink::for_test(2);
        provider.search(
            SearchRequest::new("calc", SearchMode::Default, "calc", 2),
            second_sink.clone(),
        );

        assert_eq!(provider.cached_apps.lock().unwrap().len(), 1);
        assert!(matches!(
            provider.activate(stale_activation),
            Err(SearchError::NotFound(_))
        ));
        assert!(matches!(
            provider.activate(second_sink.snapshot()[0].activation.clone()),
            Ok(ActionResult::LaunchApp(_))
        ));
    }
}
