//! Shared application state for the API server.

use std::sync::Arc;

use phantom_inventory::balance::BalanceTracker;
use phantom_inventory::pnl::PnlTracker;
use phantom_inventory::risk::RiskManager;
use phantom_metrics::health::HealthRegistry;

use crate::ws::EventBus;

/// Shared state available to all API handlers.
///
/// Cloned into each request handler via axum's `State` extractor.
/// All fields use `Arc` for cheap cloning and shared ownership.
#[derive(Clone)]
pub struct AppState {
    /// Health registry for liveness/readiness probes.
    pub health: Arc<HealthRegistry>,
    /// P&L tracker for fill accounting.
    pub pnl: Arc<PnlTracker>,
    /// Balance tracker for wallet balances.
    pub balances: Arc<BalanceTracker>,
    /// Risk manager for risk exposure data.
    pub risk: Arc<RiskManager>,
    /// Event bus for WebSocket broadcasting.
    pub event_bus: Arc<EventBus>,
}

impl AppState {
    /// Creates a new application state with the given components.
    pub fn new(
        health: Arc<HealthRegistry>,
        pnl: Arc<PnlTracker>,
        balances: Arc<BalanceTracker>,
        risk: Arc<RiskManager>,
        event_bus: Arc<EventBus>,
    ) -> Self {
        Self {
            health,
            pnl,
            balances,
            risk,
            event_bus,
        }
    }

    /// Creates a state with default (empty) components, useful for testing.
    pub fn with_defaults() -> Self {
        Self {
            health: Arc::new(HealthRegistry::default()),
            pnl: Arc::new(PnlTracker::with_defaults()),
            balances: Arc::new(BalanceTracker::with_defaults()),
            risk: Arc::new(RiskManager::with_defaults()),
            event_bus: Arc::new(EventBus::with_defaults()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_with_defaults() {
        let state = AppState::with_defaults();
        assert_eq!(state.health.component_count(), 0);
        assert_eq!(state.pnl.fill_count(), 0);
    }

    #[test]
    fn state_is_clone() {
        let state = AppState::with_defaults();
        let cloned = state.clone();
        // Both point to the same underlying data.
        assert_eq!(
            Arc::strong_count(&state.health),
            Arc::strong_count(&cloned.health)
        );
    }
}
