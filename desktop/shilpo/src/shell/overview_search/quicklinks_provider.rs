use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use shilpo_m3e::IconName;

use super::{
    activation_cache::ActivationCache,
    parser::SearchMode,
    ranking,
    sink::SearchSink,
    types::{
        ActionResult, CompletionState, LatencyClass, ProviderId, ResultCategory, SearchActivation,
        SearchCandidate, SearchError, SearchProvider, SearchRequest, SearchResultIcon,
    },
};

#[derive(Debug, Clone)]
enum QuicklinkTarget {
    OpenPath(PathBuf),
    OpenUri(String),
    ExecuteCommand(String),
    OpenWeb(String),
    CopyKeybinding(String),
}

/// Provider covering query synthesis targets: paths, URIs, web search, shell commands, and keybindings.
#[derive(Clone)]
pub struct QuicklinksSearchProvider {
    keybindings: Vec<(String, String)>,
    cached_targets: Arc<Mutex<ActivationCache<QuicklinkTarget>>>,
}

impl QuicklinksSearchProvider {
    /// Creates a new quicklinks search provider with available keybindings.
    pub fn new(keybindings: Vec<(String, String)>) -> Self {
        Self {
            keybindings,
            cached_targets: Arc::new(Mutex::new(ActivationCache::default())),
        }
    }
}

impl SearchProvider for QuicklinksSearchProvider {
    fn id(&self) -> ProviderId {
        ProviderId::from_static("quicklinks-search")
    }

