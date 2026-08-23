use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use shilpo_m3e::IconName;

use super::{
    activation_cache::ActivationCache,
    parser::SearchMode,
    sink::SearchSink,
    types::{
        ActionResult, CompletionState, LatencyClass, ProviderId, ResultCategory, SearchActivation,
        SearchCandidate, SearchError, SearchProvider, SearchRequest, SearchResultIcon,
    },
};
use crate::actions::ActionDescriptor;

/// Provider that searches available desktop and system actions.
#[derive(Clone)]
pub struct ActionSearchProvider {
    actions: Vec<ActionDescriptor>,
    cached_actions: Arc<Mutex<ActivationCache<ActionDescriptor>>>,
}

impl ActionSearchProvider {
    /// Creates a new action search provider with the given action descriptors.
    pub fn new(actions: Vec<ActionDescriptor>) -> Self {
        Self {
            actions,
            cached_actions: Arc::new(Mutex::new(ActivationCache::default())),
        }
    }
}

impl SearchProvider for ActionSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from_static("action-search")
    }

    fn declared_modes(&self) -> std::borrow::Cow<'static, [SearchMode]> {
        std::borrow::Cow::Borrowed(&[SearchMode::Default, SearchMode::Actions])
    }

    fn prefix_icon(&self, mode: SearchMode) -> Option<IconName> {
        match mode {
            SearchMode::Actions => Some(IconName::Settings),
            _ => None,
        }
    }

    fn search(&self, request: SearchRequest, sink: SearchSink) {
        let query_generation = request.generation;
        let provider_id = self.id();
        let mut next_cache = HashMap::new();

        for action in &self.actions {
            // Actions requiring user input (e.g. text or number arguments) are excluded from
            // overview search results for now. Dedicated parameter/input-collection UI will be
            // introduced in follow-up work (#204). Only zero-input actions that can be executed
            // immediately with NoInput requirement are surfaced.
            if !action.input.can_invoke_without_input() {
                continue;
            }

            let canonical_id = format!("action:{}", action.id);
            let act_key = canonical_id.clone();
            next_cache.insert(act_key.clone(), action.clone());

            let candidate = SearchCandidate {
                provider_id: provider_id.clone(),
                canonical_id,
                generation: query_generation,
                title: action.label.clone(),
                subtitle: Some(format!("System Action ({})", action.name)),
                aliases: vec![action.name.clone()],
                keywords: Vec::new(),
                category: ResultCategory::Action,
                latency: LatencyClass::Instant,
                completion: CompletionState::Complete,
                icon: SearchResultIcon::Named(IconName::Settings),
                activation_verb: "Run".to_string(),
                match_positions: Vec::new(),
                activation: SearchActivation::new(act_key),
            };

            sink.push(candidate);
        }

        self.cached_actions
            .lock()
            .unwrap()
            .replace(query_generation, next_cache);
    }

    fn activate(&self, activation: SearchActivation) -> Result<ActionResult, SearchError> {
        let action = self
            .cached_actions
            .lock()
            .unwrap()
            .get_cloned(&activation.payload)
            .ok_or_else(|| SearchError::NotFound(activation.payload.clone()))?;

        Ok(ActionResult::InvokeAction(action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ActionCategory, ActionId, ActionInputRequirement};
    use crate::shell::overview_search::sink::SinkConfig;

    fn sample_actions() -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor {
                id: ActionId::ToggleOverview,
                name: "toggle-overview".to_string(),
                label: "Toggle Overview".to_string(),
                category: ActionCategory::Overlay,
                input: ActionInputRequirement::NoInput,
                enabled: true,
            },
            ActionDescriptor {
                id: ActionId::FocusWindow,
                name: "focus-window".to_string(),
                label: "Focus Window".to_string(),
                category: ActionCategory::Navigation,
                input: ActionInputRequirement::WindowId,
                enabled: true,
            },
            ActionDescriptor {
                id: ActionId::Quit,
                name: "quit".to_string(),
                label: "Quit Shilpo".to_string(),
                category: ActionCategory::System,
                input: ActionInputRequirement::NoInput,
                enabled: true,
            },
        ]
    }

    #[test]
    fn test_action_search_emits_executable_actions() {
        let provider = ActionSearchProvider::new(sample_actions());
        assert_eq!(
            provider.declared_modes().as_ref(),
            &[SearchMode::Default, SearchMode::Actions]
        );
        assert_eq!(
            provider.prefix_icon(SearchMode::Actions),
            Some(IconName::Settings)
        );

        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("/toggle", SearchMode::Actions, "toggle", 1),
            sink.clone(),
        );

        let candidates = sink.snapshot();
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|c| c.title == "Toggle Overview"));
        assert!(candidates.iter().any(|c| c.title == "Quit Shilpo"));
    }

    #[test]
    fn test_actions_requiring_input_are_excluded() {
        let actions = sample_actions();
        let provider = ActionSearchProvider::new(actions);

        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("focus", SearchMode::Default, "focus", 1),
            sink.clone(),
        );

        let candidates = sink.snapshot();
        // Focus Window requires WindowId input and must be excluded
        assert!(!candidates.iter().any(|c| c.title == "Focus Window"));
    }

    #[test]
    fn test_action_activation() {
        let provider = ActionSearchProvider::new(sample_actions());
        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("/toggle", SearchMode::Actions, "toggle", 1),
            sink.clone(),
        );

        let candidates = sink.snapshot();
        let toggle_cand = candidates
            .iter()
            .find(|c| c.title == "Toggle Overview")
            .unwrap();

        let result = provider.activate(toggle_cand.activation.clone()).unwrap();
        match result {
            ActionResult::InvokeAction(desc) => {
                assert_eq!(desc.id, ActionId::ToggleOverview);
                assert_eq!(desc.name, "toggle-overview");
            }
            other => panic!("expected InvokeAction, got {other:?}"),
        }
    }

    #[test]
    fn test_action_activation_cache_replaces_previous_generation() {
        let provider = ActionSearchProvider::new(sample_actions());
        let first_sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("/toggle", SearchMode::Actions, "toggle", 1),
            first_sink,
        );
        let second_sink = SearchSink::new(2, SinkConfig::default());
        provider.search(
            SearchRequest::new("/quit", SearchMode::Actions, "quit", 2),
            second_sink.clone(),
        );

        assert_eq!(provider.cached_actions.lock().unwrap().len(), 2);
        assert!(matches!(
            provider.activate(second_sink.snapshot()[0].activation.clone()),
            Ok(ActionResult::InvokeAction(_))
        ));
    }
}
