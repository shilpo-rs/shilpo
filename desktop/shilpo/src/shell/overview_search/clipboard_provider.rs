use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use shilpo_m3e::IconName;
use shilpo_services::ClipboardItem;
use tokio::sync::watch;

use super::{
    activation_cache::ActivationCache,
    parser::SearchMode,
    sink::SearchSink,
    types::{
        ActionResult, CompletionState, LatencyClass, ProviderId, ResultCategory, SearchActivation,
        SearchCandidate, SearchError, SearchProvider, SearchRequest, SearchResultIcon,
    },
};

/// Provider that searches clipboard history subscribed via [`ClipboardService`].
#[derive(Clone)]
pub struct ClipboardSearchProvider {
    subscription: Option<watch::Receiver<Vec<ClipboardItem>>>,
    cached_items: Arc<Mutex<ActivationCache<ClipboardItem>>>,
}

impl ClipboardSearchProvider {
    /// Creates a new clipboard search provider holding the watch subscription receiver.
    pub fn new(subscription: Option<watch::Receiver<Vec<ClipboardItem>>>) -> Self {
        Self {
            subscription,
            cached_items: Arc::new(Mutex::new(ActivationCache::default())),
        }
    }
}

impl SearchProvider for ClipboardSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from_static("clipboard-search")
    }

    fn declared_modes(&self) -> std::borrow::Cow<'static, [SearchMode]> {
        std::borrow::Cow::Borrowed(&[SearchMode::Clipboard])
    }

    fn prefix_icon(&self, mode: SearchMode) -> Option<IconName> {
        match mode {
            SearchMode::Clipboard => Some(IconName::Star),
            _ => None,
        }
    }

    fn search(&self, request: SearchRequest, sink: SearchSink) {
        let query_generation = request.generation;
        let provider_id = self.id();
        let mut next_cache = HashMap::new();

        // Fresh read from the watch channel subscription on every search() invocation
        let items = self
            .subscription
            .as_ref()
            .map(|rx| rx.borrow().clone())
            .unwrap_or_default();

        for item in items {
            let canonical_id = format!("clipboard:{}", item.id);
            let act_key = canonical_id.clone();
            next_cache.insert(act_key.clone(), item.clone());

            let (title, subtitle) = match &item.content {
                shilpo_services::ClipboardContent::Text(text) => (
                    text.clone(),
                    Some(format!(
                        "Copied at {}",
                        item.last_copied_at
                            .with_timezone(&chrono::Local)
                            .format("%H:%M:%S")
                    )),
                ),
                shilpo_services::ClipboardContent::FileReference(paths) => {
                    let title = if paths.len() == 1 {
                        paths[0]
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| paths[0].display().to_string())
                    } else {
                        format!("{} files", paths.len())
                    };
                    let subtitle = if paths.len() == 1 {
                        Some(paths[0].display().to_string())
                    } else {
                        Some(
                            paths
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                        )
                    };
                    (title, subtitle)
                }
                shilpo_services::ClipboardContent::Image => (
                    "[Image]".to_string(),
                    Some(format!(
                        "Copied at {}",
                        item.last_copied_at
                            .with_timezone(&chrono::Local)
                            .format("%H:%M:%S")
                    )),
                ),
            };

            let candidate = SearchCandidate {
                provider_id: provider_id.clone(),
                canonical_id,
                generation: query_generation,
                title,
                subtitle,
                aliases: Vec::new(),
                keywords: Vec::new(),
                category: ResultCategory::Clipboard,
                latency: LatencyClass::Instant,
                completion: CompletionState::Complete,
                icon: SearchResultIcon::Named(IconName::Star),
                activation_verb: "Copy".to_string(),
                match_positions: Vec::new(),
                activation: SearchActivation::new(act_key),
            };

            sink.push(candidate);
        }

        self.cached_items
            .lock()
            .unwrap()
            .replace(query_generation, next_cache);
    }

    fn activate(&self, activation: SearchActivation) -> Result<ActionResult, SearchError> {
        let item = self
            .cached_items
            .lock()
            .unwrap()
            .get_cloned(&activation.payload)
            .ok_or_else(|| SearchError::NotFound(activation.payload.clone()))?;

        Ok(ActionResult::CopyClipboard(item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::overview_search::sink::SinkConfig;

    fn sample_item(text: &str, secs: i64) -> ClipboardItem {
        ClipboardItem::new_text(
            text.to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(secs),
        )
    }

    #[test]
    fn test_clipboard_declared_modes_and_prefix_icon() {
        let provider = ClipboardSearchProvider::new(None);
        assert_eq!(provider.declared_modes().as_ref(), &[SearchMode::Clipboard]);
        assert_eq!(
            provider.prefix_icon(SearchMode::Clipboard),
            Some(IconName::Star)
        );
        assert_eq!(provider.prefix_icon(SearchMode::Default), None);
    }

    #[test]
    fn test_clipboard_candidates_reflect_live_history_changes_between_searches() {
        let (tx, rx) = watch::channel(vec![sample_item("first copied text", 1)]);
        let provider = ClipboardSearchProvider::new(Some(rx));

        // First search sees initial clipboard item
        let sink1 = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new(";first", SearchMode::Clipboard, "first", 1),
            sink1.clone(),
        );
        let candidates1 = sink1.snapshot();
        assert_eq!(candidates1.len(), 1);
        assert_eq!(candidates1[0].title, "first copied text");

        // Update clipboard channel with new items while provider instance is held
        tx.send(vec![
            sample_item("first copied text", 1),
            sample_item("second copied text", 2),
        ])
        .unwrap();

        // Second search on same provider instance sees updated history
        let sink2 = SearchSink::new(2, SinkConfig::default());
        provider.search(
            SearchRequest::new(";second", SearchMode::Clipboard, "second", 2),
            sink2.clone(),
        );
        let candidates2 = sink2.snapshot();
        assert_eq!(candidates2.len(), 2);
        assert!(candidates2.iter().any(|c| c.title == "second copied text"));
    }

    #[test]
    fn test_clipboard_activation() {
        let (_tx, rx) = watch::channel(vec![sample_item("hello clipboard", 42)]);
        let provider = ClipboardSearchProvider::new(Some(rx));

        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new(";hello", SearchMode::Clipboard, "hello", 1),
            sink.clone(),
        );
        let candidate = &sink.snapshot()[0];

        let result = provider.activate(candidate.activation.clone()).unwrap();
        match result {
            ActionResult::CopyClipboard(item) => {
                assert_eq!(item.text().unwrap(), "hello clipboard");
            }
            other => panic!("expected CopyClipboard, got {other:?}"),
        }
    }

    #[test]
    fn test_clipboard_identical_content_yields_identical_canonical_id() {
        let now = chrono::Utc::now();
        let item1 = ClipboardItem::new_text("identical text".into(), now);
        let item2 =
            ClipboardItem::new_text("identical text".into(), now + chrono::Duration::seconds(5));

        assert_eq!(item1.id, item2.id);

        let (_tx1, rx1) = watch::channel(vec![item1]);
        let provider1 = ClipboardSearchProvider::new(Some(rx1));
        let sink1 = SearchSink::new(1, SinkConfig::default());
        provider1.search(
            SearchRequest::new(";identical", SearchMode::Clipboard, "identical", 1),
            sink1.clone(),
        );

        let (_tx2, rx2) = watch::channel(vec![item2]);
        let provider2 = ClipboardSearchProvider::new(Some(rx2));
        let sink2 = SearchSink::new(2, SinkConfig::default());
        provider2.search(
            SearchRequest::new(";identical", SearchMode::Clipboard, "identical", 2),
            sink2.clone(),
        );

        assert_eq!(
            sink1.snapshot()[0].canonical_id,
            sink2.snapshot()[0].canonical_id
        );
    }

    #[test]
    fn test_clipboard_activation_cache_replaces_sensitive_previous_generation() {
        let sensitive = sample_item("secret-password", 1);
        let retained = sample_item("safe-value", 2);
        let (tx, rx) = watch::channel(vec![sensitive, retained.clone()]);
        let provider = ClipboardSearchProvider::new(Some(rx));

        let first_sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new(";", SearchMode::Clipboard, "", 1),
            first_sink.clone(),
        );
        let stale_activation = first_sink
            .snapshot()
            .into_iter()
            .find(|candidate| candidate.title == "secret-password")
            .unwrap()
            .activation;

        tx.send(vec![retained]).unwrap();
        let second_sink = SearchSink::new(2, SinkConfig::default());
        provider.search(
            SearchRequest::new(";safe", SearchMode::Clipboard, "safe", 2),
            second_sink.clone(),
        );

        assert_eq!(provider.cached_items.lock().unwrap().len(), 1);
        assert!(matches!(
            provider.activate(stale_activation),
            Err(SearchError::NotFound(_))
        ));
        assert!(matches!(
            provider.activate(second_sink.snapshot()[0].activation.clone()),
            Ok(ActionResult::CopyClipboard(_))
        ));
    }
}