    fn declared_modes(&self) -> std::borrow::Cow<'static, [SearchMode]> {
        std::borrow::Cow::Borrowed(&[
            SearchMode::Default,
            SearchMode::Command,
            SearchMode::WebSearch,
            SearchMode::Keybindings,
        ])
    }

    fn prefix_icon(&self, mode: SearchMode) -> Option<IconName> {
        match mode {
            SearchMode::Command => Some(IconName::Terminal),
            SearchMode::WebSearch => Some(IconName::Search),
            SearchMode::Keybindings => Some(IconName::Star),
            _ => None,
        }
    }

    fn search(&self, request: SearchRequest, sink: SearchSink) {
        let query_generation = request.generation;
        let provider_id = self.id();
        let mut next_cache = HashMap::new();

        match request.mode {
            SearchMode::Default => {
                // expand_path's path.exists() executes off the UI thread because coordinator search()
                // dispatches on worker threads inside std::thread::scope. We keep the existence
                // check to avoid offering invalid filesystem paths as candidates.
                if let Some(path) = ranking::expand_path(&request.raw_query) {
                    let canonical_id = format!("path:{}", path.display());
                    let act_key = canonical_id.clone();
                    next_cache.insert(act_key.clone(), QuicklinkTarget::OpenPath(path.clone()));

                    sink.push(SearchCandidate {
                        provider_id: provider_id.clone(),
                        canonical_id,
                        generation: query_generation,
                        title: path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("File")
                            .to_string(),
                        subtitle: Some(path.display().to_string()),
                        aliases: Vec::new(),
                        keywords: Vec::new(),
                        category: ResultCategory::FilePath,
                        latency: LatencyClass::Instant,
                        completion: CompletionState::Complete,
                        icon: SearchResultIcon::Named(IconName::Folder),
                        activation_verb: "Open path".to_string(),
                        match_positions: Vec::new(),
                        activation: SearchActivation::new(act_key),
                    });
                } else if ranking::is_uri_spec(&request.raw_query) {
                    let uri = request.raw_query.trim().to_string();
                    let canonical_id = format!("uri:{}", uri);
                    let act_key = canonical_id.clone();
                    next_cache.insert(act_key.clone(), QuicklinkTarget::OpenUri(uri.clone()));

                    sink.push(SearchCandidate {
                        provider_id: provider_id.clone(),
                        canonical_id,
                        generation: query_generation,
                        title: uri,
                        subtitle: Some("Web or protocol URI".to_string()),
                        aliases: Vec::new(),
                        keywords: Vec::new(),
                        category: ResultCategory::Uri,
                        latency: LatencyClass::Instant,
                        completion: CompletionState::Complete,
                        icon: SearchResultIcon::Named(IconName::Star),
                        activation_verb: "Open link".to_string(),
                        match_positions: Vec::new(),
                        activation: SearchActivation::new(act_key),
                    });
                }

                if !request.query.trim().is_empty() {
                    let command = request.query.trim().to_string();
                    let canonical_id = format!("cmd:{}", command);
                    let act_key = canonical_id.clone();
                    next_cache.insert(
                        act_key.clone(),
                        QuicklinkTarget::ExecuteCommand(command.clone()),
                    );

                    sink.push(SearchCandidate {
                        provider_id: provider_id.clone(),
                        canonical_id,
                        generation: query_generation,
                        title: command,
                        subtitle: Some("Run command".to_string()),
                        aliases: Vec::new(),
                        keywords: Vec::new(),
                        category: ResultCategory::Command,
                        latency: LatencyClass::Instant,
                        completion: CompletionState::Complete,
                        icon: SearchResultIcon::Named(IconName::Terminal),
                        activation_verb: "Run".to_string(),
                        match_positions: Vec::new(),
                        activation: SearchActivation::new(act_key),
                    });
                }

                if !request.query.trim().is_empty() {
                    let q = request.query.trim();
                    let url = format!(
                        "https://www.google.com/search?q={}",
                        percent_encode_query(q)
                    );
                    let canonical_id = format!("web:{}", url);
                    let act_key = canonical_id.clone();
                    next_cache.insert(act_key.clone(), QuicklinkTarget::OpenWeb(url));

                    sink.push(SearchCandidate {
                        provider_id: provider_id.clone(),
                        canonical_id,
                        generation: query_generation,
                        title: q.to_string(),
                        subtitle: Some("Search the web".to_string()),
                        aliases: Vec::new(),
                        keywords: Vec::new(),
                        category: ResultCategory::WebSearch,
                        latency: LatencyClass::Instant,
                        completion: CompletionState::Complete,
                        icon: SearchResultIcon::Named(IconName::Search),
                        activation_verb: "Search".to_string(),
                        match_positions: Vec::new(),
                        activation: SearchActivation::new(act_key),
                    });
                }
            }
            SearchMode::Command => {
                if !request.query.trim().is_empty()
                    && shilpo_services::find_terminal_emulator().is_some()
                {
                    let cmd = request.query.trim().to_string();
                    let canonical_id = format!("cmd:{}", cmd);
                    let act_key = canonical_id.clone();
                    next_cache.insert(
                        act_key.clone(),
                        QuicklinkTarget::ExecuteCommand(cmd.clone()),
                    );

                    sink.push(SearchCandidate {
                        provider_id: provider_id.clone(),
                        canonical_id,
                        generation: query_generation,
                        title: format!("$ {}", cmd),
                        subtitle: Some("Execute shell command in terminal".to_string()),
                        aliases: Vec::new(),
                        keywords: Vec::new(),
                        category: ResultCategory::Command,
                        latency: LatencyClass::Instant,
                        completion: CompletionState::Complete,
                        icon: SearchResultIcon::Named(IconName::Terminal),
                        activation_verb: "Run command".to_string(),
                        match_positions: Vec::new(),
                        activation: SearchActivation::new(act_key),
                    });
                }
            }
            SearchMode::WebSearch => {
                if !request.query.trim().is_empty() {
                    let encoded = percent_encode_query(request.query.trim());
                    let url = format!("https://www.google.com/search?q={}", encoded);
                    let canonical_id = format!("web:{}", url);
                    let act_key = canonical_id.clone();
                    next_cache.insert(act_key.clone(), QuicklinkTarget::OpenWeb(url.clone()));

                    sink.push(SearchCandidate {
                        provider_id: provider_id.clone(),
                        canonical_id,
                        generation: query_generation,
                        title: format!("Search Google for \"{}\"", request.query.trim()),
                        subtitle: Some(url),
                        aliases: Vec::new(),
                        keywords: Vec::new(),
                        category: ResultCategory::WebSearch,
                        latency: LatencyClass::Instant,
                        completion: CompletionState::Complete,
                        icon: SearchResultIcon::Named(IconName::Search),
                        activation_verb: "Search".to_string(),
                        match_positions: Vec::new(),
                        activation: SearchActivation::new(act_key),
                    });
                }
            }
            SearchMode::Keybindings => {
                for (shortcut, label) in &self.keybindings {
                    let canonical_id = format!("keybinding:{}", shortcut);
                    let act_key = canonical_id.clone();
                    next_cache.insert(
                        act_key.clone(),
                        QuicklinkTarget::CopyKeybinding(shortcut.clone()),
                    );

                    sink.push(SearchCandidate {
                        provider_id: provider_id.clone(),
                        canonical_id,
                        generation: query_generation,
                        title: shortcut.clone(),
                        subtitle: Some(label.clone()),
                        aliases: Vec::new(),
                        keywords: Vec::new(),
                        category: ResultCategory::Keybinding,
                        latency: LatencyClass::Instant,
                        completion: CompletionState::Complete,
                        icon: SearchResultIcon::Named(IconName::Star),
                        activation_verb: "Copy shortcut".to_string(),
                        match_positions: Vec::new(),
                        activation: SearchActivation::new(act_key),
                    });
                }
            }
            _ => {}
        }

        self.cached_targets
            .lock()
            .unwrap()
            .replace(query_generation, next_cache);
    }

    fn activate(&self, activation: SearchActivation) -> Result<ActionResult, SearchError> {
        let target = self
            .cached_targets
            .lock()
            .unwrap()
            .get_cloned(&activation.payload)
            .ok_or_else(|| SearchError::NotFound(activation.payload.clone()))?;

        match target {
            QuicklinkTarget::OpenPath(path) => Ok(ActionResult::OpenPath(path)),
            QuicklinkTarget::OpenUri(uri) => Ok(ActionResult::OpenUri(uri)),
            QuicklinkTarget::ExecuteCommand(cmd) => Ok(ActionResult::ExecuteCommand(cmd)),
            QuicklinkTarget::OpenWeb(url) => Ok(ActionResult::OpenWeb(url)),
            QuicklinkTarget::CopyKeybinding(shortcut) => Ok(ActionResult::CopyKeybinding(shortcut)),
        }
    }
}

