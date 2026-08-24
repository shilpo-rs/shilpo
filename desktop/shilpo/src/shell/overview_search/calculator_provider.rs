use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use shilpo_m3e::IconName;

use super::{
    activation_cache::ActivationCache,
    calculator,
    parser::SearchMode,
    sink::SearchSink,
    types::{
        ActionResult, CompletionState, LatencyClass, ProviderId, ResultCategory, SearchActivation,
        SearchCandidate, SearchError, SearchProvider, SearchRequest, SearchResultIcon,
    },
};

/// Provider that calculates arithmetic expressions and provides copyable results.
#[derive(Clone, Default)]
pub struct CalculatorSearchProvider {
    cached_results: Arc<Mutex<ActivationCache<String>>>,
}

impl CalculatorSearchProvider {
    /// Creates a new calculator search provider.
    pub fn new() -> Self {
        Self {
            cached_results: Arc::new(Mutex::new(ActivationCache::default())),
        }
    }
}

impl SearchProvider for CalculatorSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from_static("calculator-search")
    }

    fn declared_modes(&self) -> std::borrow::Cow<'static, [SearchMode]> {
        std::borrow::Cow::Borrowed(&[SearchMode::Calculator])
    }

    fn prefix_icon(&self, mode: SearchMode) -> Option<IconName> {
        match mode {
            SearchMode::Calculator => Some(IconName::Star),
            _ => None,
        }
    }

    fn search(&self, request: SearchRequest, sink: SearchSink) {
        let query_generation = request.generation;
        let provider_id = self.id();
        let mut next_cache = HashMap::new();

        if let Some(val) = calculator::evaluate_expression(&request.query) {
            let canonical_id = format!("calc:{}", val);
            let act_key = canonical_id.clone();
            next_cache.insert(act_key.clone(), val.clone());

            let candidate = SearchCandidate {
                provider_id: provider_id.clone(),
                canonical_id,
                generation: query_generation,
                title: val,
                subtitle: Some(format!("= {}", request.query)),
                aliases: Vec::new(),
                keywords: Vec::new(),
                category: ResultCategory::Calculator,
                latency: LatencyClass::Instant,
                completion: CompletionState::Complete,
                icon: SearchResultIcon::Named(IconName::Star),
                activation_verb: "Copy result".to_string(),
                match_positions: Vec::new(),
                activation: SearchActivation::new(act_key),
            };

            sink.push(candidate);
        }

        self.cached_results
            .lock()
            .unwrap()
            .replace(query_generation, next_cache);
    }

    fn activate(&self, activation: SearchActivation) -> Result<ActionResult, SearchError> {
        let val = self
            .cached_results
            .lock()
            .unwrap()
            .get_cloned(&activation.payload)
            .ok_or_else(|| SearchError::NotFound(activation.payload.clone()))?;

        Ok(ActionResult::CopyCalculation(val))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::overview_search::sink::SinkConfig;

    #[test]
    fn test_calculator_declared_modes_and_prefix_icon() {
        let provider = CalculatorSearchProvider::new();
        assert_eq!(
            provider.declared_modes().as_ref(),
            &[SearchMode::Calculator]
        );
        assert_eq!(
            provider.prefix_icon(SearchMode::Calculator),
            Some(IconName::Star)
        );
        assert_eq!(provider.prefix_icon(SearchMode::Default), None);
    }

    #[test]
    fn test_calculator_evaluates_arithmetic_expressions() {
        let provider = CalculatorSearchProvider::new();
        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("=2+2", SearchMode::Calculator, "2+2", 1),
            sink.clone(),
        );

        let candidates = sink.snapshot();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].title, "4");
        assert_eq!(candidates[0].category, ResultCategory::Calculator);
    }

    #[test]
    fn test_calculator_activation() {
        let provider = CalculatorSearchProvider::new();
        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("10 * (5 - 3)", SearchMode::Calculator, "10 * (5 - 3)", 1),
            sink.clone(),
        );

        let candidate = &sink.snapshot()[0];
        assert_eq!(candidate.title, "20");

        let result = provider.activate(candidate.activation.clone()).unwrap();
        match result {
            ActionResult::CopyCalculation(val) => assert_eq!(val, "20"),
            other => panic!("expected CopyCalculation, got {other:?}"),
        }
    }

    #[test]
    fn test_calculator_activation_cache_replaces_previous_generation() {
        let provider = CalculatorSearchProvider::new();
        let first_sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("=2+2", SearchMode::Calculator, "2+2", 1),
            first_sink.clone(),
        );
        let stale_activation = first_sink.snapshot()[0].activation.clone();

        let second_sink = SearchSink::new(2, SinkConfig::default());
        provider.search(
            SearchRequest::new("=3+3", SearchMode::Calculator, "3+3", 2),
            second_sink.clone(),
        );

        assert_eq!(provider.cached_results.lock().unwrap().len(), 1);
        assert!(matches!(
            provider.activate(stale_activation),
            Err(SearchError::NotFound(_))
        ));
        assert_eq!(
            provider
                .activate(second_sink.snapshot()[0].activation.clone())
                .unwrap(),
            ActionResult::CopyCalculation("6".to_string())
        );
    }
}
