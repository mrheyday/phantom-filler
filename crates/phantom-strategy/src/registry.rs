//! Strategy registry for dynamic strategy management.

use std::sync::Arc;

use dashmap::DashMap;
use phantom_common::traits::FillStrategy;
use tracing::{debug, info, warn};

/// Entry in the strategy registry, wrapping a strategy with its enabled state.
struct RegistryEntry {
    strategy: Arc<dyn FillStrategy>,
    enabled: bool,
}

/// Thread-safe registry for managing fill strategies at runtime.
///
/// Strategies can be registered, unregistered, enabled, and disabled dynamically.
/// The registry maintains strategies keyed by name and provides sorted iteration
/// by priority.
pub struct StrategyRegistry {
    strategies: DashMap<String, RegistryEntry>,
}

impl StrategyRegistry {
    /// Creates a new empty registry.
    pub fn new() -> Self {
        Self {
            strategies: DashMap::new(),
        }
    }

    /// Registers a strategy. Replaces any existing strategy with the same name.
    pub fn register(&self, strategy: Arc<dyn FillStrategy>) {
        let name = strategy.name().to_string();
        let priority = strategy.priority();
        info!(name = %name, priority, "registering strategy");
        self.strategies.insert(
            name,
            RegistryEntry {
                strategy,
                enabled: true,
            },
        );
    }

    /// Unregisters a strategy by name. Returns true if removed.
    pub fn unregister(&self, name: &str) -> bool {
        if self.strategies.remove(name).is_some() {
            info!(name = %name, "unregistered strategy");
            true
        } else {
            warn!(name = %name, "attempted to unregister unknown strategy");
            false
        }
    }

    /// Returns a strategy by name, if it exists and is enabled.
    pub fn get(&self, name: &str) -> Option<Arc<dyn FillStrategy>> {
        self.strategies.get(name).and_then(|entry| {
            if entry.enabled {
                Some(Arc::clone(&entry.strategy))
            } else {
                None
            }
        })
    }

    /// Returns all enabled strategies, sorted by priority (lower = higher priority).
    pub fn active_strategies(&self) -> Vec<Arc<dyn FillStrategy>> {
        let mut strategies: Vec<_> = self
            .strategies
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| Arc::clone(&entry.strategy))
            .collect();
        strategies.sort_by_key(|s| s.priority());
        strategies
    }

    /// Returns all registered strategy names and their enabled state.
    pub fn list(&self) -> Vec<(String, bool, u32)> {
        let mut entries: Vec<_> = self
            .strategies
            .iter()
            .map(|entry| {
                (
                    entry.key().clone(),
                    entry.enabled,
                    entry.strategy.priority(),
                )
            })
            .collect();
        entries.sort_by_key(|(_, _, priority)| *priority);
        entries
    }

    /// Enables a strategy by name. Returns true if found.
    pub fn enable(&self, name: &str) -> bool {
        if let Some(mut entry) = self.strategies.get_mut(name) {
            entry.enabled = true;
            debug!(name = %name, "enabled strategy");
            true
        } else {
            warn!(name = %name, "attempted to enable unknown strategy");
            false
        }
    }

    /// Disables a strategy by name. Returns true if found.
    pub fn disable(&self, name: &str) -> bool {
        if let Some(mut entry) = self.strategies.get_mut(name) {
            entry.enabled = false;
            debug!(name = %name, "disabled strategy");
            true
        } else {
            warn!(name = %name, "attempted to disable unknown strategy");
            false
        }
    }

    /// Returns the number of registered strategies (both enabled and disabled).
    pub fn len(&self) -> usize {
        self.strategies.len()
    }

    /// Returns true if no strategies are registered.
    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Returns the number of enabled strategies.
    pub fn active_count(&self) -> usize {
        self.strategies.iter().filter(|e| e.enabled).count()
    }
}

impl Default for StrategyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use phantom_common::error::StrategyResult;
    use phantom_common::traits::{EvaluationContext, FillDecision};
    use phantom_common::types::DutchAuctionOrder;

    /// A mock strategy for testing.
    struct MockStrategy {
        name: String,
        priority: u32,
    }

    impl MockStrategy {
        fn new(name: &str, priority: u32) -> Self {
            Self {
                name: name.to_string(),
                priority,
            }
        }
    }

    #[async_trait]
    impl FillStrategy for MockStrategy {
        fn name(&self) -> &str {
            &self.name
        }

        fn priority(&self) -> u32 {
            self.priority
        }

        async fn evaluate(
            &self,
            _order: &DutchAuctionOrder,
            _context: &EvaluationContext,
        ) -> StrategyResult<Option<FillDecision>> {
            Ok(None)
        }
    }

    #[test]
    fn register_and_list() {
        let registry = StrategyRegistry::new();
        registry.register(Arc::new(MockStrategy::new("alpha", 10)));
        registry.register(Arc::new(MockStrategy::new("beta", 5)));

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.active_count(), 2);

        let list = registry.list();
        assert_eq!(list[0].0, "beta"); // priority 5 first
        assert_eq!(list[1].0, "alpha"); // priority 10 second
    }

    #[test]
    fn unregister() {
        let registry = StrategyRegistry::new();
        registry.register(Arc::new(MockStrategy::new("alpha", 10)));
        assert!(registry.unregister("alpha"));
        assert!(!registry.unregister("alpha"));
        assert!(registry.is_empty());
    }

    #[test]
    fn enable_disable() {
        let registry = StrategyRegistry::new();
        registry.register(Arc::new(MockStrategy::new("alpha", 10)));

        assert!(registry.disable("alpha"));
        assert_eq!(registry.active_count(), 0);
        assert!(registry.get("alpha").is_none());

        assert!(registry.enable("alpha"));
        assert_eq!(registry.active_count(), 1);
        assert!(registry.get("alpha").is_some());
    }

    #[test]
    fn enable_disable_unknown() {
        let registry = StrategyRegistry::new();
        assert!(!registry.enable("unknown"));
        assert!(!registry.disable("unknown"));
    }

    #[test]
    fn active_strategies_sorted_by_priority() {
        let registry = StrategyRegistry::new();
        registry.register(Arc::new(MockStrategy::new("low", 100)));
        registry.register(Arc::new(MockStrategy::new("high", 1)));
        registry.register(Arc::new(MockStrategy::new("mid", 50)));

        let active = registry.active_strategies();
        assert_eq!(active.len(), 3);
        assert_eq!(active[0].name(), "high");
        assert_eq!(active[1].name(), "mid");
        assert_eq!(active[2].name(), "low");
    }

    #[test]
    fn disabled_strategies_excluded_from_active() {
        let registry = StrategyRegistry::new();
        registry.register(Arc::new(MockStrategy::new("alpha", 1)));
        registry.register(Arc::new(MockStrategy::new("beta", 2)));
        registry.disable("alpha");

        let active = registry.active_strategies();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name(), "beta");
    }

    #[test]
    fn replace_existing_strategy() {
        let registry = StrategyRegistry::new();
        registry.register(Arc::new(MockStrategy::new("alpha", 10)));
        registry.register(Arc::new(MockStrategy::new("alpha", 5)));

        assert_eq!(registry.len(), 1);
        let list = registry.list();
        assert_eq!(list[0].2, 5); // updated priority
    }

    #[test]
    fn default_creates_empty_registry() {
        let registry = StrategyRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.active_count(), 0);
    }
}