fn percent_encode_query(query: &str) -> String {
    let mut encoded = String::with_capacity(query.len());
    for byte in query.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else if byte == b' ' {
            encoded.push('+');
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::overview_search::sink::SinkConfig;

    fn test_keybindings() -> Vec<(String, String)> {
        vec![
            ("Super+Space".to_string(), "Toggle Overview".to_string()),
            ("Super+Q".to_string(), "Close Window".to_string()),
        ]
    }

    #[test]
    fn test_quicklinks_declared_modes_and_prefix_icons() {
        let provider = QuicklinksSearchProvider::new(test_keybindings());
        assert_eq!(
            provider.declared_modes().as_ref(),
            &[
                SearchMode::Default,
                SearchMode::Command,
                SearchMode::WebSearch,
                SearchMode::Keybindings,
            ]
        );
        assert_eq!(
            provider.prefix_icon(SearchMode::Command),
            Some(IconName::Terminal)
        );
        assert_eq!(
            provider.prefix_icon(SearchMode::WebSearch),
            Some(IconName::Search)
        );
        assert_eq!(
            provider.prefix_icon(SearchMode::Keybindings),
            Some(IconName::Star)
        );
        assert_eq!(provider.prefix_icon(SearchMode::Default), None);
    }

    #[test]
    fn test_path_and_uri_candidates_in_default_mode() {
        let provider = QuicklinksSearchProvider::new(test_keybindings());
        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("/tmp", SearchMode::Default, "/tmp", 1),
            sink.clone(),
        );

        let candidates = sink.snapshot();
        assert!(
            candidates
                .iter()
                .any(|c| c.category == ResultCategory::FilePath)
        );

        let sink_uri = SearchSink::new(2, SinkConfig::default());
        provider.search(
            SearchRequest::new(
                "https://example.com",
                SearchMode::Default,
                "https://example.com",
                2,
            ),
            sink_uri.clone(),
        );
        let candidates_uri = sink_uri.snapshot();
        assert!(
            candidates_uri
                .iter()
                .any(|c| c.category == ResultCategory::Uri)
        );
    }

    #[test]
    fn test_keybinding_search_and_activation() {
        let provider = QuicklinksSearchProvider::new(test_keybindings());
        let sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("<Super", SearchMode::Keybindings, "Super", 1),
            sink.clone(),
        );

        let candidates = sink.snapshot();
        assert_eq!(candidates.len(), 2);
        let cand = candidates
            .iter()
            .find(|c| c.title == "Super+Space")
            .unwrap();

        let result = provider.activate(cand.activation.clone()).unwrap();
        match result {
            ActionResult::CopyKeybinding(shortcut) => assert_eq!(shortcut, "Super+Space"),
            other => panic!("expected CopyKeybinding, got {other:?}"),
        }
    }

    #[test]
    fn test_command_and_web_search_activations() {
        let provider = QuicklinksSearchProvider::new(Vec::new());

        // Web search activation
        let sink_web = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("?rust lang", SearchMode::WebSearch, "rust lang", 1),
            sink_web.clone(),
        );
        let web_cand = &sink_web.snapshot()[0];
        let web_res = provider.activate(web_cand.activation.clone()).unwrap();
        match web_res {
            ActionResult::OpenWeb(url) => {
                assert_eq!(url, "https://www.google.com/search?q=rust+lang")
            }
            other => panic!("expected OpenWeb, got {other:?}"),
        }

        // Path activation
        let sink_path = SearchSink::new(2, SinkConfig::default());
        provider.search(
            SearchRequest::new("/tmp", SearchMode::Default, "/tmp", 2),
            sink_path.clone(),
        );
        let path_cand = sink_path
            .snapshot()
            .into_iter()
            .find(|c| c.category == ResultCategory::FilePath)
            .unwrap();
        let path_res = provider.activate(path_cand.activation).unwrap();
        match path_res {
            ActionResult::OpenPath(p) => assert_eq!(p, PathBuf::from("/tmp")),
            other => panic!("expected OpenPath, got {other:?}"),
        }
    }

    #[test]
    fn test_quicklinks_activation_cache_replaces_previous_generation() {
        let provider = QuicklinksSearchProvider::new(Vec::new());
        let first_sink = SearchSink::new(1, SinkConfig::default());
        provider.search(
            SearchRequest::new("?first", SearchMode::WebSearch, "first", 1),
            first_sink.clone(),
        );
        let stale_activation = first_sink.snapshot()[0].activation.clone();

        let second_sink = SearchSink::new(2, SinkConfig::default());
        provider.search(
            SearchRequest::new("?second", SearchMode::WebSearch, "second", 2),
            second_sink.clone(),
        );

        assert_eq!(provider.cached_targets.lock().unwrap().len(), 1);
        assert!(matches!(
            provider.activate(stale_activation),
            Err(SearchError::NotFound(_))
        ));
        assert!(matches!(
            provider.activate(second_sink.snapshot()[0].activation.clone()),
            Ok(ActionResult::OpenWeb(_))
        ));
    }
}
