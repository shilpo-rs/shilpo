use std::collections::HashMap;

/// Provider-owned activation values for the newest completed search generation.
///
/// Replacing the whole generation keeps retained values bounded by one search,
/// while the generation fence prevents a slow, stale search from overwriting
/// activation state produced by a newer query.
pub(super) struct ActivationCache<T> {
    generation: Option<u64>,
    entries: HashMap<String, T>,
}

impl<T> Default for ActivationCache<T> {
    fn default() -> Self {
        Self {
            generation: None,
            entries: HashMap::new(),
        }
    }
}

impl<T> ActivationCache<T> {
    /// Atomically replaces the cache when `generation` is not stale.
    pub(super) fn replace(&mut self, generation: u64, entries: HashMap<String, T>) -> bool {
        if self
            .generation
            .is_some_and(|current_generation| generation < current_generation)
        {
            return false;
        }

        self.generation = Some(generation);
        self.entries = entries;
        true
    }

    pub(super) fn get_cloned(&self, key: &str) -> Option<T>
    where
        T: Clone,
    {
        self.entries.get(key).cloned()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_generation_cannot_replace_newer_activation_state() {
        let mut cache = ActivationCache::default();
        assert!(cache.replace(2, HashMap::from([("new".to_string(), "value")])));
        assert!(!cache.replace(1, HashMap::from([("stale".to_string(), "value")])));

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get_cloned("new"), Some("value"));
        assert_eq!(cache.get_cloned("stale"), None);
    }
}
